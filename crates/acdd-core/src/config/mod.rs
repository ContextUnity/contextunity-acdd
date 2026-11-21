//! Workspace adapter configuration, scanner limits, and path resolution.

pub mod linked;
pub mod loader;
pub mod paths;
pub mod schema;

use crate::errors::{ErrorCode, StateError};

pub use crate::fs::{checked_child, safe_relative};
pub use crate::response::{Detail, ResponsePolicy, SourceContextPolicy};
pub use crate::subsystems::{SubsystemPreset, SubsystemRegistry};
pub use crate::sys::memory_budget_bytes;

pub use loader::{
    load_adapter, load_adapter_report, load_adapter_strict, persisted_adapter_path, policy,
    read_policy_from_value,
};
pub use paths::{
    bytes_from_mebibytes, check_root_scope, memory_budget_mb_from_sources, resolve_code_db_path,
    resolve_custom_db_path, resolve_effective_state_db_path, resolve_file_path,
    resolve_memory_budget_bytes, resolve_state_db_path, workspace_path,
};
pub use schema::{
    default_specs, Adapter, AdapterFile, LinkedWorkspace, LinkedWorkspaceConfig, ScannerLimits,
    SpecCollectionConfig, StrictAdapterReport,
};

/// Canonical workspace adapter configuration filename.
pub const ACDD_CONFIG_FILE: &str = "acdd.yaml";
/// Canonical workspace state/metadata directory relative to the workspace root.
pub const ACDD_DIR: &str = ".acdd";
/// Canonical directory holding ACDD-managed Git worktrees relative to the primary root.
pub const WORKTREES_DIR: &str = ".worktrees";
/// Default state database path relative to primary worktree root.
pub const DEFAULT_STATE_DB: &str = ".acdd/state.sqlite";
/// Explicit relative alias for [`DEFAULT_STATE_DB`].
pub const DEFAULT_STATE_DB_REL: &str = DEFAULT_STATE_DB;
/// Default code-map database path relative to workspace root.
pub const DEFAULT_CODE_DB: &str = ".acdd/code-map.sqlite";
/// Explicit relative alias for [`DEFAULT_CODE_DB`].
pub const DEFAULT_CODE_DB_REL: &str = DEFAULT_CODE_DB;
/// Canonical Git ref prefix for ACDD workspace snapshots.
pub const SNAPSHOT_REF_PREFIX: &str = "refs/acdd/snapshots/";
/// Canonical Git commit trailer key recording ACDD claim attribution.
pub const CLAIM_TRAILER_KEY: &str = "Acdd-Claim";

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
/// Default number of retained run trace logs.
pub const DEFAULT_RETAIN_TRACES: usize = 20;
/// Default specification ID numeric width.
pub const DEFAULT_SPEC_DIGITS: usize = 3;

/// Current engine schema version stored in database metadata.
pub const ENGINE_SCHEMA_VERSION: &str = "15";

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

pub(super) fn error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

pub(super) fn schema_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        StateError::new(ErrorCode::ProfileSchemaInvalid, message.into()),
    )
}
