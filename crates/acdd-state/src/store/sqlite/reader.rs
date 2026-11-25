//! Read-only SQLite queries and strict domain-newtype row decoders (`StateReader`).

use crate::types::{
    ArchiveCleanupEntry, ArtifactKind, BlackboardListReq, BlackboardPage, BlackboardRecord,
    CheckResultRecord, GateArtifactRecord, GateClaimRecord, GateNodeRecord, GateTraceRecord,
    ListRunsPage, ListRunsReq, NodeStatus, ObservabilitySnapshot, RunGraphSnapshot, RunRecord,
    RunTombstoneRecord, SubtaskRecord, TaskDependencyRecord, TraceStatus,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::same_filesystem_path;
use acdd_core::ids::{CommitOid, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Row,
};
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

fn col<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    idx: usize,
    field: &str,
) -> Result<T, StateError> {
    row.get(idx).map_err(|err| map_sql_err(err, field))
}

fn col_enum<T: FromStr<Err = StateError>>(
    row: &Row<'_>,
    idx: usize,
    field: &str,
) -> Result<T, StateError> {
    parse_enum(&col::<String>(row, idx, field)?, field)
}

fn col_id<T>(
    row: &Row<'_>,
    idx: usize,
    ctor: impl FnOnce(String) -> Result<T, acdd_core::ids::IdError>,
    field: &str,
) -> Result<T, StateError> {
    parse_id(col(row, idx, field)?, ctor, field)
}

fn col_opt_id<T>(
    row: &Row<'_>,
    idx: usize,
    ctor: impl FnOnce(String) -> Result<T, acdd_core::ids::IdError>,
    field: &str,
) -> Result<Option<T>, StateError> {
    parse_opt_id(col(row, idx, field)?, ctor, field)
}

fn col_u32(row: &Row<'_>, idx: usize, field: &str) -> Result<u32, StateError> {
    to_u32(col(row, idx, field)?, field)
}

fn col_u64(row: &Row<'_>, idx: usize, field: &str) -> Result<u64, StateError> {
    to_u64(col(row, idx, field)?, field)
}

fn col_bool(row: &Row<'_>, idx: usize, field: &str) -> Result<bool, StateError> {
    to_bool_flag(col(row, idx, field)?, field)
}

fn col_json<T: DeserializeOwned>(row: &Row<'_>, idx: usize, field: &str) -> Result<T, StateError> {
    parse_json_col(&col::<String>(row, idx, field)?, field)
}

fn decode_run_row(row: &Row<'_>) -> Result<RunRecord, StateError> {
    Ok(RunRecord {
        run_id: col_id(row, 0, RunId::new, "runs.run_id")?,
        run_name: col_id(row, 1, RunName::new, "runs.run_name")?,
        collection: col(row, 2, "runs.collection")?,
        workspace: col(row, 3, "runs.workspace")?,
        profile_id: col(row, 4, "runs.profile_id")?,
        profile_json: col(row, 5, "runs.profile_json")?,
        spec_path: col(row, 6, "runs.spec_path")?,
        spec_id: col(row, 7, "runs.spec_id")?,
        spec_digest: col(row, 8, "runs.spec_digest")?,
        is_ephemeral: col_bool(row, 9, "runs.is_ephemeral")?,
        isolation: col_enum(row, 10, "runs.isolation")?,
        target_base: col(row, 11, "runs.target_base")?,
        target_base_commit: col_opt_id(row, 12, CommitOid::new, "runs.target_base_commit")?,
        branch: col(row, 13, "runs.branch")?,
        worktree: col(row, 14, "runs.worktree")?,
        trace_id: col(row, 15, "runs.trace_id")?,
        status: col_enum(row, 16, "runs.status")?,
        cancellation_reason: col(row, 17, "runs.cancellation_reason")?,
        root_cycle_count: col_u32(row, 18, "runs.root_cycle_count")?,
        generation: col_u64(row, 19, "runs.generation")?,
        created_at: col(row, 20, "runs.created_at")?,
        completed_at: col(row, 21, "runs.completed_at")?,
    })
}

fn decode_node_row(row: &Row<'_>) -> Result<GateNodeRecord, StateError> {
    Ok(GateNodeRecord {
        run_id: col_id(row, 0, RunId::new, "gate_nodes.run_id")?,
        node_id: col(row, 1, "gate_nodes.node_id")?,
        parent_id: col(row, 2, "gate_nodes.parent_id")?,
        kind: col_enum(row, 3, "gate_nodes.kind")?,
        task_ref: col_opt_id(row, 4, TaskRef::new, "gate_nodes.task_ref")?,
        gate_id: col(row, 5, "gate_nodes.gate_id")?,
        seq_order: col_u32(row, 6, "gate_nodes.seq_order")?,
        status: col_enum(row, 7, "gate_nodes.status")?,
        node_version: col_u64(row, 8, "gate_nodes.node_version")?,
        isolation: col_enum(row, 9, "gate_nodes.isolation")?,
        attempt: col_u32(row, 10, "gate_nodes.attempt")?,
        cycle_count: col_u32(row, 11, "gate_nodes.cycle_count")?,
        max_attempts: col_u32(row, 12, "gate_nodes.max_attempts")?,
        claim_revision: col_u32(row, 13, "gate_nodes.claim_revision")?,
        contract_revision: col_u32(row, 14, "gate_nodes.contract_revision")?,
        worker_id: col_opt_id(row, 15, WorkerId::new, "gate_nodes.worker_id")?,
        session_fingerprint: col(row, 16, "gate_nodes.session_fingerprint")?,
        branch: col(row, 17, "gate_nodes.branch")?,
        worktree: col(row, 18, "gate_nodes.worktree")?,
        baseline_commit: col_opt_id(row, 19, CommitOid::new, "gate_nodes.baseline_commit")?,
        claim_tree: col_opt_id(row, 20, TreeOid::new, "gate_nodes.claim_tree")?,
        accepted_tree: col_opt_id(row, 21, TreeOid::new, "gate_nodes.accepted_tree")?,
        accepted_definition_digest: col(row, 22, "gate_nodes.accepted_definition_digest")?,
        tags: col_json(row, 23, "gate_nodes.tags_json")?,
        invariants: col_json(row, 24, "gate_nodes.invariants_json")?,
        scope: col_json(row, 25, "gate_nodes.scope_json")?,
        extended_scope: col_json(row, 26, "gate_nodes.extended_scope_json")?,
        active_span_id: col(row, 27, "gate_nodes.active_span_id")?,
    })
}

fn decode_dependency_row(row: &Row<'_>) -> Result<TaskDependencyRecord, StateError> {
    let depends_on_ref_raw: String = col(row, 4, "task_dependencies.depends_on_ref")?;
    Ok(TaskDependencyRecord {
        run_id: col_id(row, 0, RunId::new, "task_dependencies.run_id")?,
        task_ref: col_id(row, 1, TaskRef::new, "task_dependencies.task_ref")?,
        depends_on_workspace: col(row, 2, "task_dependencies.depends_on_workspace")?,
        depends_on_run_id: col_id(row, 3, RunId::new, "task_dependencies.depends_on_run_id")?,
        depends_on_ref: if depends_on_ref_raw == TaskRef::WHOLE_SPEC {
            TaskRef::whole_spec()
        } else {
            parse_id(
                depends_on_ref_raw,
                TaskRef::new,
                "task_dependencies.depends_on_ref",
            )?
        },
        satisfied: col_bool(row, 5, "task_dependencies.satisfied")?,
    })
}

fn decode_subtask_row(row: &Row<'_>) -> Result<SubtaskRecord, StateError> {
    Ok(SubtaskRecord {
        run_id: col_id(row, 0, RunId::new, "subtasks.run_id")?,
        node_id: col(row, 1, "subtasks.node_id")?,
        subtask_ref: col_id(row, 2, SubtaskRef::new, "subtasks.subtask_ref")?,
        seq_order: col_u32(row, 3, "subtasks.seq_order")?,
        title: col(row, 4, "subtasks.title")?,
        status: col_enum(row, 5, "subtasks.status")?,
        evidence: col(row, 6, "subtasks.evidence")?,
        updated_at: col(row, 7, "subtasks.updated_at")?,
    })
}

fn decode_claim_row(row: &Row<'_>) -> Result<GateClaimRecord, StateError> {
    Ok(GateClaimRecord {
        claim_id: Some(col(row, 0, "gate_claims.claim_id")?),
        run_id: col_id(row, 1, RunId::new, "gate_claims.run_id")?,
        node_id: col(row, 2, "gate_claims.node_id")?,
        claim_revision: col_u32(row, 3, "gate_claims.claim_revision")?,
        worker_id: col_id(row, 4, WorkerId::new, "gate_claims.worker_id")?,
        session_fingerprint: col(row, 5, "gate_claims.session_fingerprint")?,
        worktree: col(row, 6, "gate_claims.worktree")?,
        span_id: col(row, 7, "gate_claims.span_id")?,
        claim_tree: col_id(row, 8, TreeOid::new, "gate_claims.claim_tree")?,
        claimed_at: col(row, 9, "gate_claims.claimed_at")?,
        ended_at: col(row, 10, "gate_claims.ended_at")?,
        outcome: col_enum(row, 11, "gate_claims.outcome")?,
    })
}

fn decode_trace_row(row: &Row<'_>) -> Result<GateTraceRecord, StateError> {
    Ok(GateTraceRecord {
        trace_row_id: Some(col(row, 0, "gate_traces.trace_row_id")?),
        run_id: col_id(row, 1, RunId::new, "gate_traces.run_id")?,
        node_id: col(row, 2, "gate_traces.node_id")?,
        claim_revision: col_u32(row, 3, "gate_traces.claim_revision")?,
        contract_revision: col_u32(row, 4, "gate_traces.contract_revision")?,
        execution_digest: col(row, 5, "gate_traces.execution_digest")?,
        idempotency_key: col(row, 6, "gate_traces.idempotency_key")?,
        command_id: col(row, 7, "gate_traces.command_id")?,
        command_line: col(row, 8, "gate_traces.command_line")?,
        action_id: col(row, 9, "gate_traces.action_id")?,
        test_target: col(row, 10, "gate_traces.test_target")?,
        status: col_enum(row, 11, "gate_traces.status")?,
        exit_code: col(row, 12, "gate_traces.exit_code")?,
        passed_tests: col_json(row, 13, "gate_traces.passed_tests_json")?,
        failed_tests: col_json(row, 14, "gate_traces.failed_tests_json")?,
        check_results: col_json::<Vec<CheckResultRecord>>(
            row,
            15,
            "gate_traces.check_results_json",
        )?,
        candidate_tree_before: col_id(row, 16, TreeOid::new, "gate_traces.candidate_tree_before")?,
        candidate_tree_after: col_opt_id(
            row,
            17,
            TreeOid::new,
            "gate_traces.candidate_tree_after",
        )?,
        log_relpath: col(row, 18, "gate_traces.log_relpath")?,
        started_at: col(row, 19, "gate_traces.started_at")?,
        finished_at: col(row, 20, "gate_traces.finished_at")?,
    })
}

fn decode_artifact_row(row: &Row<'_>) -> Result<GateArtifactRecord, StateError> {
    Ok(GateArtifactRecord {
        run_id: col_id(row, 0, RunId::new, "gate_artifacts.run_id")?,
        node_id: col(row, 1, "gate_artifacts.node_id")?,
        claim_revision: col_u32(row, 2, "gate_artifacts.claim_revision")?,
        kind: col_enum(row, 3, "gate_artifacts.kind")?,
        payload_json: col(row, 4, "gate_artifacts.payload_json")?,
        snapshot_commit: col_opt_id(row, 5, CommitOid::new, "gate_artifacts.snapshot_commit")?,
        created_at: col(row, 6, "gate_artifacts.created_at")?,
    })
}

fn decode_blackboard_row(row: &Row<'_>) -> Result<BlackboardRecord, StateError> {
    Ok(BlackboardRecord {
        id: Some(col(row, 0, "blackboard.id")?),
        run_id: col_id(row, 1, RunId::new, "blackboard.run_id")?,
        node_id: col(row, 2, "blackboard.node_id")?,
        recipient_node_id: col(row, 3, "blackboard.recipient_node_id")?,
        subtask_ref: col_opt_id(row, 4, SubtaskRef::new, "blackboard.subtask_ref")?,
        gate: col(row, 5, "blackboard.gate")?,
        author: col(row, 6, "blackboard.author")?,
        topic: col_enum(row, 7, "blackboard.topic")?,
        status: col_enum(row, 8, "blackboard.status")?,
        payload: col(row, 9, "blackboard.payload")?,
        created_at: col(row, 10, "blackboard.created_at")?,
    })
}

fn decode_tombstone_row(row: &Row<'_>) -> Result<RunTombstoneRecord, StateError> {
    Ok(RunTombstoneRecord {
        run_id: col_id(row, 0, RunId::new, "run_tombstones.run_id")?,
        run_name: col_id(row, 1, RunName::new, "run_tombstones.run_name")?,
        collection: col(row, 2, "run_tombstones.collection")?,
        workspace: col(row, 3, "run_tombstones.workspace")?,
        spec_id: col(row, 4, "run_tombstones.spec_id")?,
        title: col(row, 5, "run_tombstones.title")?,
        profile_id: col(row, 6, "run_tombstones.profile_id")?,
        is_ephemeral: col_bool(row, 7, "run_tombstones.is_ephemeral")?,
        generation: col_u64(row, 8, "run_tombstones.generation")?,
        terminal_status: col_enum(row, 9, "run_tombstones.terminal_status")?,
        target_base: col(row, 10, "run_tombstones.target_base")?,
        target_base_commit: col_opt_id(
            row,
            11,
            CommitOid::new,
            "run_tombstones.target_base_commit",
        )?,
        archive_path: col(row, 12, "run_tombstones.archive_path")?,
        closed_commit: col_opt_id(row, 13, CommitOid::new, "run_tombstones.closed_commit")?,
        cleanup_pending: col_bool(row, 14, "run_tombstones.cleanup_pending")?,
        cleanup_manifest: col_json::<Vec<ArchiveCleanupEntry>>(
            row,
            15,
            "run_tombstones.cleanup_manifest_json",
        )?,
        summary_json: col(row, 16, "run_tombstones.summary_json")?,
        metrics_json: col(row, 17, "run_tombstones.metrics_json")?,
        closed_at: col(row, 18, "run_tombstones.closed_at")?,
    })
}

fn query_opt_decoded<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    decoder: fn(&Row<'_>) -> Result<T, StateError>,
    context: &str,
) -> Result<Option<T>, StateError> {
    conn.query_row(sql, params, |row| Ok(decoder(row)))
        .optional()
        .map_err(|err| map_sql_err(err, context))?
        .transpose()
}

fn collect_rows<T>(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
    decoder: fn(&Row<'_>) -> Result<T, StateError>,
    context: &str,
) -> Result<Vec<T>, StateError> {
    let mapped = stmt
        .query_map(params, |row| Ok(decoder(row)))
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
    let run_opt = query_opt_decoded(
        conn,
        &run_sql,
        params![run_id.as_str()],
        decode_run_row,
        "load_run_graph(runs)",
    )?;
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

fn query_paginated_table<T>(
    conn: &Connection,
    (table, cols_sql, order_by): (&str, &str, &str),
    where_sql: &str,
    bind_values: Vec<SqlValue>,
    (limit, offset): (usize, usize),
    decode: fn(&Row<'_>) -> Result<T, StateError>,
) -> Result<(Vec<T>, usize), StateError> {
    let count_sql = format!("SELECT COUNT(*) FROM {table}{where_sql}");
    let total_raw: i64 = conn
        .query_row(&count_sql, params_from_iter(bind_values.iter()), |row| {
            row.get(0)
        })
        .map_err(|err| map_sql_err(err, &format!("list_{table}(count)")))?;
    let total = to_usize(total_raw, &format!("{table}_total"))?;

    let mut page_params = bind_values;
    page_params.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(100)));
    page_params.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));

    let page_sql =
        format!("SELECT {cols_sql} FROM {table}{where_sql} ORDER BY {order_by} LIMIT ? OFFSET ?");
    let mut stmt = conn
        .prepare(&page_sql)
        .map_err(|err| map_sql_err(err, &format!("prepare list_{table}")))?;
    let rows = collect_rows(
        &mut stmt,
        params_from_iter(page_params.iter()),
        decode,
        &format!("list_{table}(rows)"),
    )?;
    Ok((rows, total))
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

    let run_cols_sql = run_cols(schema_version);
    let (runs, total_runs) = query_paginated_table(
        conn,
        ("runs", run_cols_sql, "created_at DESC, run_id ASC"),
        &where_sql,
        bind_values,
        (limit, offset),
        decode_run_row,
    )?;

    let (tombstones, total_tombstones) = if req.include_tombstones {
        let (tomb_where, tomb_values) = if let Some(ws) = &req.workspace {
            (" WHERE workspace = ?", vec![SqlValue::Text(ws.clone())])
        } else {
            ("", Vec::new())
        };
        query_paginated_table(
            conn,
            (
                "run_tombstones",
                TOMBSTONE_COLS,
                "closed_at DESC, run_id ASC",
            ),
            tomb_where,
            tomb_values,
            (limit, offset),
            decode_tombstone_row,
        )?
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
    let exact_run_sql =
        format!("SELECT {run_cols_sql} FROM runs WHERE status = 'active' AND worktree = ?1");
    if let Some(run) = query_opt_decoded(
        conn,
        &exact_run_sql,
        params![worktree],
        decode_run_row,
        "resolve_active_run_by_worktree(runs)",
    )? {
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
        let (rid_raw, wt) =
            row.map_err(|err| map_sql_err(err, "resolve_active_run_by_worktree(node_wt row)"))?;
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
            Ok((
                col_id(row, 0, RunId::new, "gate_nodes.run_id")?,
                col(row, 1, "gate_nodes.node_id")?,
                col_enum(row, 2, "gate_nodes.status")?,
            ))
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
    query_opt_decoded(
        conn,
        &sql,
        params![run_id.as_str(), node_id],
        decode_node_row,
        "load_node",
    )
}

pub(crate) fn latest_claim(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
) -> Result<Option<GateClaimRecord>, StateError> {
    query_opt_decoded(
        conn,
        "SELECT claim_id, run_id, node_id, claim_revision, worker_id, session_fingerprint,
                worktree, span_id, claim_tree, claimed_at, ended_at, outcome
         FROM gate_claims
         WHERE run_id = ?1 AND node_id = ?2
         ORDER BY claim_revision DESC
         LIMIT 1",
        params![run_id.as_str(), node_id],
        decode_claim_row,
        "latest_claim",
    )
}

pub(crate) fn latest_trace(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
    claim_revision: u32,
) -> Result<Option<GateTraceRecord>, StateError> {
    query_opt_decoded(
        conn,
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
        "latest_trace",
    )
}

pub(crate) fn load_artifact(
    conn: &Connection,
    run_id: &RunId,
    node_id: &str,
    claim_revision: u32,
    kind: ArtifactKind,
) -> Result<Option<GateArtifactRecord>, StateError> {
    query_opt_decoded(
        conn,
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
        "load_artifact",
    )
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
    query_opt_decoded(
        conn,
        &sql,
        params![run_id.as_str()],
        decode_tombstone_row,
        "load_tombstone",
    )
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
        let (run_name, spec_path) =
            row.map_err(|err| map_sql_err(err, "max_spec_number(runs row)"))?;
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
        for candidate in [Some(run_name), spec_id, archive_path]
            .into_iter()
            .flatten()
        {
            if let Some(num) = extract_spec_number(&candidate, prefix) {
                best = Some(best.map_or(num, |cur| cur.max(num)));
            }
        }
    }

    Ok(best)
}

pub(crate) fn observability_snapshot(
    conn: &Connection,
) -> Result<ObservabilitySnapshot, StateError> {
    with_read_tx(conn, "begin observability_snapshot", || {
        observability_snapshot_inner(conn)
    })
}

fn observability_snapshot_inner(conn: &Connection) -> Result<ObservabilitySnapshot, StateError> {
    let active_runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE status = 'active'",
            [],
            |row| row.get(0),
        )
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
