//! Workspace adapter loading, strict validation, and SQLite policy serialization.

use super::constants::{
    ACDD_CONFIG_FILE, DEFAULT_IGNORED_NAMES, DEFAULT_LINT_DIR, DEFAULT_RETAIN_TRACES,
    DEFAULT_SPEC_DIGITS, DEFAULT_STATE_DB,
};
use super::linked::resolve_linked_workspaces;
use super::refs::{
    collect_confined_yaml_files, resolve_and_read_confined_ref, validate_confined_search_dirs,
};
use super::schema::{
    default_lint_dirs, default_specs, default_specs_dirs, default_workflows_dirs,
    deserialize_optional_spec_fields, validate_and_normalize_specs, Adapter, AdapterFile,
    LinkedWorkspace, RoadmapDisplayConfig, ScannerLimits, SpecConfig, SpecItemConfig,
    StrictAdapterReport,
};
use crate::errors::{AcddError, ErrorCode};
use crate::fs::{checked_child, safe_relative};
use crate::lint::{ArchRuleSpec, ArchitectureConfig, ComplexityLimits};
use crate::response::ResponsePolicy;
use crate::schema::SchemaDef;
use crate::subsystems::{SubsystemId, SubsystemRegistry};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn config_schema_err(message: impl Into<String>) -> AcddError {
    AcddError::new(ErrorCode::WorkflowSchemaInvalid, message)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSpecFile {
    #[serde(default)]
    base: Option<String>,
    #[serde(default, rename = "ref")]
    spec_ref: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    roadmap: Option<bool>,
    #[serde(default)]
    prefixes: Option<Vec<String>>,
    #[serde(default)]
    digits: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_optional_spec_fields")]
    fields: Option<SchemaDef>,
    #[serde(default)]
    items: BTreeMap<String, SpecItemConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LintFileConfig {
    #[serde(default)]
    zones: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    rules: Vec<ArchRuleSpec>,
    #[serde(default)]
    complexity: Option<ComplexityLimits>,
    #[serde(default)]
    unused_public_api: Option<bool>,
}

fn parse_external_spec_yaml(
    rel_path: &str,
    yaml: &str,
) -> std::result::Result<ExternalSpecFile, AcddError> {
    let ext: ExternalSpecFile = serde_yaml::from_str(yaml).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("invalid external spec file '{rel_path}': {err}"),
        )
    })?;
    if ext.base.is_some() || ext.spec_ref.is_some() {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "external spec file '{rel_path}' must not declare 'base' or 'ref' (multi-hop spec inheritance is forbidden)"
            ),
        ));
    }
    Ok(ext)
}

/// Validate one external `.acdd/specs/*.yaml` definition file against `SpecConfig` external schema rules.
pub fn validate_external_spec_yaml(
    rel_path: &str,
    yaml: &str,
) -> std::result::Result<(), AcddError> {
    let ext = parse_external_spec_yaml(rel_path, yaml)?;
    let mut synthetic = BTreeMap::from([(
        rel_path.to_owned(),
        SpecConfig {
            path: Some("docs/specs".to_owned()),
            workflow: ext.workflow,
            roadmap: ext.roadmap,
            prefixes: ext.prefixes.unwrap_or_default(),
            digits: ext.digits.unwrap_or(DEFAULT_SPEC_DIGITS),
            fields: ext.fields,
            items: ext.items,
            ..SpecConfig::default()
        },
    )]);
    validate_and_normalize_specs(&mut synthetic)
}

/// Validate one standalone `.acdd/lint/*.yaml` file's YAML schema and rule syntax (cross-file zones checked on merge).
pub fn validate_lint_file_yaml(rel_path: &str, yaml: &str) -> std::result::Result<(), AcddError> {
    let parsed: LintFileConfig = serde_yaml::from_str(yaml)
        .map_err(|err| config_schema_err(format!("invalid lint file '{rel_path}': {err}")))?;
    if let Some(ref complexity) = parsed.complexity {
        complexity.validate()?;
    }
    let mut tmp = ArchitectureConfig {
        zones: parsed.zones,
        rules: Vec::new(),
        complexity: parsed.complexity.unwrap_or_default(),
        unused_public_api: parsed.unused_public_api.unwrap_or(false),
    };
    tmp.validate()?;
    let mut seen_ids = BTreeSet::new();
    for rule in parsed.rules {
        if !seen_ids.insert(rule.id.clone()) {
            return Err(config_schema_err(format!(
                "duplicate rule id '{}' in lint file '{rel_path}'",
                rule.id
            )));
        }
        rule.validate(None)?;
        tmp.rules.push(rule);
    }
    Ok(())
}

fn merge_fields_schema(
    base: Option<SchemaDef>,
    overlay: Option<SchemaDef>,
    ctx: &str,
) -> std::result::Result<Option<SchemaDef>, AcddError> {
    match (base, overlay) {
        (None, None) => Ok(None),
        (Some(s), None) | (None, Some(s)) => Ok(Some(s)),
        (Some(mut base_s), Some(overlay_s)) => {
            base_s.merge_object_overlay(&overlay_s, ctx)?;
            Ok(Some(base_s))
        }
    }
}

fn resolve_specs_refs(
    canonical_root: &Path,
    specs_dirs: &[String],
    specs: &mut BTreeMap<String, SpecConfig>,
    raw_yaml: Option<&str>,
) -> std::result::Result<(), AcddError> {
    let raw_specs_map: Option<serde_yaml::Mapping> = raw_yaml
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(s).ok())
        .and_then(|v| v.get("specs").and_then(serde_yaml::Value::as_mapping).cloned());
    for (name, cfg) in specs.iter_mut() {
        let raw_entry = raw_specs_map
            .as_ref()
            .and_then(|m| m.get(serde_yaml::Value::String(name.clone())))
            .and_then(serde_yaml::Value::as_mapping);
        let explicit_prefixes = raw_entry.is_some_and(|m| {
            m.contains_key(serde_yaml::Value::String("prefixes".to_owned()))
        });
        let explicit_digits = raw_entry.is_some_and(|m| {
            m.contains_key(serde_yaml::Value::String("digits".to_owned()))
        });
        if cfg.base.is_some() && cfg.spec_ref.is_some() {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "specs.{name} cannot declare both 'base' and 'ref' (single-hop inheritance only)"
                ),
            ));
        }
        let Some(ref raw_ref) = cfg.spec_ref else {
            continue;
        };
        let resolved = resolve_and_read_confined_ref(
            canonical_root,
            raw_ref,
            specs_dirs,
            ErrorCode::SpecSchemaInvalid,
            "spec",
        )?;
        let yaml = std::str::from_utf8(&resolved.bytes).map_err(|err| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "external spec file '{}' is not valid UTF-8: {err}",
                    resolved.rel_path
                ),
            )
        })?;
        let ext = parse_external_spec_yaml(&resolved.rel_path, yaml)?;
        cfg.workflow = cfg.workflow.take().or(ext.workflow);
        cfg.roadmap = cfg.roadmap.or(ext.roadmap);
        if !explicit_prefixes && cfg.prefixes.is_empty() {
            if let Some(prefixes) = ext.prefixes {
                cfg.prefixes = prefixes;
            }
        }
        if !explicit_digits && cfg.digits == DEFAULT_SPEC_DIGITS {
            if let Some(digits) = ext.digits {
                cfg.digits = digits;
            }
        }
        cfg.fields = merge_fields_schema(
            ext.fields,
            cfg.fields.take(),
            &format!("specs.{name}.fields"),
        )?;
        for (col_key, ext_item) in ext.items {
            if let Some(local_item) = cfg.items.get_mut(&col_key) {
                local_item.item = local_item.item.take().or(ext_item.item);
                local_item.fields = merge_fields_schema(
                    ext_item.fields,
                    local_item.fields.take(),
                    &format!("specs.{name}.items.{col_key}.fields"),
                )?;
            } else {
                cfg.items.insert(col_key, ext_item);
            }
        }
    }
    Ok(())
}

fn load_lint_architecture(
    canonical_root: &Path,
    lint_dirs: Option<&[String]>,
) -> std::result::Result<ArchitectureConfig, AcddError> {
    let files = collect_confined_yaml_files(
        canonical_root,
        lint_dirs,
        &[DEFAULT_LINT_DIR.to_owned()],
        ErrorCode::WorkflowSchemaInvalid,
        "lint",
    )?;
    let mut merged = ArchitectureConfig::default();
    let mut seen_rule_ids = BTreeSet::new();
    let mut complexity_owner: Option<String> = None;
    let mut unused_api_owner: Option<String> = None;

    for file in files {
        let yaml = std::str::from_utf8(&file.bytes).map_err(|err| {
            config_schema_err(format!("lint file '{}' is not valid UTF-8: {err}", file.rel_path))
        })?;
        let parsed: LintFileConfig = serde_yaml::from_str(yaml).map_err(|err| {
            config_schema_err(format!("invalid lint file '{}': {err}", file.rel_path))
        })?;
        for (zone_name, globs) in parsed.zones {
            if merged.zones.contains_key(&zone_name) {
                return Err(config_schema_err(format!(
                    "duplicate architecture zone '{zone_name}' in '{}'",
                    file.rel_path
                )));
            }
            merged.zones.insert(zone_name, globs);
        }
        for rule in parsed.rules {
            if !seen_rule_ids.insert(rule.id.clone()) {
                return Err(config_schema_err(format!(
                    "duplicate architecture rule id '{}' in '{}'",
                    rule.id, file.rel_path
                )));
            }
            merged.rules.push(rule);
        }
        if let Some(complexity) = parsed.complexity {
            if let Some(ref prev) = complexity_owner {
                return Err(config_schema_err(format!(
                    "conflicting 'complexity' setting in '{}' (already declared in '{prev}')",
                    file.rel_path
                )));
            }
            complexity_owner = Some(file.rel_path.clone());
            merged.complexity = complexity;
        }
        if let Some(unused_public_api) = parsed.unused_public_api {
            if let Some(ref prev) = unused_api_owner {
                return Err(config_schema_err(format!(
                    "conflicting 'unused_public_api' setting in '{}' (already declared in '{prev}')",
                    file.rel_path
                )));
            }
            unused_api_owner = Some(file.rel_path.clone());
            merged.unused_public_api = unused_public_api;
        }
    }

    merged.validate()?;
    Ok(merged)
}

/// Loads and normalizes workspace adapter configuration (logging linked-workspace warnings to stderr).
pub fn load_adapter(
    root: &Path,
    adapter_path: Option<&Path>,
) -> std::result::Result<Adapter, AcddError> {
    let report = load_adapter_inner(root, adapter_path, false)?;
    for msg in &report.linked_workspace_errors {
        eprintln!("[acdd] {msg}; skipping");
    }
    Ok(report.adapter)
}

/// Loads a sibling checkout's adapter configuration for cross-worktree roadmap discovery.
///
/// Strips only the removed pre-AC-006 top-level `architecture:` key before strict
/// `deny_unknown_fields` validation so a migrated worktree can read a pre-migration
/// sibling checkout's own `specs` and `linked_workspaces` while still failing closed on
/// any unrelated unknown key.
pub fn load_sibling_checkout_adapter(root: &Path) -> std::result::Result<Adapter, AcddError> {
    let report = load_adapter_inner(root, None, true)?;
    for msg in &report.linked_workspace_errors {
        eprintln!("[acdd] {msg}; skipping");
    }
    Ok(report.adapter)
}

/// Loads and normalizes workspace adapter configuration, returning all linked-workspace validation errors.
pub fn load_adapter_report(
    root: &Path,
    adapter_path: Option<&Path>,
) -> std::result::Result<StrictAdapterReport, AcddError> {
    load_adapter_inner(root, adapter_path, false)
}

fn insert_if_some<T: serde::Serialize>(
    map: &mut Map<String, Value>,
    key: &str,
    value: Option<&T>,
) -> std::result::Result<(), AcddError> {
    if let Some(val) = value {
        let json_val = serde_json::to_value(val).map_err(|e| config_schema_err(e.to_string()))?;
        map.insert(key.to_owned(), json_val);
    }
    Ok(())
}

fn adapter_index_identity(
    raw: Option<&AdapterFile>,
    architecture: &ArchitectureConfig,
) -> std::result::Result<Option<Value>, AcddError> {
    let mut map = Map::new();
    if let Some(raw) = raw {
        if !raw.sensitive.is_empty() {
            insert_if_some(&mut map, "sensitive", Some(&raw.sensitive))?;
        }
        insert_if_some(&mut map, "limits", raw.limits.as_ref())?;
        insert_if_some(&mut map, "adapter_version", raw.adapter_version.as_ref())?;
        insert_if_some(&mut map, "roots", raw.roots.as_ref())?;
        insert_if_some(&mut map, "ignore", raw.ignore.as_ref())?;
        insert_if_some(&mut map, "eligible_roots", raw.eligible_roots.as_ref())?;
        insert_if_some(&mut map, "docs", raw.docs.as_ref())?;
        insert_if_some(
            &mut map,
            "excluded_directory_names",
            raw.excluded_directory_names.as_ref(),
        )?;
        insert_if_some(
            &mut map,
            "excluded_file_names",
            raw.excluded_file_names.as_ref(),
        )?;
        insert_if_some(
            &mut map,
            "linked_workspaces",
            raw.linked_workspaces.as_ref(),
        )?;
        insert_if_some(&mut map, "workspaces", raw.workspaces.as_ref())?;
        insert_if_some(&mut map, "owners", raw.owners.as_ref())?;
        insert_if_some(&mut map, "aliases", raw.aliases.as_ref())?;
    }
    if !architecture.is_empty() {
        insert_if_some(&mut map, "architecture", Some(architecture))?;
    }
    Ok((!map.is_empty()).then_some(Value::Object(map)))
}

fn parse_adapter_config_file(
    canonical_root: &Path,
    adapter_path: Option<&Path>,
    selected: Option<&Path>,
    strip_legacy_architecture: bool,
) -> std::result::Result<(AdapterFile, Option<PathBuf>, Option<String>), AcddError> {
    let Some(path) = selected else {
        return Ok((AdapterFile::default(), None, None));
    };
    let metadata = fs::symlink_metadata(path).map_err(|e| config_schema_err(e.to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(config_schema_err("adapter must not be a symlink"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| config_schema_err(e.to_string()))?;
    if !canonical.starts_with(canonical_root) {
        return Err(config_schema_err("adapter must be inside workspace root"));
    }
    let explicit_adapter_path = adapter_path.is_some().then(|| canonical.clone());
    let content = fs::read_to_string(&canonical).map_err(|e| config_schema_err(e.to_string()))?;
    let parsed: AdapterFile = if strip_legacy_architecture {
        let mut yaml_val: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| config_schema_err(format!("invalid acdd.yaml schema: {e}")))?;
        if let Some(map) = yaml_val.as_mapping_mut() {
            for legacy_key in ["architecture", "profiles_dirs"] {
                map.remove(serde_yaml::Value::String(legacy_key.to_owned()));
            }
        }
        serde_yaml::from_value(yaml_val)
            .map_err(|e| config_schema_err(format!("invalid acdd.yaml schema: {e}")))?
    } else {
        serde_yaml::from_str(&content)
            .map_err(|e| config_schema_err(format!("invalid acdd.yaml schema: {e}")))?
    };
    Ok((parsed, explicit_adapter_path, Some(content)))
}

fn load_adapter_inner(
    root: &Path,
    adapter_path: Option<&Path>,
    strip_legacy_architecture: bool,
) -> std::result::Result<StrictAdapterReport, AcddError> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let default_path = canonical_root.join(ACDD_CONFIG_FILE);
    let default_path = default_path.exists().then_some(default_path);
    let selected = adapter_path.or(default_path.as_deref());
    let (raw, explicit_adapter_path, raw_yaml) = parse_adapter_config_file(
        &canonical_root,
        adapter_path,
        selected,
        strip_legacy_architecture,
    )?;
    let adapter_version = raw.adapter_version.as_ref().map(|v| match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    raw.response
        .validate()
        .map_err(|e| config_schema_err(e.to_string()))?;
    if let Some(limits) = &raw.limits {
        limits
            .validate()
            .map_err(|e| config_schema_err(e.to_string()))?;
    }
    let roadmap = raw.roadmap.clone().unwrap_or_default();
    roadmap.validate()?;
    let mut modules = raw.modules.clone();
    if let Some(preset) = raw.preset {
        modules.preset = preset;
    }
    modules
        .validate()
        .map_err(|e| config_schema_err(e.to_string()))?;
    let specs_dirs = raw.specs_dirs.clone().unwrap_or_else(default_specs_dirs);
    validate_confined_search_dirs(&specs_dirs, "specs_dirs", ErrorCode::SpecSchemaInvalid)?;
    let workflows_dirs = raw
        .workflows_dirs
        .clone()
        .unwrap_or_else(default_workflows_dirs);
    validate_confined_search_dirs(
        &workflows_dirs,
        "workflows_dirs",
        ErrorCode::WorkflowSchemaInvalid,
    )?;
    let lint_dirs = raw.lint_dirs.clone().unwrap_or_else(default_lint_dirs);
    validate_confined_search_dirs(&lint_dirs, "lint_dirs", ErrorCode::WorkflowSchemaInvalid)?;
    let architecture = load_lint_architecture(&canonical_root, raw.lint_dirs.as_deref())?;

    let mut specs = raw.specs.clone().unwrap_or_else(default_specs);
    resolve_specs_refs(
        &canonical_root,
        &specs_dirs,
        &mut specs,
        raw_yaml.as_deref(),
    )?;
    validate_and_normalize_specs(&mut specs)?;
    let state_db = raw
        .state_db
        .clone()
        .unwrap_or_else(|| DEFAULT_STATE_DB.to_owned());
    if state_db.trim().is_empty() {
        return Err(config_schema_err("state_db must not be empty"));
    }
    let retain_traces = raw.retain_traces.unwrap_or(DEFAULT_RETAIN_TRACES);
    if retain_traces == 0 {
        return Err(config_schema_err("retain_traces must be at least 1"));
    }

    let identity = adapter_index_identity(selected.is_some().then_some(&raw), &architecture)?;
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
    let digest =
        if identity.is_none() && code_enabled && docs_enabled && specs_paths == default_specs_paths
        {
            String::new()
        } else {
            let payload = json!({
                "identity": identity.unwrap_or(Value::Null),
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
    let mut roots = roots.into_iter().try_fold(Vec::new(), |mut values, item| {
        if item == "." {
            values.push(root.to_path_buf());
            return Ok(values);
        }
        let relative = Path::new(&item);
        if !safe_relative(relative) {
            return Err(config_schema_err(
                "adapter roots must be relative plain paths",
            ));
        }
        let candidate =
            checked_child(root, relative).map_err(|e| config_schema_err(e.to_string()))?;
        if candidate.exists() {
            let metadata =
                fs::symlink_metadata(&candidate).map_err(|e| config_schema_err(e.to_string()))?;
            if metadata.file_type().is_symlink() {
                return Err(config_schema_err("adapter root must not be a symlink"));
            }
            values.push(candidate);
        }
        Ok(values)
    })?;
    let raw_docs = raw.docs.unwrap_or_default();
    for item in &raw_docs {
        if item.contains('*') || item.contains('?') {
            continue;
        }
        let relative = Path::new(item);
        if !safe_relative(relative) {
            return Err(config_schema_err(
                "adapter document roots must be relative plain paths",
            ));
        }
        let candidate =
            checked_child(root, relative).map_err(|e| config_schema_err(e.to_string()))?;
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
            specs_dirs,
            workflows_dirs,
            lint_dirs,
            roadmap,
            state_db,
            retain_traces,
            architecture,
            debug: raw.debug
                || std::env::var("ACDD_DEBUG")
                    .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                || std::env::var("DEBUG").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
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
        "specs_dirs": adapter.specs_dirs,
        "workflows_dirs": adapter.workflows_dirs,
        "lint_dirs": adapter.lint_dirs,
        "roadmap": adapter.roadmap,
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
    limits.validate()?;
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
    let mut specs: BTreeMap<String, SpecConfig> =
        parse_persisted_or(value, "specs", default_specs)?;
    validate_and_normalize_specs(&mut specs)?;
    let specs_dirs: Vec<String> = parse_persisted_or(value, "specs_dirs", default_specs_dirs)?;
    validate_confined_search_dirs(&specs_dirs, "specs_dirs", ErrorCode::SpecSchemaInvalid)?;
    let workflows_dirs: Vec<String> =
        parse_persisted_or(value, "workflows_dirs", default_workflows_dirs)?;
    validate_confined_search_dirs(
        &workflows_dirs,
        "workflows_dirs",
        ErrorCode::WorkflowSchemaInvalid,
    )?;
    let lint_dirs: Vec<String> = parse_persisted_or(value, "lint_dirs", default_lint_dirs)?;
    validate_confined_search_dirs(&lint_dirs, "lint_dirs", ErrorCode::WorkflowSchemaInvalid)?;
    let roadmap: RoadmapDisplayConfig =
        parse_persisted_or(value, "roadmap", RoadmapDisplayConfig::default)?;
    roadmap.validate()?;
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
        specs_dirs,
        workflows_dirs,
        lint_dirs,
        roadmap,
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
    use crate::errors::ErrorCode;

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
        assert_eq!(
            defaults.specs["milestones"].path.as_deref(),
            Some("docs/milestones")
        );
        assert_eq!(defaults.specs["milestones"].workflow, None);
        assert_eq!(defaults.specs["plans"].path.as_deref(), Some("docs/plans"));
        assert_eq!(defaults.specs["plans"].workflow, None);
        assert!(defaults.specs["plans"].prefixes.is_empty());
        assert_eq!(defaults.workflows_dirs, [".acdd/workflows"]);
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
workflows_dirs: [custom/workflows]
response: { page_size: 10 }
debug: true
specs:
  milestones:
    path: docs/milestones
    workflow: custom_workflow
    prefixes: [MS]
    digits: 5
  plans:
    path: docs/plans
    workflow: another_workflow
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
        ] {
            std::fs::write(dir.join(ACDD_CONFIG_FILE), changed_yaml).unwrap();
            let changed = load_adapter(&dir, None).unwrap();
            assert_ne!(changed.digest, base.digest, "{changed_yaml}");
        }
        std::fs::create_dir_all(dir.join(".acdd/lint")).unwrap();
        std::fs::write(
            dir.join(".acdd/lint/rules.yaml"),
            "zones:\n  core: [\"crates/acdd-core/src/**\"]\nrules:\n  - id: r1\n    kind: doc_integrity\n",
        )
        .unwrap();
        std::fs::write(dir.join(ACDD_CONFIG_FILE), base_yaml).unwrap();
        let with_lint = load_adapter(&dir, None).unwrap();
        assert_ne!(with_lint.digest, base.digest);
        std::fs::remove_dir_all(dir.join(".acdd/lint")).unwrap();

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
    fn strict_adapter_rejects_unknown_fields_and_invalid_linked_workspaces() {
        let _cleanup = crate::fs::ScopedTempDir::create("acdd_strict_cfg").unwrap();
        let dir = _cleanup.path().to_path_buf();

        // 1. deny_unknown_fields on AdapterFile (including removed inline architecture:), LinkedWorkspaceConfig, ScannerLimits -> WorkflowSchemaInvalid.
        for bad_yaml in [
            "unknown_top_level: true\n",
            "limits:\n  unknown_limit: 5\n",
            "linked_workspaces:\n  - name: dep\n    path: .\n    unknown_lw: 1\n",
            "architecture:\n  zones:\n    core: [\"crates/acdd-core/src/**\"]\n",
            "not: [valid: yaml\n",
        ] {
            std::fs::write(dir.join(ACDD_CONFIG_FILE), bad_yaml).unwrap();
            let err = load_adapter(&dir, None).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::WorkflowSchemaInvalid,
                "{bad_yaml}: {err}"
            );
        }
        std::fs::write(dir.join(ACDD_CONFIG_FILE), "roots: [.]\n").unwrap();
        std::fs::create_dir_all(dir.join(".acdd/lint")).unwrap();
        for bad_lint_yaml in [
            "unknown_arch: 1\n",
            "zones:\n  core: [\"crates/acdd-core/**\"]\nrules:\n  - id: r1\n    kind: forbid_dependency\n    from:\n      zones: [\"missing\"]\n    to:\n      zones: [\"core\"]\n",
        ] {
            std::fs::write(dir.join(".acdd/lint/bad.yaml"), bad_lint_yaml).unwrap();
            let err = load_adapter(&dir, None).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::WorkflowSchemaInvalid,
                "{bad_lint_yaml}: {err}"
            );
        }
        std::fs::remove_dir_all(dir.join(".acdd/lint")).unwrap();

        // 2. Unknown config keys are rejected via deny_unknown_fields -> WorkflowSchemaInvalid.
        for unknown_key in ["unknown_top_level_key", "bogus_config_key"] {
            let yaml = format!("{unknown_key}: bad_val\n");
            std::fs::write(dir.join(ACDD_CONFIG_FILE), &yaml).unwrap();
            let err = load_adapter(&dir, None).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::WorkflowSchemaInvalid,
                "{unknown_key}: {err}"
            );
        }

        // 3. load_adapter_report linked_workspaces validation.
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

        // load_adapter_report surfaces every linked-workspace error directly.
        let report = load_adapter_report(&dir, None).unwrap();
        let expected_err_count = if cfg!(unix) { 5 } else { 4 };
        assert_eq!(
            report.linked_workspace_errors.len(),
            expected_err_count,
            "{:?}",
            report.linked_workspace_errors
        );
        assert!(
            report
                .linked_workspace_errors
                .iter()
                .any(|e| e.contains("duplicate")),
            "{:?}",
            report.linked_workspace_errors
        );

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
            ("workflows_dirs", json!(true)),
            ("workflows_dirs", json!(["../escape"])),
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
