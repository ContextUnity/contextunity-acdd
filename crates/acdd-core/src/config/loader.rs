//! Workspace adapter loading, strict validation, and SQLite policy serialization.

use super::linked::resolve_linked_workspaces;
use super::schema::{
    default_profiles_dirs, default_specs, validate_and_normalize_specs, Adapter, AdapterFile,
    LinkedWorkspace, ScannerLimits, SpecCollectionConfig, StrictAdapterReport,
};
use super::{
    checked_child, error, safe_relative, schema_error, ACDD_CONFIG_FILE, DEFAULT_IGNORED_NAMES,
    DEFAULT_RETAIN_TRACES, DEFAULT_STATE_DB, LEGACY_05_KEYS,
};
use crate::errors::{ErrorCode, StateError};
use crate::lint::ArchitectureConfig;
use crate::response::ResponsePolicy;
use crate::subsystems::{SubsystemId, SubsystemRegistry};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Loads and normalizes workspace adapter configuration (logging linked-workspace warnings to stderr).
pub fn load_adapter(root: &Path, adapter_path: Option<&Path>) -> std::io::Result<Adapter> {
    let report = load_adapter_inner(root, adapter_path)?;
    for msg in &report.linked_workspace_errors {
        eprintln!("[acdd] {msg}; skipping");
    }
    Ok(report.adapter)
}

/// Loads and normalizes workspace adapter configuration, returning all linked-workspace validation errors.
pub fn load_adapter_report(
    root: &Path,
    adapter_path: Option<&Path>,
) -> std::io::Result<StrictAdapterReport> {
    load_adapter_inner(root, adapter_path)
}

/// Loads and normalizes workspace adapter configuration, failing closed on any linked-workspace error.
pub fn load_adapter_strict(root: &Path, adapter_path: Option<&Path>) -> std::io::Result<Adapter> {
    let report = load_adapter_report(root, adapter_path)?;
    if !report.linked_workspace_errors.is_empty() {
        return Err(schema_error(report.linked_workspace_errors.join("; ")));
    }
    Ok(report.adapter)
}

fn load_adapter_inner(
    root: &Path,
    adapter_path: Option<&Path>,
) -> std::io::Result<StrictAdapterReport> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let default_path = canonical_root.join(ACDD_CONFIG_FILE);
    let default_path = default_path.exists().then_some(default_path);
    let selected = adapter_path.or(default_path.as_deref());
    let mut explicit_adapter_path = None;
    let (raw, identity, adapter_version) = match selected {
        None => (AdapterFile::default(), None, None),
        Some(path) => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() {
                return Err(error("adapter must not be a symlink"));
            }
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                return Err(error("adapter must be inside workspace root"));
            }
            if adapter_path.is_some() {
                explicit_adapter_path = Some(canonical.clone());
            }
            let content = fs::read_to_string(&canonical)?;
            let mut identity: serde_yaml::Value = serde_yaml::from_str(&content)
                .map_err(|e| schema_error(format!("invalid acdd.yaml: {e}")))?;
            if let Some(mapping) = identity.as_mapping() {
                let found_legacy: Vec<&str> = LEGACY_05_KEYS
                    .iter()
                    .copied()
                    .filter(|key| mapping.contains_key(serde_yaml::Value::String((*key).to_owned())))
                    .collect();
                if !found_legacy.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        StateError::new(
                            ErrorCode::ConfigMigrationRequired,
                            format!(
                                "legacy 0.5.x acdd.yaml keys require migration: {}",
                                found_legacy.join(", ")
                            ),
                        )
                        .with_hint(
                            "Follow docs/runbooks/migration-0.6.md to update 0.5.x configuration keys",
                        ),
                    ));
                }
            }
            if let Some(mapping) = identity.as_mapping_mut() {
                for excluded in [
                    "response",
                    "DEBUG",
                    "debug",
                    "modules",
                    "profile",
                    "state_db",
                    "retain_traces",
                    "profiles_dirs",
                    "specs",
                ] {
                    mapping.remove(serde_yaml::Value::String(excluded.into()));
                }
            }
            let parsed: AdapterFile = serde_yaml::from_str(&content)
                .map_err(|e| schema_error(format!("invalid acdd.yaml schema: {e}")))?;
            let version_str = parsed.adapter_version.as_ref().map(|v| match v {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            (parsed, Some(identity), version_str)
        }
    };
    raw.response
        .validate()
        .map_err(|e| schema_error(e.to_string()))?;
    raw.architecture
        .validate()
        .map_err(|e| schema_error(e.to_string()))?;
    if let Some(limits) = &raw.limits {
        limits.validate().map_err(|e| schema_error(e.to_string()))?;
    }
    let mut modules = raw.modules.clone();
    if let Some(profile) = raw.profile {
        modules.profile = profile;
    }
    modules
        .validate()
        .map_err(|e| schema_error(e.to_string()))?;
    let mut specs = raw.specs.clone().unwrap_or_else(default_specs);
    validate_and_normalize_specs(&mut specs)?;
    let profiles_dirs = raw
        .profiles_dirs
        .clone()
        .unwrap_or_else(default_profiles_dirs);
    for dir in &profiles_dirs {
        let trimmed = dir.trim();
        if trimmed.is_empty() || !safe_relative(Path::new(trimmed)) {
            return Err(schema_error(format!(
                "profiles_dirs entry '{dir}' must be a relative plain path"
            )));
        }
    }
    let state_db = raw
        .state_db
        .clone()
        .unwrap_or_else(|| DEFAULT_STATE_DB.to_owned());
    if state_db.trim().is_empty() {
        return Err(schema_error("state_db must not be empty"));
    }
    let retain_traces = raw.retain_traces.unwrap_or(DEFAULT_RETAIN_TRACES);
    if retain_traces == 0 {
        return Err(schema_error("retain_traces must be at least 1"));
    }

    let code_enabled = modules.is_subsystem_enabled(SubsystemId::Code);
    let docs_enabled = modules.is_subsystem_enabled(SubsystemId::Docs);
    let specs_paths: BTreeMap<&str, Vec<&str>> = specs
        .iter()
        .map(|(k, v)| (k.as_str(), v.patterns()))
        .collect();
    let default_specs_map = default_specs();
    let default_specs_paths: BTreeMap<&str, Vec<&str>> = default_specs_map
        .iter()
        .map(|(k, v)| (k.as_str(), v.patterns()))
        .collect();
    let identity_is_empty = identity
        .as_ref()
        .is_none_or(|val| val.is_null() || val.as_mapping().is_some_and(|m| m.is_empty()));
    let digest = if identity_is_empty
        && code_enabled
        && docs_enabled
        && specs_paths == default_specs_paths
    {
        String::new()
    } else {
        let identity_json = match identity {
            Some(ref val)
                if !val.is_null() && !val.as_mapping().is_some_and(|m| m.is_empty()) =>
            {
                serde_json::to_value(val).map_err(|e| error(e.to_string()))?
            }
            _ => Value::Null,
        };
        let payload = json!({
            "identity": identity_json,
            "modules_code": code_enabled,
            "modules_docs": docs_enabled,
            "specs_paths": specs_paths,
        });
        crate::format::canonical_json_sha256(&payload)
    };
    let roots = raw
        .roots
        .or(raw.eligible_roots)
        .unwrap_or_else(|| vec![".".into()]);
    let roots = roots.into_iter().try_fold(Vec::new(), |mut values, item| {
        if item == "." {
            values.push(root.to_path_buf());
            return Ok(values);
        }
        let relative = Path::new(&item);
        if !safe_relative(relative) {
            return Err(error("adapter roots must be relative plain paths"));
        }
        let candidate = checked_child(root, relative)?;
        if candidate.exists() {
            let metadata = fs::symlink_metadata(&candidate)?;
            if metadata.file_type().is_symlink() {
                return Err(error("adapter root must not be a symlink"));
            }
            values.push(candidate);
        }
        Ok(values)
    })?;
    let mut roots = roots;
    let raw_docs = raw.docs.unwrap_or_default();
    for item in &raw_docs {
        if item.contains('*') || item.contains('?') {
            continue;
        }
        let relative = Path::new(item);
        if !safe_relative(relative) {
            return Err(error("adapter document roots must be relative plain paths"));
        }
        let candidate = checked_child(root, relative)?;
        if candidate.exists() {
            roots.push(candidate);
        }
    }
    roots.sort();
    roots.dedup();

    let configs = raw.linked_workspaces.or(raw.workspaces).unwrap_or_default();
    let (linked_workspaces, linked_workspace_errors) =
        resolve_linked_workspaces(&canonical_root, configs);

    let milestones = specs
        .get("milestones")
        .map(|cfg| cfg.patterns().into_iter().map(str::to_owned).collect())
        .unwrap_or_else(|| vec!["docs/milestones".into()]);
    let plans = specs
        .get("plans")
        .map(|cfg| cfg.patterns().into_iter().map(str::to_owned).collect())
        .unwrap_or_else(|| vec!["docs/plans".into()]);

    Ok(StrictAdapterReport {
        adapter: Adapter {
            sensitive: raw.sensitive,
            adapter_path: explicit_adapter_path,
            response: raw.response,
            modules,
            limits: raw.limits.unwrap_or_default(),
            roots,
            docs: raw_docs,
            ignored_names: raw
                .ignore
                .unwrap_or_default()
                .into_iter()
                .chain(raw.excluded_directory_names.unwrap_or_default())
                .chain(raw.excluded_file_names.unwrap_or_default())
                .chain(DEFAULT_IGNORED_NAMES.iter().copied().map(str::to_owned))
                .collect(),
            adapter_version,
            digest,
            linked_workspaces,
            owners: raw.owners.unwrap_or_default(),
            aliases: raw.aliases.unwrap_or_default(),
            milestones,
            plans,
            specs,
            profiles_dirs,
            state_db,
            retain_traces,
            architecture: raw.architecture,
            debug: raw.debug
                || std::env::var("ACDD_DEBUG")
                    .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                || std::env::var("DEBUG")
                    .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        },
        linked_workspace_errors,
    })
}

/// Formats the adapter configuration for serialization into SQLite database metadata.
pub fn policy(root: &Path, adapter: &Adapter) -> Value {
    let canonical_root = root.canonicalize().ok();
    let strip = |path: &Path| {
        path.strip_prefix(root)
            .ok()
            .or_else(|| {
                canonical_root
                    .as_deref()
                    .and_then(|can| path.strip_prefix(can).ok())
            })
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    json!({
        "adapter_path": adapter.adapter_path.as_ref().map(|path| strip(path)),
        "roots": adapter.roots.iter().map(|p| strip(p)).collect::<Vec<_>>(),
        "docs": adapter.docs,
        "sensitive": adapter.sensitive,
        "limits": &adapter.limits,
        "ignored_names": adapter.ignored_names,
        "adapter_version": adapter.adapter_version,
        "digest": adapter.digest,
        "linked_workspaces": adapter.linked_workspaces,
        "owners": adapter.owners,
        "aliases": adapter.aliases,
        "milestones": adapter.milestones,
        "plans": adapter.plans,
        "specs": adapter.specs,
        "profiles_dirs": adapter.profiles_dirs,
        "state_db": adapter.state_db,
        "retain_traces": adapter.retain_traces,
        "architecture": &adapter.architecture,
    })
}

/// Resolves the persisted adapter path recorded in SQLite database metadata.
pub fn persisted_adapter_path(root: &Path, value: &Value) -> Result<Option<PathBuf>> {
    match value.get("adapter_path") {
        None | Some(Value::Null) => Ok(None),
        Some(path) => {
            let raw = path.as_str().context("invalid persisted adapter path")?;
            anyhow::ensure!(!raw.is_empty(), "invalid persisted adapter path");
            let candidate = Path::new(raw);
            let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            if candidate.is_absolute() {
                if let Ok(rel) = candidate
                    .strip_prefix(&base)
                    .or_else(|_| candidate.strip_prefix(root))
                {
                    return Ok(Some(checked_child(&base, rel)?));
                }
                anyhow::ensure!(
                    !candidate
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                    "persisted adapter path must not contain parent traversal"
                );
                crate::fs::check_ancestors_no_symlinks_abs(candidate)?;
                if let Ok(meta) = fs::symlink_metadata(candidate) {
                    anyhow::ensure!(
                        !meta.file_type().is_symlink(),
                        "persisted adapter path must not be a symlink"
                    );
                }
                Ok(Some(crate::fs::normalize_path(candidate)))
            } else {
                Ok(Some(checked_child(&base, candidate)?))
            }
        }
    }
}

fn parse_persisted_or<T: serde::de::DeserializeOwned>(
    value: &Value,
    field: &str,
    default: impl FnOnce() -> T,
) -> Result<T> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(default()),
        Some(v) => {
            serde_json::from_value(v.clone()).with_context(|| format!("invalid persisted {field}"))
        }
    }
}

/// Reads and reconstructs an Adapter from a deserialized JSON Value.
pub fn read_policy_from_value(root: &Path, value: &Value) -> Result<Adapter> {
    let adapter_path = persisted_adapter_path(root, value)?;
    let roots = value["roots"]
        .as_array()
        .context("invalid persisted roots")?
        .iter()
        .map(|v| {
            let p = v.as_str().context("invalid root")?;
            if p.is_empty() {
                Ok(root.to_path_buf())
            } else {
                Ok(checked_child(root, Path::new(p))?)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let limits: ScannerLimits =
        serde_json::from_value(value["limits"].clone()).context("invalid persisted limits")?;
    let ignored_names: BTreeSet<String> = serde_json::from_value(value["ignored_names"].clone())
        .context("invalid persisted ignored names")?;
    let adapter_version = parse_persisted_or(value, "adapter_version", || None)?;
    let digest = parse_persisted_or(value, "digest", String::new)?;
    let linked_workspaces: Vec<LinkedWorkspace> =
        parse_persisted_or(value, "linked_workspaces", Vec::new)?;
    let owners: BTreeMap<String, String> = parse_persisted_or(value, "owners", BTreeMap::new)?;
    let aliases: BTreeMap<String, String> = parse_persisted_or(value, "aliases", BTreeMap::new)?;
    let milestones: Vec<String> = parse_persisted_or(value, "milestones", Vec::new)?;
    let plans: Vec<String> = parse_persisted_or(value, "plans", Vec::new)?;
    let mut specs: BTreeMap<String, SpecCollectionConfig> =
        parse_persisted_or(value, "specs", default_specs)?;
    validate_and_normalize_specs(&mut specs)?;
    let profiles_dirs: Vec<String> =
        parse_persisted_or(value, "profiles_dirs", default_profiles_dirs)?;
    let state_db: String = parse_persisted_or(value, "state_db", || DEFAULT_STATE_DB.to_owned())?;
    anyhow::ensure!(!state_db.trim().is_empty(), "invalid persisted state_db");
    let retain_traces: usize =
        parse_persisted_or(value, "retain_traces", || DEFAULT_RETAIN_TRACES)?;
    anyhow::ensure!(retain_traces > 0, "invalid persisted retain_traces");
    let sensitive: Vec<String> = parse_persisted_or(value, "sensitive", Vec::new)?;
    let architecture: ArchitectureConfig =
        parse_persisted_or(value, "architecture", ArchitectureConfig::default)?;
    architecture.validate()?;
    let docs: Vec<String> = parse_persisted_or(value, "docs", Vec::new)?;
    Ok(Adapter {
        adapter_path,
        response: ResponsePolicy::default(),
        modules: SubsystemRegistry::default(),
        sensitive,
        limits,
        roots,
        docs,
        ignored_names,
        adapter_version,
        digest,
        linked_workspaces,
        owners,
        aliases,
        milestones,
        plans,
        specs,
        profiles_dirs,
        state_db,
        retain_traces,
        architecture,
        debug: false,
    })
}

#[cfg(test)]
mod tests {
    use super::super::paths::{resolve_code_db_path, resolve_state_db_path};
    use super::*;

    #[test]
    fn specs_collections_and_adapter_digest_isolation_matrix() {
        let _cleanup = crate::fs::ScopedTempDir::create("acdd_specs_digest").unwrap();
        let dir = _cleanup.path().to_path_buf();

        let defaults = load_adapter(&dir, None).unwrap();
        assert_eq!(
            DEFAULT_IGNORED_NAMES.len(),
            DEFAULT_IGNORED_NAMES
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            "DEFAULT_IGNORED_NAMES must not contain duplicates"
        );
        assert_eq!(defaults.specs["milestones"].path.as_deref(), Some("docs/milestones"));
        assert_eq!(defaults.specs["milestones"].profile, None);
        assert_eq!(defaults.specs["plans"].path.as_deref(), Some("docs/plans"));
        assert_eq!(defaults.specs["plans"].profile, None);
        assert!(defaults.specs["plans"].prefixes.is_empty());
        assert_eq!(defaults.profiles_dirs, [".acdd/profiles"]);
        assert_eq!(defaults.state_db, ".acdd/state.sqlite");
        assert_eq!(defaults.retain_traces, 20);
        assert_eq!(defaults.digest, "");

        std::fs::write(dir.join(ACDD_CONFIG_FILE), "# comment only\n").unwrap();
        assert_eq!(load_adapter(&dir, None).unwrap().digest, "");

        let base_yaml = "\
roots: [.]
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3
modules:
  code: true
  docs: true
  state: true
";
        std::fs::write(dir.join(ACDD_CONFIG_FILE), base_yaml).unwrap();
        let base = load_adapter(&dir, None).unwrap();
        assert!(!base.digest.is_empty());

        // Non-index fields must NOT change adapter.digest.
        let invariant_yaml = "\
roots: [.]
state_db: .acdd/custom-state.sqlite
retain_traces: 99
profiles_dirs: [custom/profiles]
response: { page_size: 10 }
debug: true
specs:
  milestones:
    path: docs/milestones
    profile: custom_profile
    prefixes: [MS]
    digits: 5
  plans:
    path: docs/plans
    profile: another_profile
    prefixes: [PL]
    digits: 4
modules:
  code: true
  docs: true
  state: false
  blackboard: false
  domains:
    http: true
";
        std::fs::write(dir.join(ACDD_CONFIG_FILE), invariant_yaml).unwrap();
        let invariant = load_adapter(&dir, None).unwrap();
        assert_eq!(invariant.digest, base.digest);
        assert!(!invariant.modules.is_enabled("state"));
        assert!(!invariant.modules.is_enabled("blackboard"));
        assert!(invariant.modules.is_domain_enabled("http"));

        // Reordered keys must produce the exact same deterministic canonical digest.
        let reordered_yaml = "\
modules:
  docs: true
  code: true
specs:
  plans:
    path: docs/plans
  milestones:
    path: docs/milestones
roots: [.]
";
        std::fs::write(dir.join(ACDD_CONFIG_FILE), reordered_yaml).unwrap();
        let reordered = load_adapter(&dir, None).unwrap();
        assert_eq!(reordered.digest, base.digest);

        // Index-affecting toggles MUST change adapter.digest.
        for changed_yaml in [
            "roots: [.]\nmodules:\n  code: false\n  docs: true\n",
            "roots: [.]\nmodules:\n  code: true\n  docs: false\n",
            "roots: [.]\nspecs:\n  milestones:\n    path: docs/contracts\n  plans:\n    path: docs/plans\n",
            "roots: [.]\narchitecture:\n  zones:\n    core: [\"crates/acdd-core/src/**\"]\n  rules:\n    - id: r1\n      kind: doc_integrity\n",
        ] {
            std::fs::write(dir.join(ACDD_CONFIG_FILE), changed_yaml).unwrap();
            let changed = load_adapter(&dir, None).unwrap();
            assert_ne!(changed.digest, base.digest, "{changed_yaml}");
        }

        // Overlapping specs paths by components must fail; sibling prefix names pass.
        std::fs::write(
            dir.join(ACDD_CONFIG_FILE),
            "specs:\n  a:\n    path: docs/specs\n  b:\n    path: docs/specs/sub\n",
        )
        .unwrap();
        assert!(load_adapter(&dir, None).is_err());

        std::fs::write(
            dir.join(ACDD_CONFIG_FILE),
            "specs:\n  a:\n    path: docs/specs\n  b:\n    path: docs/specs_v2\n",
        )
        .unwrap();
        let derived = load_adapter(&dir, None).unwrap();
        assert_eq!(derived.milestones, vec!["docs/milestones"]);
        assert_eq!(derived.plans, vec!["docs/plans"]);

        std::fs::write(
            dir.join(ACDD_CONFIG_FILE),
            "specs:\n  milestones:\n    path: custom/ms\n  plans:\n    path: custom/pl\n",
        )
        .unwrap();
        let derived_custom = load_adapter(&dir, None).unwrap();
        assert_eq!(derived_custom.milestones, vec!["custom/ms"]);
        assert_eq!(derived_custom.plans, vec!["custom/pl"]);
    }

    #[test]
    fn strict_adapter_rejects_unknown_fields_legacy_05_keys_and_invalid_linked_workspaces() {
        let _cleanup = crate::fs::ScopedTempDir::create("acdd_strict_cfg").unwrap();
        let dir = _cleanup.path().to_path_buf();

        // 1. deny_unknown_fields on AdapterFile, LinkedWorkspaceConfig, ScannerLimits, ArchitectureConfig -> ProfileSchemaInvalid.
        for bad_yaml in [
            "unknown_top_level: true\n",
            "limits:\n  unknown_limit: 5\n",
            "linked_workspaces:\n  - name: dep\n    path: .\n    unknown_lw: 1\n",
            "architecture:\n  unknown_arch: 1\n",
            "architecture:\n  zones:\n    core: [\"crates/acdd-core/**\"]\n  rules:\n    - id: r1\n      kind: forbid_dependency\n      from:\n        zones: [\"missing\"]\n      to:\n        zones: [\"core\"]\n",
            "not: [valid: yaml\n",
        ] {
            std::fs::write(dir.join(ACDD_CONFIG_FILE), bad_yaml).unwrap();
            let err: anyhow::Error = load_adapter(&dir, None).unwrap_err().into();
            let state_err = StateError::from_anyhow(&err).expect("StateError expected");
            assert_eq!(
                state_err.code,
                ErrorCode::ProfileSchemaInvalid,
                "{bad_yaml}: {state_err}"
            );
        }

        // 2. Legacy 0.5 keys -> ConfigMigrationRequired via StateError::from_anyhow.
        for legacy_key in [
            "milestones",
            "plans",
            "tasks_db",
            "workflows_dir",
            "milestone_prefix",
            "plan_prefix",
            "task_repository",
            "task_project",
            "acdd_profile",
        ] {
            let yaml = if matches!(legacy_key, "milestones" | "plans") {
                format!("{legacy_key}: [docs/{legacy_key}]\n")
            } else {
                format!("{legacy_key}: legacy_val\n")
            };
            std::fs::write(dir.join(ACDD_CONFIG_FILE), &yaml).unwrap();
            let err: anyhow::Error = load_adapter(&dir, None).unwrap_err().into();
            let state_err = StateError::from_anyhow(&err).expect("StateError expected");
            assert_eq!(
                state_err.code,
                ErrorCode::ConfigMigrationRequired,
                "{legacy_key}: {state_err}"
            );
            assert!(
                state_err
                    .hint
                    .as_deref()
                    .is_some_and(|h| h.contains("migration-0.6.md")),
                "{legacy_key}: {:?}",
                state_err.hint
            );
        }

        // 3. load_adapter_report / load_adapter_strict linked_workspaces validation.
        let valid_ws = dir.join("valid_ws");
        let file_not_dir = dir.join("not_a_dir.txt");
        let _outside_cleanup = crate::fs::ScopedTempDir::create("acdd_strict_outside").unwrap();
        let outside_dir = _outside_cleanup.path().to_path_buf();
        std::fs::create_dir_all(&valid_ws).unwrap();
        std::fs::write(&file_not_dir, "plain file").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_dir, dir.join("symlink_ws")).unwrap();

        let lw_yaml = "\
linked_workspaces:
  - name: \"\"
    path: valid_ws
  - name: dup
    path: valid_ws
  - name: dup
    path: valid_ws
  - name: missing
    path: does_not_exist
  - name: file_target
    path: not_a_dir.txt
  - name: disabled_missing
    path: does_not_exist
    enabled: false
";
        let full_lw_yaml = if cfg!(unix) {
            format!("{lw_yaml}  - name: escaped\n    path: symlink_ws\n")
        } else {
            lw_yaml.to_owned()
        };
        std::fs::write(dir.join(ACDD_CONFIG_FILE), &full_lw_yaml).unwrap();

        // Backwards-compatible load_adapter succeeds with valid entries while skipping invalid ones.
        let non_strict = load_adapter(&dir, None).unwrap();
        assert_eq!(non_strict.linked_workspaces.len(), 1);
        assert_eq!(non_strict.linked_workspaces[0].name, "dup");

        // load_adapter_report surfaces every linked-workspace error; load_adapter_strict fails closed.
        let report = load_adapter_report(&dir, None).unwrap();
        let expected_err_count = if cfg!(unix) { 5 } else { 4 };
        assert_eq!(
            report.linked_workspace_errors.len(),
            expected_err_count,
            "{:?}",
            report.linked_workspace_errors
        );
        let strict_err: anyhow::Error = load_adapter_strict(&dir, None).unwrap_err().into();
        let state_err = StateError::from_anyhow(&strict_err).expect("StateError expected");
        assert_eq!(state_err.code, ErrorCode::ProfileSchemaInvalid);

        // 4. resolve_state_db_path / resolve_code_db_path helpers.
        let mut adapter = Adapter::default();
        assert_eq!(
            resolve_state_db_path(&dir, &adapter),
            dir.join(".acdd/state.sqlite")
        );
        adapter.state_db = "custom/state.sqlite".to_owned();
        assert_eq!(
            resolve_state_db_path(&dir, &adapter),
            dir.join("custom/state.sqlite")
        );
        assert_eq!(
            resolve_code_db_path(&dir, None),
            dir.join(".acdd/code-map.sqlite")
        );
        assert_eq!(
            resolve_code_db_path(&dir, Some(Path::new(""))),
            dir.join(".acdd/code-map.sqlite")
        );
        assert_eq!(
            resolve_code_db_path(&dir, Some(Path::new("custom/code.sqlite"))),
            dir.join("custom/code.sqlite")
        );

        // 5. read_policy_from_value fails closed on malformed non-null fields, preserves architecture, & handles external adapter_path.
        let arch_cfg = serde_yaml::from_str::<ArchitectureConfig>(
            "zones:\n  core: [\"crates/acdd-core/src/**\"]\nrules:\n  - id: r1\n    kind: doc_integrity\n",
        )
        .unwrap();
        let mut base_adapter = Adapter {
            roots: vec![dir.clone()],
            architecture: arch_cfg.clone(),
            ..Default::default()
        };
        let external_cfg = outside_dir.join("external-adapter.yaml");
        std::fs::write(&external_cfg, "roots: [.]\n").unwrap();
        base_adapter.adapter_path = Some(external_cfg.clone());
        let valid_policy = policy(&dir, &base_adapter);
        let roundtripped = read_policy_from_value(&dir, &valid_policy).unwrap();
        assert_eq!(
            roundtripped.adapter_path.as_deref(),
            Some(crate::fs::normalize_path(&external_cfg).as_path())
        );
        assert_eq!(roundtripped.architecture, arch_cfg);

        for (field, bad_value) in [
            ("linked_workspaces", json!("not-an-array")),
            ("owners", json!(42)),
            ("aliases", json!(["bad"])),
            ("milestones", json!({"bad": true})),
            ("plans", json!(123)),
            ("specs", json!(["not-a-map"])),
            ("profiles_dirs", json!(true)),
            ("state_db", json!(99)),
            ("state_db", json!("   ")),
            ("retain_traces", json!("five")),
            ("retain_traces", json!(0)),
            ("sensitive", json!("not-a-list")),
            ("adapter_version", json!(["v1"])),
            ("digest", json!(12345)),
            ("adapter_path", json!("")),
            ("adapter_path", json!("../escape.yaml")),
            ("architecture", json!("not-an-object")),
            ("architecture", json!({"zones": {"bad": ["../escape"]}})),
            (
                "architecture",
                json!({
                    "rules": [{
                        "id": "r1",
                        "kind": "forbid_dependency",
                        "from": {"zones": ["missing"]},
                        "to": {"paths": ["src/**"]}
                    }]
                }),
            ),
        ] {
            let mut tampered = valid_policy.clone();
            tampered[field] = bad_value;
            assert!(
                read_policy_from_value(&dir, &tampered).is_err(),
                "read_policy_from_value must fail closed on invalid '{field}'"
            );
        }
        #[cfg(unix)]
        {
            let link_cfg = outside_dir.join("link-adapter.yaml");
            std::os::unix::fs::symlink(&external_cfg, &link_cfg).unwrap();
            let mut tampered = valid_policy.clone();
            tampered["adapter_path"] = json!(link_cfg.to_string_lossy().to_string());
            assert!(
                read_policy_from_value(&dir, &tampered).is_err(),
                "persisted symlink external adapter_path must fail closed"
            );
        }
    }
}
