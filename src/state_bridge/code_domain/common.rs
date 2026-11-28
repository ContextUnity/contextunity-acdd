//! Shared `CodeDomainProvider` struct and helper functions (`code_domain` leaf module).

pub(super) use super::super::common::{collect_git_diff_paths, effective_baseline_ref};
use acdd_core::config::Adapter;
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::path_policy::{expand_glob_braces, normalize_rel_path, WorkspacePathPolicy};
use acdd_core::response::{Detail, QueryOptions};
use acdd_index::ast::extract_typed;
use acdd_index::db::paging;
use acdd_index::languages;
use acdd_index::models::tags::NodeKind;
use acdd_index::models::{Facts, Node};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(super) struct ParsedScopedFile {
    pub(super) source_digest: String,
    pub(super) facts: Facts,
}

pub(super) struct ScopedCodeSnapshot {
    pub(super) worktree: PathBuf,
    pub(super) files: Vec<String>,
    pub(super) parsed: BTreeMap<String, ParsedScopedFile>,
}

/// Host `code` domain provider exposing `code.seam`, `code.signature_scope`, `code.lint`, and `code.*` context lenses.
pub struct CodeDomainProvider {
    pub(super) root: PathBuf,
    pub(super) config: Adapter,
    pub(super) code_db: Option<PathBuf>,
    pub(super) snapshot_cache: Mutex<Option<Arc<ScopedCodeSnapshot>>>,
}

impl CodeDomainProvider {
    /// Construct a `CodeDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self::with_code_db(root, config, None)
    }

    /// Construct a `CodeDomainProvider` bound to `root`, workspace `config`, and an optional explicit `code_db` path.
    pub fn with_code_db(root: PathBuf, config: Adapter, code_db: Option<PathBuf>) -> Self {
        Self {
            root,
            config,
            code_db,
            snapshot_cache: Mutex::new(None),
        }
    }

    pub(super) fn index_generation(conn: Option<&Connection>) -> String {
        let Some(conn) = conn else {
            return "no-index".to_owned();
        };
        let opts = QueryOptions {
            limit: 1,
            offset: 0,
            detail: Detail::Compact,
            generation: None,
        };
        paging::generation(conn, &opts).unwrap_or_else(|_| "unknown".to_owned())
    }
}

pub(super) fn is_callable_kind(kind: &str) -> bool {
    kind == NodeKind::Function.as_str() || kind == NodeKind::Method.as_str()
}

pub(super) fn is_non_symbol_container(kind: &str) -> bool {
    kind == NodeKind::Module.as_str()
        || kind == "module_declaration"
        || kind == NodeKind::Field.as_str()
        || kind == "field_declaration"
}

pub(super) fn is_interface_like_kind(kind: &str) -> bool {
    kind == NodeKind::Trait.as_str() || kind == NodeKind::Interface.as_str()
}

pub(super) fn collect_scoped_worktree_files(
    worktree: &Path,
    scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Vec<String> {
    if scope.is_empty() || !worktree.exists() {
        return Vec::new();
    }
    let mut found = BTreeSet::new();
    let mut needs_walk = false;
    for entry in scope {
        for expanded in expand_glob_braces(entry) {
            if expanded.contains('*')
                || expanded.contains('?')
                || expanded.contains('[')
                || expanded.ends_with('/')
            {
                needs_walk = true;
                continue;
            }
            if let Some(norm) = normalize_rel_path(&expanded) {
                if let Ok(abs) = checked_child(worktree, Path::new(&norm)) {
                    if abs.is_dir() {
                        needs_walk = true;
                    } else if policy.is_candidate_included(&norm) && abs.is_file() {
                        found.insert(norm);
                    }
                }
            }
        }
    }
    if !needs_walk {
        return found.into_iter().collect();
    }
    for_each_candidate_file(worktree, policy, false, |_, rel_str| {
        if policy.is_scope_admitted(&rel_str, scope) {
            found.insert(rel_str);
        }
    });
    found.into_iter().collect()
}

pub(crate) fn for_each_candidate_file(
    worktree: &Path,
    policy: &WorkspacePathPolicy,
    skip_internal_artifacts: bool,
    mut f: impl FnMut(&Path, String),
) {
    let walker = ignore::WalkBuilder::new(worktree)
        .follow_links(false)
        .standard_filters(true)
        .hidden(false)
        .filter_entry(move |e| {
            !matches!(e.file_name().to_str(), Some(".git" | ".worktrees"))
                && (!skip_internal_artifacts
                    || !matches!(e.file_name().to_str(), Some("target" | ".acdd")))
        })
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(worktree) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if policy.is_candidate_included(&rel_str) {
            f(entry.path(), rel_str);
        }
    }
}

pub(super) fn collect_deleted_scoped_files(
    worktree: &Path,
    scope: &[String],
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Vec<String> {
    if scope.is_empty() {
        return Vec::new();
    }
    collect_git_diff_paths(worktree, policy, baseline, "--diff-filter=D", |norm| {
        policy.is_scope_admitted(norm, scope)
    })
}

pub(super) fn load_live_facts(
    worktree: &Path,
    rel_path: &str,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) {
    if live_facts.contains_key(rel_path) {
        return;
    }
    let Some(lang) = languages::for_path(Path::new(rel_path)) else {
        return;
    };
    let Ok(abs) = checked_child(worktree, Path::new(rel_path)) else {
        return;
    };
    let Ok(source) = fs::read_to_string(&abs) else {
        return;
    };
    file_hashes.insert(rel_path.to_owned(), sha256_hex(source.as_bytes()));
    if let Ok(typed) = extract_typed(rel_path, lang.id(), &source) {
        live_facts.insert(rel_path.to_owned(), typed.into_public());
    }
}

pub(super) fn node_matches_symbol(node: &Node, raw_symbol: &str) -> bool {
    let normalized = raw_symbol.replace("::", ".");
    node.name == raw_symbol
        || node.qualname == normalized
        || node.qualname.ends_with(&format!(".{normalized}"))
}

pub(super) fn contains_identifier(text: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    text.match_indices(ident).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + ident.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric() && c != '_')
            && after.is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

pub(super) fn normalize_sig(sig: &str) -> String {
    sig.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn public_callable_nodes(facts: &Facts) -> Vec<Node> {
    let exports: BTreeSet<String> = facts
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .filter_map(|n| n.details["exports"].as_array())
        .flatten()
        .filter(|e| e["module"].is_null() && e["type_only"] != Value::Bool(true))
        .filter_map(|e| e["local"].as_str().map(str::to_owned))
        .collect();
    facts
        .nodes
        .iter()
        .filter(|n| {
            is_callable_kind(n.kind.as_str())
                && !n.is_test
                && (n.details["is_public"].as_bool() == Some(true)
                    || n.details["visibility"].as_str() == Some("public")
                    || n.details["default_export"].as_bool() == Some(true)
                    || exports.contains(&n.name))
        })
        .cloned()
        .collect()
}
