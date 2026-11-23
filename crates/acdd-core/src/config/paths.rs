//! Workspace path resolution, DB/memory budget resolution, and root scope checks.

use super::constants::{error, ACDD_CONFIG_FILE, DEFAULT_CODE_DB};
use super::schema::{Adapter, ScannerLimits};
use crate::fs::checked_child;
use crate::sys::memory_budget_bytes;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolves the configured `state_db` path against the primary workspace root.
pub fn resolve_state_db_path(root: &Path, adapter: &Adapter) -> PathBuf {
    adapter.resolve_state_db_path(root)
}

/// Resolves an optional explicit DB path override against `root`, ignoring empty/whitespace values.
pub fn resolve_custom_db_path(root: &Path, custom_db: Option<&Path>) -> Option<PathBuf> {
    match custom_db {
        Some(p) if !p.as_os_str().is_empty() && !p.to_str().is_some_and(|s| s.trim().is_empty()) => {
            Some(if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            })
        }
        _ => None,
    }
}

/// Resolves the effective `state.sqlite` path from `primary`, optional explicit override, and [`Adapter`].
pub fn resolve_effective_state_db_path(
    primary: &Path,
    custom_db: Option<&Path>,
    adapter: &Adapter,
) -> PathBuf {
    resolve_custom_db_path(primary, custom_db)
        .unwrap_or_else(|| adapter.resolve_state_db_path(primary))
}

/// Resolves the code-map database path against `root`, defaulting to `.acdd/code-map.sqlite`.
pub fn resolve_code_db_path(root: &Path, custom_db: Option<&Path>) -> PathBuf {
    resolve_custom_db_path(root, custom_db).unwrap_or_else(|| root.join(DEFAULT_CODE_DB))
}

/// Resolves the transient working-memory budget from YAML, environment, or available memory.
pub fn resolve_memory_budget_bytes(limits: &ScannerLimits) -> std::io::Result<u64> {
    if let Some(mebibytes) = limits.memory_budget_mb {
        return bytes_from_mebibytes(mebibytes);
    }
    let environment = match std::env::var("ACDD_MEMORY_BUDGET_MB") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(error("ACDD_MEMORY_BUDGET_MB must be valid UTF-8"));
        }
    };
    let configured_mb = memory_budget_mb_from_sources(None, environment.as_deref())?;
    let Some(mebibytes) = configured_mb else {
        return Ok(memory_budget_bytes());
    };
    bytes_from_mebibytes(mebibytes)
}

/// Parses and validates transient memory budget from configuration or environment.
fn memory_budget_mb_from_sources(
    configured_mb: Option<u64>,
    environment_mb: Option<&str>,
) -> std::io::Result<Option<u64>> {
    match configured_mb {
        Some(value) => Ok(Some(value)),
        None => environment_mb
            .map(|value| {
                let mebibytes = value
                    .parse::<u64>()
                    .map_err(|_| error("ACDD_MEMORY_BUDGET_MB must be a positive integer"))?;
                if mebibytes == 0 {
                    return Err(error("ACDD_MEMORY_BUDGET_MB must be a positive integer"));
                }
                Ok(mebibytes)
            })
            .transpose(),
    }
}

/// Converts mebibytes to bytes with overflow validation.
fn bytes_from_mebibytes(mebibytes: u64) -> std::io::Result<u64> {
    if mebibytes == 0 {
        return Err(error("memory budget must be greater than zero MiB"));
    }
    mebibytes
        .checked_mul(1024 * 1024)
        .ok_or_else(|| error("memory budget in MiB overflows the byte range"))
}

/// Resolves a workspace-relative file path against workspace root and linked workspaces.
pub fn resolve_file_path(
    root: &Path,
    adapter: &Adapter,
    file_path: &str,
) -> std::io::Result<PathBuf> {
    if file_path.starts_with('[') {
        if let Some((prefix, rest)) = file_path.split_once("]/") {
            let ws_name = prefix.trim_start_matches('[');
            if let Some(lw) = adapter.linked_workspaces.iter().find(|w| w.name == ws_name) {
                let cleaned = rest.strip_prefix("./").unwrap_or(rest);
                return checked_child(&lw.path, Path::new(cleaned));
            }
        }
    }
    let p = Path::new(file_path);
    if p.is_absolute() {
        if p.components()
            .any(|comp| matches!(comp, std::path::Component::ParentDir))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path must be a relative plain path without parent traversal",
            ));
        }
        // Reject any absolute path that traverses a symlink before considering fallback.
        let mut cur = PathBuf::new();
        for comp in p.components() {
            cur.push(comp);
            if let Ok(meta) = std::fs::symlink_metadata(&cur) {
                if meta.file_type().is_symlink() && !crate::fs::is_system_root_symlink(&cur) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "path must not traverse a symlink",
                    ));
                }
            }
        }
        if let Ok(rel) = p.strip_prefix(root) {
            return checked_child(root, rel);
        }
        if let (Ok(can_p), Ok(can_root)) = (p.canonicalize(), root.canonicalize()) {
            if let Ok(rel) = can_p.strip_prefix(&can_root) {
                return checked_child(&can_root, rel);
            }
        }
        for lw in &adapter.linked_workspaces {
            if let Ok(rel) = p.strip_prefix(&lw.path) {
                return checked_child(&lw.path, rel);
            }
            if let (Ok(can_p), Ok(can_lw)) = (p.canonicalize(), lw.path.canonicalize()) {
                if let Ok(rel) = can_p.strip_prefix(&can_lw) {
                    return checked_child(&can_lw, rel);
                }
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path must be a relative plain path",
        ));
    }
    let cleaned = file_path.strip_prefix("./").unwrap_or(file_path);
    checked_child(root, Path::new(cleaned))
}

fn check_system_or_home_dir(root: &Path) -> std::io::Result<()> {
    let Some(parent) = root.parent() else {
        return Ok(());
    };
    let name = root.file_name().unwrap_or_default().to_string_lossy();
    let is_top_system_dir = parent == Path::new("/")
        && matches!(
            name.as_ref(),
            "home" | "Users" | "var" | "tmp" | "usr" | "etc" | "opt"
        );
    let is_macos_private_system_dir =
        parent == Path::new("/private") && matches!(name.as_ref(), "var" | "tmp" | "etc");
    if is_top_system_dir || is_macos_private_system_dir {
        return Err(error(format!(
            "refusing to index system directory '{root:?}'; specify a repository directory or set limits.allow_broad_root: true"
        )));
    }
    if parent == Path::new("/home") || parent == Path::new("/Users") {
        return Err(error(format!(
            "refusing to index user home directory '{root:?}'; specify a repository directory or set limits.allow_broad_root: true"
        )));
    }
    Ok(())
}

fn count_git_subdirs_in(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_dir()))
        .filter(|entry| entry.path().join(".git").exists())
        .count()
}

fn has_multiple_git_subdirs(root: &Path) -> Option<usize> {
    let entries = fs::read_dir(root).ok()?;
    let mut git_subdirs = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let path = entry.path();
        if path.join(".git").exists() {
            git_subdirs += 1;
        } else {
            git_subdirs += count_git_subdirs_in(&path);
        }
        if git_subdirs >= 2 {
            return Some(git_subdirs);
        }
    }
    None
}

/// Checks root scope against filesystem roots, system directories, user home directories, and multi-repository containers.
pub fn check_root_scope(root: &Path, allow_broad: bool) -> std::io::Result<()> {
    if allow_broad
        || std::env::var("ACDD_ALLOW_BROAD_ROOT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    {
        return Ok(());
    }

    if root == Path::new("/") {
        return Err(error(
            "refusing to index filesystem root '/'; specify a repository directory or set limits.allow_broad_root: true",
        ));
    }

    check_system_or_home_dir(root)?;

    let is_git_repo = root.join(".git").exists();
    let has_manifest = root.join(ACDD_CONFIG_FILE).exists()
        || root.join("Cargo.toml").exists()
        || root.join("package.json").exists()
        || root.join("pyproject.toml").exists()
        || root.join("go.mod").exists();

    if !is_git_repo && !has_manifest {
        if let Some(git_subdirs) = has_multiple_git_subdirs(root) {
            return Err(error(format!(
                "workspace root '{root:?}' appears to be a multi-repository container ({git_subdirs}+ repos detected); specify a single repository or set limits.allow_broad_root: true"
            )));
        }
    }

    Ok(())
}

/// Separates a leading workspace bracket from a relative path, e.g. "[workspace]/foo/bar" -> ("[workspace]", "foo/bar").
pub fn workspace_path(path: &str) -> (&str, &str) {
    match path.split_once('/') {
        Some((head, tail)) if head.starts_with('[') && head.ends_with(']') => (head, tail),
        _ => ("", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_file_path_handles_dot_slash_and_absolute_paths() {
        let _cleanup = crate::fs::ScopedTempDir::create("acdd_test").unwrap();
        let temp_dir = _cleanup.path().to_path_buf();
        let file_path = temp_dir.join("test.rs");
        std::fs::write(&file_path, "pub fn test() {}").unwrap();

        let adapter = Adapter {
            roots: vec![temp_dir.clone()],
            ..Default::default()
        };

        // 1. Plain relative
        let resolved = resolve_file_path(&temp_dir, &adapter, "test.rs").unwrap();
        assert_eq!(resolved, file_path);

        // 2. Leading ./
        let resolved = resolve_file_path(&temp_dir, &adapter, "./test.rs").unwrap();
        assert_eq!(resolved, file_path);

        // 3. Absolute path inside root
        let resolved = resolve_file_path(&temp_dir, &adapter, file_path.to_str().unwrap()).unwrap();
        assert_eq!(resolved, file_path);

        // 4. Absolute path outside root
        let outside = std::env::temp_dir().join("some_outside_file_123.rs");
        let err = resolve_file_path(&temp_dir, &adapter, outside.to_str().unwrap()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        std::fs::remove_file(&file_path).ok();
        std::fs::remove_dir(&temp_dir).ok();
    }

    #[test]
    fn check_root_scope_rejects_system_and_macos_private_roots_but_allows_projects() {
        for system in [
            "/var",
            "/tmp",
            "/etc",
            "/private/var",
            "/private/tmp",
            "/private/etc",
        ] {
            let err = check_root_scope(Path::new(system), false).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{system}");
        }

        let _cleanup = crate::fs::ScopedTempDir::create("acdd_scope_ok").unwrap();
        let temp_project = _cleanup.path().to_path_buf();
        std::fs::write(
            temp_project.join("Cargo.toml"),
            "[package]\nname = \"ok\"\n",
        )
        .unwrap();
        assert!(check_root_scope(&temp_project, false).is_ok());
        let _ = std::fs::remove_dir_all(&temp_project);
    }

    #[test]
    fn workspace_constants_and_db_path_resolution_matrix() {
        assert_eq!(super::super::constants::ACDD_CONFIG_FILE, super::super::ACDD_CONFIG_FILE);
        assert_eq!(super::super::constants::ACDD_DIR, ".acdd");
        assert_eq!(super::super::constants::WORKTREES_DIR, ".worktrees");
        assert_eq!(super::super::constants::DEFAULT_STATE_DB, ".acdd/state.sqlite");
        assert_eq!(super::super::constants::DEFAULT_CODE_DB, ".acdd/code-map.sqlite");
        assert_eq!(super::super::constants::SNAPSHOT_REF_PREFIX, "refs/acdd/snapshots/");
        assert_eq!(super::super::constants::CLAIM_TRAILER_KEY, "Acdd-Claim");

        let root = Path::new("/repo");
        let adapter = Adapter::default();
        assert_eq!(
            resolve_effective_state_db_path(root, None, &adapter),
            root.join(".acdd/state.sqlite")
        );
        assert_eq!(
            resolve_effective_state_db_path(root, Some(Path::new("   ")), &adapter),
            root.join(".acdd/state.sqlite")
        );
        assert_eq!(
            resolve_effective_state_db_path(root, Some(Path::new("alt/state.sqlite")), &adapter),
            root.join("alt/state.sqlite")
        );
    }

    #[test]
    fn explicit_memory_budget_selection() {
        for (mib, expected) in [(50, Some(50 * 1024 * 1024)), (0, None), (u64::MAX, None)] {
            let result = bytes_from_mebibytes(mib);
            if let Some(bytes) = expected {
                assert_eq!(result.unwrap(), bytes, "{mib} MiB");
            } else {
                assert!(result.is_err(), "{mib} MiB must be rejected");
            }
        }
        for (yaml, environment, expected) in [
            (Some(200), Some("not-a-number"), Some(Some(200))),
            (None, Some("50"), Some(Some(50))),
            (None, Some("0"), None),
            (None, Some("bad"), None),
            (None, None, Some(None)),
        ] {
            let result = memory_budget_mb_from_sources(yaml, environment);
            if let Some(budget) = expected {
                assert_eq!(
                    result.unwrap(),
                    budget,
                    "yaml={yaml:?}, env={environment:?}"
                );
            } else {
                assert!(result.is_err(), "yaml={yaml:?}, env={environment:?}");
            }
        }
    }
}
