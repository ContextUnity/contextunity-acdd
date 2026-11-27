//! Confined CLI path and workflow resolution helpers (`ADR-0002`).

use acdd_core::config::{load_adapter, Adapter};
use acdd_state::workflow::resolve_optional_workflow_ref_with_dirs;
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};

/// Immutable path choices for one in-process CLI invocation.
pub(super) struct ResolvedPaths {
    pub config: Option<PathBuf>,
    pub adapter: Adapter,
    pub workflow_ref: Option<String>,
}

impl ResolvedPaths {
    pub fn resolve(root: &Path, config: Option<&Path>, workflow: Option<&Path>) -> Result<Self> {
        let config_path = match config {
            Some(path) => Some(confined_file(root, path)?),
            None => root
                .join("acdd.yaml")
                .is_file()
                .then(|| root.join("acdd.yaml")),
        };
        let adapter = load_adapter(root, config_path.as_deref())?;
        let workflow_ref = match workflow {
            Some(path) => {
                let raw = path.to_string_lossy();
                let (target_str, pin) = match raw.rsplit_once(':') {
                    Some((prefix, suffix))
                        if !prefix.is_empty()
                            && !suffix.is_empty()
                            && !suffix.contains(['/', '\\']) =>
                    {
                        (prefix, Some(suffix))
                    }
                    _ => (raw.as_ref(), None),
                };
                if acdd_state::workflow::embedded_workflow(target_str).is_some()
                    || (!target_str.contains(['/', '\\']) && !root.join(target_str).is_file())
                {
                    Some(raw.into_owned())
                } else {
                    let confined = confined_file(root, Path::new(target_str))?;
                    let canonical_root = root.canonicalize()?;
                    let relative = confined
                        .strip_prefix(&canonical_root)
                        .context("CLI_PATH_INVALID: workflow escapes root")?
                        .to_string_lossy()
                        .replace('\\', "/");
                    Some(match pin {
                        Some(pin) => format!("{relative}:{pin}"),
                        None => relative,
                    })
                }
            }
            None => None,
        };
        if workflow_ref.is_some() {
            let _resolved = resolve_optional_workflow_ref_with_dirs(
                workflow_ref.as_deref(),
                Some(root),
                Some(&adapter.modules),
                Some(&adapter.workflows_dirs),
            )?;
        }
        Ok(Self {
            config: config_path,
            adapter,
            workflow_ref,
        })
    }
}

pub(super) fn confined_file(root: &Path, path: &Path) -> Result<PathBuf> {
    let selected = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let selected = selected
        .canonicalize()
        .with_context(|| format!("CLI_PATH_MISSING: {}", selected.display()))?;
    ensure!(
        selected.starts_with(root.canonicalize()?) && selected.is_file(),
        "CLI_PATH_INVALID: expected a file confined to the selected root"
    );
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolve_accepts_absolute_profile_through_canonical_vs_alias_root() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let base_dir = acdd_core::fs::ScopedTempDir::create("contextunity-test-cli-paths").unwrap();
        let base = base_dir.path();
        let real_repo = base.join("private_var/repo");
        fs::create_dir_all(real_repo.join(".acdd/workflows")).unwrap();
        let profile_file = real_repo.join(".acdd/workflows/default.yaml");
        fs::write(&profile_file, "version: 2\ninherit: code\n").unwrap();

        let alias_repo = base.join("var_repo");
        symlink(&real_repo, &alias_repo).unwrap();

        let canonical_root = real_repo.canonicalize().unwrap();
        let alias_profile = alias_repo.join(".acdd/workflows/default.yaml");
        let resolved = ResolvedPaths::resolve(&canonical_root, None, Some(&alias_profile)).unwrap();
        assert!(resolved.workflow_ref.is_some());

        let outside = base.join("outside-profile.yaml");
        fs::write(&outside, "version: 2\n").unwrap();
        let err = ResolvedPaths::resolve(&canonical_root, None, Some(&outside))
            .err()
            .expect("outside profile must fail");
        assert!(err.to_string().contains("CLI_PATH_INVALID"), "{err:#}");
    }
}
