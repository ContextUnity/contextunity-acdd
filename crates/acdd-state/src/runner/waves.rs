//! Topological DAG wave computation and automated `acdd run` wave execution loop.

use crate::machine::{compute_schedule, MachineProfileInput};
use crate::types::{IsolationMode, RunGraphSnapshot, ValidatedObservations};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{RunId, TaskRef};
use serde::{Deserialize, Serialize};

/// Configuration for `run_wave_scheduler`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveRunConfig {
    /// Target active run identifier.
    pub run_id: RunId,
    /// Requested parallelism (`--jobs N`; forced to `1` under `IsolationMode::InPlace`).
    pub jobs: usize,
    /// Whether committing/integrating onto `main` / `master` is permitted.
    pub allow_main: bool,
    /// Dry-run flag (compute waves without executing mutating steps).
    pub dry_run: bool,
}

/// Summary of an automated `acdd run` wave execution pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveExecutionReport {
    /// Number of waves dispatched.
    pub waves_executed: u32,
    /// Total gates completed during the run.
    pub gates_completed: u32,
    /// Tasks integrated at wave boundaries.
    pub tasks_integrated: Vec<TaskRef>,
    /// Reason execution halted early (if any).
    pub halted_reason: Option<String>,
}

/// Resolve the effective wave concurrency (`1` when `IsolationMode::InPlace`, else `requested_jobs.max(1)`).
pub fn effective_wave_jobs(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    requested_jobs: usize,
) -> usize {
    if snapshot.run.isolation == IsolationMode::InPlace
        || profile.git.isolation == IsolationMode::InPlace
    {
        1
    } else {
        requested_jobs.max(1)
    }
}

/// Compute ordered non-overlapping ready waves from `snapshot` using `machine::schedule`.
pub fn compute_ready_waves(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    max_jobs: usize,
) -> Vec<Vec<String>> {
    compute_ready_waves_checked(snapshot, profile, max_jobs).unwrap_or_default()
}

fn compute_ready_waves_checked(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    max_jobs: usize,
) -> Result<Vec<Vec<String>>, StateError> {
    let report = compute_schedule(snapshot, &ValidatedObservations::default())?;
    let effective_jobs = effective_wave_jobs(snapshot, profile, max_jobs);
    let mut waves = Vec::new();

    if !report.pre_task_satisfied && !report.ready_root_nodes.is_empty() {
        for chunk in report.ready_root_nodes.chunks(effective_jobs) {
            waves.push(chunk.to_vec());
        }
    }

    for task_wave in report.waves {
        let node_ids: Vec<String> = task_wave
            .into_iter()
            .map(|tref| format!("task:{tref}"))
            .collect();
        for chunk in node_ids.chunks(effective_jobs) {
            waves.push(chunk.to_vec());
        }
    }

    if report.all_tasks_completed && !report.ready_root_nodes.is_empty() {
        for chunk in report.ready_root_nodes.chunks(effective_jobs) {
            waves.push(chunk.to_vec());
        }
    }

    Ok(waves)
}

/// Execute ready waves up to completion or halt using caller-provided step execution callback.
pub fn run_wave_scheduler(
    config: &WaveRunConfig,
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    step_fn: &mut dyn FnMut(&str) -> Result<bool, StateError>,
) -> Result<WaveExecutionReport, StateError> {
    if snapshot.run.run_id != config.run_id {
        return Err(StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!(
                "wave config run_id '{}' does not match snapshot run_id '{}'",
                config.run_id, snapshot.run.run_id
            ),
        ));
    }

    let waves = compute_ready_waves_checked(snapshot, profile, config.jobs)?;
    if config.dry_run {
        return Ok(WaveExecutionReport {
            waves_executed: 0,
            gates_completed: 0,
            tasks_integrated: Vec::new(),
            halted_reason: Some("dry_run".to_owned()),
        });
    }

    let mut report = WaveExecutionReport::default();
    for wave in waves {
        if wave.is_empty() {
            continue;
        }
        report.waves_executed = report.waves_executed.saturating_add(1);
        for node_id in &wave {
            let progressed = step_fn(node_id)?;
            if !progressed {
                report.halted_reason = Some(format!("step '{node_id}' halted without progress"));
                return Ok(report);
            }
            report.gates_completed = report.gates_completed.saturating_add(1);
            if let Some(tref_raw) = node_id.strip_prefix("task:") {
                if let Ok(tref) = TaskRef::new(tref_raw) {
                    report.tasks_integrated.push(tref);
                }
            }
        }
    }

    Ok(report)
}
