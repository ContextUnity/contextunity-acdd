//! Read-only SQLite queries and strict domain-newtype row decoders (`StateReader`).

use crate::types::{
    ArchiveCleanupEntry, ArtifactKind, BlackboardListReq, BlackboardPage, BlackboardRecord,
    CheckResultRecord, GateArtifactRecord, GateClaimRecord, GateNodeRecord, GateTraceRecord,
    ListRunsPage, ListRunsReq, NodeStatus, ObservabilitySnapshot, RunGraphSnapshot, RunRecord,
    RunTombstoneRecord, SubtaskRecord, TaskDependencyRecord, TraceStatus,
};
use acdd_core::fs::same_filesystem_path;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{CommitOid, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

struct ReadTxGuard<'a> {
    conn: &'a Connection,
    active: bool,
}

impl Drop for ReadTxGuard<'_> {
    fn drop(&mut self) {
        if self.active && !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK;");
        }
    }
}

fn with_read_tx<T>(
    conn: &Connection,
    context: &str,
    f: impl FnOnce() -> Result<T, StateError>,
) -> Result<T, StateError> {
    let started = if conn.is_autocommit() {
        conn.execute_batch("BEGIN DEFERRED;")
            .map_err(|err| map_sql_err(err, context))?;
        true
    } else {
        false
    };
    let _guard = ReadTxGuard {
        conn,
        active: started,
    };
    f()
}

fn map_sql_err(err: rusqlite::Error, context: &str) -> StateError {
    if let rusqlite::Error::SqliteFailure(sqlite_err, _) = &err {
        if matches!(
            sqlite_err.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ) {
            return StateError::new(
                ErrorCode::BackendConflict,
                format!("{context}: SQLite busy/locked: {err}"),
            );
        }
    }
    StateError::new(ErrorCode::ProfileSchemaInvalid, format!("{context}: {err}"))
}

fn parse_enum<T: FromStr<Err = StateError>>(raw: &str, field: &str) -> Result<T, StateError> {
    raw.parse::<T>().map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid {field} '{raw}': {}", err.message),
        )
    })
}

fn parse_id<T>(
    raw: String,
    ctor: impl FnOnce(String) -> Result<T, acdd_core::ids::IdError>,
    field: &str,
) -> Result<T, StateError> {
    ctor(raw).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid {field} in storage: {err}"),
        )
    })
}

fn parse_opt_id<T>(
    raw: Option<String>,
    ctor: impl FnOnce(String) -> Result<T, acdd_core::ids::IdError>,
    field: &str,
) -> Result<Option<T>, StateError> {
    raw.map(|v| parse_id(v, ctor, field)).transpose()
}

fn parse_json_col<T: DeserializeOwned>(raw: &str, field: &str) -> Result<T, StateError> {
    serde_json::from_str(raw).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid JSON in {field}: {err}"),
        )
    })
}

fn to_u32(val: i64, field: &str) -> Result<u32, StateError> {
    u32::try_from(val).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("integer overflow/negative value for {field}: {val}"),
        )
    })
}

fn to_u64(val: i64, field: &str) -> Result<u64, StateError> {
    u64::try_from(val).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("integer overflow/negative value for {field}: {val}"),
        )
    })
}

fn to_usize(val: i64, field: &str) -> Result<usize, StateError> {
    usize::try_from(val).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("integer overflow/negative value for {field}: {val}"),
        )
    })
}

fn to_bool_flag(val: i64, field: &str) -> Result<bool, StateError> {
    match val {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid boolean integer for {field}: {other}"),
        )),
    }
}

fn decode_run_row(row: &Row<'_>) -> rusqlite::Result<Result<RunRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let run_name_raw: String = row.get(1)?;
    let collection: Option<String> = row.get(2)?;
    let workspace: String = row.get(3)?;
    let profile_id: String = row.get(4)?;
    let profile_json: String = row.get(5)?;
    let spec_path: Option<String> = row.get(6)?;
    let spec_id: Option<String> = row.get(7)?;
    let spec_digest: String = row.get(8)?;
    let is_ephemeral_raw: i64 = row.get(9)?;
    let isolation_raw: String = row.get(10)?;
    let target_base: Option<String> = row.get(11)?;
    let target_base_commit_raw: Option<String> = row.get(12)?;
    let branch: Option<String> = row.get(13)?;
    let worktree: Option<String> = row.get(14)?;
    let trace_id: String = row.get(15)?;
    let status_raw: String = row.get(16)?;
    let cancellation_reason: Option<String> = row.get(17)?;
    let root_cycle_count_raw: i64 = row.get(18)?;
    let generation_raw: i64 = row.get(19)?;
    let created_at: String = row.get(20)?;
    let completed_at: Option<String> = row.get(21)?;

    Ok((|| {
        Ok(RunRecord {
            run_id: parse_id(run_id_raw, RunId::new, "runs.run_id")?,
            run_name: parse_id(run_name_raw, RunName::new, "runs.run_name")?,
            collection,
            workspace,
            profile_id,
            profile_json,
            spec_path,
            spec_id,
            spec_digest,
            is_ephemeral: to_bool_flag(is_ephemeral_raw, "runs.is_ephemeral")?,
            isolation: parse_enum(&isolation_raw, "runs.isolation")?,
            target_base,
            target_base_commit: parse_opt_id(
                target_base_commit_raw,
                CommitOid::new,
                "runs.target_base_commit",
            )?,
            branch,
            worktree,
            trace_id,
            status: parse_enum(&status_raw, "runs.status")?,
            cancellation_reason,
            root_cycle_count: to_u32(root_cycle_count_raw, "runs.root_cycle_count")?,
            generation: to_u64(generation_raw, "runs.generation")?,
            created_at,
            completed_at,
        })
    })())
}

fn decode_node_row(row: &Row<'_>) -> rusqlite::Result<Result<GateNodeRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let node_id: String = row.get(1)?;
    let parent_id: Option<String> = row.get(2)?;
    let kind_raw: String = row.get(3)?;
    let task_ref_raw: Option<String> = row.get(4)?;
    let gate_id: String = row.get(5)?;
    let seq_order_raw: i64 = row.get(6)?;
    let status_raw: String = row.get(7)?;
    let node_version_raw: i64 = row.get(8)?;
    let isolation_raw: String = row.get(9)?;
    let attempt_raw: i64 = row.get(10)?;
    let cycle_count_raw: i64 = row.get(11)?;
    let max_attempts_raw: i64 = row.get(12)?;
    let claim_revision_raw: i64 = row.get(13)?;
    let contract_revision_raw: i64 = row.get(14)?;
    let worker_id_raw: Option<String> = row.get(15)?;
    let session_fingerprint: Option<String> = row.get(16)?;
    let branch: Option<String> = row.get(17)?;
    let worktree: Option<String> = row.get(18)?;
    let baseline_commit_raw: Option<String> = row.get(19)?;
    let claim_tree_raw: Option<String> = row.get(20)?;
    let accepted_tree_raw: Option<String> = row.get(21)?;
    let accepted_definition_digest: Option<String> = row.get(22)?;
    let tags_json: String = row.get(23)?;
    let invariants_json: String = row.get(24)?;
    let scope_json: String = row.get(25)?;
    let extended_scope_json: String = row.get(26)?;
    let active_span_id: Option<String> = row.get(27)?;

    Ok((|| {
        Ok(GateNodeRecord {
            run_id: parse_id(run_id_raw, RunId::new, "gate_nodes.run_id")?,
            node_id,
            parent_id,
            kind: parse_enum(&kind_raw, "gate_nodes.kind")?,
            task_ref: parse_opt_id(task_ref_raw, TaskRef::new, "gate_nodes.task_ref")?,
            gate_id,
            seq_order: to_u32(seq_order_raw, "gate_nodes.seq_order")?,
            status: parse_enum(&status_raw, "gate_nodes.status")?,
            node_version: to_u64(node_version_raw, "gate_nodes.node_version")?,
            isolation: parse_enum(&isolation_raw, "gate_nodes.isolation")?,
            attempt: to_u32(attempt_raw, "gate_nodes.attempt")?,
            cycle_count: to_u32(cycle_count_raw, "gate_nodes.cycle_count")?,
            max_attempts: to_u32(max_attempts_raw, "gate_nodes.max_attempts")?,
            claim_revision: to_u32(claim_revision_raw, "gate_nodes.claim_revision")?,
            contract_revision: to_u32(contract_revision_raw, "gate_nodes.contract_revision")?,
            worker_id: parse_opt_id(worker_id_raw, WorkerId::new, "gate_nodes.worker_id")?,
            session_fingerprint,
            branch,
            worktree,
            baseline_commit: parse_opt_id(
                baseline_commit_raw,
                CommitOid::new,
                "gate_nodes.baseline_commit",
            )?,
            claim_tree: parse_opt_id(claim_tree_raw, TreeOid::new, "gate_nodes.claim_tree")?,
            accepted_tree: parse_opt_id(
                accepted_tree_raw,
                TreeOid::new,
                "gate_nodes.accepted_tree",
            )?,
            accepted_definition_digest,
            tags: parse_json_col(&tags_json, "gate_nodes.tags_json")?,
            invariants: parse_json_col(&invariants_json, "gate_nodes.invariants_json")?,
            scope: parse_json_col(&scope_json, "gate_nodes.scope_json")?,
            extended_scope: parse_json_col(
                &extended_scope_json,
                "gate_nodes.extended_scope_json",
            )?,
            active_span_id,
        })
    })())
}

fn decode_dependency_row(row: &Row<'_>) -> rusqlite::Result<Result<TaskDependencyRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let task_ref_raw: String = row.get(1)?;
    let depends_on_workspace: String = row.get(2)?;
    let depends_on_run_id_raw: String = row.get(3)?;
    let depends_on_ref_raw: String = row.get(4)?;
    let satisfied_raw: i64 = row.get(5)?;

    Ok((|| {
        Ok(TaskDependencyRecord {
            run_id: parse_id(run_id_raw, RunId::new, "task_dependencies.run_id")?,
            task_ref: parse_id(task_ref_raw, TaskRef::new, "task_dependencies.task_ref")?,
            depends_on_workspace,
            depends_on_run_id: parse_id(
                depends_on_run_id_raw,
                RunId::new,
                "task_dependencies.depends_on_run_id",
            )?,
            depends_on_ref: if depends_on_ref_raw == TaskRef::WHOLE_SPEC {
                TaskRef::whole_spec()
            } else {
                parse_id(
                    depends_on_ref_raw,
                    TaskRef::new,
                    "task_dependencies.depends_on_ref",
                )?
            },
            satisfied: to_bool_flag(satisfied_raw, "task_dependencies.satisfied")?,
        })
    })())
}

fn decode_subtask_row(row: &Row<'_>) -> rusqlite::Result<Result<SubtaskRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let node_id: String = row.get(1)?;
    let subtask_ref_raw: String = row.get(2)?;
    let seq_order_raw: i64 = row.get(3)?;
    let title: String = row.get(4)?;
    let status_raw: String = row.get(5)?;
    let evidence: Option<String> = row.get(6)?;
    let updated_at: String = row.get(7)?;

    Ok((|| {
        Ok(SubtaskRecord {
            run_id: parse_id(run_id_raw, RunId::new, "subtasks.run_id")?,
            node_id,
            subtask_ref: parse_id(subtask_ref_raw, SubtaskRef::new, "subtasks.subtask_ref")?,
            seq_order: to_u32(seq_order_raw, "subtasks.seq_order")?,
            title,
            status: parse_enum(&status_raw, "subtasks.status")?,
            evidence,
            updated_at,
        })
    })())
}

fn decode_claim_row(row: &Row<'_>) -> rusqlite::Result<Result<GateClaimRecord, StateError>> {
    let claim_id: i64 = row.get(0)?;
    let run_id_raw: String = row.get(1)?;
    let node_id: String = row.get(2)?;
    let claim_revision_raw: i64 = row.get(3)?;
    let worker_id_raw: String = row.get(4)?;
    let session_fingerprint: Option<String> = row.get(5)?;
    let worktree: String = row.get(6)?;
    let span_id: String = row.get(7)?;
    let claim_tree_raw: String = row.get(8)?;
    let claimed_at: String = row.get(9)?;
    let ended_at: Option<String> = row.get(10)?;
    let outcome_raw: String = row.get(11)?;

    Ok((|| {
        Ok(GateClaimRecord {
            claim_id: Some(claim_id),
            run_id: parse_id(run_id_raw, RunId::new, "gate_claims.run_id")?,
            node_id,
            claim_revision: to_u32(claim_revision_raw, "gate_claims.claim_revision")?,
            worker_id: parse_id(worker_id_raw, WorkerId::new, "gate_claims.worker_id")?,
            session_fingerprint,
            worktree,
            span_id,
            claim_tree: parse_id(claim_tree_raw, TreeOid::new, "gate_claims.claim_tree")?,
            claimed_at,
            ended_at,
            outcome: parse_enum(&outcome_raw, "gate_claims.outcome")?,
        })
    })())
}

fn decode_trace_row(row: &Row<'_>) -> rusqlite::Result<Result<GateTraceRecord, StateError>> {
    let trace_row_id: i64 = row.get(0)?;
    let run_id_raw: String = row.get(1)?;
    let node_id: String = row.get(2)?;
    let claim_revision_raw: i64 = row.get(3)?;
    let contract_revision_raw: i64 = row.get(4)?;
    let execution_digest: String = row.get(5)?;
    let idempotency_key: Option<String> = row.get(6)?;
    let command_id: Option<String> = row.get(7)?;
    let command_line: Option<String> = row.get(8)?;
    let action_id: Option<String> = row.get(9)?;
    let test_target: Option<String> = row.get(10)?;
    let status_raw: String = row.get(11)?;
    let exit_code: Option<i32> = row.get(12)?;
    let passed_tests_json: String = row.get(13)?;
    let failed_tests_json: String = row.get(14)?;
    let check_results_json: String = row.get(15)?;
    let candidate_tree_before_raw: String = row.get(16)?;
    let candidate_tree_after_raw: Option<String> = row.get(17)?;
    let log_relpath: String = row.get(18)?;
    let started_at: String = row.get(19)?;
    let finished_at: Option<String> = row.get(20)?;

    Ok((|| {
        Ok(GateTraceRecord {
            trace_row_id: Some(trace_row_id),
            run_id: parse_id(run_id_raw, RunId::new, "gate_traces.run_id")?,
            node_id,
            claim_revision: to_u32(claim_revision_raw, "gate_traces.claim_revision")?,
            contract_revision: to_u32(contract_revision_raw, "gate_traces.contract_revision")?,
            execution_digest,
            idempotency_key,
            command_id,
            command_line,
            action_id,
            test_target,
            status: parse_enum(&status_raw, "gate_traces.status")?,
            exit_code,
            passed_tests: parse_json_col(&passed_tests_json, "gate_traces.passed_tests_json")?,
            failed_tests: parse_json_col(&failed_tests_json, "gate_traces.failed_tests_json")?,
            check_results: parse_json_col::<Vec<CheckResultRecord>>(
                &check_results_json,
                "gate_traces.check_results_json",
            )?,
            candidate_tree_before: parse_id(
                candidate_tree_before_raw,
                TreeOid::new,
                "gate_traces.candidate_tree_before",
            )?,
            candidate_tree_after: parse_opt_id(
                candidate_tree_after_raw,
                TreeOid::new,
                "gate_traces.candidate_tree_after",
            )?,
            log_relpath,
            started_at,
            finished_at,
        })
    })())
}

fn decode_artifact_row(row: &Row<'_>) -> rusqlite::Result<Result<GateArtifactRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let node_id: String = row.get(1)?;
    let claim_revision_raw: i64 = row.get(2)?;
    let kind_raw: String = row.get(3)?;
    let payload_json: String = row.get(4)?;
    let snapshot_commit_raw: Option<String> = row.get(5)?;
    let created_at: String = row.get(6)?;

    Ok((|| {
        Ok(GateArtifactRecord {
            run_id: parse_id(run_id_raw, RunId::new, "gate_artifacts.run_id")?,
            node_id,
            claim_revision: to_u32(claim_revision_raw, "gate_artifacts.claim_revision")?,
            kind: parse_enum(&kind_raw, "gate_artifacts.kind")?,
            payload_json,
            snapshot_commit: parse_opt_id(
                snapshot_commit_raw,
                CommitOid::new,
                "gate_artifacts.snapshot_commit",
            )?,
            created_at,
        })
    })())
}

fn decode_blackboard_row(row: &Row<'_>) -> rusqlite::Result<Result<BlackboardRecord, StateError>> {
    let id: i64 = row.get(0)?;
    let run_id_raw: String = row.get(1)?;
    let node_id: Option<String> = row.get(2)?;
    let recipient_node_id: Option<String> = row.get(3)?;
    let subtask_ref_raw: Option<String> = row.get(4)?;
    let gate: Option<String> = row.get(5)?;
    let author: String = row.get(6)?;
    let topic_raw: String = row.get(7)?;
    let status_raw: String = row.get(8)?;
    let payload: String = row.get(9)?;
    let created_at: String = row.get(10)?;

    Ok((|| {
        Ok(BlackboardRecord {
            id: Some(id),
            run_id: parse_id(run_id_raw, RunId::new, "blackboard.run_id")?,
            node_id,
            recipient_node_id,
            subtask_ref: parse_opt_id(subtask_ref_raw, SubtaskRef::new, "blackboard.subtask_ref")?,
            gate,
            author,
            topic: parse_enum(&topic_raw, "blackboard.topic")?,
            status: parse_enum(&status_raw, "blackboard.status")?,
            payload,
            created_at,
        })
    })())
}

fn decode_tombstone_row(row: &Row<'_>) -> rusqlite::Result<Result<RunTombstoneRecord, StateError>> {
    let run_id_raw: String = row.get(0)?;
    let run_name_raw: String = row.get(1)?;
    let collection: Option<String> = row.get(2)?;
    let workspace: String = row.get(3)?;
    let spec_id: Option<String> = row.get(4)?;
    let title: String = row.get(5)?;
    let profile_id: String = row.get(6)?;
    let is_ephemeral_raw: i64 = row.get(7)?;
    let generation_raw: i64 = row.get(8)?;
    let terminal_status_raw: String = row.get(9)?;
    let target_base: Option<String> = row.get(10)?;
    let target_base_commit_raw: Option<String> = row.get(11)?;
    let archive_path: Option<String> = row.get(12)?;
    let closed_commit_raw: Option<String> = row.get(13)?;
    let cleanup_pending_raw: i64 = row.get(14)?;
    let cleanup_manifest_json: String = row.get(15)?;
    let summary_json: String = row.get(16)?;
    let metrics_json: String = row.get(17)?;
    let closed_at: String = row.get(18)?;

    Ok((|| {
        Ok(RunTombstoneRecord {
            run_id: parse_id(run_id_raw, RunId::new, "run_tombstones.run_id")?,
            run_name: parse_id(run_name_raw, RunName::new, "run_tombstones.run_name")?,
            collection,
            workspace,
            spec_id,
            title,
            profile_id,
            is_ephemeral: to_bool_flag(is_ephemeral_raw, "run_tombstones.is_ephemeral")?,
            generation: to_u64(generation_raw, "run_tombstones.generation")?,
            terminal_status: parse_enum(&terminal_status_raw, "run_tombstones.terminal_status")?,
            target_base,
            target_base_commit: parse_opt_id(
                target_base_commit_raw,
                CommitOid::new,
                "run_tombstones.target_base_commit",
            )?,
            archive_path,
            closed_commit: parse_opt_id(
                closed_commit_raw,
                CommitOid::new,
                "run_tombstones.closed_commit",
            )?,
            cleanup_pending: to_bool_flag(cleanup_pending_raw, "run_tombstones.cleanup_pending")?,
            cleanup_manifest: parse_json_col::<Vec<ArchiveCleanupEntry>>(
                &cleanup_manifest_json,
                "run_tombstones.cleanup_manifest_json",
            )?,
            summary_json,
            metrics_json,
            closed_at,
        })
    })())
}

fn collect_rows<T>(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
    decoder: fn(&Row<'_>) -> rusqlite::Result<Result<T, StateError>>,
    context: &str,
) -> Result<Vec<T>, StateError> {
    let mapped = stmt
        .query_map(params, decoder)
        .map_err(|err| map_sql_err(err, context))?;
    let mut items = Vec::new();
    for item in mapped {
        let decoded = item.map_err(|err| map_sql_err(err, context))??;
        items.push(decoded);
    }
    Ok(items)
}

const RUN_COLS_V1: &str = "run_id, run_name, collection, workspace, profile_id, profile_json, \
    spec_path, NULL AS spec_id, spec_digest, is_ephemeral, isolation, target_base, target_base_commit, \
    branch, worktree, trace_id, status, cancellation_reason, root_cycle_count, generation, \
    created_at, completed_at";

const RUN_COLS_V2: &str = "run_id, run_name, collection, workspace, profile_id, profile_json, \
    spec_path, spec_id, spec_digest, is_ephemeral, isolation, target_base, target_base_commit, \
    branch, worktree, trace_id, status, cancellation_reason, root_cycle_count, generation, \
    created_at, completed_at";

const RUN_COLS_JOIN_V1: &str = "r.run_id, r.run_name, r.collection, r.workspace, r.profile_id, \
    r.profile_json, r.spec_path, NULL AS spec_id, r.spec_digest, r.is_ephemeral, r.isolation, \
    r.target_base, r.target_base_commit, r.branch, r.worktree, r.trace_id, r.status, \
    r.cancellation_reason, r.root_cycle_count, r.generation, r.created_at, r.completed_at";

const RUN_COLS_JOIN_V2: &str = "r.run_id, r.run_name, r.collection, r.workspace, r.profile_id, \
    r.profile_json, r.spec_path, r.spec_id, r.spec_digest, r.is_ephemeral, r.isolation, \
    r.target_base, r.target_base_commit, r.branch, r.worktree, r.trace_id, r.status, \
    r.cancellation_reason, r.root_cycle_count, r.generation, r.created_at, r.completed_at";

fn run_cols(schema_version: u32) -> &'static str {
    if schema_version < super::schema::CURRENT_SCHEMA_VERSION {
        RUN_COLS_V1
    } else {
        RUN_COLS_V2
    }
}

fn run_cols_join(schema_version: u32) -> &'static str {
    if schema_version < super::schema::CURRENT_SCHEMA_VERSION {
        RUN_COLS_JOIN_V1
    } else {
        RUN_COLS_JOIN_V2
    }
}

const NODE_COLS: &str = "run_id, node_id, parent_id, kind, task_ref, gate_id, seq_order, \
    status, node_version, isolation, attempt, cycle_count, max_attempts, claim_revision, \
    contract_revision, worker_id, session_fingerprint, branch, worktree, baseline_commit, \
    claim_tree, accepted_tree, accepted_definition_digest, tags_json, invariants_json, \
    scope_json, extended_scope_json, active_span_id";

const TOMBSTONE_COLS: &str = "run_id, run_name, collection, workspace, spec_id, title, \
    profile_id, is_ephemeral, generation, terminal_status, target_base, target_base_commit, \
    archive_path, closed_commit, cleanup_pending, cleanup_manifest_json, summary_json, \
    metrics_json, closed_at";

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn load_run_graph(
    conn: &Connection,
    run_id: &RunId,
) -> Result<Option<RunGraphSnapshot>, StateError> {
    load_run_graph_with_version(conn, run_id, super::schema::CURRENT_SCHEMA_VERSION)
}

pub(crate) fn load_run_graph_with_version(
    conn: &Connection,
    run_id: &RunId,
    schema_version: u32,
) -> Result<Option<RunGraphSnapshot>, StateError> {
    with_read_tx(conn, "begin load_run_graph", || {
        load_run_graph_inner(conn, run_id, schema_version)
    })
}

fn load_run_graph_inner(
    conn: &Connection,
    run_id: &RunId,
    schema_version: u32,
) -> Result<Option<RunGraphSnapshot>, StateError> {
    let run_cols_sql = run_cols(schema_version);
    let run_sql = format!("SELECT {run_cols_sql} FROM runs WHERE run_id = ?1");
    let run_opt = conn
        .query_row(&run_sql, params![run_id.as_str()], decode_run_row)
        .optional()
        .map_err(|err| map_sql_err(err, "load_run_graph(runs)"))?
        .transpose()?;
    let Some(run) = run_opt else {
        return Ok(None);
    };

    let nodes_sql = format!(
        "SELECT {NODE_COLS} FROM gate_nodes WHERE run_id = ?1 ORDER BY seq_order ASC, node_id ASC"
    );
    let mut nodes_stmt = conn
        .prepare(&nodes_sql)
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(gate_nodes)"))?;
    let nodes = collect_rows(
        &mut nodes_stmt,
        params![run_id.as_str()],
        decode_node_row,
        "load_run_graph(gate_nodes)",
    )?;

    let mut deps_stmt = conn
        .prepare(
            "SELECT run_id, task_ref, depends_on_workspace, depends_on_run_id, depends_on_ref, satisfied
             FROM task_dependencies
             WHERE run_id = ?1
             ORDER BY task_ref ASC, depends_on_workspace ASC, depends_on_run_id ASC, depends_on_ref ASC",
        )
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(task_dependencies)"))?;
    let dependencies = collect_rows(
        &mut deps_stmt,
        params![run_id.as_str()],
        decode_dependency_row,
        "load_run_graph(task_dependencies)",
    )?;

    let mut subtasks_stmt = conn
        .prepare(
            "SELECT run_id, node_id, subtask_ref, seq_order, title, status, evidence, updated_at
             FROM subtasks
             WHERE run_id = ?1
             ORDER BY node_id ASC, seq_order ASC, subtask_ref ASC",
        )
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(subtasks)"))?;
    let subtasks = collect_rows(
        &mut subtasks_stmt,
        params![run_id.as_str()],
        decode_subtask_row,
        "load_run_graph(subtasks)",
    )?;

    let mut claims_stmt = conn
        .prepare(
            "SELECT claim_id, run_id, node_id, claim_revision, worker_id, session_fingerprint,
                    worktree, span_id, claim_tree, claimed_at, ended_at, outcome
             FROM gate_claims
             WHERE run_id = ?1
             ORDER BY node_id ASC, claim_revision ASC",
        )
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(gate_claims)"))?;
    let claims = collect_rows(
        &mut claims_stmt,
        params![run_id.as_str()],
        decode_claim_row,
        "load_run_graph(gate_claims)",
    )?;

    let mut traces_stmt = conn
        .prepare(
            "SELECT trace_row_id, run_id, node_id, claim_revision, contract_revision,
                    execution_digest, idempotency_key, command_id, command_line, action_id,
                    test_target, status, exit_code,
                    passed_tests_json, failed_tests_json, check_results_json,
                    candidate_tree_before, candidate_tree_after, log_relpath, started_at, finished_at
             FROM gate_traces
             WHERE run_id = ?1
             ORDER BY trace_row_id ASC",
        )
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(gate_traces)"))?;
    let traces = collect_rows(
        &mut traces_stmt,
        params![run_id.as_str()],
        decode_trace_row,
        "load_run_graph(gate_traces)",
    )?;

    let mut artifacts_stmt = conn
        .prepare(
            "SELECT run_id, node_id, claim_revision, kind, payload_json, snapshot_commit, created_at
             FROM gate_artifacts
             WHERE run_id = ?1
             ORDER BY node_id ASC, claim_revision ASC, kind ASC",
        )
        .map_err(|err| map_sql_err(err, "prepare load_run_graph(gate_artifacts)"))?;
    let artifacts = collect_rows(
        &mut artifacts_stmt,
        params![run_id.as_str()],
        decode_artifact_row,
        "load_run_graph(gate_artifacts)",
    )?;

    Ok(Some(RunGraphSnapshot {
        run,
        nodes,
        dependencies,
        subtasks,
        claims,
        traces,
        artifacts,
    }))
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn list_runs(conn: &Connection, req: &ListRunsReq) -> Result<ListRunsPage, StateError> {
    list_runs_with_version(conn, req, super::schema::CURRENT_SCHEMA_VERSION)
}

pub(crate) fn list_runs_with_version(
    conn: &Connection,
    req: &ListRunsReq,
    schema_version: u32,
) -> Result<ListRunsPage, StateError> {
    with_read_tx(conn, "begin list_runs", || {
        list_runs_inner(conn, req, schema_version)
    })
}

fn list_runs_inner(
    conn: &Connection,
    req: &ListRunsReq,
    schema_version: u32,
) -> Result<ListRunsPage, StateError> {
    let limit = req.limit.clamp(1, 100);
    let offset = req.offset;

    let mut clauses: Vec<&'static str> = Vec::new();
    let mut bind_values: Vec<SqlValue> = Vec::new();
    if let Some(ws) = &req.workspace {
        clauses.push("workspace = ?");
        bind_values.push(SqlValue::Text(ws.clone()));
    }
    if let Some(status) = req.status {
        clauses.push("status = ?");
        bind_values.push(SqlValue::Text(status.as_str().to_owned()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    if let Some(expected_gen) = req.generation {
        let gen_sql = format!("SELECT COALESCE(MAX(generation), 0) FROM runs{where_sql}");
        let observed_raw: i64 = conn
            .query_row(&gen_sql, params_from_iter(bind_values.iter()), |row| {
                row.get(0)
            })
            .map_err(|err| map_sql_err(err, "list_runs(generation)"))?;
        let observed = to_u64(observed_raw, "runs.generation")?;
        if observed != expected_gen {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "runs generation drifted during pagination: expected {expected_gen}, observed {observed}"
                ),
            ));
        }
    }

    let count_sql = format!("SELECT COUNT(*) FROM runs{where_sql}");
    let total_runs_raw: i64 = conn
        .query_row(&count_sql, params_from_iter(bind_values.iter()), |row| {
            row.get(0)
        })
        .map_err(|err| map_sql_err(err, "list_runs(count)"))?;
    let total_runs = to_usize(total_runs_raw, "total_runs")?;

    let mut page_params = bind_values;
    page_params.push(SqlValue::Integer(
        i64::try_from(limit).unwrap_or(100),
    ));
    page_params.push(SqlValue::Integer(
        i64::try_from(offset).unwrap_or(i64::MAX),
    ));

    let run_cols_sql = run_cols(schema_version);
    let page_sql = format!(
        "SELECT {run_cols_sql} FROM runs{where_sql} ORDER BY created_at DESC, run_id ASC LIMIT ? OFFSET ?"
    );
    let mut stmt = conn
        .prepare(&page_sql)
        .map_err(|err| map_sql_err(err, "prepare list_runs"))?;
    let runs = collect_rows(
        &mut stmt,
        params_from_iter(page_params.iter()),
        decode_run_row,
        "list_runs(rows)",
    )?;

    let (tombstones, total_tombstones) = if req.include_tombstones {
        let mut tomb_clauses: Vec<&'static str> = Vec::new();
        let mut tomb_values: Vec<SqlValue> = Vec::new();
        if let Some(ws) = &req.workspace {
            tomb_clauses.push("workspace = ?");
            tomb_values.push(SqlValue::Text(ws.clone()));
        }
        let tomb_where = if tomb_clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", tomb_clauses.join(" AND "))
        };

        let count_tomb_sql = format!("SELECT COUNT(*) FROM run_tombstones{tomb_where}");
        let total_tomb_raw: i64 = conn
            .query_row(&count_tomb_sql, params_from_iter(tomb_values.iter()), |row| {
                row.get(0)
            })
            .map_err(|err| map_sql_err(err, "list_runs(tombstones count)"))?;
        let total_tomb = to_usize(total_tomb_raw, "total_tombstones")?;

        let mut tomb_page_params = tomb_values;
        tomb_page_params.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(100)));
        tomb_page_params.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));

        let tomb_sql = format!(
            "SELECT {TOMBSTONE_COLS} FROM run_tombstones{tomb_where} ORDER BY closed_at DESC, run_id ASC LIMIT ? OFFSET ?"
        );
        let mut tomb_stmt = conn
            .prepare(&tomb_sql)
            .map_err(|err| map_sql_err(err, "prepare list_runs(tombstones)"))?;
        let items = collect_rows(
            &mut tomb_stmt,
            params_from_iter(tomb_page_params.iter()),
            decode_tombstone_row,
            "list_runs(tombstones)",
        )?;
        (items, total_tomb)
    } else {
        (Vec::new(), 0)
    };

    Ok(ListRunsPage {
        runs,
        tombstones,
        total_runs,
        total_tombstones,
        limit,
        offset,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn resolve_active_run_by_worktree(
    conn: &Connection,
    worktree: &str,
) -> Result<Option<RunRecord>, StateError> {
    resolve_active_run_by_worktree_with_version(
        conn,
        worktree,
        super::schema::CURRENT_SCHEMA_VERSION,
    )
}

pub(crate) fn resolve_active_run_by_worktree_with_version(
    conn: &Connection,
    worktree: &str,
    schema_version: u32,
) -> Result<Option<RunRecord>, StateError> {
    with_read_tx(conn, "begin resolve_active_run_by_worktree", || {
        resolve_active_run_by_worktree_inner(conn, worktree, schema_version)
    })
}

fn resolve_active_run_by_worktree_inner(
    conn: &Connection,
    worktree: &str,
    schema_version: u32,
) -> Result<Option<RunRecord>, StateError> {
    let run_cols_sql = run_cols(schema_version);
    // 1. Direct exact match on runs.worktree (enforced unique for active runs by uq_runs_active_worktree).
    let exact_run_sql = format!(
        "SELECT {run_cols_sql} FROM runs WHERE status = 'active' AND worktree = ?1"
    );
    if let Some(run) = conn
        .query_row(&exact_run_sql, params![worktree], decode_run_row)
        .optional()
        .map_err(|err| map_sql_err(err, "resolve_active_run_by_worktree(runs)"))?
        .transpose()?
    {
        return Ok(Some(run));
    }

    // 2. Exact match on an active run's task/node worktree.
    let run_cols_join_sql = run_cols_join(schema_version);
    let node_sql = format!(
        "SELECT DISTINCT {run_cols_join_sql}
         FROM runs r
         JOIN gate_nodes gn ON gn.run_id = r.run_id
         WHERE r.status = 'active' AND gn.worktree = ?1"
    );
    let mut node_stmt = conn
        .prepare(&node_sql)
        .map_err(|err| map_sql_err(err, "prepare resolve_active_run_by_worktree(nodes)"))?;
    let node_matches = collect_rows(
        &mut node_stmt,
        params![worktree],
        decode_run_row,
        "resolve_active_run_by_worktree(nodes)",
    )?;
    if node_matches.len() > 1 {
        return Err(StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!("multiple active runs reference worktree '{worktree}'"),
        ));
    }
    if let Some(run) = node_matches.into_iter().next() {
        return Ok(Some(run));
    }

    // 3. Filesystem-equivalent path fallback (e.g. macOS /var -> /private/var or uncanonicalized alias).
    let target_path = Path::new(worktree);
    let all_active_sql = format!("SELECT {run_cols_sql} FROM runs WHERE status = 'active'");
    let mut active_stmt = conn
        .prepare(&all_active_sql)
        .map_err(|err| map_sql_err(err, "prepare resolve_active_run_by_worktree(all_active)"))?;
    let active_runs = collect_rows(
        &mut active_stmt,
        [],
        decode_run_row,
        "resolve_active_run_by_worktree(all_active)",
    )?;

    let mut matched_ids = BTreeSet::new();
    let mut matched_run: Option<RunRecord> = None;

    for run in &active_runs {
        if let Some(ref wt) = run.worktree {
            if same_filesystem_path(Path::new(wt), target_path)
                && matched_ids.insert(run.run_id.clone())
                && matched_run.is_none()
            {
                matched_run = Some(run.clone());
            }
        }
    }

    let mut wt_stmt = conn
        .prepare(
            "SELECT DISTINCT gn.run_id, gn.worktree
             FROM gate_nodes gn
             JOIN runs r ON r.run_id = gn.run_id
             WHERE r.status = 'active' AND gn.worktree IS NOT NULL",
        )
        .map_err(|err| map_sql_err(err, "prepare resolve_active_run_by_worktree(node_wt)"))?;
    let wt_rows = wt_stmt
        .query_map([], |row| {
            let rid: String = row.get(0)?;
            let wt: String = row.get(1)?;
            Ok((rid, wt))
        })
        .map_err(|err| map_sql_err(err, "resolve_active_run_by_worktree(node_wt)"))?;

    for row in wt_rows {
        let (rid_raw, wt) = row.map_err(|err| map_sql_err(err, "resolve_active_run_by_worktree(node_wt row)"))?;
        if same_filesystem_path(Path::new(&wt), target_path) {
            let rid = parse_id(rid_raw, RunId::new, "gate_nodes.run_id")?;
            if matched_ids.insert(rid.clone()) {
                matched_run = active_runs.iter().find(|r| r.run_id == rid).cloned();
            }
        }
    }

    if matched_ids.len() > 1 {
        return Err(StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!("multiple active runs match worktree '{worktree}'"),
        ));
    }
    Ok(matched_run)
}

pub(crate) fn load_task_statuses(
    conn: &Connection,
) -> Result<Vec<(RunId, String, NodeStatus)>, StateError> {
    let mut stmt = conn
        .prepare("SELECT run_id, node_id, status FROM gate_nodes WHERE kind = 'task'")
        .map_err(|err| map_sql_err(err, "prepare task statuses"))?;
    collect_rows(
        &mut stmt,
        [],
        |row| {
            let run: String = row.get(0)?;
            let node: String = row.get(1)?;
            let status: String = row.get(2)?;
            Ok((|| {
                Ok((
                    parse_id(run, RunId::new, "gate_nodes.run_id")?,
                    node,
                    parse_enum(&status, "gate_nodes.status")?,
                ))
            })())
        },
        "load task statuses",
    )
}

pub(crate) fn load_node(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
) -> Result<Option<GateNodeRecord>, StateError> {
    let sql = format!("SELECT {NODE_COLS} FROM gate_nodes WHERE run_id = ?1 AND node_id = ?2");
    conn.query_row(&sql, params![run_id.as_str(), node_id], decode_node_row)
        .optional()
        .map_err(|err| map_sql_err(err, "load_node"))?
        .transpose()
}

pub(crate) fn latest_claim(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
) -> Result<Option<GateClaimRecord>, StateError> {
    conn.query_row(
        "SELECT claim_id, run_id, node_id, claim_revision, worker_id, session_fingerprint,
                worktree, span_id, claim_tree, claimed_at, ended_at, outcome
         FROM gate_claims
         WHERE run_id = ?1 AND node_id = ?2
         ORDER BY claim_revision DESC
         LIMIT 1",
        params![run_id.as_str(), node_id],
        decode_claim_row,
    )
    .optional()
    .map_err(|err| map_sql_err(err, "latest_claim"))?
    .transpose()
}

pub(crate) fn latest_trace(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
    claim_revision: u32,
) -> Result<Option<GateTraceRecord>, StateError> {
    conn.query_row(
        "SELECT trace_row_id, run_id, node_id, claim_revision, contract_revision,
                execution_digest, idempotency_key, command_id, command_line, action_id,
                test_target, status, exit_code,
                passed_tests_json, failed_tests_json, check_results_json,
                candidate_tree_before, candidate_tree_after, log_relpath, started_at, finished_at
         FROM gate_traces
         WHERE run_id = ?1 AND node_id = ?2 AND claim_revision = ?3
         ORDER BY trace_row_id DESC
         LIMIT 1",
        params![run_id.as_str(), node_id, i64::from(claim_revision)],
        decode_trace_row,
    )
    .optional()
    .map_err(|err| map_sql_err(err, "latest_trace"))?
    .transpose()
}

pub(crate) fn load_artifact(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
    claim_revision: u32,
    kind: ArtifactKind,
) -> Result<Option<GateArtifactRecord>, StateError> {
    conn.query_row(
        "SELECT run_id, node_id, claim_revision, kind, payload_json, snapshot_commit, created_at
         FROM gate_artifacts
         WHERE run_id = ?1 AND node_id = ?2 AND claim_revision = ?3 AND kind = ?4",
        params![
            run_id.as_str(),
            node_id,
            i64::from(claim_revision),
            kind.as_str()
        ],
        decode_artifact_row,
    )
    .optional()
    .map_err(|err| map_sql_err(err, "load_artifact"))?
    .transpose()
}

pub(crate) fn list_blackboard(
    conn: &Connection,
    run_id: &RunId,
    req: &BlackboardListReq,
) -> Result<BlackboardPage, StateError> {
    with_read_tx(conn, "begin list_blackboard", || {
        list_blackboard_inner(conn, run_id, req)
    })
}

fn list_blackboard_inner(
    conn: &Connection,
    run_id: &RunId,
    req: &BlackboardListReq,
) -> Result<BlackboardPage, StateError> {
    let limit = req.limit.clamp(1, 100);
    let offset = req.offset;

    let run_gen: Option<i64> = conn
        .query_row(
            "SELECT generation FROM runs WHERE run_id = ?1",
            params![run_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| map_sql_err(err, "list_blackboard(runs.generation)"))?;

    let observed_gen = match run_gen {
        Some(g) => Some(to_u64(g, "runs.generation")?),
        None => {
            let tomb_gen: Option<i64> = conn
                .query_row(
                    "SELECT generation FROM run_tombstones WHERE run_id = ?1",
                    params![run_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| map_sql_err(err, "list_blackboard(tombstones.generation)"))?;
            tomb_gen
                .map(|g| to_u64(g, "run_tombstones.generation"))
                .transpose()?
        }
    };

    if let Some(expected_gen) = req.generation {
        if observed_gen != Some(expected_gen) {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "blackboard run generation mismatch for '{}': expected {expected_gen}, observed {:?}",
                    run_id, observed_gen
                ),
            ));
        }
    }

    let mut clauses = vec!["run_id = ?"];
    let mut bind_values = vec![SqlValue::Text(run_id.as_str().to_owned())];
    if let Some(node_id) = &req.node_id {
        clauses.push("node_id = ?");
        bind_values.push(SqlValue::Text(node_id.clone()));
    }
    if let Some(recipient_node_id) = &req.recipient_node_id {
        clauses.push("recipient_node_id = ?");
        bind_values.push(SqlValue::Text(recipient_node_id.clone()));
    }
    if let Some(topic) = req.topic {
        clauses.push("topic = ?");
        bind_values.push(SqlValue::Text(topic.as_str().to_owned()));
    }
    if let Some(status) = req.status {
        clauses.push("status = ?");
        bind_values.push(SqlValue::Text(status.as_str().to_owned()));
    }
    let where_sql = clauses.join(" AND ");

    let count_sql = format!("SELECT COUNT(*) FROM blackboard WHERE {where_sql}");
    let total_raw: i64 = conn
        .query_row(&count_sql, params_from_iter(bind_values.iter()), |row| {
            row.get(0)
        })
        .map_err(|err| map_sql_err(err, "list_blackboard(count)"))?;
    let total = to_usize(total_raw, "blackboard.total")?;

    let mut page_params = bind_values;
    page_params.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(100)));
    page_params.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));

    let page_sql = format!(
        "SELECT id, run_id, node_id, recipient_node_id, subtask_ref, gate, author, topic, status, payload, created_at
         FROM blackboard
         WHERE {where_sql}
         ORDER BY id ASC
         LIMIT ? OFFSET ?"
    );
    let mut stmt = conn
        .prepare(&page_sql)
        .map_err(|err| map_sql_err(err, "prepare list_blackboard"))?;
    let items = collect_rows(
        &mut stmt,
        params_from_iter(page_params.iter()),
        decode_blackboard_row,
        "list_blackboard(rows)",
    )?;

    Ok(BlackboardPage {
        items,
        total,
        generation: observed_gen.unwrap_or(0),
        limit,
        offset,
    })
}

pub(crate) fn load_tombstone(
    conn: &Connection,
    run_id: &RunId,
) -> Result<Option<RunTombstoneRecord>, StateError> {
    let sql = format!("SELECT {TOMBSTONE_COLS} FROM run_tombstones WHERE run_id = ?1");
    conn.query_row(&sql, params![run_id.as_str()], decode_tombstone_row)
        .optional()
        .map_err(|err| map_sql_err(err, "load_tombstone"))?
        .transpose()
}

fn extract_spec_number(candidate: &str, prefix: &str) -> Option<u32> {
    let stem = Path::new(candidate)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(candidate);
    let rest = stem.strip_prefix(prefix)?.strip_prefix('-')?;
    let digits = rest.split('-').next()?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok()
}

pub(crate) fn max_spec_number(
    conn: &Connection,
    collection: &str,
    prefix: &str,
) -> Result<Option<u32>, StateError> {
    with_read_tx(conn, "begin max_spec_number", || {
        max_spec_number_inner(conn, collection, prefix)
    })
}

fn max_spec_number_inner(
    conn: &Connection,
    collection: &str,
    prefix: &str,
) -> Result<Option<u32>, StateError> {
    let mut best: Option<u32> = None;

    let mut runs_stmt = conn
        .prepare("SELECT run_name, spec_path FROM runs WHERE collection = ?1")
        .map_err(|err| map_sql_err(err, "prepare max_spec_number(runs)"))?;
    let runs_rows = runs_stmt
        .query_map(params![collection], |row| {
            let run_name: String = row.get(0)?;
            let spec_path: Option<String> = row.get(1)?;
            Ok((run_name, spec_path))
        })
        .map_err(|err| map_sql_err(err, "max_spec_number(runs)"))?;
    for row in runs_rows {
        let (run_name, spec_path) = row.map_err(|err| map_sql_err(err, "max_spec_number(runs row)"))?;
        for candidate in [Some(run_name), spec_path].into_iter().flatten() {
            if let Some(num) = extract_spec_number(&candidate, prefix) {
                best = Some(best.map_or(num, |cur| cur.max(num)));
            }
        }
    }

    let mut tomb_stmt = conn
        .prepare("SELECT run_name, spec_id, archive_path FROM run_tombstones WHERE collection = ?1")
        .map_err(|err| map_sql_err(err, "prepare max_spec_number(run_tombstones)"))?;
    let tomb_rows = tomb_stmt
        .query_map(params![collection], |row| {
            let run_name: String = row.get(0)?;
            let spec_id: Option<String> = row.get(1)?;
            let archive_path: Option<String> = row.get(2)?;
            Ok((run_name, spec_id, archive_path))
        })
        .map_err(|err| map_sql_err(err, "max_spec_number(run_tombstones)"))?;
    for row in tomb_rows {
        let (run_name, spec_id, archive_path) =
            row.map_err(|err| map_sql_err(err, "max_spec_number(run_tombstones row)"))?;
        for candidate in [Some(run_name), spec_id, archive_path].into_iter().flatten() {
            if let Some(num) = extract_spec_number(&candidate, prefix) {
                best = Some(best.map_or(num, |cur| cur.max(num)));
            }
        }
    }

    Ok(best)
}

pub(crate) fn observability_snapshot(conn: &Connection) -> Result<ObservabilitySnapshot, StateError> {
    with_read_tx(conn, "begin observability_snapshot", || {
        observability_snapshot_inner(conn)
    })
}

fn observability_snapshot_inner(conn: &Connection) -> Result<ObservabilitySnapshot, StateError> {
    let active_runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs WHERE status = 'active'", [], |row| {
            row.get(0)
        })
        .map_err(|err| map_sql_err(err, "observability_snapshot(active_runs)"))?;

    let completed_runs: i64 = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM run_tombstones WHERE terminal_status = 'completed') +
                (SELECT COUNT(*) FROM runs WHERE status = 'completed')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| map_sql_err(err, "observability_snapshot(completed_runs)"))?;

    let cleared_runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tombstones WHERE terminal_status = 'cleared'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| map_sql_err(err, "observability_snapshot(cleared_runs)"))?;

    let mut nodes_by_status = BTreeMap::new();
    let mut node_stmt = conn
        .prepare("SELECT status, COUNT(*) FROM gate_nodes GROUP BY status")
        .map_err(|err| map_sql_err(err, "prepare observability_snapshot(nodes)"))?;
    let node_rows = node_stmt
        .query_map([], |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count))
        })
        .map_err(|err| map_sql_err(err, "observability_snapshot(nodes)"))?;
    for row in node_rows {
        let (status_raw, count_raw) =
            row.map_err(|err| map_sql_err(err, "observability_snapshot(node row)"))?;
        let status: NodeStatus = parse_enum(&status_raw, "gate_nodes.status")?;
        nodes_by_status.insert(
            status.as_str().to_owned(),
            to_usize(count_raw, "nodes_by_status")?,
        );
    }

    let active_claims: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM gate_claims WHERE outcome = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| map_sql_err(err, "observability_snapshot(active_claims)"))?;

    let mut traces_by_status = BTreeMap::new();
    let mut trace_stmt = conn
        .prepare("SELECT status, COUNT(*) FROM gate_traces GROUP BY status")
        .map_err(|err| map_sql_err(err, "prepare observability_snapshot(traces)"))?;
    let trace_rows = trace_stmt
        .query_map([], |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count))
        })
        .map_err(|err| map_sql_err(err, "observability_snapshot(traces)"))?;
    for row in trace_rows {
        let (status_raw, count_raw) =
            row.map_err(|err| map_sql_err(err, "observability_snapshot(trace row)"))?;
        let status: TraceStatus = parse_enum(&status_raw, "gate_traces.status")?;
        traces_by_status.insert(
            status.as_str().to_owned(),
            to_usize(count_raw, "traces_by_status")?,
        );
    }

    let open_blackboard_items: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM blackboard WHERE status = 'open'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| map_sql_err(err, "observability_snapshot(open_blackboard_items)"))?;

    Ok(ObservabilitySnapshot {
        active_runs: to_usize(active_runs, "active_runs")?,
        completed_runs: to_usize(completed_runs, "completed_runs")?,
        cleared_runs: to_usize(cleared_runs, "cleared_runs")?,
        nodes_by_status,
        active_claims: to_usize(active_claims, "active_claims")?,
        traces_by_status,
        open_blackboard_items: to_usize(open_blackboard_items, "open_blackboard_items")?,
    })
}
