use super::*;
use std::path::PathBuf;

/// Resolve a workflow document against its declared inheritance target.
pub fn resolve_effective_workflow_profile(
    root: &Path,
    path: &Path,
    yaml: &str,
) -> Result<GateProfile> {
    fn resolve(
        root: &Path,
        path: &Path,
        yaml: &str,
        stack: &mut Vec<PathBuf>,
    ) -> Result<GateProfile> {
        let relative = path.to_string_lossy();
        parse_profile_reference(&relative, false)?;
        let confined = crate::tasks::confined_path(root, &relative)?;
        let identity = if confined.exists() {
            confined.canonicalize()?
        } else {
            confined
        };
        if stack.contains(&identity) {
            bail!(
                "ACDD_PROFILE_INHERIT_CYCLE: cyclic profile inheritance at {}",
                path.display()
            );
        }
        stack.push(identity);
        let document = parse_profile_document(yaml)?;
        let mut base = match &document.inherit {
            InheritTarget::None => GateProfile::default(),
            InheritTarget::Default => (*compiled()).clone(),
            InheritTarget::Workspace => {
                if path == Path::new(".acdd/profile.yaml") {
                    bail!("ACDD_PROFILE_INVALID: root workspace profile cannot inherit itself");
                }
                let mut base = if let Some(settings) = crate::tasks::configuration::current(root) {
                    (*settings.profile).clone()
                } else {
                    let config = crate::tasks::configuration::config_path(root);
                    let selected = if config.is_file() {
                        let (value, _) = parse_config_metadata_limit(&config)?;
                        value
                            .get("acdd_profile")
                            .and_then(serde_yaml::Value::as_str)
                            .filter(|name| *name != "plan")
                            .map(str::to_owned)
                    } else {
                        None
                    };
                    let selected = selected.filter(|reference| {
                        parse_profile_reference(reference, false)
                            .is_ok_and(|(selected_path, _)| selected_path != path)
                    });
                    if let Some(reference) = selected {
                        let (relative, _) = parse_profile_reference(&reference, false)?;
                        load_profile_path(root, &reference, false)?;
                        let target =
                            crate::tasks::confined_path(root, &relative.to_string_lossy())?;
                        let text = std::fs::read_to_string(target)?;
                        resolve(root, &relative, &text, stack)?
                    } else {
                        let repository = crate::tasks::confined_path(root, ".acdd/profile.yaml")?;
                        if repository.is_file() {
                            let text = std::fs::read_to_string(repository)?;
                            resolve(root, Path::new(".acdd/profile.yaml"), &text, stack)?
                        } else {
                            (*compiled()).clone()
                        }
                    }
                };
                base.gates.clear();
                base.persistence = None;
                base
            }
            InheritTarget::Path(reference) => {
                let ref_str = reference.to_string_lossy();
                parse_profile_reference(&ref_str, false)?;
                let target = crate::tasks::confined_path(root, &ref_str)?;
                let contents = std::fs::read_to_string(&target)
                    .with_context(|| format!("ACDD_PROFILE_NOT_FOUND: {}", target.display()))?;
                let mut base = resolve(root, reference, &contents, stack)?;
                base.gates.clear();
                base.persistence = None;
                base
            }
        };
        merge_profile_document(&mut base, document);
        stack.pop();
        base.validate()?;
        Ok(base)
    }
    resolve(root, path, yaml, &mut Vec::new())
}

pub fn parse_profile_reference(
    reference: &str,
    require_pin: bool,
) -> Result<(PathBuf, Option<String>)> {
    let (path, pin) = match reference.rsplit_once(':') {
        Some((path, pin)) if !path.is_empty() && !pin.is_empty() => {
            (path, Some(pin.to_ascii_lowercase()))
        }
        _ => (reference, None),
    };
    if require_pin && pin.is_none() {
        bail!("ACDD_PROFILE_PIN_REQUIRED: use '<path>:<sha256_prefix>'");
    }
    if let Some(pin) = &pin {
        if pin.len() < 7 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("ACDD_PROFILE_PIN_INVALID: SHA-256 prefix must contain at least 7 hexadecimal characters");
        }
    }
    let path = Path::new(path);
    if !acdd_core::fs::is_normalized_relative(path) {
        bail!("ACDD_PROFILE_PATH_INVALID: profile path must be relative and confined to the workspace");
    }
    Ok((path.to_path_buf(), pin))
}

pub(super) fn load_profile_path(
    root: &Path,
    reference: &str,
    require_pin: bool,
) -> Result<ParsedProfile> {
    let (relative, pin) = parse_profile_reference(reference, require_pin)?;
    let path = crate::tasks::confined_path(root, &relative.to_string_lossy())?;
    let contents = std::fs::read(&path)
        .with_context(|| format!("ACDD_PROFILE_NOT_FOUND: {}", path.display()))?;
    if let Some(prefix) = pin {
        use sha2::{Digest, Sha256};
        let digest = format!("{:x}", Sha256::digest(&contents));
        if !digest.starts_with(&prefix) {
            bail!("TASK_PROFILE_TAMPERED: profile SHA-256 does not match pinned prefix '{prefix}'");
        }
    }
    let text = std::str::from_utf8(&contents)
        .context("ACDD_PROFILE_SCHEMA_INVALID: profile must be UTF-8 YAML")?;
    parse_profile_document(text).with_context(|| format!("invalid profile in {}", path.display()))
}

/// Load one pinned task or milestone override above the workspace base profile.
pub fn load_pinned(root: &Path, reference: &str) -> Result<Arc<GateProfile>> {
    let base = load_for_workspace(root)?;
    load_pinned_over(root, reference, &base)
}

/// Load one pinned task or milestone override above an already-selected workspace base profile.
pub fn load_pinned_over(
    root: &Path,
    reference: &str,
    base_profile: &GateProfile,
) -> Result<Arc<GateProfile>> {
    let config_path = if let Some(settings) = crate::tasks::configuration::current(root) {
        if let Some(ref config) = settings.config {
            if !config.is_file() {
                bail!(
                    "ACDD_PROFILE_SCHEMA_INVALID: selected workspace config {} does not exist",
                    config.display()
                );
            }
            Some(config.clone())
        } else {
            None
        }
    } else {
        Some(crate::tasks::configuration::config_path(root)).filter(|path| path.is_file())
    };
    let mut base = base_profile.clone();
    merge_profile_document(&mut base, load_profile_path(root, reference, true)?);
    let profile = super::apply_config_metadata_limit(config_path.as_deref(), base)?;
    if !profile.gates.iter().any(|g| {
        gate_is_delivery(g)
            || g.gates.iter().any(gate_is_delivery)
            || g.tasks
                .as_ref()
                .is_some_and(|t| t.gates.iter().any(gate_is_delivery))
    }) {
        bail!("ACDD_PROFILE_INVALID: a merged workspace profile must contain a terminal delivery gate");
    }
    Ok(profile)
}

/// Load a task profile override, then milestone override, then workspace profile.
pub fn load_for_task(
    root: &Path,
    task_profile: Option<&str>,
    milestone_profile: Option<&str>,
) -> Result<Arc<GateProfile>> {
    match task_profile.or(milestone_profile) {
        Some(reference) => load_pinned(root, reference),
        None => load_for_workspace(root),
    }
}
