//! Adapter schema definitions, spec collection configuration, and validation.

use super::constants::{
    error, DEFAULT_MAX_FILES, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_METADATA_BYTES,
    DEFAULT_MAX_TOTAL_BYTES, DEFAULT_RETAIN_TRACES, DEFAULT_SPEC_DIGITS, DEFAULT_STATE_DB,
    MAX_METADATA_BYTES,
};
use crate::fs::checked_child;
use crate::lint::ArchitectureConfig;
use crate::response::ResponsePolicy;
use crate::subsystems::SubsystemRegistry;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn default_spec_digits() -> usize {
    DEFAULT_SPEC_DIGITS
}

pub(super) fn default_profiles_dirs() -> Vec<String> {
    vec![".acdd/profiles".into()]
}

/// Configuration for a spec collection under `specs:` in `acdd.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpecCollectionConfig {
    /// Legacy single workspace-relative directory path containing spec documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Multi-entry workspace-relative paths or glob patterns (e.g. `extensions/*/docs/milestones`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Optional explicit gate profile for specs in this collection (else workspace default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Allowed spec ID prefixes (e.g. `["AC"]`).
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// Zero-padded numeric suffix width for drafted spec IDs.
    #[serde(default = "default_spec_digits")]
    pub digits: usize,
}

impl SpecCollectionConfig {
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

    /// Return `true` if `rel` (a workspace-relative file or directory path) falls under any pattern.
    pub fn matches_rel_path(&self, rel: &str) -> bool {
        let trimmed = rel.trim().replace('\\', "/");
        let norm = trimmed.strip_prefix("./").unwrap_or(&trimmed).trim_end_matches('/');
        if norm.is_empty() {
            return false;
        }
        self.patterns().into_iter().any(|pat| {
            crate::path_policy::glob_matches(pat, norm)
                || crate::path_policy::glob_matches(&format!("{pat}/**"), norm)
        })
    }

    /// Resolve the owning concrete collection root directory for a matched workspace-relative spec path.
    ///
    /// For example, `docs/milestones/team/target.md` under `docs/milestones` (or `docs/milestones/**`)
    /// resolves to `docs/milestones`, and `extensions/pkg/docs/milestones/team/target.md` under
    /// `extensions/*/docs/milestones` resolves to `extensions/pkg/docs/milestones`.
    pub fn matched_concrete_root(&self, rel: &str) -> Option<String> {
        let trimmed = rel.trim().replace('\\', "/");
        let norm = trimmed.strip_prefix("./").unwrap_or(&trimmed).trim_end_matches('/');
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

    /// Expand all configured patterns into deterministic, sorted existing directories confined to `root`.
    ///
    /// Non-existent literal directories are omitted gracefully; symlinks along literal paths
    /// or escaping `root` fail closed, and wildcard (`*` / `**`) expansion never follows symlinks.
    pub fn resolve_dirs(&self, root: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut dirs = BTreeSet::new();
        for pat in self.patterns() {
            for variant in crate::path_policy::expand_glob_braces(pat) {
                let norm = normalize_spec_collection_path(&variant)?;
                let segs: Vec<&str> = norm.split('/').collect();
                expand_spec_dir_segments(root, Path::new(""), &segs, &mut dirs)?;
            }
        }
        Ok(dirs.into_iter().collect())
    }
}

fn expand_spec_dir_segments(
    root: &Path,
    current_rel: &Path,
    segs: &[&str],
    out: &mut BTreeSet<PathBuf>,
) -> std::io::Result<()> {
    if segs.is_empty() {
        if current_rel.as_os_str().is_empty()
            || current_rel
                .components()
                .any(|c| c.as_os_str() == "archive")
        {
            return Ok(());
        }
        let candidate = checked_child(root, current_rel)?;
        match fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(error(format!(
                    "spec collection '{}' must not be a symlink",
                    current_rel.to_string_lossy()
                )));
            }
            Ok(meta) if meta.is_dir() => {
                out.insert(candidate);
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        return Ok(());
    }

    let (first, rest) = (segs[0], &segs[1..]);
    if first == "**" || first.contains('*') || first.contains('?') {
        if first == "**" {
            expand_spec_dir_segments(root, current_rel, rest, out)?;
        }
        let base = if current_rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            checked_child(root, current_rel)?
        };
        let entries = match fs::read_dir(&base) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let next_segs = if first == "**" { segs } else { rest };
        for entry in entries {
            let entry = entry?;
            if entry.file_name() == "archive" {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            if first == "**" {
                expand_spec_dir_segments(root, &current_rel.join(entry.file_name()), next_segs, out)?;
            } else {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if crate::path_policy::glob_matches(first, &name) {
                    expand_spec_dir_segments(root, &current_rel.join(name), next_segs, out)?;
                }
            }
        }
    } else {
        expand_spec_dir_segments(root, &current_rel.join(first), rest, out)?;
    }
    Ok(())
}

/// Returns the default `specs:` collections (`milestones` and `plans`).
pub fn default_specs() -> BTreeMap<String, SpecCollectionConfig> {
    BTreeMap::from([
        (
            "milestones".to_owned(),
            SpecCollectionConfig {
                path: Some("docs/milestones".to_owned()),
                paths: Vec::new(),
                profile: None,
                prefixes: vec!["AC".to_owned()],
                digits: DEFAULT_SPEC_DIGITS,
            },
        ),
        (
            "plans".to_owned(),
            SpecCollectionConfig {
                path: Some("docs/plans".to_owned()),
                paths: Vec::new(),
                profile: None,
                prefixes: Vec::new(),
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
    if normalized.is_empty() || !crate::path_policy::is_valid_scope_glob(&normalized) {
        return Err(error(format!(
            "spec collection path '{raw}' must be a normalized workspace-relative path or glob"
        )));
    }
    Ok(normalized)
}

pub(super) fn validate_and_normalize_specs(
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
        if let Some(ref mut p) = cfg.path {
            *p = normalize_spec_collection_path(p)?;
        }
        for p in &mut cfg.paths {
            *p = normalize_spec_collection_path(p)?;
        }
        let pats: Vec<String> = cfg.patterns().into_iter().map(str::to_owned).collect();
        if pats.is_empty() {
            return Err(error(format!(
                "specs.{name} must configure 'path' or non-empty 'paths'"
            )));
        }
        normalized_entries.push((name.clone(), pats));
    }
    for (i, (left_name, left_pats)) in normalized_entries.iter().enumerate() {
        for (right_name, right_pats) in normalized_entries.iter().skip(i + 1) {
            for pat_a in left_pats {
                for pat_b in right_pats {
                    if crate::path_policy::patterns_overlap(pat_a, pat_b) {
                        return Err(error(format!(
                            "specs.{left_name} pattern '{pat_a}' overlaps specs.{right_name} pattern '{pat_b}'"
                        )));
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
    ) -> std::result::Result<Option<BTreeSet<String>>, crate::errors::StateError> {
        self.allowed_binary_extensions
            .as_ref()
            .map(crate::path_policy::normalize_binary_extensions)
            .transpose()
            .map_err(|msg| {
                crate::errors::StateError::new(
                    crate::errors::ErrorCode::ProfileSchemaInvalid,
                    msg,
                )
            })
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
    pub profile: Option<crate::subsystems::SubsystemPreset>,
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
    /// Optional architectural zones and lint rules (`architecture:`).
    #[serde(default)]
    pub architecture: ArchitectureConfig,
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
    /// Configured documentation root patterns/files (`docs:`).
    pub docs: Vec<String>,
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
    /// Configured architectural zones and static lint rules.
    pub architecture: ArchitectureConfig,
    /// Whether debug applies.
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
            profiles_dirs: default_profiles_dirs(),
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
        limits.allowed_binary_extensions = Some(vec![".PNG".to_owned(), "mo".to_owned(), "c++".to_owned()]);
        assert!(limits.validate().is_ok());

        limits.max_snapshot_file_bytes = Some(0);
        assert!(limits.validate().unwrap_err().to_string().contains("max_snapshot_file_bytes"));
        limits.max_snapshot_file_bytes = Some(1024);

        for bad_ext in ["", ".", "..png", "png jpg", "*.png", "a/png", "a\\png", "png?"] {
            limits.allowed_binary_extensions = Some(vec![bad_ext.to_owned()]);
            assert!(limits.validate().is_err(), "expected invalid extension '{bad_ext}' to fail");
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

        let missing_col = SpecCollectionConfig {
            path: Some("docs/nonexistent".to_owned()),
            paths: Vec::new(),
            profile: None,
            prefixes: Vec::new(),
            digits: 3,
        };
        assert!(missing_col.resolve_dirs(root).unwrap().is_empty());

        // Recursive `**` glob excludes `archive/` directories and `matched_concrete_root` resolves owning collection root.
        std::fs::create_dir_all(root.join("docs/milestones/archive")).unwrap();
        std::fs::create_dir_all(root.join("docs/milestones/team")).unwrap();
        let rec_col = SpecCollectionConfig {
            path: Some("docs/milestones/**".to_owned()),
            paths: Vec::new(),
            profile: None,
            prefixes: vec!["AC".to_owned()],
            digits: 3,
        };
        let rec_dirs = rec_col.resolve_dirs(root).unwrap();
        assert!(!rec_dirs.contains(&root.join("docs/milestones/archive")));
        assert!(rec_dirs.contains(&root.join("docs/milestones/team")));
        assert_eq!(
            ms.matched_concrete_root("docs/milestones/team/target.md").as_deref(),
            Some("docs/milestones")
        );
        assert_eq!(
            ms.matched_concrete_root("extensions/alpha/docs/milestones/team/target.md")
                .as_deref(),
            Some("extensions/alpha/docs/milestones")
        );
        assert_eq!(
            rec_col.matched_concrete_root("docs/milestones/team/target.md").as_deref(),
            Some("docs/milestones")
        );
        let deep_col = SpecCollectionConfig {
            path: Some("extensions/**/docs/milestones/**".to_owned()),
            paths: Vec::new(),
            profile: None,
            prefixes: vec!["AC".to_owned()],
            digits: 3,
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
            let res = crate::fs::write_new_confined(
                root,
                Path::new("wx-only/review-probe.md"),
                b"probe",
            );
            std::fs::set_permissions(&wx_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(res.is_err(), "0o300 dir must fail before publishing target");
            assert!(
                !wx_dir.join("review-probe.md").exists(),
                "failed write_new_confined must not leave stranded target"
            );
        }
    }
}
