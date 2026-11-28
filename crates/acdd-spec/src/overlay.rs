//! Read-only runtime overlay boundary (`SpecRuntimeOverlay`) decoupling static spec operations from SQLite state.

use crate::types::{RoadmapActiveGateEntry, RoadmapDepEntry};
use acdd_core::config::SpecConfig;
use acdd_core::errors::AcddError;
use acdd_core::ids::{RunId, TaskRef};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Task execution status observed from an active `Run` for planned-spec reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTaskStatus {
    /// Ready to be claimed or started.
    Ready,
    /// Actively executing.
    InProgress,
    /// Completed and accepted.
    Completed,
    /// Any other non-active/non-completed state.
    Other,
}

/// Live run or tombstone match resolved for an external `depends_on` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDepResolution {
    /// Target matched an active `Run` in `state.sqlite`.
    ActiveRun {
        /// Matched active run ID.
        run_id: RunId,
        /// Optional specification ID bound to the active run.
        spec_id: Option<String>,
        /// Whether the requested `task_ref` exists on the run graph.
        task_exists: bool,
        /// Whether the requested `task_ref` is completed (`false` for run-level deps).
        task_completed: bool,
    },
    /// Target matched a terminal `RunTombstoneRecord` in `state.sqlite`.
    Tombstone {
        /// Whether the run was cleared or superseded by a cleared tombstone.
        cleared: bool,
        /// Task verification outcome when `cleared == false`.
        task_outcome: Result<(), AcddError>,
    },
}

/// Projected task entry inside an active `Run` for roadmap rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActiveTask {
    /// Task reference (`T-01`, etc.).
    pub task_ref: String,
    /// Effective display status (`completed`, `cancelled`, `in_progress`, `ready`, `planned`).
    pub status: String,
    /// Active gate cursors currently `in_progress` on this task.
    pub active_gates: Vec<RoadmapActiveGateEntry>,
    /// Effective task write scope globs.
    pub scope: Vec<String>,
    /// Resolved dependency entries for this task.
    pub depends_on: Vec<RoadmapDepEntry>,
    /// Local task blockers that are not yet satisfied.
    pub unsatisfied_local_blockers: BTreeSet<String>,
    /// Whether this task has any unsatisfied external dependency.
    pub has_unsatisfied_external_dep: bool,
}

/// Projected cross-run dependency edge from an active `Run` graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActiveExternalDep {
    /// Source task reference within the active run.
    pub source_task_ref: TaskRef,
    /// Target workspace path (`"."` or canonical checkout path).
    pub target_workspace: String,
    /// Target run ID recorded on the dependency row.
    pub target_run_id: RunId,
    /// Target task reference (`__spec__` for whole-spec dependencies).
    pub target_task_ref: TaskRef,
    /// Effective lookup selector for resolving the target run or planned spec.
    pub target_selector: String,
    /// Whether this dependency edge is currently satisfied.
    pub satisfied: bool,
}

/// Projected active `Run` summary consumed by `acdd-spec` roadmap discovery, graph building, and rendering.
///
/// External dependency evaluation and per-task drill-down rows are resolved lazily via
/// [`SpecRuntimeOverlay::active_run_external_deps`] and [`SpecRuntimeOverlay::project_active_run_tasks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActiveRun {
    /// Active run ID.
    pub run_id: RunId,
    /// Bound specification document ID, if any.
    pub spec_id: Option<String>,
    /// Bound specification file relative path, if any.
    pub spec_path: Option<String>,
    /// Canonical or raw worktree path recorded on the run, if any.
    pub worktree: Option<String>,
    /// Local Git branch recorded on the run, if any.
    pub branch: Option<String>,
    /// Total task count on the run graph (`0` when no graph row exists).
    pub total_tasks: usize,
    /// Completed task count on the run graph.
    pub completed_tasks: usize,
    /// Active `in_progress` gate cursors on the run graph.
    pub active_gates: Vec<RoadmapActiveGateEntry>,
    /// Completed task references within this active run.
    pub completed_task_refs: BTreeSet<TaskRef>,
    /// Intra-run `(target_task_ref, source_task_ref)` task blocker pairs from graph dependencies.
    pub local_task_blocks: Vec<(TaskRef, TaskRef)>,
    /// Whether the run graph records any cross-run dependency edges.
    pub has_external_deps: bool,
}

/// Read-only runtime state overlay supplied by `acdd-state` (or [`NoopRuntimeOverlay`] for stateless spec mode).
pub trait SpecRuntimeOverlay {
    /// Highest sequential spec number recorded in runtime state for `(collection, prefix)`.
    fn max_spec_number(&self, _collection: &str, _prefix: &str) -> Result<Option<u32>, AcddError> {
        Ok(None)
    }

    /// Return task execution statuses for an active `Run` matching `(rel_path, spec_id)` at `root`.
    fn active_task_statuses_for_spec(
        &self,
        _root: &Path,
        _rel_path: &str,
        _spec_id: &str,
    ) -> BTreeMap<String, RuntimeTaskStatus> {
        BTreeMap::new()
    }

    /// Resolve an active `Run` matching `run_sel` (optionally scoped to `run_checkout`) in `ws_path`.
    fn resolve_active_run_dep(
        &self,
        _ws_path: &Path,
        _collections: &BTreeMap<String, SpecConfig>,
        _run_sel: &str,
        _task_sel: Option<&str>,
        _run_checkout: Option<&Path>,
    ) -> Result<Option<RuntimeDepResolution>, AcddError> {
        Ok(None)
    }

    /// Resolve a closed/cleared `RunTombstoneRecord` matching `run_sel` in `ws_path`.
    fn resolve_tombstone_dep(
        &self,
        _ws_path: &Path,
        _collections: &BTreeMap<String, SpecConfig>,
        _run_sel: &str,
        _task_sel: Option<&str>,
        _dep_label: &str,
    ) -> Result<Option<RuntimeDepResolution>, AcddError> {
        Ok(None)
    }

    /// Load lightweight active-run summaries keyed by `spec_id` strictly from runtime state without touching worktree configs or spec files.
    fn load_roadmap_active_runs(&self) -> Result<BTreeMap<String, Vec<RuntimeActiveRun>>, AcddError> {
        Ok(BTreeMap::new())
    }

    /// Lazily evaluate cross-run external dependencies for an included active `Run`.
    fn active_run_external_deps(
        &self,
        _run_id: &RunId,
        _owned_worktrees: &[PathBuf],
    ) -> Result<Vec<RuntimeActiveExternalDep>, AcddError> {
        Ok(Vec::new())
    }

    /// Lazily project per-task drill-down rows for an included active `Run` when requested.
    fn project_active_run_tasks(
        &self,
        _run_id: &RunId,
        _owned_worktrees: &[PathBuf],
    ) -> Result<Vec<RuntimeActiveTask>, AcddError> {
        Ok(Vec::new())
    }
}

/// Stateless no-op [`SpecRuntimeOverlay`] returning empty active-run and tombstone state.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRuntimeOverlay;

impl SpecRuntimeOverlay for NoopRuntimeOverlay {}
