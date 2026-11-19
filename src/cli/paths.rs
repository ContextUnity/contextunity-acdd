//! Confined CLI path and profile resolution helpers (`ADR-0002`).

use acdd_core::config::{load_adapter, Adapter};
use acdd_state::profile::{resolve_profile_ref_with_dirs, ResolvedGateProfile};
use anyhow::{ensure, Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Immutable path choices for one in-process CLI invocation.
pub(super) struct ResolvedPaths {
    pub config: Option<PathBuf>,
    pub adapter: Adapter,
    pub profile_ref: Option<String>,
    #[allow(dead_code)]
    pub profile: Arc<ResolvedGateProfile>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub explicit_profile: bool,
}

impl ResolvedPaths {
    pub fn resolve(root: &Path, config: Option<&Path>, profile: Option<&Path>) -> Result<Self> {
        let config_path = match config {
            Some(path) => Some(confined_file(root, path)?),
            None => root
                .join("acdd.yaml")
                .is_file()
                .then(|| root.join("acdd.yaml")),
        };
        let adapter = load_adapter(root, config_path.as_deref())
            .map_err(|err| anyhow::anyhow!("ACDD_PROFILE_SCHEMA_INVALID: {err}"))?;
        let profile_ref = match profile {
            Some(path) => {
                let raw = path.to_string_lossy();
                if matches!(raw.as_ref(), "default" | "code" | "in_place" | "workspace") {
                    Some(raw.into_owned())
                } else {
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
                    let confined = confined_file(root, Path::new(target_str))?;
                    let canonical_root = root.canonicalize()?;
                    let relative = confined
                        .strip_prefix(&canonical_root)
                        .context("CLI_PATH_INVALID: profile escapes root")?
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
        let effective_ref = profile_ref.as_deref().unwrap_or("workspace");
        let resolved = resolve_profile_ref_with_dirs(
            effective_ref,
            Some(root),
            Some(&adapter.modules),
            Some(&adapter.profiles_dirs),
        )?;
        Ok(Self {
            config: config_path,
            adapter,
            profile_ref,
            profile: Arc::new(resolved),
            explicit_profile: profile.is_some(),
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

        let base_dir =
            acdd_core::fs::ScopedTempDir::create("contextunity-test-cli-paths").unwrap();
        let base = base_dir.path();
        let real_repo = base.join("private_var/repo");
        fs::create_dir_all(real_repo.join(".acdd")).unwrap();
        let profile_file = real_repo.join(".acdd/profile.yaml");
        fs::write(&profile_file, "version: 2\ninherit: default\n").unwrap();

        let alias_repo = base.join("var_repo");
        symlink(&real_repo, &alias_repo).unwrap();

        let canonical_root = real_repo.canonicalize().unwrap();
        let alias_profile = alias_repo.join(".acdd/profile.yaml");
        let resolved = ResolvedPaths::resolve(&canonical_root, None, Some(&alias_profile)).unwrap();
        assert!(resolved.explicit_profile);

        let outside = base.join("outside-profile.yaml");
        fs::write(&outside, "version: 2\n").unwrap();
        let err = ResolvedPaths::resolve(&canonical_root, None, Some(&outside))
            .err()
            .expect("outside profile must fail");
        assert!(err.to_string().contains("CLI_PATH_INVALID"), "{err:#}");
    }
}
