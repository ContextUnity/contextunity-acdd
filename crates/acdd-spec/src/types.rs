//! Pure specification value types and stateless request/response DTOs.

use acdd_core::errors::{ErrorCode, AcddError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Completion status of a task subtask.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum SubtaskStatus {
    /// Not started yet.
    #[serde(rename = "pending")]
    Pending,
    /// Currently being worked on.
    #[serde(rename = "in_progress")]
    InProgress,
    /// Finished with mandatory non-empty `evidence`.
    #[serde(rename = "completed")]
    Completed,
    /// Cancelled with mandatory non-empty `evidence`.
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl SubtaskStatus {
    /// Return the canonical lowercase wire/SQL string representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for SubtaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SubtaskStatus {
    type Err = AcddError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("invalid SubtaskStatus value '{other}'"),
            )),
        }
    }
}

/// Invariants applicability and verification state stored in `gate_nodes.invariants_json` and receipts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvariantsState {
    /// Invariant identifiers applicable to this task or node.
    #[serde(default)]
    pub applicable: Vec<String>,
    /// Invariant identifiers proven/verified during gate execution.
    #[serde(default)]
    pub verified: Vec<String>,
}

/// Recorded scope extension entry stored in `gate_nodes.extended_scope_json` and archive receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtendedScopeEntry {
    /// Admitted path or glob pattern added via `extend_scope`.
    pub path: String,
    /// Non-empty justification recorded with the extension.
    pub reason: String,
}

/// Request payload for drafting the next numbered spec document in a configured spec collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftReq {
    /// Target spec key from `acdd.yaml` `specs:` (e.g. `"milestones"`, `"plans"`).
    pub spec: String,
    /// URL/file slug for the drafted spec document.
    pub slug: String,
    /// Optional prefix override (must belong to `spec.prefixes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Optional human-readable title (defaults to formatted `slug`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional workflow override written into frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

/// Response payload returned by spec drafting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftResp {
    /// Generated sequential spec ID (e.g. `"AC-008"`).
    pub spec_id: String,
    /// Relative filesystem path of the created spec Markdown file.
    pub spec_path: String,
    /// Spec key that owns the file.
    pub spec: String,
}

/// Read-only request payload for workspace/document validation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateReq {
    /// Optional workspace-relative document path; `None` validates the whole workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional explicit config path override (`--config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_config: Option<PathBuf>,
    /// Optional explicit workflow override (`--workflow`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_workflow: Option<String>,
}

/// Single diagnostic entry emitted by workspace/document validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateDiagnostic {
    /// Outcome status (`"pass"` or `"fail"`).
    pub status: String,
    /// Stable diagnostic code string.
    pub code: String,
    /// Workspace-relative file path.
    pub path: String,
    /// One-based line number (0 when unknown).
    pub line: usize,
    /// One-based column number (0 when unknown).
    pub column: usize,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Summary counters returned by workspace/document validation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateSummary {
    /// Passed diagnostic checks count.
    pub passed: usize,
    /// Failed diagnostic checks count.
    pub failed: usize,
    /// Warnings count.
    pub warnings: usize,
    /// Validated documents count.
    pub documents_validated: usize,
}

/// Typed response payload returned by workspace/document validation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateResp {
    /// Overall validation pass (`failed == 0`).
    pub valid: bool,
    /// Validation scope description (`"workspace"` or `"document:<path>"`).
    pub scope: String,
    /// Aggregate summary counts.
    pub summary: ValidateSummary,
    /// Diagnostic list ordered deterministically.
    #[serde(default)]
    pub diagnostics: Vec<ValidateDiagnostic>,
    /// Structured advisory warnings (JSON objects).
    #[serde(default)]
    pub warnings: Vec<Value>,
}

/// Read-only request payload for roadmap DAG synthesis.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoadmapReq {
    /// Optional spec ID filter / drill-down selector (including archived specs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// Optional spec key display filter applied after building the full cross-spec DAG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Show only Wave 0 ready specs.
    #[serde(default)]
    pub ready_only: bool,
    /// Show only in-progress specs (active runs).
    #[serde(default)]
    pub active_only: bool,
    /// Include intra-spec task drill-down for every returned spec.
    #[serde(default)]
    pub include_tasks: bool,
    /// Include archived specs in `archived` output (archived specs always count in `archived_count` and dependency resolution).
    #[serde(default)]
    pub archived: bool,
    /// Restrict spec discovery strictly to the current workspace/worktree (`--current`).
    #[serde(default)]
    pub current_only: bool,
}

/// Worktree copy summary when a spec diverges across Git worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapWorktreeCopy {
    /// Worktree label/path (e.g. `"main"` or `".worktrees/feat-x"`).
    pub worktree: String,
    /// Lifecycle status in that worktree (`"archived"`, `"active"`, `"planned"`, `"cancelled"`).
    pub status: String,
    /// Whether this copy was selected as canonical.
    pub canonical: bool,
}

/// Resolved dependency status entry in [`RoadmapSpecEntry`] or [`RoadmapTaskEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapDepEntry {
    /// Raw `depends_on` string as written in the spec.
    pub raw: String,
    /// Resolved status (`satisfied`, `archived`, `active`, `planned`, `cancelled`, `cleared`, `missing`).
    pub status: String,
    /// Whether the dependency is currently satisfied.
    pub satisfied: bool,
}

/// Active gate cursor and worker entry on an in-progress run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapActiveGateEntry {
    /// Node cursor string (e.g. `T-02.build`).
    pub cursor: String,
    /// Gate node status (e.g. `active`).
    pub status: String,
    /// Claiming worker ID if claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// Task-level drill-down entry inside [`RoadmapSpecEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapTaskEntry {
    /// Task reference (`T-01`, etc.).
    pub task_ref: String,
    /// Task target summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Live or planned task status (`planned`, `ready`, `in_progress`, `completed`, `cancelled`).
    pub status: String,
    /// Intra-spec topological wave (`0..M`).
    pub wave: usize,
    /// Task dependencies with resolved satisfaction status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RoadmapDepEntry>,
    /// Reverse dependencies (`<task_ref>` or `<spec_id>:<task_ref>`) blocked by this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    /// Effective write scope globs for this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Other tasks in the same intra-spec wave whose scopes overlap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_overlaps: Vec<String>,
    /// Active gate cursors on this task (when part of an active run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_gates: Vec<RoadmapActiveGateEntry>,
}

/// Spec entry in `RoadmapResp` (`in_progress`, `waves`, or `archived`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapSpecEntry {
    /// Specification document ID.
    pub spec_id: String,
    /// Specification title.
    pub title: String,
    /// Spec key (`plans`, `milestones`, `specs`, etc.).
    pub spec: String,
    /// Workspace-relative spec file path.
    pub path: String,
    /// Document type (`contract`, `plan`, etc.).
    pub doc_type: String,
    /// Effective spec status (`planned`, `ready`, `in_progress`, `completed`, `cancelled`, `archived`).
    pub status: String,
    /// Active run ID if this spec has an active run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Active run worktree path if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Source worktree label when discovered only in a secondary worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_worktree: Option<String>,
    /// Divergent worktree copies when the same `spec_id` differs across Git worktrees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub divergence: Vec<RoadmapWorktreeCopy>,
    /// Number of completed tasks (from active run or spec document).
    pub completed_tasks: usize,
    /// Total number of tasks in this spec.
    pub total_tasks: usize,
    /// Active gate cursors on the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_gates: Vec<RoadmapActiveGateEntry>,
    /// Transitive count of downstream specs unblocked by this spec.
    pub unblocks_specs: usize,
    /// Transitive count of downstream tasks unblocked by this spec.
    pub unblocks_tasks: usize,
    /// True when no other spec in the same wave/active tier overlaps this spec's write scope.
    pub parallel_safe: bool,
    /// Spec IDs in the same wave/active tier whose write scopes overlap this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_overlaps: Vec<String>,
    /// Spec-level dependencies with resolved satisfaction status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RoadmapDepEntry>,
    /// Reverse dependencies (`<spec_id>` or `<spec_id>:<task_ref>`) blocked by this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    /// Task drill-down entries (populated when `spec_id` is requested or `include_tasks` is true).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<RoadmapTaskEntry>,
}

/// Topological wave of planned specs ordered by critical-path priority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapWave {
    /// Zero-based wave index (`0` = ready now).
    pub wave: usize,
    /// Specs in this wave sorted descending by `(unblocks_specs, unblocks_tasks)` then `spec_id`.
    pub specs: Vec<RoadmapSpecEntry>,
}

/// Read-only response returned by roadmap synthesis.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapResp {
    /// Specs with an active `Run`.
    pub in_progress: Vec<RoadmapSpecEntry>,
    /// Planned specs grouped into topological waves (`Wave 0` = ready).
    pub waves: Vec<RoadmapWave>,
    /// Archived specs (populated when `archived == true` or `spec_id` targets an archived spec).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived: Vec<RoadmapSpecEntry>,
    /// Total archived specs discovered across configured collections.
    pub archived_count: usize,
    /// Dependency cycles detected across planned specs (each inner `Vec` is an SCC).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycles: Vec<Vec<String>>,
    /// Non-fatal discovery or dependency parsing warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
