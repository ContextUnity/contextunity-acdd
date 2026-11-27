use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Canonical machine-readable diagnostic codes serialized as `SCREAMING_SNAKE_CASE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // --- Exit 2: Usage / Config / Schema ---
    /// State database schema is uninitialized or incompatible and requires migration or recreation.
    ConfigMigrationRequired,
    /// Workflow YAML, schema definition, or typed payload failed validation.
    #[serde(rename = "ACDD_WORKFLOW_SCHEMA_INVALID")]
    WorkflowSchemaInvalid,
    /// Specification Markdown/YAML frontmatter, collection, or dependency graph failed validation.
    #[serde(rename = "ACDD_SPEC_SCHEMA_INVALID")]
    SpecSchemaInvalid,
    /// A gate references a command slot marked `unbound: true`.
    CommandUnbound,
    /// A command rejects `--test` filters or has malformed filter configuration.
    CommandTestFilterUnsupported,
    /// A chained shell command requires an explicit `{test_filter}` placeholder when `--test` is used.
    CommandTestFilterPlaceholderRequired,
    /// A gate references a guidance slot missing from `workflow.guidance`.
    UnknownGuidanceSlot,
    /// Mandatory `contract + guidance` content exceeds the reserved response byte budget.
    GuidanceBudgetExceeded,
    /// Multiple active runs match or no worktree/run selector disambiguates the target run.
    RunSelectorAmbiguous,
    /// The requested subsystem or tool is disabled in `modules:`.
    FeatureDisabled,
    /// A role-assigned gate requires a configured runner command during automated `acdd run`.
    WorkflowRunnerRequired,

    // --- Exit 3: State / Gate / Scope Violation ---
    /// The node already holds an active claim owned by another worker/session.
    TaskAlreadyClaimed,
    /// The supplied `claim_revision` does not match the node's current active claim revision.
    StaleClaimRevision,
    /// An independent gate rejected the same `worker_id` that authored a predecessor gate.
    TaskReviewNotIndependent,
    /// Working-tree modifications fall outside `is_agent_writable` for the admitted scope.
    OutOfScopeChanges,
    /// Requested scope extension collides with another already-claimed incomplete task.
    TaskScopeConflict,
    /// Required verification trace is missing or stale relative to the current tree/revision.
    TaskProofUnobserved,
    /// Working tree drifted from an immutable claim tree or required predecessor snapshot.
    TaskCandidateMismatch,
    /// Completed task deltas are missing from the target integration worktree.
    TaskNotIntegrated,
    /// A new task was synced after pre-task artifacts were already accepted.
    PreTaskArtifactStale,
    /// Reopening a completed task requires `--cascade` because downstream tasks depend on it.
    DependentTaskCompleted,
    /// Required domain provider capability (`call`, `checks`, `context`, or plugin tool) is unavailable.
    DomainCapabilityUnavailable,
    /// Operation attempted to commit or integrate onto a protected main/master branch without `--allow-main`.
    MainBranchProtected,

    // --- Exit 4: Execution / Git / Backend / Uncertainty ---
    /// Gate command, check, or proof contract verification failed.
    GateVerificationFailed,
    /// Role runner exited with status 0 without submitting a `pass` or `reject` decision.
    RunnerNoSubmission,
    /// Branch integration failed due to merge or rebase conflicts (cleanly aborted).
    GitIntegrationConflict,
    /// Unexpected external Git HEAD movement prevented idempotent commit recovery.
    ExternalGitDrift,
    /// A mutating `DomainProvider` action crashed mid-execution and cannot confirm its outcome.
    MutatingActionUncertainOutcome,
    /// Optimistic concurrency precondition (`node_version` or `runs.generation`) conflicted in storage.
    BackendConflict,
}

impl ErrorCode {
    /// Complete ordered list of all canonical error codes.
    pub const ALL: [ErrorCode; 29] = [
        Self::ConfigMigrationRequired,
        Self::WorkflowSchemaInvalid,
        Self::SpecSchemaInvalid,
        Self::CommandUnbound,
        Self::CommandTestFilterUnsupported,
        Self::CommandTestFilterPlaceholderRequired,
        Self::UnknownGuidanceSlot,
        Self::GuidanceBudgetExceeded,
        Self::RunSelectorAmbiguous,
        Self::FeatureDisabled,
        Self::WorkflowRunnerRequired,
        Self::TaskAlreadyClaimed,
        Self::StaleClaimRevision,
        Self::TaskReviewNotIndependent,
        Self::OutOfScopeChanges,
        Self::TaskScopeConflict,
        Self::TaskProofUnobserved,
        Self::TaskCandidateMismatch,
        Self::TaskNotIntegrated,
        Self::PreTaskArtifactStale,
        Self::DependentTaskCompleted,
        Self::DomainCapabilityUnavailable,
        Self::MainBranchProtected,
        Self::GateVerificationFailed,
        Self::RunnerNoSubmission,
        Self::GitIntegrationConflict,
        Self::ExternalGitDrift,
        Self::MutatingActionUncertainOutcome,
        Self::BackendConflict,
    ];

    /// Return the stable `SCREAMING_SNAKE_CASE` wire code.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ConfigMigrationRequired => "CONFIG_MIGRATION_REQUIRED",
            Self::WorkflowSchemaInvalid => "ACDD_WORKFLOW_SCHEMA_INVALID",
            Self::SpecSchemaInvalid => "ACDD_SPEC_SCHEMA_INVALID",
            Self::CommandUnbound => "COMMAND_UNBOUND",
            Self::CommandTestFilterUnsupported => "COMMAND_TEST_FILTER_UNSUPPORTED",
            Self::CommandTestFilterPlaceholderRequired => {
                "COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED"
            }
            Self::UnknownGuidanceSlot => "UNKNOWN_GUIDANCE_SLOT",
            Self::GuidanceBudgetExceeded => "GUIDANCE_BUDGET_EXCEEDED",
            Self::RunSelectorAmbiguous => "RUN_SELECTOR_AMBIGUOUS",
            Self::FeatureDisabled => "FEATURE_DISABLED",
            Self::WorkflowRunnerRequired => "WORKFLOW_RUNNER_REQUIRED",
            Self::TaskAlreadyClaimed => "TASK_ALREADY_CLAIMED",
            Self::StaleClaimRevision => "STALE_CLAIM_REVISION",
            Self::TaskReviewNotIndependent => "TASK_REVIEW_NOT_INDEPENDENT",
            Self::OutOfScopeChanges => "OUT_OF_SCOPE_CHANGES",
            Self::TaskScopeConflict => "TASK_SCOPE_CONFLICT",
            Self::TaskProofUnobserved => "TASK_PROOF_UNOBSERVED",
            Self::TaskCandidateMismatch => "TASK_CANDIDATE_MISMATCH",
            Self::TaskNotIntegrated => "TASK_NOT_INTEGRATED",
            Self::PreTaskArtifactStale => "PRE_TASK_ARTIFACT_STALE",
            Self::DependentTaskCompleted => "DEPENDENT_TASK_COMPLETED",
            Self::DomainCapabilityUnavailable => "DOMAIN_CAPABILITY_UNAVAILABLE",
            Self::MainBranchProtected => "MAIN_BRANCH_PROTECTED",
            Self::GateVerificationFailed => "GATE_VERIFICATION_FAILED",
            Self::RunnerNoSubmission => "RUNNER_NO_SUBMISSION",
            Self::GitIntegrationConflict => "GIT_INTEGRATION_CONFLICT",
            Self::ExternalGitDrift => "EXTERNAL_GIT_DRIFT",
            Self::MutatingActionUncertainOutcome => "MUTATING_ACTION_UNCERTAIN_OUTCOME",
            Self::BackendConflict => "BACKEND_CONFLICT",
        }
    }

    /// Return the canonical CLI process exit code (`2`, `3`, or `4`).
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::ConfigMigrationRequired
            | Self::WorkflowSchemaInvalid
            | Self::SpecSchemaInvalid
            | Self::CommandUnbound
            | Self::CommandTestFilterUnsupported
            | Self::CommandTestFilterPlaceholderRequired
            | Self::UnknownGuidanceSlot
            | Self::GuidanceBudgetExceeded
            | Self::RunSelectorAmbiguous
            | Self::FeatureDisabled
            | Self::WorkflowRunnerRequired => 2,
            Self::TaskAlreadyClaimed
            | Self::StaleClaimRevision
            | Self::TaskReviewNotIndependent
            | Self::OutOfScopeChanges
            | Self::TaskScopeConflict
            | Self::TaskProofUnobserved
            | Self::TaskCandidateMismatch
            | Self::TaskNotIntegrated
            | Self::PreTaskArtifactStale
            | Self::DependentTaskCompleted
            | Self::DomainCapabilityUnavailable
            | Self::MainBranchProtected => 3,
            Self::GateVerificationFailed
            | Self::RunnerNoSubmission
            | Self::GitIntegrationConflict
            | Self::ExternalGitDrift
            | Self::MutatingActionUncertainOutcome
            | Self::BackendConflict => 4,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ErrorCode {
    type Err = AcddError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == s)
            .ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("unknown error code '{s}'"),
                )
            })
    }
}

/// Structured diagnostic error carrying a canonical `ErrorCode`, human message, and optional remediation hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcddError {
    /// Stable machine-readable error code.
    pub code: ErrorCode,
    /// Contextual explanation of the failure.
    pub message: String,
    /// Optional remediation guidance for the operator or agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl AcddError {
    /// Construct a new `AcddError` without a hint.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    /// Attach a remediation hint to this error.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Return the CLI exit code associated with `self.code`.
    pub const fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// Extract a `&AcddError` from an `anyhow::Error` chain if present.
    pub fn from_anyhow(err: &anyhow::Error) -> Option<&Self> {
        err.chain()
            .find_map(|cause| cause.downcast_ref::<AcddError>())
    }
}

impl fmt::Display for AcddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " (hint: {hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for AcddError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_wire_roundtrip_and_exit_codes() {
        let cases: &[(ErrorCode, &str, i32)] = &[
            (
                ErrorCode::ConfigMigrationRequired,
                "CONFIG_MIGRATION_REQUIRED",
                2,
            ),
            (
                ErrorCode::WorkflowSchemaInvalid,
                "ACDD_WORKFLOW_SCHEMA_INVALID",
                2,
            ),
            (ErrorCode::SpecSchemaInvalid, "ACDD_SPEC_SCHEMA_INVALID", 2),
            (ErrorCode::CommandUnbound, "COMMAND_UNBOUND", 2),
            (
                ErrorCode::CommandTestFilterUnsupported,
                "COMMAND_TEST_FILTER_UNSUPPORTED",
                2,
            ),
            (
                ErrorCode::CommandTestFilterPlaceholderRequired,
                "COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED",
                2,
            ),
            (ErrorCode::UnknownGuidanceSlot, "UNKNOWN_GUIDANCE_SLOT", 2),
            (
                ErrorCode::GuidanceBudgetExceeded,
                "GUIDANCE_BUDGET_EXCEEDED",
                2,
            ),
            (ErrorCode::RunSelectorAmbiguous, "RUN_SELECTOR_AMBIGUOUS", 2),
            (ErrorCode::FeatureDisabled, "FEATURE_DISABLED", 2),
            (
                ErrorCode::WorkflowRunnerRequired,
                "WORKFLOW_RUNNER_REQUIRED",
                2,
            ),
            (ErrorCode::TaskAlreadyClaimed, "TASK_ALREADY_CLAIMED", 3),
            (ErrorCode::StaleClaimRevision, "STALE_CLAIM_REVISION", 3),
            (
                ErrorCode::TaskReviewNotIndependent,
                "TASK_REVIEW_NOT_INDEPENDENT",
                3,
            ),
            (ErrorCode::OutOfScopeChanges, "OUT_OF_SCOPE_CHANGES", 3),
            (ErrorCode::TaskScopeConflict, "TASK_SCOPE_CONFLICT", 3),
            (ErrorCode::TaskProofUnobserved, "TASK_PROOF_UNOBSERVED", 3),
            (
                ErrorCode::TaskCandidateMismatch,
                "TASK_CANDIDATE_MISMATCH",
                3,
            ),
            (ErrorCode::TaskNotIntegrated, "TASK_NOT_INTEGRATED", 3),
            (
                ErrorCode::PreTaskArtifactStale,
                "PRE_TASK_ARTIFACT_STALE",
                3,
            ),
            (
                ErrorCode::DependentTaskCompleted,
                "DEPENDENT_TASK_COMPLETED",
                3,
            ),
            (
                ErrorCode::DomainCapabilityUnavailable,
                "DOMAIN_CAPABILITY_UNAVAILABLE",
                3,
            ),
            (ErrorCode::MainBranchProtected, "MAIN_BRANCH_PROTECTED", 3),
            (
                ErrorCode::GateVerificationFailed,
                "GATE_VERIFICATION_FAILED",
                4,
            ),
            (ErrorCode::RunnerNoSubmission, "RUNNER_NO_SUBMISSION", 4),
            (
                ErrorCode::GitIntegrationConflict,
                "GIT_INTEGRATION_CONFLICT",
                4,
            ),
            (ErrorCode::ExternalGitDrift, "EXTERNAL_GIT_DRIFT", 4),
            (
                ErrorCode::MutatingActionUncertainOutcome,
                "MUTATING_ACTION_UNCERTAIN_OUTCOME",
                4,
            ),
            (ErrorCode::BackendConflict, "BACKEND_CONFLICT", 4),
        ];

        assert_eq!(cases.len(), ErrorCode::ALL.len());
        for &(code, wire, expected_exit) in cases {
            assert_eq!(code.as_str(), wire);
            assert_eq!(code.exit_code(), expected_exit);
            assert_eq!(code.to_string(), wire);
            assert_eq!(ErrorCode::from_str(wire).unwrap(), code);

            let serialized = serde_json::to_string(&code).unwrap();
            assert_eq!(serialized, format!("\"{wire}\""));
            let deserialized: ErrorCode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, code);
        }

        assert!(ErrorCode::from_str("UNKNOWN_CODE").is_err());
    }

    #[test]
    fn acdd_error_display_and_anyhow_downcast() {
        let err = AcddError::new(ErrorCode::CommandUnbound, "command 'test' is unbound")
            .with_hint("bind 'test' in .acdd/workflows/default.yaml");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            err.to_string(),
            "COMMAND_UNBOUND: command 'test' is unbound (hint: bind 'test' in .acdd/workflows/default.yaml)"
        );

        let wrapped = anyhow::Error::new(err.clone()).context("while validating gate");
        assert_eq!(AcddError::from_anyhow(&wrapped), Some(&err));
    }
}
