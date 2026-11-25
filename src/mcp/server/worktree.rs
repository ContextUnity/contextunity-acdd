//! Explicit worktree and linked-workspace target root/database resolution.

use super::types::{rebase_invocation_settings, Server};
use acdd_core::fs::canonicalize_macos_var;
use acdd_index::scanner;
use anyhow::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

fn canonicalize_existing_dir(path: &std::path::Path) -> Result<PathBuf> {
    let canon = path.canonicalize()?;
    Ok(canonicalize_macos_var(&canon))
}

impl Server {
    /// Resolves an explicit worktree using a caller-supplied root [`scanner::Adapter`] when available.
    pub(crate) fn resolve_target_root_and_db_with_adapter(
        &self,
        requested_worktree: Option<&str>,
        root_adapter: Option<&scanner::Adapter>,
    ) -> Result<(PathBuf, PathBuf)> {
        let root = canonicalize_existing_dir(&self.root)?;
        let target = match requested_worktree {
            None => root.clone(),
            Some(requested) => {
                let requested = requested.trim();
                if requested.is_empty() {
                    root.clone()
                } else {
                    let repository = acdd_core::git::primary_worktree_root(&root)
                        .map(|p| canonicalize_macos_var(&p))
                        .unwrap_or_else(|| root.clone());
                    let loaded_owned;
                    let adapter_ref = match root_adapter {
                        Some(adapter) => Some(adapter),
                        None => {
                            loaded_owned = self.load_adapter(&root).ok();
                            loaded_owned.as_ref()
                        }
                    };
                    let linked: Vec<(String, PathBuf)> = adapter_ref
                        .map(|adapter| {
                            adapter
                                .linked_workspaces
                                .iter()
                                .filter_map(|lw| {
                                    canonicalize_existing_dir(&lw.path)
                                        .ok()
                                        .map(|p| (lw.name.clone(), p))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    resolve_explicit_worktree_with_linked(&root, &repository, &linked, requested)?
                }
            }
        };
        let db = if target == root {
            self.db.clone()
        } else if let Some(settings) = &self.invocation {
            rebase_invocation_settings(settings, &target).code_db
        } else {
            acdd_core::config::resolve_code_db_path(&target, None)
        };
        Ok((target, db))
    }
}

fn collect_worktree_candidates(
    repository: &std::path::Path,
    repo_common_dir: Option<&std::path::Path>,
    linked_workspaces: &[(String, PathBuf)],
) -> Result<BTreeSet<PathBuf>> {
    let mut candidates = BTreeSet::new();
    if repository.join(".git").exists() || acdd_core::git::git_common_dir(repository).is_ok() {
        let output =
            acdd_core::git::git_command(repository, &["worktree", "list", "--porcelain", "-z"])?;
        if !output.status.success() {
            anyhow::bail!(
                "WORKTREE_RESOLUTION_FAILED: could not list Git worktrees for '{}': {}",
                repository.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let output = String::from_utf8(output.stdout)?;
        for field in output.split('\0') {
            if let Some(path) = field.strip_prefix("worktree ") {
                insert_worktree_candidate(std::path::Path::new(path), &mut candidates)?;
            }
        }
    }

    let worktrees_dir = repository.join(".worktrees");
    match std::fs::symlink_metadata(&worktrees_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(meta) if meta.is_dir() => match std::fs::read_dir(&worktrees_dir) {
            Ok(entries) => {
                let mut paths = Vec::new();
                for entry in entries {
                    let entry = entry?;
                    let path = entry.path();
                    if std::fs::symlink_metadata(&path)
                        .is_ok_and(|entry_meta| entry_meta.file_type().is_symlink())
                    {
                        continue;
                    }
                    paths.push(path);
                }
                paths.sort();
                for path in paths {
                    if let Some(rcd) = repo_common_dir {
                        let Ok(cand_common) = acdd_core::git::git_common_dir(&path) else {
                            continue;
                        };
                        if canonicalize_macos_var(&cand_common) != rcd {
                            continue;
                        }
                    }
                    insert_worktree_candidate(&path, &mut candidates)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        },
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    for (_, lw_path) in linked_workspaces {
        insert_worktree_candidate(lw_path, &mut candidates)?;
    }

    Ok(candidates)
}

fn is_owned_worktree_path(
    canon: &std::path::Path,
    root: &std::path::Path,
    repo_common_dir: Option<&std::path::Path>,
    linked_workspaces: &[(String, PathBuf)],
) -> bool {
    if let Some(rcd) = repo_common_dir {
        if canon == rcd || canon.starts_with(rcd) {
            return false;
        }
        if let Ok(cand_common) = acdd_core::git::git_common_dir(canon) {
            if canonicalize_macos_var(&cand_common) == rcd {
                return true;
            }
        }
    }
    if canon == root {
        return true;
    }
    for (_, lw_path) in linked_workspaces {
        if canon == lw_path || canon.starts_with(lw_path) {
            return true;
        }
        if let (Ok(cand_common), Ok(lw_common)) = (
            acdd_core::git::git_common_dir(canon),
            acdd_core::git::git_common_dir(lw_path),
        ) {
            if canonicalize_macos_var(&cand_common) == canonicalize_macos_var(&lw_common) {
                return true;
            }
        }
    }
    repo_common_dir.is_none() && canon.starts_with(root)
}

fn resolve_explicit_worktree_with_linked(
    root: &std::path::Path,
    repository: &std::path::Path,
    linked_workspaces: &[(String, PathBuf)],
    requested: &str,
) -> Result<PathBuf> {
    let root = canonicalize_existing_dir(root)?;
    let repository = canonicalize_existing_dir(repository)?;
    if requested == "." {
        return Ok(root);
    }

    let repo_common_dir = acdd_core::git::git_common_dir(&repository)
        .ok()
        .map(|p| canonicalize_macos_var(&p));

    let requested_path = std::path::Path::new(requested);
    if requested_path.is_absolute() || requested.contains('/') || requested.contains('\\') {
        let path = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            root.join(requested_path)
        };
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                let canon = canonicalize_existing_dir(&path)?;
                if !is_owned_worktree_path(
                    &canon,
                    &root,
                    repo_common_dir.as_deref(),
                    linked_workspaces,
                ) {
                    anyhow::bail!(
                        "WORKTREE_NOT_FOUND: explicit worktree path '{}' is outside this repository's git_common_dir and linked_workspaces",
                        path.display()
                    );
                }
                return Ok(canon);
            }
            Ok(_) => anyhow::bail!(
                "WORKTREE_NOT_FOUND: explicit worktree path '{}' is not a directory",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("WORKTREE_NOT_FOUND: explicit worktree path '{requested}' not found")
            }
            Err(error) => return Err(error.into()),
        }
    }

    let candidates =
        collect_worktree_candidates(&repository, repo_common_dir.as_deref(), linked_workspaces)?;

    let linked_names: BTreeMap<&std::path::Path, &str> = linked_workspaces
        .iter()
        .map(|(name, path)| (path.as_path(), name.as_str()))
        .collect();

    for tier in [
        WorktreeMatchTier::Exact,
        WorktreeMatchTier::Prefix,
        WorktreeMatchTier::Suffix,
        WorktreeMatchTier::Substring,
    ] {
        let mut first_match = None;
        let mut ambiguous = false;
        for candidate in &candidates {
            let alias = linked_names.get(candidate.as_path()).copied();
            if !candidate_matches_with_alias(candidate, alias, requested, tier) {
                continue;
            }
            if first_match.is_some() {
                ambiguous = true;
                break;
            }
            first_match = Some(candidate);
        }
        if ambiguous {
            let matches = candidates
                .iter()
                .filter(|candidate| {
                    let alias = linked_names.get(candidate.as_path()).copied();
                    candidate_matches_with_alias(candidate, alias, requested, tier)
                })
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "WORKTREE_AMBIGUOUS: '{requested}' matches {matches}. Do: pass a more specific worktree name or path."
            );
        }
        if let Some(candidate) = first_match {
            return Ok(candidate.clone());
        }
    }

    anyhow::bail!(
        "WORKTREE_NOT_FOUND: no worktree matching '{requested}'. Do: pass a path or unique worktree name."
    )
}

fn insert_worktree_candidate(
    path: &std::path::Path,
    candidates: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        Ok(metadata) if metadata.is_dir() => {
            candidates.insert(canonicalize_existing_dir(path)?);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum WorktreeMatchTier {
    Exact,
    Prefix,
    Suffix,
    Substring,
}

fn name_matches(name: &str, requested: &str, tier: WorktreeMatchTier) -> bool {
    match tier {
        WorktreeMatchTier::Exact => name == requested,
        WorktreeMatchTier::Prefix => {
            name.starts_with(requested) || matches_numeric_worktree_prefix(name, requested)
        }
        WorktreeMatchTier::Suffix => name.ends_with(requested),
        WorktreeMatchTier::Substring => name.contains(requested),
    }
}

fn candidate_matches_with_alias(
    path: &std::path::Path,
    alias: Option<&str>,
    requested: &str,
    tier: WorktreeMatchTier,
) -> bool {
    let dir_matches = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name_matches(name, requested, tier));
    dir_matches || alias.is_some_and(|name| name_matches(name, requested, tier))
}

fn matches_numeric_worktree_prefix(name: &str, requested: &str) -> bool {
    if requested.is_empty() || !requested.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let digit_end = name
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(name.len());
    if digit_end == 0 || !name[digit_end..].is_empty() && !name[digit_end..].starts_with('-') {
        return false;
    }
    normalize_milestone_digits(&name[..digit_end]) == normalize_milestone_digits(requested)
}

fn normalize_milestone_digits(digits: &str) -> &str {
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        "0"
    } else {
        digits
    }
}

#[cfg(test)]
#[test]
fn worktree_name_contract() {
    let _cleanup = acdd_core::fs::ScopedTempDir::create("acdd-worktree-name").unwrap();
    let base = _cleanup.path();
    let root = base.join("repo");
    let external = base.join("unrelated-external");
    let linked_ws = base.join("linked-ws");
    let selected = root.join(".worktrees/049-selected");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::create_dir_all(&linked_ws).unwrap();

    let canon_root = canonicalize_existing_dir(&root).unwrap();
    let selected = canonicalize_existing_dir(&selected).unwrap();
    let canon_linked = canonicalize_existing_dir(&linked_ws).unwrap();

    let server = Server::new(root.clone(), root.join(".acdd/code-map.sqlite"));
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(None, None).unwrap().0,
        canon_root
    );
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(Some(""), None).unwrap().0,
        canon_root
    );
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(Some("   "), None).unwrap().0,
        canon_root
    );

    let result = resolve_explicit_worktree_with_linked(&root, &root, &[], "selected");
    let aliases: Vec<_> = ["049", "49", "049-sel", "9-selec"]
        .into_iter()
        .map(|alias| (alias, resolve_explicit_worktree_with_linked(&root, &root, &[], alias)))
        .collect();

    let outside_err = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        external.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(outside_err.to_string().contains("WORKTREE_NOT_FOUND"));

    let linked_by_alias = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        "backend",
    )
    .unwrap();
    assert_eq!(linked_by_alias, canon_linked);

    let linked_by_path = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        canon_linked.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(linked_by_path, canon_linked);

    std::fs::create_dir_all(root.join(".worktrees/050-selected")).unwrap();
    let ambiguous = resolve_explicit_worktree_with_linked(&root, &root, &[], "selected");
    let missing = resolve_explicit_worktree_with_linked(&root, &root, &[], "absent");
    assert_eq!(result.unwrap(), selected);
    for (alias, result) in aliases {
        assert_eq!(
            result.unwrap_or_else(|error| panic!("{alias}: {error}")),
            selected
        );
    }
    assert!(ambiguous
        .unwrap_err()
        .to_string()
        .contains("WORKTREE_AMBIGUOUS"));
    assert!(missing
        .unwrap_err()
        .to_string()
        .contains("WORKTREE_NOT_FOUND"));

    #[cfg(unix)]
    {
        // Symlink inside `.worktrees` targeting an external directory must never be admitted.
        let escaped_link = root.join(".worktrees/999-escaped");
        std::os::unix::fs::symlink(&external, &escaped_link).unwrap();
        for probe in ["999-escaped", "999", ".worktrees/999-escaped"] {
            let err = resolve_explicit_worktree_with_linked(&root, &root, &[], probe).unwrap_err();
            assert!(err.to_string().contains("WORKTREE_NOT_FOUND"), "{probe}: {err}");
        }
        // Symlinked `.worktrees` directory itself must also be skipped.
        let sym_repo = base.join("sym-repo");
        std::fs::create_dir_all(&sym_repo).unwrap();
        assert!(acdd_core::git::git_command(&sym_repo, &["init"]).unwrap().status.success());
        std::os::unix::fs::symlink(&external, sym_repo.join(".worktrees")).unwrap();
        let sym_err =
            resolve_explicit_worktree_with_linked(&sym_repo, &sym_repo, &[], "unrelated-external")
                .unwrap_err();
        assert!(sym_err.to_string().contains("WORKTREE_NOT_FOUND"), "{sym_err}");
        let dotgit_err =
            resolve_explicit_worktree_with_linked(&sym_repo, &sym_repo, &[], ".git").unwrap_err();
        assert!(dotgit_err.to_string().contains("WORKTREE_NOT_FOUND"), "{dotgit_err}");
    }
}
