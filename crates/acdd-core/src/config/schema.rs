//! Adapter schema definitions, spec collection configuration, and validation.

use super::constants::{
    DEFAULT_LINT_DIR, DEFAULT_MAX_FILES, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_METADATA_BYTES,
    DEFAULT_MAX_TOTAL_BYTES, DEFAULT_RETAIN_TRACES, DEFAULT_SPECS_DIR, DEFAULT_SPEC_DIGITS,
    DEFAULT_STATE_DB, DEFAULT_WORKFLOWS_DIR, MAX_METADATA_BYTES,
};
use crate::errors::{AcddError, ErrorCode};
use crate::lint::ArchitectureConfig;
use crate::response::ResponsePolicy;
use crate::schema::SchemaDef;
use crate::subsystems::SubsystemRegistry;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn default_spec_digits() -> usize {
    DEFAULT_SPEC_DIGITS
}

pub(super) fn default_specs_dirs() -> Vec<String> {
    vec![DEFAULT_SPECS_DIR.into()]
}

pub(super) fn default_workflows_dirs() -> Vec<String> {
    vec![DEFAULT_WORKFLOWS_DIR.into()]
}

pub(super) fn default_lint_dirs() -> Vec<String> {
    vec![DEFAULT_LINT_DIR.into()]
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSpecFields {
    Schema(Box<SchemaDef>),
    Properties(BTreeMap<String, SchemaDef>),
}

pub(super) fn deserialize_optional_spec_fields<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<SchemaDef>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<RawSpecFields>::deserialize(deserializer)?;
    Ok(raw.map(|r| match r {
        RawSpecFields::Schema(schema) => *schema,
        RawSpecFields::Properties(properties) => SchemaDef {
            properties,
            ..SchemaDef::object()
        },
    }))
}

/// ANSI color mode for CLI roadmap rendering (`acdd.yaml` `roadmap.color`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadmapColorMode {
    /// Enable ANSI colors when stdout is a TTY and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always emit ANSI colors (unless overridden by `NO_COLOR` or CLI `--color`).
    Always,
    /// Never emit ANSI escape sequences.
    Never,
}

/// Workspace CLI roadmap display configuration (`roadmap:` in `acdd.yaml`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoadmapDisplayConfig {
    /// Ordered field/readiness badge names projected on roadmap entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
    /// Default grouping axis (`wave`, `spec`, or custom schema field name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    /// Default ANSI color policy (`auto | always | never`).
    #[serde(default)]
    pub color: RoadmapColorMode,
}

impl RoadmapDisplayConfig {
    /// Validate roadmap display settings.
    pub fn validate(&self) -> std::result::Result<(), AcddError> {
        for badge in &self.badges {
            if badge.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "roadmap.badges entries must not be empty",
                ));
            }
        }
        if let Some(ref group_by) = self.group_by {
            if group_by.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "roadmap.group_by must not be empty when specified",
                ));
            }
        }
        Ok(())
    }
}

/// Named item collection declaration under `specs.<name>.items.<collection>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpecItemConfig {
    /// Singular Markdown heading token (`### <item>: <ref>`), defaulting to the collection key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// Optional `SchemaDef` (or property map) validating item-specific fields.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_spec_fields",
        skip_serializing_if = "Option::is_none"
    )]
    pub fields: Option<SchemaDef>,
}

impl SpecItemConfig {
    /// Effective singular heading token (`### <item>: <ref>`).
    pub fn effective_item_token<'a>(&'a self, collection_key: &'a str) -> &'a str {
        self.item.as_deref().unwrap_or(collection_key)
    }
}

/// Configuration for a spec collection under `specs:` in `acdd.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpecConfig {
    /// Legacy single workspace-relative directory path containing spec documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Multi-entry workspace-relative paths or glob patterns (e.g. `extensions/*/docs/milestones`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Optional built-in Spec base (`contract` or `memory`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Optional external spec schema reference (`<name> | <path>[:<sha256_prefix>]`) resolved in `specs_dirs`.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
    /// Optional explicit workflow for specs in this collection (else workspace default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Whether specs in this collection appear in `acdd roadmap` by default (`true` on `contract`, `false` on `memory`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roadmap: Option<bool>,
    /// Allowed spec ID prefixes (e.g. `["AC"]`).
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// Zero-padded numeric suffix width for drafted spec IDs.
    #[serde(default = "default_spec_digits")]
    pub digits: usize,
    /// Optional `SchemaDef` (or property map) validating extra Spec frontmatter fields.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_spec_fields",
        skip_serializing_if = "Option::is_none"
    )]
    pub fields: Option<SchemaDef>,
    /// Optional named item collections (`spec.<collection>` -> `### <item>: <ref>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, SpecItemConfig>,
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            path: None,
            paths: Vec::new(),
            base: None,
            spec_ref: None,
            workflow: None,
            roadmap: None,
            prefixes: Vec::new(),
            digits: DEFAULT_SPEC_DIGITS,
            fields: None,
            items: BTreeMap::new(),
        }
    }
}

impl SpecConfig {
    /// Effective built-in base (`"contract"` or `"memory"`) for a named collection,
    /// returning `None` when `ref:` binds an external `.acdd/specs/*.yaml` schema directly.
    pub fn effective_builtin_base(&self, collection_key: Option<&str>) -> Option<&str> {
        if let Some(ref base) = self.base {
            return Some(base.as_str());
        }
        if self.spec_ref.is_some() {
            return None;
        }
        if collection_key == Some("memory") {
            return Some("memory");
        }
        Some("contract")
    }

    /// Effective default roadmap visibility taking collection key (`"memory"` shorthand) into account.
    pub fn effective_roadmap(&self, collection_key: Option<&str>) -> bool {
        self.roadmap
            .unwrap_or_else(|| self.effective_builtin_base(collection_key) != Some("memory"))
    }
    /// Return all configured normalized patterns (`path` followed by `paths`, deduplicated in order).
    pub fn patterns(&self) -> Vec<&str> {
        let mut out = Vec::new();
        if let Some(ref p) = self.path {
            if !p.is_empty() {
                out.push(p.as_str());
            }
        }
        for p in &self.paths {
            if !p.is_empty() && !out.contains(&p.as_str()) {
                out.push(p.as_str());
            }
        }
        out
    }

    /// Return the first literal non-glob pattern suitable as a target directory for `acdd draft`.
    pub fn default_draft_dir(&self) -> Option<&str> {
        self.patterns().into_iter().find(|pat| {
            !pat.contains('*') && !pat.contains('?') && !pat.contains('{') && !pat.contains('[')
        })
    }

    /// Return the length of the longest configured pattern matching `rel` (a workspace-relative
    /// file or directory path), or `None` if no pattern matches.
    pub fn longest_matching_pattern_len(&self, rel: &str) -> Option<usize> {
        let trimmed = rel.trim().replace('\\', "/");
        let norm = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if norm.is_empty() {
            return None;
        }
        self.patterns()
            .into_iter()
            .filter(|pat| {
                crate::path_policy::glob_matches(pat, norm)
                    || crate::path_policy::glob_matches(&format!("{pat}/**"), norm)
            })
            .map(str::len)
            .max()
    }

    /// Return `true` if `rel` (a workspace-relative file or directory path) falls under any pattern.
    pub fn matches_rel_path(&self, rel: &str) -> bool {
        self.longest_matching_pattern_len(rel).is_some()
    }

    /// Resolve the owning concrete collection root directory for a matched workspace-relative spec path.
    ///
    /// For example, `docs/milestones/team/target.md` under `docs/milestones` (or `docs/milestones/**`)
    /// resolves to `docs/milestones`, and `extensions/pkg/docs/milestones/team/target.md` under
    /// `extensions/*/docs/milestones` resolves to `extensions/pkg/docs/milestones`.
    pub fn matched_concrete_root(&self, rel: &str) -> Option<String> {
        let trimmed = rel.trim().replace('\\', "/");
        let norm = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if norm.is_empty() {
            return None;
        }
        let rel_segs: Vec<&str> = norm.split('/').collect();
        let max_dir_segs = rel_segs.len().saturating_sub(1).max(1);
        let mut best: Option<String> = None;
        for pat in self.patterns() {
            for variant in crate::path_policy::expand_glob_braces(pat) {
                let mut dir_segs: Vec<&str> = variant.split('/').collect();
                while dir_segs.last() == Some(&"**") && dir_segs.len() > 1 {
                    dir_segs.pop();
                }
                let dir_pattern = dir_segs.join("/");
                if let Some(k) = (1..=max_dir_segs).find(|&k| {
                    !rel_segs[..k].contains(&"archive")
                        && crate::path_policy::glob_matches(&dir_pattern, &rel_segs[..k].join("/"))
                }) {
                    let root = rel_segs[..k].join("/");
                    if best.as_ref().is_none_or(|cur| root.len() > cur.len()) {
                        best = Some(root);
                    }
                }
            }
        }
        best
    }

    /// Return `true` if `rel_posix` is a direct `.md` child of `<concrete_root>/archive/`
    /// owned by this collection.
    pub fn is_direct_archive_file(&self, rel_posix: &str) -> bool {
        let norm = rel_posix.strip_prefix("./").unwrap_or(rel_posix);
        if norm.contains('\\') || !norm.ends_with(".md") {
            return false;
        }
        let Some((archive_dir, file_name)) = norm.rsplit_once('/') else {
            return false;
        };
        if file_name.is_empty()
            || file_name == "."
            || file_name == ".."
            || archive_dir
                .split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return false;
        }
        let Some((root_rel, archive_leaf)) = archive_dir.rsplit_once('/') else {
            return false;
        };
        if archive_leaf != "archive" || root_rel.is_empty() {
            return false;
        }
        self.matched_concrete_root(norm).as_deref() == Some(root_rel)
    }
}

/// Returns the default `specs:` collections (`milestones` and `plans`).
pub fn default_specs() -> BTreeMap<String, SpecConfig> {
    BTreeMap::from([
        (
            "milestones".to_owned(),
            SpecConfig {
                path: Some("docs/milestones".to_owned()),
                prefixes: vec!["AC".to_owned()],
                ..SpecConfig::default()
            },
        ),
        (
            "plans".to_owned(),
            SpecConfig {
                path: Some("docs/plans".to_owned()),
                ..SpecConfig::default()
            },
        ),
    ])
}

fn validate_spec_fields_schema(schema: &SchemaDef, ctx: &str) -> std::result::Result<(), AcddError> {
    if schema.schema_type != "object" {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("{ctx} schema must have type 'object', got '{}'", schema.schema_type),
        ));
    }
    schema
        .validate_schema(ctx)
        .map_err(|err| AcddError::new(ErrorCode::SpecSchemaInvalid, err.message))
}

pub(super) fn normalize_spec_collection_path(raw: &str) -> std::result::Result<String, AcddError> {
    let normalized = raw
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned();
    if normalized.is_empty() || !crate::path_policy::is_valid_scope_glob(&normalized) {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "spec collection path '{raw}' must be a normalized workspace-relative path or glob"
            ),
        ));
    }
    Ok(normalized)
}

pub(super) fn validate_and_normalize_specs(
    specs: &mut BTreeMap<String, SpecConfig>,
) -> std::result::Result<(), AcddError> {
    let mut normalized_entries: Vec<(String, Vec<String>)> = Vec::with_capacity(specs.len());
    for (name, cfg) in specs.iter_mut() {
        if name.trim().is_empty() {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                "spec collection name must not be empty",
            ));
        }
        if let Some(ref base) = cfg.base {
            if !matches!(base.as_str(), "contract" | "memory") {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "specs.{name}.base '{base}' must be 'contract' or 'memory' (multi-hop spec inheritance is forbidden)"
                    ),
                ));
            }
            if cfg.spec_ref.is_some() {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "specs.{name} cannot declare both 'base' and 'ref' (single-hop inheritance only)"
                    ),
                ));
            }
        }
        if let Some(ref workflow) = cfg.workflow {
            if workflow.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("specs.{name}.workflow must not be empty when specified"),
                ));
            }
        }
        if !(1..=16).contains(&cfg.digits) {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("specs.{name}.digits must be in 1..=16, got {}", cfg.digits),
            ));
        }
        if let Some(ref fields) = cfg.fields {
            validate_spec_fields_schema(fields, &format!("specs.{name}.fields"))?;
        }
        let mut seen_item_tokens = BTreeSet::new();
        for (col_key, item_cfg) in &cfg.items {
            if col_key.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("specs.{name}.items collection key must not be empty"),
                ));
            }
            if let Some(ref token) = item_cfg.item {
                if token.trim().is_empty() || token.trim() != token || token.contains(':') {
                    return Err(AcddError::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!("specs.{name}.items.{col_key}.item '{token}' is invalid"),
                    ));
                }
            }
            let effective_token = item_cfg.effective_item_token(col_key);
            if !seen_item_tokens.insert(effective_token.to_owned()) {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "specs.{name}.items declares duplicate heading token '{effective_token}'"
                    ),
                ));
            }
            if let Some(ref item_fields) = item_cfg.fields {
                validate_spec_fields_schema(
                    item_fields,
                    &format!("specs.{name}.items.{col_key}.fields"),
                )?;
            }
        }
        if let Some(ref mut p) = cfg.path {
            *p = normalize_spec_collection_path(p)?;
        }
        for p in &mut cfg.paths {
            *p = normalize_spec_collection_path(p)?;
        }
        let pats: Vec<String> = cfg.patterns().into_iter().map(str::to_owned).collect();
        if pats.is_empty() {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("specs.{name} must configure 'path' or non-empty 'paths'"),
            ));
        }
        normalized_entries.push((name.clone(), pats));
    }
    for (i, (left_name, left_pats)) in normalized_entries.iter().enumerate() {
        for (right_name, right_pats) in normalized_entries.iter().skip(i + 1) {
            for pat_a in left_pats {
                for pat_b in right_pats {
                    if crate::path_policy::patterns_overlap(pat_a, pat_b) {
                        return Err(AcddError::new(
                            ErrorCode::SpecSchemaInvalid,
                            format!(
                                "specs.{left_name} pattern '{pat_a}' overlaps specs.{right_name} pattern '{pat_b}'"
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

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
/// Resource ceilings enforced during repository scanning and metadata serialization.
pub struct ScannerLimits {
    #[serde(default = "default_max_files")]
    /// Maximum number of source/doc files admitted per scan.
    pub max_files: usize,
    #[serde(default = "default_max_file_bytes")]
    /// Maximum byte size of a single indexed file.
    pub max_file_bytes: u64,
    #[serde(default = "default_max_total_bytes")]
    /// Maximum cumulative source bytes admitted across a scan.
    pub max_total_bytes: u64,
    #[serde(default = "default_max_metadata_bytes")]
    /// Authoritative maximum size in bytes for metadata and notes.
    pub max_metadata_bytes: usize,
    #[serde(default)]
    /// Whether broad root paths (e.g. `.`) are permitted without error.
    pub allow_broad_root: bool,
    #[serde(default, skip_serializing)]
    /// Optional transient memory budget in MiB; YAML takes precedence over the environment.
    pub memory_budget_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional maximum snapshot/commit blob size in bytes (`None` disables the ceiling).
    pub max_snapshot_file_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional allowlist of binary extensions exempt from NUL-byte rejection (`None` disables NUL check).
    pub allowed_binary_extensions: Option<Vec<String>>,
}

impl ScannerLimits {
    /// Validates scanner and metadata limits against authoritative ceilings.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=MAX_METADATA_BYTES).contains(&self.max_metadata_bytes),
            "limits.max_metadata_bytes must be 1..={MAX_METADATA_BYTES}"
        );
        if self.max_snapshot_file_bytes == Some(0) {
            anyhow::bail!("limits.max_snapshot_file_bytes must be >= 1");
        }
        if let Some(ref exts) = self.allowed_binary_extensions {
            crate::path_policy::normalize_binary_extensions(exts).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    /// Validate and normalize `allowed_binary_extensions` into a lowercase set, preserving `None`.
    pub fn normalized_allowed_binary_extensions(
        &self,
    ) -> std::result::Result<Option<BTreeSet<String>>, AcddError> {
        self.allowed_binary_extensions
            .as_ref()
            .map(crate::path_policy::normalize_binary_extensions)
            .transpose()
            .map_err(|msg| AcddError::new(ErrorCode::WorkflowSchemaInvalid, msg))
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
            max_snapshot_file_bytes: None,
            allowed_binary_extensions: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
/// Raw deserialized `acdd.yaml` workspace adapter manifest.
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
    /// Optional top-level subsystem preset (`index | spec | state | full | minimal`).
    #[serde(default)]
    pub preset: Option<crate::subsystems::SubsystemPreset>,
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
    pub specs: Option<BTreeMap<String, SpecConfig>>,
    /// Directories searched for external spec schema files (`specs_dirs:`).
    #[serde(default)]
    pub specs_dirs: Option<Vec<String>>,
    /// Directories searched for custom workflows.
    #[serde(default)]
    pub workflows_dirs: Option<Vec<String>>,
    /// Directories searched for externalized lint/architecture rules (`lint_dirs:`).
    #[serde(default)]
    pub lint_dirs: Option<Vec<String>>,
    /// Optional CLI roadmap display settings (`roadmap:`).
    #[serde(default)]
    pub roadmap: Option<RoadmapDisplayConfig>,
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
/// Raw YAML configuration entry for one linked secondary workspace.
pub struct LinkedWorkspaceConfig {
    /// Unique alias referencing this linked workspace.
    pub name: String,
    /// Relative or canonical filesystem path to the linked workspace root.
    pub path: String,
    #[serde(default)]
    /// Optional flag to disable linking without deleting the config block.
    pub enabled: Option<bool>,
    #[serde(default)]
    /// Optional source root overrides for the linked workspace.
    pub roots: Option<Vec<String>>,
    #[serde(default, alias = "doc_roots")]
    /// Optional documentation root overrides for the linked workspace.
    pub docs: Option<Vec<String>>,
    #[serde(default)]
    /// Optional directory/file names ignored when scanning the linked workspace.
    pub ignore: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Normalized and resolved secondary workspace configuration.
pub struct LinkedWorkspace {
    /// Unique alias referencing this linked workspace.
    pub name: String,
    /// Canonical filesystem root of the linked workspace.
    pub path: PathBuf,
    /// Resolved source roots inside the linked workspace.
    pub roots: Vec<PathBuf>,
    /// Directory and file names ignored when scanning this workspace.
    pub ignored_names: BTreeSet<String>,
}

#[derive(Debug, Clone)]
/// Validated, normalized workspace adapter configuration resolved from `acdd.yaml`.
pub struct Adapter {
    /// Workspace-relative glob patterns blocked by the reader.
    pub sensitive: Vec<String>,
    /// Resolved path to the `acdd.yaml` manifest, if present.
    pub adapter_path: Option<PathBuf>,
    /// Response pagination and byte-budget defaults.
    pub response: ResponsePolicy,
    /// Enabled ACDD MCP subsystems.
    pub modules: SubsystemRegistry,
    /// File count and byte-size limits for workspace scanning.
    pub limits: ScannerLimits,
    /// Normalized source roots admitted for indexing.
    pub roots: Vec<PathBuf>,
    /// Configured documentation root patterns/files (`docs:`).
    pub docs: Vec<String>,
    /// Exact directory/file names excluded from workspace scans.
    pub ignored_names: BTreeSet<String>,
    /// Optional `adapter_version` pin declared in `acdd.yaml`.
    pub adapter_version: Option<String>,
    /// Content hash of the normalized adapter configuration.
    pub digest: String,
    /// Resolved linked secondary workspaces.
    pub linked_workspaces: Vec<LinkedWorkspace>,
    /// Path-pattern to owner-name mappings (`owners:`).
    pub owners: BTreeMap<String, String>,
    /// Path/symbol alias mappings (`aliases:`).
    pub aliases: BTreeMap<String, String>,
    /// Milestone directories for task discovery; Markdown documents remain indexed.
    pub milestones: Vec<String>,
    /// Plan directories for task discovery; Markdown documents remain indexed.
    pub plans: Vec<String>,
    /// Configured spec collections (`specs:`).
    pub specs: BTreeMap<String, SpecConfig>,
    /// Directories searched for external spec schema files (`specs_dirs:`).
    pub specs_dirs: Vec<String>,
    /// Directories searched for custom workflows.
    pub workflows_dirs: Vec<String>,
    /// Directories searched for externalized lint/architecture rules (`lint_dirs:`).
    pub lint_dirs: Vec<String>,
    /// Resolved CLI roadmap display settings (`roadmap:`).
    pub roadmap: RoadmapDisplayConfig,
    /// State SQLite database path.
    pub state_db: String,
    /// Number of retained run trace files.
    pub retain_traces: usize,
    /// Configured architectural zones and static lint rules (hydrated from `lint_dirs`).
    pub architecture: ArchitectureConfig,
    /// Enables daily redacted JSONL CLI/MCP telemetry logs under `.acdd/logs/`.
    pub debug: bool,
}

impl Adapter {
    /// Returns all configured spec collection directory patterns (`specs.*.path` / `specs.*.paths`).
    pub fn spec_paths(&self) -> Vec<String> {
        self.specs
            .values()
            .flat_map(|s| s.patterns().into_iter().map(str::to_owned))
            .collect()
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
            docs: Vec::new(),
            ignored_names: BTreeSet::new(),
            adapter_version: None,
            digest: String::new(),
            linked_workspaces: Vec::new(),
            owners: BTreeMap::new(),
            aliases: BTreeMap::new(),
            milestones: vec!["docs/milestones".into()],
            plans: vec!["docs/plans".into()],
            specs: default_specs(),
            specs_dirs: default_specs_dirs(),
            workflows_dirs: default_workflows_dirs(),
            lint_dirs: default_lint_dirs(),
            roadmap: RoadmapDisplayConfig::default(),
            state_db: DEFAULT_STATE_DB.into(),
            retain_traces: DEFAULT_RETAIN_TRACES,
            architecture: ArchitectureConfig::default(),
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

#[cfg(test)]
mod tests {
    use super::super::loader::load_adapter;
    use super::super::ACDD_CONFIG_FILE;
    use super::*;

    #[test]
    fn scanner_limits_validate_authoritative_metadata_bounds() {
        let mut limits = ScannerLimits::default();
        assert_eq!(limits.max_metadata_bytes, 2048);
        assert_eq!(limits.max_snapshot_file_bytes, None);
        assert_eq!(limits.allowed_binary_extensions, None);
        assert!(limits.validate().is_ok());

        limits.max_metadata_bytes = 256;
        limits.max_snapshot_file_bytes = Some(1024);
        limits.allowed_binary_extensions =
            Some(vec![".PNG".to_owned(), "mo".to_owned(), "c++".to_owned()]);
        assert!(limits.validate().is_ok());

        limits.max_snapshot_file_bytes = Some(0);
        assert!(limits
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max_snapshot_file_bytes"));
        limits.max_snapshot_file_bytes = Some(1024);

        for bad_ext in [
            "", ".", "..png", "png jpg", "*.png", "a/png", "a\\png", "png?",
        ] {
            limits.allowed_binary_extensions = Some(vec![bad_ext.to_owned()]);
            assert!(
                limits.validate().is_err(),
                "expected invalid extension '{bad_ext}' to fail"
            );
        }
        limits.allowed_binary_extensions = None;

        limits.max_metadata_bytes = 0;
        let err_zero = limits.validate().unwrap_err();
        assert!(err_zero.to_string().contains("must be 1..=2048"));

        limits.max_metadata_bytes = 2049;
        let err_large = limits.validate().unwrap_err();
        assert!(err_large.to_string().contains("must be 1..=2048"));
    }

    #[test]
    fn spec_collection_multi_paths_globs_and_confinement() {
        let dir = crate::fs::ScopedTempDir::create("acdd_spec_multi_paths").unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs/milestones")).unwrap();
        std::fs::create_dir_all(root.join("extensions/beta/docs/milestones")).unwrap();
        std::fs::create_dir_all(root.join("extensions/alpha/docs/milestones")).unwrap();
        std::fs::create_dir_all(root.join("docs/plans")).unwrap();

        std::fs::write(
            root.join(ACDD_CONFIG_FILE),
            "roots: [.]\nspecs:\n  milestones:\n    paths:\n      - docs/milestones\n      - extensions/*/docs/milestones\n    prefixes: [AC]\n    digits: 3\n  plans:\n    path: docs/plans\n    prefixes: []\n    digits: 3\n",
        )
        .unwrap();

        let adapter = load_adapter(root, None).unwrap();
        let ms = &adapter.specs["milestones"];
        assert_eq!(
            ms.patterns(),
            vec!["docs/milestones", "extensions/*/docs/milestones"]
        );
        assert_eq!(ms.default_draft_dir(), Some("docs/milestones"));
        assert!(ms.matches_rel_path("docs/milestones/AC-001.md"));
        assert!(ms.matches_rel_path("extensions/alpha/docs/milestones/AC-002.md"));
        assert!(!ms.matches_rel_path("docs/plans/my-plan.md"));
        assert!(adapter.specs["plans"].prefixes.is_empty());

        let dirs = ms.resolve_dirs(root).unwrap();
        assert_eq!(
            dirs,
            vec![
                root.join("docs/milestones"),
                root.join("extensions/alpha/docs/milestones"),
                root.join("extensions/beta/docs/milestones"),
            ]
        );

        let missing_col = SpecConfig {
            path: Some("docs/nonexistent".to_owned()),
            ..SpecConfig::default()
        };
        assert!(missing_col.resolve_dirs(root).unwrap().is_empty());

        // Recursive `**` glob excludes `archive/` directories and `matched_concrete_root` resolves owning collection root.
        std::fs::create_dir_all(root.join("docs/milestones/archive")).unwrap();
        std::fs::create_dir_all(root.join("docs/milestones/team")).unwrap();
        let rec_col = SpecConfig {
            path: Some("docs/milestones/**".to_owned()),
            prefixes: vec!["AC".to_owned()],
            ..SpecConfig::default()
        };
        let rec_dirs = rec_col.resolve_dirs(root).unwrap();
        assert!(!rec_dirs.contains(&root.join("docs/milestones/archive")));
        assert!(rec_dirs.contains(&root.join("docs/milestones/team")));
        assert_eq!(
            ms.matched_concrete_root("docs/milestones/team/target.md")
                .as_deref(),
            Some("docs/milestones")
        );
        assert_eq!(
            ms.matched_concrete_root("extensions/alpha/docs/milestones/team/target.md")
                .as_deref(),
            Some("extensions/alpha/docs/milestones")
        );
        assert_eq!(
            rec_col
                .matched_concrete_root("docs/milestones/team/target.md")
                .as_deref(),
            Some("docs/milestones")
        );
        let deep_col = SpecConfig {
            path: Some("extensions/**/docs/milestones/**".to_owned()),
            prefixes: vec!["AC".to_owned()],
            ..SpecConfig::default()
        };
        assert_eq!(
            deep_col
                .matched_concrete_root("extensions/pkg/docs/milestones/team/target.md")
                .as_deref(),
            Some("extensions/pkg/docs/milestones")
        );

        // Invalid pattern matrix must fail closed.
        for bad_yaml in [
            "roots: [.]\nspecs:\n  milestones:\n    paths: []\n",
            "roots: [.]\nspecs:\n  milestones:\n    path: ../outside\n",
            "roots: [.]\nspecs:\n  milestones:\n    path: /abs/path\n",
            "roots: [.]\nspecs:\n  milestones:\n    paths: [docs/{unclosed]\n",
            "roots: [.]\nspecs:\n  a:\n    path: docs/specs\n  b:\n    path: docs/specs/sub\n",
        ] {
            std::fs::write(root.join(ACDD_CONFIG_FILE), bad_yaml).unwrap();
            assert!(
                load_adapter(root, None).is_err(),
                "expected error for:\n{bad_yaml}"
            );
        }

        #[cfg(unix)]
        {
            let outside = crate::fs::ScopedTempDir::create("acdd_spec_outside").unwrap();
            std::fs::create_dir_all(outside.path().join("docs/milestones")).unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("extensions/evil")).unwrap();
            let dirs_after_symlink = ms.resolve_dirs(root).unwrap();
            assert_eq!(dirs_after_symlink.len(), 3);

            use std::os::unix::fs::PermissionsExt;
            let wx_dir = root.join("wx-only");
            std::fs::create_dir_all(&wx_dir).unwrap();
            std::fs::set_permissions(&wx_dir, std::fs::Permissions::from_mode(0o300)).unwrap();
            let res =
                crate::fs::write_new_confined(root, Path::new("wx-only/review-probe.md"), b"probe");
            std::fs::set_permissions(&wx_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(res.is_err(), "0o300 dir must fail before publishing target");
            assert!(
                !wx_dir.join("review-probe.md").exists(),
                "failed write_new_confined must not leave stranded target"
            );
        }
    }
}
