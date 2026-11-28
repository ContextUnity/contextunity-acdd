//! Worktree directory walking and digest helpers for `DocsDomainProvider`.

use super::super::common::combine_file_hashes;
use acdd_core::format::sha256_hex;
use acdd_core::fs::{checked_child, is_doc_path};
use acdd_core::path_policy::{
    expand_glob_braces, glob_matches, normalize_rel_path, scopes_overlap, should_skip_walk_dir,
    WorkspacePathPolicy,
};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub(super) fn compute_docs_digest(
    root: &Path,
    scope: Option<&[String]>,
    policy: &WorkspacePathPolicy,
) -> Result<String> {
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let matched = collect_matching_worktree_files_where(root, |rel| {
        if !is_doc_path(rel) {
            return false;
        }
        if let Some(scope_list) = scope {
            if !scope_list.is_empty()
                && !scope_list.iter().any(|pat| glob_matches(pat, rel))
                && !policy.is_scope_admitted(rel, scope_list)
            {
                return false;
            }
        }
        true
    });
    for (abs, rel) in matched {
        if let Ok(bytes) = fs::read(&abs) {
            digests.insert(rel, sha256_hex(&bytes));
        }
    }
    Ok(combine_file_hashes(&digests))
}

fn split_literal_glob_prefix(replaced: &str) -> (Vec<&str>, bool) {
    let mut literal_segs: Vec<&str> = Vec::new();
    let mut has_wildcard = false;
    for seg in replaced.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            literal_segs.clear();
            has_wildcard = false;
            break;
        }
        if seg.contains('*') || seg.contains('?') || seg.contains('[') {
            has_wildcard = true;
            break;
        }
        literal_segs.push(seg);
    }
    (literal_segs, has_wildcard)
}

fn for_each_glob_literal_prefix<F>(pattern: &str, mut on_prefix: F)
where
    F: FnMut(&[&str], bool),
{
    for variant in expand_glob_braces(pattern) {
        let trimmed = variant.trim().trim_start_matches("./");
        if trimmed.is_empty() {
            continue;
        }
        let replaced = trimmed.replace('\\', "/");
        let (literal_segs, has_wildcard) = split_literal_glob_prefix(&replaced);
        on_prefix(&literal_segs, has_wildcard);
    }
}

pub(super) fn collect_matching_worktree_files_for_globs<F>(
    worktree: &Path,
    globs: &[String],
    mut accept: F,
) -> Vec<(PathBuf, String)>
where
    F: FnMut(&str) -> bool,
{
    if globs.is_empty() {
        return Vec::new();
    }
    let mut walk_roots: BTreeSet<PathBuf> = BTreeSet::new();
    let mut direct_files: BTreeSet<String> = BTreeSet::new();
    for pattern in globs {
        for_each_glob_literal_prefix(pattern, |literal_segs, has_wildcard| {
            if has_wildcard && literal_segs.is_empty() {
                walk_roots.insert(worktree.to_path_buf());
            } else if !literal_segs.is_empty() {
                let prefix = literal_segs.join("/");
                if let Ok(abs) = checked_child(worktree, Path::new(&prefix)) {
                    if abs.is_dir() {
                        walk_roots.insert(abs);
                    } else if !has_wildcard && abs.is_file() {
                        direct_files.insert(prefix);
                    }
                }
            }
        });
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for rel in direct_files {
        if accept(&rel) && seen.insert(rel.clone()) {
            if let Ok(abs) = checked_child(worktree, Path::new(&rel)) {
                if fs::symlink_metadata(&abs)
                    .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
                {
                    out.push((abs, rel));
                }
            }
        }
    }
    for root_dir in walk_roots {
        for (abs, rel) in collect_matching_worktree_files_under_where(worktree, &root_dir, |r| {
            accept(r)
        }) {
            if seen.insert(rel.clone()) {
                out.push((abs, rel));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

pub(super) fn collect_matching_worktree_files_where<F>(
    worktree: &Path,
    accept: F,
) -> Vec<(PathBuf, String)>
where
    F: FnMut(&str) -> bool,
{
    collect_matching_worktree_files_under_where(worktree, worktree, accept)
}

fn collect_matching_worktree_files_under_where<F>(
    worktree: &Path,
    start_dir: &Path,
    mut accept: F,
) -> Vec<(PathBuf, String)>
where
    F: FnMut(&str) -> bool,
{
    if !start_dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(start_dir)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_type().is_symlink() {
                return false;
            }
            if !e.file_type().is_dir() {
                return true;
            }
            let parent_name = e
                .path()
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str());
            let Some(dir_name) = e.file_name().to_str() else {
                return true;
            };
            !should_skip_walk_dir(parent_name, dir_name)
        })
        .filter_map(Result::ok)
    {
        if entry.file_type().is_symlink() || !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel_raw) = entry.path().strip_prefix(worktree) else {
            continue;
        };
        let Some(rel) = normalize_rel_path(&rel_raw.to_string_lossy()) else {
            continue;
        };
        if !accept(&rel) {
            continue;
        }
        let Ok(abs) = checked_child(worktree, Path::new(&rel)) else {
            continue;
        };
        if fs::symlink_metadata(&abs)
            .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
        {
            out.push((abs, rel));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn collect_pattern_agent_dirs(
    worktree: &Path,
    pattern: &str,
    found: &mut BTreeMap<String, bool>,
    walk_roots: &mut BTreeSet<PathBuf>,
) {
    for_each_glob_literal_prefix(pattern, |literal_segs, has_wildcard| {
        let mut prefix = String::new();
        for seg in literal_segs {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            let cand = format!("{prefix}/AGENTS.md");
            if checked_child(worktree, Path::new(&cand)).is_ok_and(|p| p.is_file()) {
                found.insert(cand, true);
            }
        }
        if has_wildcard && prefix.is_empty() {
            walk_roots.insert(worktree.to_path_buf());
        } else if !prefix.is_empty() {
            if let Ok(abs) = checked_child(worktree, Path::new(&prefix)) {
                if abs.is_dir() {
                    walk_roots.insert(abs);
                }
            }
        }
    });
}

fn walk_agents_md_root(
    worktree: &Path,
    root_dir: &Path,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
    found: &mut BTreeMap<String, bool>,
) {
    for (_, rel) in collect_matching_worktree_files_under_where(worktree, root_dir, |rel| {
        if rel == "AGENTS.md" || !rel.ends_with("/AGENTS.md") {
            return false;
        }
        let Some(dir_rel) = rel.rsplit_once('/').map(|(d, _)| d.to_owned()) else {
            return false;
        };
        scopes_overlap(admitted_scope, &[dir_rel]) || policy.is_scope_admitted(rel, admitted_scope)
    }) {
        found.entry(rel).or_insert(false);
    }
}

pub(super) fn collect_scoped_agents_md(
    worktree: &Path,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> BTreeMap<String, bool> {
    let mut found = BTreeMap::new();
    if admitted_scope.is_empty() || !worktree.exists() {
        return found;
    }

    let mut walk_roots: BTreeSet<PathBuf> = BTreeSet::new();
    for pattern in admitted_scope {
        collect_pattern_agent_dirs(worktree, pattern, &mut found, &mut walk_roots);
    }
    for root_dir in walk_roots {
        walk_agents_md_root(worktree, &root_dir, admitted_scope, policy, &mut found);
    }

    found
}
