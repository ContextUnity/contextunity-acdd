//! Linked workspace path validation and canonical root collection.

use super::schema::{LinkedWorkspace, LinkedWorkspaceConfig};
use super::{safe_relative, DEFAULT_IGNORED_NAMES};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn validate_linked_workspace_target(
    canonical_root: &Path,
    lw_name: &str,
    raw_path_str: &str,
) -> std::result::Result<PathBuf, String> {
    if raw_path_str.trim().is_empty() {
        return Err(format!("linked workspace '{lw_name}' has an empty path"));
    }
    let raw_path = Path::new(raw_path_str);
    let mut abs_target = if raw_path.is_absolute() {
        crate::fs::normalize_path(raw_path)
    } else {
        crate::fs::normalize_path(&canonical_root.join(raw_path))
    };

    if raw_path.is_relative() && safe_relative(raw_path) {
        let mut cursor = canonical_root.to_path_buf();
        for comp in raw_path.components() {
            let name = match comp {
                std::path::Component::CurDir => continue,
                std::path::Component::Normal(n) => n,
                _ => {
                    return Err(format!(
                        "linked workspace '{lw_name}' path '{raw_path_str}' is invalid"
                    ));
                }
            };
            cursor.push(name);
            match fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!(
                        "linked workspace '{lw_name}' path '{raw_path_str}' traverses or targets a symlink"
                    ));
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(format!(
                        "linked workspace '{lw_name}' path '{raw_path_str}' does not exist"
                    ));
                }
                Err(err) => {
                    return Err(format!(
                        "linked workspace '{lw_name}' path '{raw_path_str}' could not be inspected: {err}"
                    ));
                }
            }
        }
        abs_target = cursor;
    } else {
        if !abs_target.is_absolute() {
            if let Ok(cwd) = std::env::current_dir() {
                abs_target = crate::fs::normalize_path(&cwd.join(abs_target));
            }
        }
        if let Err(err) = crate::fs::check_ancestors_no_symlinks_abs(&abs_target) {
            return Err(format!(
                "linked workspace '{lw_name}' path '{raw_path_str}' traverses a symlink: {err}"
            ));
        }
    }

    let target_meta = if raw_path.is_absolute() {
        fs::metadata(&abs_target)
    } else {
        fs::symlink_metadata(&abs_target)
    };
    match target_meta {
        Ok(meta) => {
            if raw_path.is_relative() && meta.file_type().is_symlink() {
                return Err(format!(
                    "linked workspace '{lw_name}' path '{raw_path_str}' must not be a symlink"
                ));
            }
            if !meta.is_dir() {
                return Err(format!(
                    "linked workspace '{lw_name}' path '{raw_path_str}' is not a directory"
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "linked workspace '{lw_name}' path '{raw_path_str}' does not exist"
            ));
        }
        Err(err) => {
            return Err(format!(
                "linked workspace '{lw_name}' path '{raw_path_str}' could not be inspected: {err}"
            ));
        }
    }

    let canonical_ws = abs_target.canonicalize().map_err(|err| {
        format!("failed to canonicalize linked workspace '{lw_name}' path '{raw_path_str}': {err}")
    })?;
    if raw_path.is_relative() && safe_relative(raw_path) && !canonical_ws.starts_with(canonical_root)
    {
        return Err(format!(
            "linked workspace '{lw_name}' path '{raw_path_str}' escapes workspace root via symlink"
        ));
    }
    Ok(canonical_ws)
}

/// Collect canonicalized, root-confined directories for a linked workspace's `roots` and `docs`.
pub(super) fn collect_confined_canonical_roots(
    canonical_ws: &Path,
    raw_roots: Vec<String>,
    docs: Vec<String>,
) -> Vec<PathBuf> {
    let mut ws_roots = Vec::new();
    let entries = raw_roots
        .into_iter()
        .map(|item| (item, false))
        .chain(docs.into_iter().map(|item| (item, true)));
    for (item, skip_globs) in entries {
        if skip_globs && (item.contains('*') || item.contains('?')) {
            continue;
        }
        if item == "." {
            ws_roots.push(canonical_ws.to_path_buf());
            continue;
        }
        let cand = canonical_ws.join(Path::new(&item));
        if cand.exists() {
            if let Ok(c) = cand.canonicalize() {
                if c.starts_with(canonical_ws) {
                    ws_roots.push(c);
                }
            }
        }
    }
    ws_roots.sort();
    ws_roots.dedup();
    ws_roots
}

pub(super) fn resolve_linked_workspaces(
    canonical_root: &Path,
    configs: Vec<LinkedWorkspaceConfig>,
) -> (Vec<LinkedWorkspace>, Vec<String>) {
    let mut linked_workspaces = Vec::new();
    let mut linked_workspace_errors = Vec::new();
    let mut seen_lw_names = BTreeSet::new();

    for lw_cfg in configs {
        let trimmed_name = lw_cfg.name.trim();
        if trimmed_name.is_empty() {
            linked_workspace_errors.push("linked_workspaces entry has an empty name".to_owned());
            continue;
        }
        if !seen_lw_names.insert(trimmed_name.to_owned()) {
            linked_workspace_errors.push(format!(
                "duplicate linked_workspaces name '{trimmed_name}'"
            ));
            continue;
        }
        if !lw_cfg.enabled.unwrap_or(true) {
            continue;
        }

        let canonical_ws =
            match validate_linked_workspace_target(canonical_root, trimmed_name, &lw_cfg.path) {
                Ok(p) => p,
                Err(msg) => {
                    linked_workspace_errors.push(msg);
                    continue;
                }
            };

        let raw_roots = lw_cfg.roots.unwrap_or_else(|| vec![".".into()]);
        let ws_roots = collect_confined_canonical_roots(
            &canonical_ws,
            raw_roots,
            lw_cfg.docs.unwrap_or_default(),
        );

        let mut ignored: BTreeSet<String> = DEFAULT_IGNORED_NAMES
            .iter()
            .copied()
            .map(str::to_owned)
            .collect();
        if let Some(user_ignores) = lw_cfg.ignore {
            ignored.extend(user_ignores);
        }

        linked_workspaces.push(LinkedWorkspace {
            name: trimmed_name.to_owned(),
            path: canonical_ws,
            roots: ws_roots,
            ignored_names: ignored,
        });
    }

    (linked_workspaces, linked_workspace_errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_confined_canonical_roots_deduplicates_and_confines_roots_and_docs() {
        let dir = crate::fs::ScopedTempDir::create("acdd_lw_roots").unwrap();
        let ws = dir.path().canonicalize().unwrap();
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::create_dir_all(ws.join("docs")).unwrap();

        #[cfg(unix)]
        let outside = {
            let out = crate::fs::ScopedTempDir::create("acdd_lw_outside").unwrap();
            std::os::unix::fs::symlink(out.path(), ws.join("escape")).unwrap();
            out
        };

        let roots = collect_confined_canonical_roots(
            &ws,
            vec![".".into(), "src".into(), "missing".into(), "escape".into()],
            vec!["docs".into(), "docs/*.md".into(), "src".into()],
        );

        assert_eq!(roots, vec![ws.clone(), ws.join("docs"), ws.join("src")]);
        #[cfg(unix)]
        drop(outside);
    }
}
