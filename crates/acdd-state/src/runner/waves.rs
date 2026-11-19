//! Automated `acdd run` wave execution report.

use acdd_core::ids::TaskRef;
use serde::{Deserialize, Serialize};

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
