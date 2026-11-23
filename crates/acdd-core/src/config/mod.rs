//! Workspace adapter configuration, scanner limits, and path resolution.

pub mod constants;
pub mod linked;
pub mod loader;
pub mod paths;
pub mod schema;

pub use crate::fs::{checked_child, safe_relative};

pub use constants::{
    ACDD_CONFIG_FILE, ACDD_DIR, CLAIM_TRAILER_KEY, DEFAULT_CODE_DB, DEFAULT_RETAIN_TRACES,
    DEFAULT_STATE_DB, ENGINE_SCHEMA_VERSION, MAX_FILE_BYTES, SNAPSHOT_REF_PREFIX, WORKTREES_DIR,
};
pub use loader::{
    load_adapter, load_adapter_report, persisted_adapter_path, policy, read_policy_from_value,
};
pub use paths::{
    check_root_scope, resolve_code_db_path, resolve_custom_db_path,
    resolve_effective_state_db_path, resolve_file_path, resolve_memory_budget_bytes,
    resolve_state_db_path, workspace_path,
};
pub use schema::{
    default_specs, Adapter, LinkedWorkspace, ScannerLimits, SpecCollectionConfig,
};
