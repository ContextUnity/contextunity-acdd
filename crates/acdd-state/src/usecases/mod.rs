//! Application use cases (`StateEngine`, `StateQueryEngine`, `StateCommand`, `StateQuery`, DTOs).

#[cfg(test)]
pub(crate) mod archive_notes;
#[cfg(not(test))]
mod archive_notes;
pub mod commands;
pub mod config;
pub mod deps;
pub mod dto;
pub mod lifecycle;
pub mod overlay;
pub mod queries;
pub mod roadmap;
pub mod run;
pub mod submit;
#[cfg(test)]
mod tests;
pub mod verify;

pub use config::StateEngineConfig;
pub use dto::*;

use self::commands::{handle_start_dyn, handle_sync_dyn};
use crate::store::sqlite::{SqliteStateBackend, SqliteStateReader};
use crate::store::traits::{StateBackend, StateReader};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
use std::sync::Arc;

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
    ) -> Result<Self, AcddError> {
        let (db_path, _) = config.resolved_state_db_path();
        Ok(Self {
            config,
            reader: Box::new(SqliteStateReader::new(db_path)),
            provider,
        })
    }

    /// Execute a read-only [`StateQuery`], enforcing `modules.state` fail-closed.
    pub fn query(&self, query: StateQuery) -> Result<StateQueryOutput, AcddError> {
        queries::execute_query(&self.config, self.reader.as_ref(), &self.provider, query)
    }

    /// Resolve a gate node's configured [`crate::workflow::ProofDef`] read-only (`SQLITE_OPEN_READ_ONLY`).
    pub fn resolve_gate_proof(
        &self,
        run: Option<&str>,
        node: &str,
    ) -> Result<crate::workflow::ProofDef, AcddError> {
        queries::resolve_gate_proof(&self.config, self.reader.as_ref(), run, node)
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
    ) -> Result<Self, AcddError> {
        if !config.modules.is_subsystem_enabled(SubsystemId::State) {
            return Err(AcddError::new(
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
        let backend =
            SqliteStateBackend::open_with_reconciler(config.sqlite_options(backup_dir), |runs| {
                lifecycle::reconcile_restored_runs_with_archives(
                    runs,
                    Some(&config.workspace_root),
                    &collection_paths,
                )
            })?;
        Ok(Self {
            config,
            backend: Arc::new(backend),
            provider,
        })
    }

    /// Execute a mutating or plugin [`StateCommand`].
    pub fn execute(&self, cmd: StateCommand) -> Result<StateCommandOutput, AcddError> {
        execute_on(&self.config, self.backend.as_ref(), &self.provider, cmd)
    }

    /// Use explicit checkout settings for start and sync.
    pub fn execute_with_source_config(
        &self,
        cmd: StateCommand,
        source_override: &Adapter,
    ) -> Result<StateCommandOutput, AcddError> {
        execute_on_dyn(
            &self.config,
            self.backend.as_ref(),
            &self.provider,
            cmd,
            Some(source_override),
        )
    }
}

/// Execute any [`StateCommand`] against an arbitrary [`StateBackend`] implementation.
pub(crate) fn execute_on<B: StateBackend + Send + Sync>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    cmd: StateCommand,
) -> Result<StateCommandOutput, AcddError> {
    execute_on_dyn(config, backend, provider, cmd, None)
}

/// Run `snapshot` without creating `.acdd/state.sqlite`.
pub fn execute_snapshot_readonly(
    config: &StateEngineConfig,
    req: SnapshotReq,
) -> Result<StateCommandOutput, AcddError> {
    lifecycle::ensure_state_enabled(config)?;
    acdd_core::fs::check_git_entry_no_symlinks(&config.workspace_root)
        .map_err(|e| AcddError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    let wt = req.worktree.as_deref().unwrap_or(&config.worktree);
    acdd_core::fs::check_git_entry_no_symlinks(wt)
        .map_err(|e| AcddError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    let reader = crate::store::sqlite::SqliteStateReader::new(config.resolved_state_db_path().0);
    commands::task_ops::handle_snapshot_dyn(config, &reader, req).map(StateCommandOutput::Snapshot)
}

fn execute_on_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    cmd: StateCommand,
    source_override: Option<&Adapter>,
) -> Result<StateCommandOutput, AcddError> {
    match &cmd {
        StateCommand::CallPluginTool(req) if req.run.is_none() && req.node.is_none() => {}
        _ => lifecycle::ensure_state_enabled(config)?,
    }
    match cmd {
        StateCommand::Start(req) => Ok(StateCommandOutput::Start(Box::new(handle_start_dyn(
            config,
            backend,
            provider,
            req,
            source_override,
        )?))),
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
            config,
            backend,
            provider,
            req,
            source_override,
        )?)),
        StateCommand::Subtask(req) => Ok(StateCommandOutput::Subtask(
            commands::handle_subtask_dyn(config, backend, req)?,
        )),
        StateCommand::BlackboardPost(req) => Ok(StateCommandOutput::BlackboardPost(
            commands::handle_blackboard_post_dyn(config, backend, req)?,
        )),
        StateCommand::Clear(req) => Ok(StateCommandOutput::Clear(Box::new(
            commands::handle_clear_dyn(config, backend, req)?,
        ))),
        StateCommand::Snapshot(req) => {
            crate::usecases::commands::task_ops::handle_snapshot_dyn(config, backend, req)
                .map(StateCommandOutput::Snapshot)
        }
        StateCommand::Run(req) => Ok(StateCommandOutput::Run(run::handle_run_dyn(
            config, backend, provider, req,
        )?)),
        StateCommand::CallPluginTool(req) => Ok(StateCommandOutput::CallPluginTool(
            commands::handle_call_plugin_tool_dyn(config, backend, provider, req)?,
        )),
    }
}
