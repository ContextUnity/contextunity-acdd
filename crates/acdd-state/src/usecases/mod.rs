//! Application use cases (`StateEngine`, `StateQueryEngine`, `StateCommand`, `StateQuery`, DTOs).

mod archive_notes;
pub mod commands;
pub mod deps;
pub mod dto;
pub mod lifecycle;
pub mod queries;
pub mod run;
pub mod submit;
#[cfg(test)]
mod tests;
pub mod verify;

pub use dto::*;

use self::commands::{handle_start_dyn, handle_sync_dyn};
use crate::store::sqlite::{SqliteBackendOptions, SqliteStateBackend, SqliteStateReader};
use crate::store::traits::{StateBackend, StateReader};
use acdd_core::config::{Adapter, SpecCollectionConfig};
use acdd_core::response::ResponsePolicy;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ports::CompositeDomainProvider;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Host configuration passed into [`StateEngine`] and [`StateQueryEngine`].
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
                (primary.join(".acdd/state.sqlite"), false)
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

/// Strictly read-only query engine operating directly over `dyn StateReader` (zero DB creation).
pub struct StateQueryEngine {
    config: StateEngineConfig,
    reader: Box<dyn StateReader>,
    provider: Arc<CompositeDomainProvider>,
}

impl StateQueryEngine {
    /// Open a zero-create read-only query engine over `state.sqlite`.
    pub fn open_read_only(
        config: StateEngineConfig,
        provider: Arc<CompositeDomainProvider>,
    ) -> Result<Self, StateError> {
        let (db_path, _) = config.resolved_state_db_path();
        Ok(Self {
            config,
            reader: Box::new(SqliteStateReader::new(db_path)),
            provider,
        })
    }

    /// Execute a read-only [`StateQuery`], enforcing `modules.state` fail-closed.
    pub fn query(&self, query: StateQuery) -> Result<StateQueryOutput, StateError> {
        queries::execute_query(&self.config, self.reader.as_ref(), &self.provider, query)
    }
}

/// Full read/write state engine coordinating `repo.lock`, `git_ops`, `StateBackend`, `runner`, and `CompositeDomainProvider`.
pub struct StateEngine {
    config: StateEngineConfig,
    backend: Arc<SqliteStateBackend>,
    provider: Arc<CompositeDomainProvider>,
}

impl StateEngine {
    /// Open (or initialize/restore) the state engine for `config`.
    pub fn open(
        config: StateEngineConfig,
        provider: Arc<CompositeDomainProvider>,
    ) -> Result<Self, StateError> {
        if !config.modules.is_enabled("state") {
            return Err(StateError::new(
                ErrorCode::FeatureDisabled,
                "state subsystem is disabled in modules:",
            ));
        }
        let (_, is_custom_db) = config.resolved_state_db_path();
        let backup_dir = if !is_custom_db {
            let primary = lifecycle::primary_workspace_root(&config);
            lifecycle::resolve_repo_layout_optional(&primary)?.map(|l| l.acdd_common_dir)
        } else {
            None
        };
        let collection_paths: Vec<String> = config
            .spec_collections
            .values()
            .flat_map(|c| c.patterns().into_iter().map(str::to_owned))
            .collect();
        let backend = SqliteStateBackend::open_with_reconciler(
            config.sqlite_options(backup_dir),
            |runs| {
                lifecycle::reconcile_restored_runs_with_archives(
                    runs,
                    Some(&config.workspace_root),
                    &collection_paths,
                )
            },
        )?;
        Ok(Self {
            config,
            backend: Arc::new(backend),
            provider,
        })
    }

    /// Access the engine configuration.
    pub fn config(&self) -> &StateEngineConfig {
        &self.config
    }

    /// Access the underlying [`SqliteStateBackend`].
    pub fn backend(&self) -> &SqliteStateBackend {
        self.backend.as_ref()
    }

    /// Execute a mutating or plugin [`StateCommand`].
    pub fn execute(&self, cmd: StateCommand) -> Result<StateCommandOutput, StateError> {
        execute_on(&self.config, self.backend.as_ref(), &self.provider, cmd)
    }

    /// Use explicit checkout settings for start and sync.
    pub fn execute_with_source_config(
        &self,
        cmd: StateCommand,
        source_override: &Adapter,
    ) -> Result<StateCommandOutput, StateError> {
        execute_on_dyn(
            &self.config,
            self.backend.as_ref(),
            &self.provider,
            cmd,
            Some(source_override),
        )
    }

    /// Execute a read-only [`StateQuery`] via the underlying `StateReader`.
    pub fn query(&self, query: StateQuery) -> Result<StateQueryOutput, StateError> {
        queries::execute_query(&self.config, self.backend.as_ref(), &self.provider, query)
    }
}

/// Execute any [`StateCommand`] against an arbitrary [`StateBackend`] implementation.
pub fn execute_on<B: StateBackend + Send + Sync>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    cmd: StateCommand,
) -> Result<StateCommandOutput, StateError> {
    execute_on_dyn(config, backend, provider, cmd, None)
}

fn execute_on_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    cmd: StateCommand,
    source_override: Option<&Adapter>,
) -> Result<StateCommandOutput, StateError> {
    match &cmd {
        StateCommand::CallPluginTool(req) if req.run.is_none() && req.node.is_none() => {}
        _ => lifecycle::ensure_state_enabled(config)?,
    }
    match cmd {
        StateCommand::Start(req) => Ok(StateCommandOutput::Start(Box::new(
            handle_start_dyn(config, backend, provider, req, source_override)?,
        ))),
        StateCommand::Draft(req) => Ok(StateCommandOutput::Draft(commands::handle_draft_dyn(
            config, backend, req,
        )?)),
        StateCommand::Claim(req) => Ok(StateCommandOutput::Claim(Box::new(
            commands::handle_claim_dyn(config, backend, provider, req)?,
        ))),
        StateCommand::Verify(req) => Ok(StateCommandOutput::Verify(verify::handle_verify_dyn(
            config, backend, provider, req,
        )?)),
        StateCommand::Submit(req) => Ok(StateCommandOutput::Submit(Box::new(
            submit::handle_submit_dyn(config, backend, provider, *req)?,
        ))),
        StateCommand::ExtendScope(req) => Ok(StateCommandOutput::ExtendScope(
            commands::handle_extend_scope_dyn(config, backend, req)?,
        )),
        StateCommand::Reopen(req) => Ok(StateCommandOutput::Reopen(commands::handle_reopen_dyn(
            config, backend, req,
        )?)),
        StateCommand::Sync(req) => Ok(StateCommandOutput::Sync(handle_sync_dyn(
            config, backend, provider, req, source_override,
        )?)),
        StateCommand::Subtask(req) => Ok(StateCommandOutput::Subtask(commands::handle_subtask_dyn(
            config, backend, req,
        )?)),
        StateCommand::BlackboardPost(req) => Ok(StateCommandOutput::BlackboardPost(
            commands::handle_blackboard_post_dyn(config, backend, req)?,
        )),
        StateCommand::Clear(req) => Ok(StateCommandOutput::Clear(Box::new(
            commands::handle_clear_dyn(config, backend, req)?,
        ))),
        StateCommand::Run(req) => Ok(StateCommandOutput::Run(run::handle_run_dyn(
            config, backend, provider, req,
        )?)),
        StateCommand::CallPluginTool(req) => Ok(StateCommandOutput::CallPluginTool(
            commands::handle_call_plugin_tool_dyn(config, backend, provider, req)?,
        )),
    }
}
