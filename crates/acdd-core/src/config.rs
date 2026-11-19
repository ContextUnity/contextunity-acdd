//! Workspace adapter configuration, scanner limits, and path resolution.

pub use crate::fs::{checked_child, safe_relative};
pub use crate::response::{Detail, ResponsePolicy, SourceContextPolicy};
pub use crate::subsystems::{SubsystemPreset, SubsystemRegistry};
pub use crate::sys::memory_budget_bytes;
use crate::errors::{ErrorCode, StateError};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Default maximum number of files admitted for indexing.
pub const DEFAULT_MAX_FILES: usize = 100_000;
/// Default maximum size in bytes for a single indexable source file (5 MiB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Default maximum total size in bytes for all indexable files (500 MiB).
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// Alias for maximum allowed file bytes.
pub const MAX_FILE_BYTES: u64 = DEFAULT_MAX_FILE_BYTES;
/// Authoritative maximum size in bytes for metadata and notes.
pub const MAX_METADATA_BYTES: usize = 2048;
/// Default maximum size in bytes for metadata and notes.
pub const DEFAULT_MAX_METADATA_BYTES: usize = MAX_METADATA_BYTES;
/// Default state database path relative to primary worktree root.
pub const DEFAULT_STATE_DB: &str = ".acdd/state.sqlite";
/// Default code-map database path relative to workspace root.
pub const DEFAULT_CODE_DB: &str = ".acdd/code-map.sqlite";
/// Default number of retained run trace logs.
pub const DEFAULT_RETAIN_TRACES: usize = 20;
/// Default specification ID numeric width.
pub const DEFAULT_SPEC_DIGITS: usize = 3;

fn default_spec_digits() -> usize {
    DEFAULT_SPEC_DIGITS
}

fn default_profiles_dirs() -> Vec<String> {
    vec![".acdd/profiles".into(), "docs/workflows".into()]
}

/// Configuration for a spec collection under `specs:` in `acdd.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpecCollectionConfig {
    /// Workspace-relative directory path containing spec documents.
    pub path: String,
    /// Optional explicit gate profile for specs in this collection (else workspace default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Allowed spec ID prefixes (e.g. `["AC"]`, `["PLAN"]`).
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// Zero-padded numeric suffix width for drafted spec IDs.
    #[serde(default = "default_spec_digits")]
    pub digits: usize,
}

/// Returns the default `specs:` collections (`milestones` and `plans`).
pub fn default_specs() -> BTreeMap<String, SpecCollectionConfig> {
    BTreeMap::from([
        (
            "milestones".to_owned(),
            SpecCollectionConfig {
                path: "docs/milestones".to_owned(),
                profile: None,
                prefixes: vec!["AC".to_owned()],
                digits: DEFAULT_SPEC_DIGITS,
            },
        ),
        (
            "plans".to_owned(),
            SpecCollectionConfig {
                path: "docs/plans".to_owned(),
                profile: None,
                prefixes: vec!["PLAN".to_owned()],
                digits: DEFAULT_SPEC_DIGITS,
            },
        ),
    ])
}

fn normalize_spec_collection_path(raw: &str) -> std::io::Result<String> {
    let normalized = raw
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned();
    if normalized.is_empty() || !crate::fs::is_normalized_relative(Path::new(&normalized)) {
        return Err(error(format!(
            "spec collection path '{raw}' must be a normalized workspace-relative path"
        )));
    }
    Ok(normalized)
}

fn validate_and_normalize_specs(
    specs: &mut BTreeMap<String, SpecCollectionConfig>,
) -> std::io::Result<()> {
    let mut normalized_entries: Vec<(String, Vec<String>)> = Vec::with_capacity(specs.len());
    for (name, cfg) in specs.iter_mut() {
        if name.trim().is_empty() {
            return Err(error("spec collection name must not be empty"));
        }
        if !(1..=16).contains(&cfg.digits) {
            return Err(error(format!(
                "specs.{name}.digits must be in 1..=16, got {}",
                cfg.digits
            )));
        }
        cfg.path = normalize_spec_collection_path(&cfg.path)?;
        let parts: Vec<String> = cfg.path.split('/').map(str::to_owned).collect();
        normalized_entries.push((name.clone(), parts));
    }
    for (i, (left_name, left_parts)) in normalized_entries.iter().enumerate() {
        for (right_name, right_parts) in normalized_entries.iter().skip(i + 1) {
            if left_parts.starts_with(right_parts) || right_parts.starts_with(left_parts) {
                return Err(error(format!(
                    "specs.{left_name}.path '{}' overlaps specs.{right_name}.path '{}' by path components",
                    left_parts.join("/"),
                    right_parts.join("/")
                )));
            }
        }
    }
    Ok(())
}

/// Standard directory and file names ignored during scanning.
pub const DEFAULT_IGNORED_NAMES: &[&str] = &[
    ".git",
    ".acdd",
    ".venv",
    "__pycache__",
    "target",
    "node_modules",
    "build",
    "dist",
    "coverage",
    ".cache",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    "vendor",
    ".cargo",
    ".idea",
    ".vscode",
    ".tox",
    ".turbo",
    "out",
    ".output",
];

fn default_max_files() -> usize {
    DEFAULT_MAX_FILES
}

fn default_max_file_bytes() -> u64 {
    DEFAULT_MAX_FILE_BYTES
}

fn default_max_total_bytes() -> u64 {
    DEFAULT_MAX_TOTAL_BYTES
}

fn default_max_metadata_bytes() -> usize {
    DEFAULT_MAX_METADATA_BYTES
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Represents scanner limits data.
pub struct ScannerLimits {
    #[serde(default = "default_max_files")]
    /// The max files value.
    pub max_files: usize,
    #[serde(default = "default_max_file_bytes")]
    /// The max file bytes value.
    pub max_file_bytes: u64,
    #[serde(default = "default_max_total_bytes")]
    /// The max total bytes value.
    pub max_total_bytes: u64,
    #[serde(default = "default_max_metadata_bytes")]
    /// Authoritative maximum size in bytes for metadata and notes.
    pub max_metadata_bytes: usize,
    #[serde(default)]
    /// Whether allow broad root applies.
    pub allow_broad_root: bool,
    #[serde(default, skip_serializing)]
    /// Optional transient memory budget in MiB; YAML takes precedence over the environment.
    pub memory_budget_mb: Option<u64>,
}

impl ScannerLimits {
    /// Validates scanner and metadata limits against authoritative ceilings.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=MAX_METADATA_BYTES).contains(&self.max_metadata_bytes),
            "limits.max_metadata_bytes must be 1..={MAX_METADATA_BYTES}"
        );
        Ok(())
    }
}

impl Default for ScannerLimits {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_FILES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_metadata_bytes: DEFAULT_MAX_METADATA_BYTES,
            allow_broad_root: false,
            memory_budget_mb: None,
        }
    }
}

fn error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn schema_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        StateError::new(ErrorCode::ProfileSchemaInvalid, message.into()),
    )
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
pub fn memory_budget_mb_from_sources(
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
pub fn bytes_from_mebibytes(mebibytes: u64) -> std::io::Result<u64> {
    if mebibytes == 0 {
        return Err(error("memory budget must be greater than zero MiB"));
    }
    mebibytes
        .checked_mul(1024 * 1024)
        .ok_or_else(|| error("memory budget in MiB overflows the byte range"))
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
/// Represents adapter file YAML representation.
pub struct AdapterFile {
    /// Workspace-relative glob patterns blocked by the reader.
    #[serde(default)]
    pub sensitive: Vec<String>,
    #[serde(default)]
    /// The response policy.
    pub response: ResponsePolicy,
    #[serde(default)]
    /// Optional MCP subsystem selection.
    #[serde(deserialize_with = "deserialize_subsystem_registry")]
    pub modules: SubsystemRegistry,
    /// Optional top-level module preset used by the workspace adapter format.
    #[serde(default)]
    pub profile: Option<SubsystemPreset>,
    #[serde(default)]
    /// Optional scanner limits.
    pub limits: Option<ScannerLimits>,
    /// Optional adapter version.
    pub adapter_version: Option<serde_json::Value>,
    /// Optional roots list.
    pub roots: Option<Vec<String>>,
    /// Optional ignore list.
    pub ignore: Option<Vec<String>>,
    /// Optional eligible roots list.
    pub eligible_roots: Option<Vec<String>>,
    #[serde(default, alias = "doc_roots")]
    /// Optional documentation roots.
    pub docs: Option<Vec<String>>,
    /// Configured spec collections (`specs:`).
    #[serde(default)]
    pub specs: Option<BTreeMap<String, SpecCollectionConfig>>,
    /// Directories searched for custom gate profiles.
    #[serde(default)]
    pub profiles_dirs: Option<Vec<String>>,
    /// State SQLite database path relative to primary worktree root.
    #[serde(default)]
    pub state_db: Option<String>,
    /// Number of retained trace files under `.acdd/run/`.
    #[serde(default)]
    pub retain_traces: Option<usize>,
    /// Optional excluded directory names.
    pub excluded_directory_names: Option<Vec<String>>,
    /// Optional excluded file names.
    pub excluded_file_names: Option<Vec<String>>,
    /// Optional linked workspaces configuration.
    pub linked_workspaces: Option<Vec<LinkedWorkspaceConfig>>,
    /// Optional workspaces alias.
    pub workspaces: Option<Vec<LinkedWorkspaceConfig>>,
    /// Optional owners map.
    pub owners: Option<BTreeMap<String, String>>,
    /// Optional aliases map.
    pub aliases: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "DEBUG", alias = "debug")]
    /// Debug mode flag.
    pub debug: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SubsystemToggleValue {
    Bool(bool),
    Domains(BTreeMap<String, bool>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SubsystemRegistryFile {
    Registry(SubsystemRegistry),
    Toggles(BTreeMap<String, SubsystemToggleValue>),
}

fn deserialize_subsystem_registry<'de, D>(
    deserializer: D,
) -> std::result::Result<SubsystemRegistry, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match SubsystemRegistryFile::deserialize(deserializer)? {
        SubsystemRegistryFile::Registry(registry) => Ok(registry),
        SubsystemRegistryFile::Toggles(toggles) => {
            let mut registry = SubsystemRegistry::default();
            for (name, value) in toggles {
                match (name.as_str(), value) {
                    ("domains", SubsystemToggleValue::Domains(domains)) => {
                        registry.domains = domains;
                    }
                    (_, SubsystemToggleValue::Bool(enabled)) => {
                        if enabled {
                            registry.enabled.insert(name);
                        } else {
                            registry.disabled.insert(name);
                        }
                    }
                    (_, SubsystemToggleValue::Domains(_)) => {
                        return Err(serde::de::Error::custom(format!(
                            "modules.{name} must be a boolean toggle"
                        )));
                    }
                }
            }
            Ok(registry)
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Configures linked workspace operations.
pub struct LinkedWorkspaceConfig {
    /// The name value.
    pub name: String,
    /// The path value.
    pub path: String,
    #[serde(default)]
    /// Optional enabled value.
    pub enabled: Option<bool>,
    #[serde(default)]
    /// Optional roots value.
    pub roots: Option<Vec<String>>,
    #[serde(default, alias = "doc_roots")]
    /// Optional docs value.
    pub docs: Option<Vec<String>>,
    #[serde(default)]
    /// Optional ignore value.
    pub ignore: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Represents linked workspace data.
pub struct LinkedWorkspace {
    /// The name value.
    pub name: String,
    /// The path value.
    pub path: PathBuf,
    /// The roots value.
    pub roots: Vec<PathBuf>,
    /// The ignored names value.
    pub ignored_names: BTreeSet<String>,
}

#[derive(Debug, Clone)]
/// Represents workspace adapter configuration.
pub struct Adapter {
    /// Workspace-relative glob patterns blocked by the reader.
    pub sensitive: Vec<String>,
    /// Optional adapter path value.
    pub adapter_path: Option<PathBuf>,
    /// The response value.
    pub response: ResponsePolicy,
    /// Enabled ACDD MCP subsystems.
    pub modules: SubsystemRegistry,
    /// The limits value.
    pub limits: ScannerLimits,
    /// The roots value.
    pub roots: Vec<PathBuf>,
    /// The ignored names value.
    pub ignored_names: BTreeSet<String>,
    /// Optional adapter version value.
    pub adapter_version: Option<String>,
    /// The digest value.
    pub digest: String,
    /// The linked workspaces value.
    pub linked_workspaces: Vec<LinkedWorkspace>,
    /// The owners value.
    pub owners: BTreeMap<String, String>,
    /// The aliases value.
    pub aliases: BTreeMap<String, String>,
    /// Milestone directories for task discovery; Markdown documents remain indexed.
    pub milestones: Vec<String>,
    /// Plan directories for task discovery; Markdown documents remain indexed.
    pub plans: Vec<String>,
    /// Configured spec collections (`specs:`).
    pub specs: BTreeMap<String, SpecCollectionConfig>,
    /// Directories searched for custom gate profiles.
    pub profiles_dirs: Vec<String>,
    /// State SQLite database path.
    pub state_db: String,
    /// Number of retained run trace files.
    pub retain_traces: usize,
    /// Whether debug applies.
    pub debug: bool,
}

impl Adapter {
    /// Returns all configured spec collection directory paths (`specs.*.path`).
    pub fn spec_paths(&self) -> Vec<String> {
        self.specs.values().map(|s| s.path.clone()).collect()
    }

    /// Resolves the configured `state_db` path against the primary worktree root.
    pub fn resolve_state_db_path(&self, base_root: &Path) -> PathBuf {
        let p = Path::new(&self.state_db);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base_root.join(p)
        }
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self {
            adapter_path: None,
            sensitive: Vec::new(),
            response: ResponsePolicy::default(),
            modules: SubsystemRegistry::default(),
            limits: ScannerLimits::default(),
            roots: Vec::new(),
            ignored_names: BTreeSet::new(),
            adapter_version: None,
            digest: String::new(),
            linked_workspaces: Vec::new(),
            owners: BTreeMap::new(),
            aliases: BTreeMap::new(),
            milestones: vec!["docs/milestones".into()],
            plans: vec!["docs/plans".into()],
            specs: default_specs(),
            profiles_dirs: default_profiles_dirs(),
            state_db: DEFAULT_STATE_DB.into(),
            retain_traces: DEFAULT_RETAIN_TRACES,
            debug: false,
        }
    }
}

/// Strict adapter validation report returning the loaded adapter plus any linked-workspace issues.
#[derive(Debug, Clone)]
pub struct StrictAdapterReport {
    /// Normalized workspace adapter configuration.
    pub adapter: Adapter,
    /// Validation errors collected for `linked_workspaces` entries.
    pub linked_workspace_errors: Vec<String>,
}

/// Legacy `0.5.x` `acdd.yaml` top-level keys that trigger `CONFIG_MIGRATION_REQUIRED`.
pub const LEGACY_05_KEYS: &[&str] = &[
    "milestones",
    "plans",
    "tasks_db",
    "workflows_dir",
    "milestone_prefix",
    "plan_prefix",
    "task_repository",
    "task_project",
    "acdd_profile",
];

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

fn validate_linked_workspace_target(
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

fn load_adapter_inner(
    root: &Path,
    adapter_path: Option<&Path>,
) -> std::io::Result<StrictAdapterReport> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let default_path = canonical_root.join("acdd.yaml");
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

    let code_enabled = modules.is_enabled("code");
    let docs_enabled = modules.is_enabled("docs");
    let specs_paths: BTreeMap<&str, &str> = specs
        .iter()
        .map(|(k, v)| (k.as_str(), v.path.as_str()))
        .collect();
    let default_specs_map = default_specs();
    let default_specs_paths: BTreeMap<&str, &str> = default_specs_map
        .iter()
        .map(|(k, v)| (k.as_str(), v.path.as_str()))
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
    for item in raw.docs.unwrap_or_default() {
        if item.contains('*') || item.contains('?') {
            continue;
        }
        let relative = Path::new(&item);
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
    let mut linked_workspaces = Vec::new();
    let mut linked_workspace_errors = Vec::new();
    let mut seen_lw_names = BTreeSet::new();
    let configs = raw.linked_workspaces.or(raw.workspaces).unwrap_or_default();
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

        let canonical_ws = match validate_linked_workspace_target(
            &canonical_root,
            trimmed_name,
            &lw_cfg.path,
        ) {
            Ok(p) => p,
            Err(msg) => {
                linked_workspace_errors.push(msg);
                continue;
            }
        };

        let raw_roots = lw_cfg.roots.unwrap_or_else(|| vec![".".into()]);
        let mut ws_roots = Vec::new();
        for item in raw_roots {
            if item == "." {
                ws_roots.push(canonical_ws.clone());
                continue;
            }
            let cand = canonical_ws.join(Path::new(&item));
            if cand.exists() {
                if let Ok(c) = cand.canonicalize() {
                    if c.starts_with(&canonical_ws) {
                        ws_roots.push(c);
                    }
                }
            }
        }
        for item in lw_cfg.docs.unwrap_or_default() {
            if item.contains('*') || item.contains('?') {
                continue;
            }
            let cand = canonical_ws.join(Path::new(&item));
            if cand.exists() {
                if let Ok(c) = cand.canonicalize() {
                    if c.starts_with(&canonical_ws) {
                        ws_roots.push(c);
                    }
                }
            }
        }
        ws_roots.sort();
        ws_roots.dedup();

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

    let milestones = specs
        .get("milestones")
        .map(|cfg| vec![cfg.path.clone()])
        .unwrap_or_else(|| vec!["docs/milestones".into()]);
    let plans = specs
        .get("plans")
        .map(|cfg| vec![cfg.path.clone()])
        .unwrap_or_else(|| vec!["docs/plans".into()]);

    Ok(StrictAdapterReport {
        adapter: Adapter {
            sensitive: raw.sensitive,
            adapter_path: explicit_adapter_path,
            response: raw.response,
            modules,
            limits: raw.limits.unwrap_or_default(),
            roots,
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
            debug: raw.debug
                || std::env::var("ACDD_DEBUG")
                    .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                || std::env::var("DEBUG")
                    .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        },
        linked_workspace_errors,
    })
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

/// Checks root scope against filesystem roots, system directories, user home directories, and multi-repository containers.
pub fn check_root_scope(root: &Path, allow_broad: bool) -> std::io::Result<()> {
    if allow_broad
        || std::env::var("ACDD_ALLOW_BROAD_ROOT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    {
        return Ok(());
    }

    // 1. Filesystem root
    if root == Path::new("/") {
        return Err(error(
            "refusing to index filesystem root '/'; specify a repository directory or set limits.allow_broad_root: true",
        ));
    }

    // 2. Direct parent is / or /home or /Users, or macOS canonical /private/{var,tmp,etc}
    if let Some(parent) = root.parent() {
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
        } else if parent == Path::new("/home") || parent == Path::new("/Users") {
            return Err(error(format!(
                "refusing to index user home directory '{root:?}'; specify a repository directory or set limits.allow_broad_root: true"
            )));
        }
    }

    // 3. Multi-repository container check:
    let is_git_repo = root.join(".git").exists();
    let has_manifest = root.join("acdd.yaml").exists()
        || root.join("Cargo.toml").exists()
        || root.join("package.json").exists()
        || root.join("pyproject.toml").exists()
        || root.join("go.mod").exists();

    if !is_git_repo && !has_manifest {
        if let Ok(entries) = fs::read_dir(root) {
            let mut git_subdirs = 0;
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        let p = entry.path();
                        if p.join(".git").exists() {
                            git_subdirs += 1;
                        } else if let Ok(sub_entries) = fs::read_dir(&p) {
                            for sub_entry in sub_entries.flatten() {
                                if let Ok(sft) = sub_entry.file_type() {
                                    if sft.is_dir() && sub_entry.path().join(".git").exists() {
                                        git_subdirs += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                if git_subdirs >= 2 {
                    return Err(error(format!(
                        "workspace root '{root:?}' appears to be a multi-repository container ({git_subdirs}+ repos detected); specify a single repository or set limits.allow_broad_root: true"
                    )));
                }
            }
        }
    }

    Ok(())
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
    Ok(Adapter {
        adapter_path,
        response: ResponsePolicy::default(),
        modules: SubsystemRegistry::default(),
        sensitive,
        limits,
        roots,
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
        debug: false,
    })
}

/// Current engine schema version stored in database metadata.
pub const ENGINE_SCHEMA_VERSION: &str = "15";

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
    fn scanner_limits_validate_authoritative_metadata_bounds() {
        let mut limits = ScannerLimits::default();
        assert_eq!(limits.max_metadata_bytes, 2048);
        assert!(limits.validate().is_ok());

        limits.max_metadata_bytes = 256;
        assert!(limits.validate().is_ok());

        limits.max_metadata_bytes = 0;
        let err_zero = limits.validate().unwrap_err();
        assert!(err_zero.to_string().contains("must be 1..=2048"));

        limits.max_metadata_bytes = 2049;
        let err_large = limits.validate().unwrap_err();
        assert!(err_large.to_string().contains("must be 1..=2048"));
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
        assert_eq!(defaults.specs["milestones"].path, "docs/milestones");
        assert_eq!(defaults.specs["milestones"].profile, None);
        assert_eq!(defaults.specs["plans"].path, "docs/plans");
        assert_eq!(defaults.specs["plans"].profile, None);
        assert_eq!(defaults.profiles_dirs, [".acdd/profiles", "docs/workflows"]);
        assert_eq!(defaults.state_db, ".acdd/state.sqlite");
        assert_eq!(defaults.retain_traces, 20);
        assert_eq!(defaults.digest, "");

        std::fs::write(dir.join("acdd.yaml"), "# comment only\n").unwrap();
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
        std::fs::write(dir.join("acdd.yaml"), base_yaml).unwrap();
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
        std::fs::write(dir.join("acdd.yaml"), invariant_yaml).unwrap();
        let invariant = load_adapter(&dir, None).unwrap();
        assert_eq!(invariant.digest, base.digest);
        assert!(!invariant.modules.is_enabled("state"));
        assert!(!invariant.modules.is_enabled("blackboard"));
        assert!(invariant.modules.is_domain_enabled("http"));
        std::fs::write(dir.join("acdd.yaml"), invariant_yaml).unwrap();
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
        std::fs::write(dir.join("acdd.yaml"), reordered_yaml).unwrap();
        let reordered = load_adapter(&dir, None).unwrap();
        assert_eq!(reordered.digest, base.digest);

        // Index-affecting toggles MUST change adapter.digest.
        for changed_yaml in [
            "roots: [.]\nmodules:\n  code: false\n  docs: true\n",
            "roots: [.]\nmodules:\n  code: true\n  docs: false\n",
            "roots: [.]\nspecs:\n  milestones:\n    path: docs/contracts\n  plans:\n    path: docs/plans\n",
        ] {
            std::fs::write(dir.join("acdd.yaml"), changed_yaml).unwrap();
            let changed = load_adapter(&dir, None).unwrap();
            assert_ne!(changed.digest, base.digest, "{changed_yaml}");
        }

        // Overlapping specs paths by components must fail; sibling prefix names pass.
        std::fs::write(
            dir.join("acdd.yaml"),
            "specs:\n  a:\n    path: docs/specs\n  b:\n    path: docs/specs/sub\n",
        )
        .unwrap();
        assert!(load_adapter(&dir, None).is_err());

        std::fs::write(
            dir.join("acdd.yaml"),
            "specs:\n  a:\n    path: docs/specs\n  b:\n    path: docs/specs_v2\n",
        )
        .unwrap();
        let derived = load_adapter(&dir, None).unwrap();
        assert_eq!(derived.milestones, vec!["docs/milestones"]);
        assert_eq!(derived.plans, vec!["docs/plans"]);

        std::fs::write(
            dir.join("acdd.yaml"),
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

        // 1. deny_unknown_fields on AdapterFile, LinkedWorkspaceConfig, ScannerLimits -> ProfileSchemaInvalid.
        for bad_yaml in [
            "unknown_top_level: true\n",
            "limits:\n  unknown_limit: 5\n",
            "linked_workspaces:\n  - name: dep\n    path: .\n    unknown_lw: 1\n",
            "not: [valid: yaml\n",
        ] {
            std::fs::write(dir.join("acdd.yaml"), bad_yaml).unwrap();
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
            std::fs::write(dir.join("acdd.yaml"), &yaml).unwrap();
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
        std::fs::write(dir.join("acdd.yaml"), &full_lw_yaml).unwrap();

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

        // 5. read_policy_from_value fails closed on malformed non-null fields & handles external adapter_path.
        let mut base_adapter = Adapter {
            roots: vec![dir.clone()],
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
