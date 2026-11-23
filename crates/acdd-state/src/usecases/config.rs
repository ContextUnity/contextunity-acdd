//! Host configuration for [`super::StateEngine`] and [`super::StateQueryEngine`].

use crate::store::sqlite::SqliteBackendOptions;
use acdd_core::config::{SpecCollectionConfig, DEFAULT_STATE_DB};
use acdd_core::response::ResponsePolicy;
use acdd_core::subsystems::SubsystemRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Host configuration passed into [`super::StateEngine`] and [`super::StateQueryEngine`].
#[derive(Debug, Clone)]
pub struct StateEngineConfig {
    /// Primary workspace root.
    pub workspace_root: PathBuf,
    /// Current caller worktree directory.
    pub worktree: PathBuf,
    /// Optional explicit `--state-db` / `acdd.yaml` `state_db` path override.
    pub state_db_override: Option<PathBuf>,
    /// Optional `code-map.sqlite` path propagated to role runners (`ACDD_CODE_DB`).
    pub code_db_path: Option<PathBuf>,
    /// Configured spec collections from `acdd.yaml` (`specs:`).
    pub spec_collections: BTreeMap<String, SpecCollectionConfig>,
    /// Configured profile search directories (`profiles_dirs`).
    pub profiles_dirs: Vec<String>,
    /// Configured linked workspace aliases (`linked_workspaces`).
    pub linked_workspaces: BTreeMap<String, PathBuf>,
    /// Active subsystem/domain enablement registry (`modules:`).
    pub modules: SubsystemRegistry,
    /// Response byte-budget policy (`response:`).
    pub response_policy: ResponsePolicy,
    /// Number of retained run trace directories under `.acdd/run/`.
    pub retain_traces: usize,
    /// Optional snapshot/commit blob size limit in bytes (`None` disables ceiling).
    pub max_snapshot_file_bytes: Option<u64>,
    /// Optional normalized lowercase binary extensions exempt from NUL-byte rejection (`None` disables NUL check).
    pub allowed_binary_extensions: Option<BTreeSet<String>>,
}

impl StateEngineConfig {
    /// Resolve the effective `state.sqlite` path and whether it is a custom override.
    pub fn resolved_state_db_path(&self) -> (PathBuf, bool) {
        match &self.state_db_override {
            Some(path) if path.is_absolute() => (path.clone(), true),
            Some(rel) => (self.workspace_root.join(rel), true),
            None => {
                let primary = acdd_core::git::primary_worktree_root(&self.workspace_root)
                    .unwrap_or_else(|| self.workspace_root.clone());
                (primary.join(DEFAULT_STATE_DB), false)
            }
        }
    }

    /// Build [`SqliteBackendOptions`] for opening the state database.
    pub fn sqlite_options(&self, backup_dir: Option<PathBuf>) -> SqliteBackendOptions {
        let (db_path, is_custom_db) = self.resolved_state_db_path();
        let spec_collection_paths = self
            .spec_collections
            .values()
            .flat_map(|c| c.patterns().into_iter().map(str::to_owned))
            .collect();
        SqliteBackendOptions {
            db_path,
            backup_dir,
            workspace_root: Some(self.workspace_root.clone()),
            spec_collection_paths,
            is_custom_db,
        }
    }
}
