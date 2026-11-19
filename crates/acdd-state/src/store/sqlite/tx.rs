//! Transactional `StateTx` implementation with strict CAS `StoragePreconditions` enforcement.

use crate::machine::SyncPlan;
use crate::store::traits::StateTx;
use crate::types::{
    BlackboardRecord, BlackboardStatusPatch, ClaimInsertInput, ClaimOutcome, GateArtifactRecord,
    GateClaimRecord, GateNodeRecord, GateTraceRecord, NodeStatus, NodeStatusPatch,
    RunGraphSnapshot, RunTombstoneRecord, StoragePreconditions, SubtaskRecord, TraceFinishInput,
    TraceStartInput, TraceStatus, TransitionPlan,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::RunId;
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};

/// Active SQLite write transaction implementing `StateTx`.
pub(crate) struct SqliteTx<'conn> {
    conn: &'conn Transaction<'conn>,
    significant_dirty: bool,
}

impl<'conn> SqliteTx<'conn> {
    pub(crate) fn new(conn: &'conn Transaction<'conn>) -> Self {
        Self {
            conn,
            significant_dirty: false,
        }
    }

    pub(crate) fn is_significant_dirty(&self) -> bool {
        self.significant_dirty
    }
}

fn map_write_err(err: rusqlite::Error, context: &str) -> StateError {
    if let rusqlite::Error::SqliteFailure(sqlite_err, _) = &err {
        match sqlite_err.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                return StateError::new(
                    ErrorCode::BackendConflict,
                    format!("{context}: SQLite busy/locked: {err}"),
                );
            }
            rusqlite::ErrorCode::ConstraintViolation => {
                // Partial unique indexes on active claims / running traces / active worktrees
                // represent concurrency or state-invariant conflicts.
                let msg = err.to_string();
                if msg.contains("uq_gate_claims_one_active")
                    || msg.contains("uq_gate_traces_one_running")
                    || msg.contains("uq_runs_active_worktree")
                    || msg.contains("UNIQUE constraint failed")
                {
                    return StateError::new(
                        ErrorCode::BackendConflict,
                        format!("{context}: uniqueness conflict: {err}"),
                    );
                }
                return StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{context}: constraint violation: {err}"),
                );
            }
            _ => {}
        }
    }
    StateError::new(ErrorCode::ProfileSchemaInvalid, format!("{context}: {err}"))
}

fn serialize_json<T: serde::Serialize + ?Sized>(val: &T, field: &str) -> Result<String, StateError> {
    serde_json::to_string(val).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize {field}: {err}"),
        )
    })
}

fn bump_write_seq(conn: &Transaction<'_>) -> Result<(), StateError> {
    let changed = conn
        .execute(
            "UPDATE _sqlite_meta SET write_seq = write_seq + 1 WHERE id = 1",
            [],
        )
        .map_err(|err| map_write_err(err, "bump write_seq"))?;
    if changed != 1 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "_sqlite_meta metadata row missing during write_seq bump",
        ));
    }
    Ok(())
}

fn check_run_generation(
    conn: &Transaction<'_>,
    run_id: &RunId,
    expected_generation: u64,
) -> Result<(), StateError> {
    let expected_i64 = i64::try_from(expected_generation).map_err(|_| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("expected_run_generation exceeds i64::MAX: {expected_generation}"),
        )
    })?;
    let observed: Option<i64> = conn
        .query_row(
            "SELECT generation FROM runs WHERE run_id = ?1",
            params![run_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| map_write_err(err, "check runs.generation"))?;
    match observed {
        Some(actual) if actual == expected_i64 => Ok(()),
        Some(actual) => Err(StateError::new(
            ErrorCode::BackendConflict,
            format!(
                "run '{}' generation mismatch: expected {}, observed {}",
                run_id, expected_generation, actual
            ),
        )),
        None => Err(StateError::new(
            ErrorCode::BackendConflict,
            format!("run '{}' does not exist", run_id),
        )),
    }
}

fn check_node_preconditions(
    conn: &Transaction<'_>,
    run_id: &RunId,
    preconditions: &StoragePreconditions,
    primary_node_id: Option<&str>,
    skip_version_check_for: &HashSet<&str>,
) -> Result<(), StateError> {
    for exp in &preconditions.expected_node_versions {
        if skip_version_check_for.contains(exp.node_id.as_str()) {
            continue;
        }
        let expected_ver = i64::try_from(exp.expected_version).map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("expected_version overflow for node '{}'", exp.node_id),
            )
        })?;
        let observed: Option<i64> = conn
            .query_row(
                "SELECT node_version FROM gate_nodes WHERE run_id = ?1 AND node_id = ?2",
                params![run_id.as_str(), exp.node_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| map_write_err(err, "check node_version"))?;
        match observed {
            Some(actual) if actual == expected_ver => {}
            Some(actual) => {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "node '{}/{}' version conflict: expected {}, observed {}",
                        run_id, exp.node_id, expected_ver, actual
                    ),
                ));
            }
            None => {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!("node '{}/{}' does not exist", run_id, exp.node_id),
                ));
            }
        }
    }

    if preconditions.expected_claim_revision.is_some()
        || preconditions.expected_contract_revision.is_some()
        || preconditions.expected_worker_id.is_some()
    {
        let target_node = primary_node_id
            .or_else(|| {
                preconditions
                    .expected_node_versions
                    .first()
                    .map(|e| e.node_id.as_str())
            })
            .ok_or_else(|| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    "StoragePreconditions specifies claim/contract/worker check without a target node",
                )
            })?;

        let row: Option<(i64, i64, Option<String>)> = conn
            .query_row(
                "SELECT claim_revision, contract_revision, worker_id
                 FROM gate_nodes
                 WHERE run_id = ?1 AND node_id = ?2",
                params![run_id.as_str(), target_node],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|err| map_write_err(err, "check claim/contract/worker preconditions"))?;

        let Some((actual_claim_rev, actual_contract_rev, actual_worker_id)) = row else {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("node '{run_id}/{target_node}' does not exist"),
            ));
        };

        if let Some(expected_claim_rev) = preconditions.expected_claim_revision {
            if actual_claim_rev != i64::from(expected_claim_rev) {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "node '{run_id}/{target_node}' claim_revision mismatch: expected {expected_claim_rev}, observed {actual_claim_rev}"
                    ),
                ));
            }
        }
        if let Some(expected_contract_rev) = preconditions.expected_contract_revision {
            if actual_contract_rev != i64::from(expected_contract_rev) {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "node '{run_id}/{target_node}' contract_revision mismatch: expected {expected_contract_rev}, observed {actual_contract_rev}"
                    ),
                ));
            }
        }
        if let Some(ref expected_worker) = preconditions.expected_worker_id {
            if actual_worker_id.as_deref() != Some(expected_worker.as_str()) {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "node '{run_id}/{target_node}' worker_id mismatch: expected '{}', observed {:?}",
                        expected_worker, actual_worker_id
                    ),
                ));
            }
        }
    }

    for pred in &preconditions.required_predecessor_trees {
        let accepted_tree: Option<Option<String>> = conn
            .query_row(
                "SELECT accepted_tree FROM gate_nodes WHERE run_id = ?1 AND node_id = ?2",
                params![run_id.as_str(), pred.node_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|err| map_write_err(err, "check required_predecessor_trees"))?;
        match accepted_tree {
            Some(Some(actual)) if actual == pred.expected_accepted_tree.as_str() => {}
            _ => {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "predecessor '{}/{}' accepted_tree drifted from expected '{}'",
                        run_id, pred.node_id, pred.expected_accepted_tree
                    ),
                ));
            }
        }
    }

    Ok(())
}

fn insert_gate_node(conn: &Transaction<'_>, node: &GateNodeRecord) -> Result<(), StateError> {
    let tags_json = serialize_json(&node.tags, "gate_nodes.tags_json")?;
    let invariants_json = serialize_json(&node.invariants, "gate_nodes.invariants_json")?;
    let scope_json = serialize_json(&node.scope, "gate_nodes.scope_json")?;
    let extended_scope_json =
        serialize_json(&node.extended_scope, "gate_nodes.extended_scope_json")?;
    let node_version_i64 = i64::try_from(node.node_version).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("node_version exceeds i64::MAX: {}", node.node_version),
        )
    })?;

    conn.execute(
        "INSERT INTO gate_nodes (
            run_id, node_id, parent_id, kind, task_ref, gate_id, seq_order, status,
            node_version, isolation, attempt, cycle_count, max_attempts, claim_revision,
            contract_revision, worker_id, session_fingerprint, branch, worktree,
            baseline_commit, claim_tree, accepted_tree, accepted_definition_digest,
            tags_json, invariants_json, scope_json, extended_scope_json, active_span_id
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19,
            ?20, ?21, ?22, ?23,
            ?24, ?25, ?26, ?27, ?28
        )",
        params![
            node.run_id.as_str(),
            node.node_id,
            node.parent_id,
            node.kind.as_str(),
            node.task_ref.as_ref().map(|t| t.as_str()),
            node.gate_id,
            i64::from(node.seq_order),
            node.status.as_str(),
            node_version_i64,
            node.isolation.as_str(),
            i64::from(node.attempt),
            i64::from(node.cycle_count),
            i64::from(node.max_attempts),
            i64::from(node.claim_revision),
            i64::from(node.contract_revision),
            node.worker_id.as_ref().map(|w| w.as_str()),
            node.session_fingerprint,
            node.branch,
            node.worktree,
            node.baseline_commit.as_ref().map(|c| c.as_str()),
            node.claim_tree.as_ref().map(|t| t.as_str()),
            node.accepted_tree.as_ref().map(|t| t.as_str()),
            node.accepted_definition_digest,
            tags_json,
            invariants_json,
            scope_json,
            extended_scope_json,
            node.active_span_id,
        ],
    )
    .map_err(|err| map_write_err(err, "insert gate_nodes"))?;
    Ok(())
}

fn order_nodes_parent_first(nodes: &[GateNodeRecord]) -> Result<Vec<&GateNodeRecord>, StateError> {
    let empty = HashSet::new();
    order_nodes_parent_first_with_existing(nodes, &empty)
}

fn order_nodes_parent_first_with_existing<'a>(
    nodes: &'a [GateNodeRecord],
    existing_parents: &HashSet<String>,
) -> Result<Vec<&'a GateNodeRecord>, StateError> {
    let mut inserted: HashSet<&str> = existing_parents.iter().map(String::as_str).collect();
    inserted.reserve(nodes.len());
    let mut remaining: Vec<&GateNodeRecord> = nodes.iter().collect();
    let mut ordered = Vec::with_capacity(nodes.len());

    while !remaining.is_empty() {
        let mut next_remaining = Vec::new();
        let mut progressed = false;
        for node in remaining {
            let ready = match node.parent_id.as_deref() {
                None => true,
                Some(pid) => inserted.contains(pid),
            };
            if ready {
                inserted.insert(node.node_id.as_str());
                ordered.push(node);
                progressed = true;
            } else {
                next_remaining.push(node);
            }
        }
        if !progressed {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "gate_nodes parent_id graph contains a cycle or missing parent",
            ));
        }
        remaining = next_remaining;
    }

    Ok(ordered)
}

fn insert_artifact(conn: &Transaction<'_>, art: &GateArtifactRecord) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO gate_artifacts (
            run_id, node_id, claim_revision, kind, payload_json, snapshot_ref, snapshot_commit, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(run_id, node_id, claim_revision, kind) DO UPDATE SET
            payload_json = excluded.payload_json,
            snapshot_ref = excluded.snapshot_ref,
            snapshot_commit = excluded.snapshot_commit,
            created_at = excluded.created_at",
        params![
            art.run_id.as_str(),
            art.node_id,
            i64::from(art.claim_revision),
            art.kind.as_str(),
            art.payload_json,
            art.snapshot_ref,
            art.snapshot_commit.as_ref().map(|c| c.as_str()),
            art.created_at,
        ],
    )
    .map_err(|err| map_write_err(err, "insert gate_artifacts"))?;
    Ok(())
}

fn upsert_subtask_row(conn: &Transaction<'_>, subtask: &SubtaskRecord) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO subtasks (
            run_id, node_id, subtask_ref, seq_order, title, status, evidence, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(run_id, node_id, subtask_ref) DO UPDATE SET
            seq_order = excluded.seq_order,
            title = excluded.title,
            status = excluded.status,
            evidence = excluded.evidence,
            updated_at = excluded.updated_at",
        params![
            subtask.run_id.as_str(),
            subtask.node_id,
            subtask.subtask_ref.as_str(),
            i64::from(subtask.seq_order),
            subtask.title,
            subtask.status.as_str(),
            subtask.evidence,
            subtask.updated_at,
        ],
    )
    .map_err(|err| map_write_err(err, "upsert subtasks"))?;
    Ok(())
}

fn insert_blackboard_row(
    conn: &Transaction<'_>,
    item: &BlackboardRecord,
) -> Result<BlackboardRecord, StateError> {
    conn.execute(
        "INSERT INTO blackboard (
            run_id, node_id, recipient_node_id, subtask_ref, gate, author, topic, status, payload, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            item.run_id.as_str(),
            item.node_id,
            item.recipient_node_id,
            item.subtask_ref.as_ref().map(|s| s.as_str()),
            item.gate,
            item.author,
            item.topic.as_str(),
            item.status.as_str(),
            item.payload,
            item.created_at,
        ],
    )
    .map_err(|err| map_write_err(err, "insert blackboard"))?;
    let id = conn.last_insert_rowid();
    let mut stored = item.clone();
    stored.id = Some(id);
    Ok(stored)
}

fn apply_node_patch(
    conn: &Transaction<'_>,
    run_id: &RunId,
    patch: &NodeStatusPatch,
    expected_version: Option<u64>,
) -> Result<(), StateError> {
    let mut sets: Vec<&'static str> = vec!["status = ?", "node_version = node_version + 1"];
    let mut values: Vec<rusqlite::types::Value> = vec![patch.new_status.as_str().to_owned().into()];

    if let Some(attempt) = patch.attempt {
        sets.push("attempt = ?");
        values.push(i64::from(attempt).into());
    }
    if let Some(cycle_count) = patch.cycle_count {
        sets.push("cycle_count = ?");
        values.push(i64::from(cycle_count).into());
    }
    if let Some(claim_rev) = patch.claim_revision {
        sets.push("claim_revision = ?");
        values.push(i64::from(claim_rev).into());
    }
    if let Some(contract_rev) = patch.contract_revision {
        sets.push("contract_revision = ?");
        values.push(i64::from(contract_rev).into());
    }
    if let Some(ref opt_worker) = patch.worker_id {
        sets.push("worker_id = ?");
        values.push(opt_worker.as_ref().map(|w| w.as_str().to_owned()).into());
    }
    if let Some(ref opt_fp) = patch.session_fingerprint {
        sets.push("session_fingerprint = ?");
        values.push(opt_fp.clone().into());
    }
    if let Some(ref opt_branch) = patch.branch {
        sets.push("branch = ?");
        values.push(opt_branch.clone().into());
    }
    if let Some(ref opt_wt) = patch.worktree {
        sets.push("worktree = ?");
        values.push(opt_wt.clone().into());
    }
    if let Some(ref opt_base) = patch.baseline_commit {
        sets.push("baseline_commit = ?");
        values.push(opt_base.as_ref().map(|c| c.as_str().to_owned()).into());
    }
    if let Some(ref opt_claim_tree) = patch.claim_tree {
        sets.push("claim_tree = ?");
        values.push(opt_claim_tree.as_ref().map(|t| t.as_str().to_owned()).into());
    }
    if let Some(ref opt_accepted_tree) = patch.accepted_tree {
        sets.push("accepted_tree = ?");
        values.push(opt_accepted_tree.as_ref().map(|t| t.as_str().to_owned()).into());
    }
    if let Some(ref opt_digest) = patch.accepted_definition_digest {
        sets.push("accepted_definition_digest = ?");
        values.push(opt_digest.clone().into());
    }
    if let Some(ref tags) = patch.tags {
        sets.push("tags_json = ?");
        values.push(serialize_json(tags, "gate_nodes.tags_json")?.into());
    }
    if let Some(ref inv) = patch.invariants {
        sets.push("invariants_json = ?");
        values.push(serialize_json(inv, "gate_nodes.invariants_json")?.into());
    }
    if let Some(ref scope) = patch.scope {
        sets.push("scope_json = ?");
        values.push(serialize_json(scope, "gate_nodes.scope_json")?.into());
    }
    if let Some(ref ext_scope) = patch.extended_scope {
        sets.push("extended_scope_json = ?");
        values.push(serialize_json(ext_scope, "gate_nodes.extended_scope_json")?.into());
    }
    if let Some(ref opt_span) = patch.active_span_id {
        sets.push("active_span_id = ?");
        values.push(opt_span.clone().into());
    }

    let mut sql = format!(
        "UPDATE gate_nodes SET {} WHERE run_id = ? AND node_id = ?",
        sets.join(", ")
    );
    values.push(run_id.as_str().to_owned().into());
    values.push(patch.node_id.clone().into());

    if let Some(ver) = expected_version {
        let ver_i64 = i64::try_from(ver).map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("expected_version overflow for node '{}'", patch.node_id),
            )
        })?;
        sql.push_str(" AND node_version = ?");
        values.push(ver_i64.into());
    }

    let changed = conn
        .execute(&sql, rusqlite::params_from_iter(values.iter()))
        .map_err(|err| map_write_err(err, "apply_node_patch"))?;
    if changed != 1 {
        return Err(StateError::new(
            ErrorCode::BackendConflict,
            format!(
                "CAS conflict updating gate_nodes '{}/{}'",
                run_id, patch.node_id
            ),
        ));
    }
    Ok(())
}

impl StateTx for SqliteTx<'_> {
    fn insert_run_graph(&mut self, graph: &RunGraphSnapshot) -> Result<(), StateError> {
        let run = &graph.run;
        let run_gen = i64::try_from(run.generation).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("run.generation exceeds i64::MAX: {}", run.generation),
            )
        })?;

        self.conn
            .execute(
                "INSERT INTO runs (
                    run_id, run_name, collection, workspace, profile_id, profile_json,
                    spec_path, spec_id, spec_digest, is_ephemeral, isolation, target_base,
                    target_base_commit, branch, worktree, trace_id, status,
                    cancellation_reason, root_cycle_count, generation, created_at, completed_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6,
                    ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17,
                    ?18, ?19, ?20, ?21, ?22
                )",
                params![
                    run.run_id.as_str(),
                    run.run_name.as_str(),
                    run.collection,
                    run.workspace,
                    run.profile_id,
                    run.profile_json,
                    run.spec_path,
                    run.spec_id,
                    run.spec_digest,
                    i64::from(run.is_ephemeral),
                    run.isolation.as_str(),
                    run.target_base,
                    run.target_base_commit.as_ref().map(|c| c.as_str()),
                    run.branch,
                    run.worktree,
                    run.trace_id,
                    run.status.as_str(),
                    run.cancellation_reason,
                    i64::from(run.root_cycle_count),
                    run_gen,
                    run.created_at,
                    run.completed_at,
                ],
            )
            .map_err(|err| map_write_err(err, "insert runs"))?;

        for node in order_nodes_parent_first(&graph.nodes)? {
            insert_gate_node(self.conn, node)?;
        }

        for dep in &graph.dependencies {
            self.conn
                .execute(
                    "INSERT INTO task_dependencies (
                        run_id, task_ref, depends_on_workspace, depends_on_run_id, depends_on_ref, satisfied
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        dep.run_id.as_str(),
                        dep.task_ref.as_str(),
                        dep.depends_on_workspace,
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str(),
                        i64::from(dep.satisfied),
                    ],
                )
                .map_err(|err| map_write_err(err, "insert task_dependencies"))?;
        }

        for subtask in &graph.subtasks {
            upsert_subtask_row(self.conn, subtask)?;
        }

        for claim in &graph.claims {
            self.conn
                .execute(
                    "INSERT INTO gate_claims (
                        run_id, node_id, claim_revision, worker_id, session_fingerprint,
                        worktree, span_id, claim_tree, claimed_at, ended_at, outcome
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        claim.run_id.as_str(),
                        claim.node_id,
                        i64::from(claim.claim_revision),
                        claim.worker_id.as_str(),
                        claim.session_fingerprint,
                        claim.worktree,
                        claim.span_id,
                        claim.claim_tree.as_str(),
                        claim.claimed_at,
                        claim.ended_at,
                        claim.outcome.as_str(),
                    ],
                )
                .map_err(|err| map_write_err(err, "insert gate_claims"))?;
        }

        for trace in &graph.traces {
            let passed_tests_json = serialize_json(&trace.passed_tests, "passed_tests_json")?;
            let failed_tests_json = serialize_json(&trace.failed_tests, "failed_tests_json")?;
            let check_results_json = serialize_json(&trace.check_results, "check_results_json")?;
            self.conn
                .execute(
                    "INSERT INTO gate_traces (
                        run_id, node_id, claim_revision, contract_revision, execution_digest,
                        idempotency_key, command_id, command_line, action_id, test_target,
                        status, exit_code, tests_passed, tests_failed, tests_ignored,
                        passed_tests_json, failed_tests_json, check_results_json,
                        candidate_tree_before, candidate_tree_after, log_relpath, started_at, finished_at
                    ) VALUES (
                        ?1, ?2, ?3, ?4, ?5,
                        ?6, ?7, ?8, ?9, ?10,
                        ?11, ?12, ?13, ?14, ?15,
                        ?16, ?17, ?18,
                        ?19, ?20, ?21, ?22, ?23
                    )",
                    params![
                        trace.run_id.as_str(),
                        trace.node_id,
                        i64::from(trace.claim_revision),
                        i64::from(trace.contract_revision),
                        trace.execution_digest,
                        trace.idempotency_key,
                        trace.command_id,
                        trace.command_line,
                        trace.action_id,
                        trace.test_target,
                        trace.status.as_str(),
                        trace.exit_code,
                        i64::from(trace.tests_passed),
                        i64::from(trace.tests_failed),
                        i64::from(trace.tests_ignored),
                        passed_tests_json,
                        failed_tests_json,
                        check_results_json,
                        trace.candidate_tree_before.as_str(),
                        trace.candidate_tree_after.as_ref().map(|t| t.as_str()),
                        trace.log_relpath,
                        trace.started_at,
                        trace.finished_at,
                    ],
                )
                .map_err(|err| map_write_err(err, "insert gate_traces"))?;
        }

        for artifact in &graph.artifacts {
            insert_artifact(self.conn, artifact)?;
        }

        self.significant_dirty = true;
        bump_write_seq(self.conn)?;
        Ok(())
    }


    fn admit_claim(
        &mut self,
        run_id: &RunId,
        preconditions: &StoragePreconditions,
        claim: &ClaimInsertInput,
    ) -> Result<GateClaimRecord, StateError> {
        let expected_gen = i64::try_from(preconditions.expected_run_generation).map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "expected_run_generation overflow: {}",
                    preconditions.expected_run_generation
                ),
            )
        })?;
        let empty_skip = HashSet::new();
        check_node_preconditions(
            self.conn,
            run_id,
            preconditions,
            Some(claim.node_id.as_str()),
            &empty_skip,
        )?;

        let run_changed = self
            .conn
            .execute(
                "UPDATE runs SET generation = generation + 1 WHERE run_id = ?1 AND generation = ?2",
                params![run_id.as_str(), expected_gen],
            )
            .map_err(|err| map_write_err(err, "admit_claim bump runs.generation"))?;
        if run_changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("admit_claim CAS conflict on runs.generation for '{run_id}'"),
            ));
        }

        // If force-claiming over an existing active claim, expected_claim_revision must be provided
        // by the pure state machine so an ordinary race cannot silently supersede an active lease.
        if preconditions.expected_claim_revision.is_some() {
            self.conn
                .execute(
                    "UPDATE gate_claims
                     SET outcome = 'superseded', ended_at = ?3
                     WHERE run_id = ?1 AND node_id = ?2 AND outcome = 'active'",
                    params![run_id.as_str(), claim.node_id, claim.claimed_at],
                )
                .map_err(|err| map_write_err(err, "admit_claim supersede active claim"))?;
        }

        let expected_node_version = preconditions
            .expected_node_versions
            .iter()
            .find(|e| e.node_id == claim.node_id)
            .map(|e| e.expected_version);

        let mut node_sql = String::from(
            "UPDATE gate_nodes SET
                status = ?1,
                node_version = node_version + 1,
                claim_revision = ?2,
                worker_id = ?3,
                session_fingerprint = ?4,
                worktree = ?5,
                branch = COALESCE(?6, branch),
                baseline_commit = COALESCE(?7, baseline_commit),
                claim_tree = ?8,
                active_span_id = ?9
             WHERE run_id = ?10 AND node_id = ?11",
        );
        let node_changed = if let Some(exp_ver) = expected_node_version {
            node_sql.push_str(" AND node_version = ?12");
            let exp_ver_i64 = i64::try_from(exp_ver).map_err(|_| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    format!("expected_version overflow for '{}'", claim.node_id),
                )
            })?;
            self.conn
                .execute(
                    &node_sql,
                    params![
                        NodeStatus::InProgress.as_str(),
                        i64::from(claim.claim_revision),
                        claim.worker_id.as_str(),
                        claim.session_fingerprint,
                        claim.worktree,
                        claim.branch,
                        claim.baseline_commit.as_ref().map(|c| c.as_str()),
                        claim.claim_tree.as_str(),
                        claim.span_id,
                        run_id.as_str(),
                        claim.node_id,
                        exp_ver_i64,
                    ],
                )
                .map_err(|err| map_write_err(err, "admit_claim update gate_nodes"))?
        } else {
            self.conn
                .execute(
                    &node_sql,
                    params![
                        NodeStatus::InProgress.as_str(),
                        i64::from(claim.claim_revision),
                        claim.worker_id.as_str(),
                        claim.session_fingerprint,
                        claim.worktree,
                        claim.branch,
                        claim.baseline_commit.as_ref().map(|c| c.as_str()),
                        claim.claim_tree.as_str(),
                        claim.span_id,
                        run_id.as_str(),
                        claim.node_id,
                    ],
                )
                .map_err(|err| map_write_err(err, "admit_claim update gate_nodes"))?
        };
        if node_changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "admit_claim CAS conflict on gate_nodes '{}/{}'",
                    run_id, claim.node_id
                ),
            ));
        }

        self.conn
            .execute(
                "INSERT INTO gate_claims (
                    run_id, node_id, claim_revision, worker_id, session_fingerprint,
                    worktree, span_id, claim_tree, claimed_at, ended_at, outcome
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
                params![
                    run_id.as_str(),
                    claim.node_id,
                    i64::from(claim.claim_revision),
                    claim.worker_id.as_str(),
                    claim.session_fingerprint,
                    claim.worktree,
                    claim.span_id,
                    claim.claim_tree.as_str(),
                    claim.claimed_at,
                    ClaimOutcome::Active.as_str(),
                ],
            )
            .map_err(|err| map_write_err(err, "admit_claim insert gate_claims"))?;
        let claim_id = self.conn.last_insert_rowid();

        self.significant_dirty = true;
        bump_write_seq(self.conn)?;

        Ok(GateClaimRecord {
            claim_id: Some(claim_id),
            run_id: run_id.clone(),
            node_id: claim.node_id.clone(),
            claim_revision: claim.claim_revision,
            worker_id: claim.worker_id.clone(),
            session_fingerprint: claim.session_fingerprint.clone(),
            worktree: claim.worktree.clone(),
            span_id: claim.span_id.clone(),
            claim_tree: claim.claim_tree.clone(),
            claimed_at: claim.claimed_at.clone(),
            ended_at: None,
            outcome: ClaimOutcome::Active,
        })
    }

    fn start_trace(
        &mut self,
        preconditions: &StoragePreconditions,
        input: &TraceStartInput,
    ) -> Result<GateTraceRecord, StateError> {
        check_run_generation(
            self.conn,
            &input.run_id,
            preconditions.expected_run_generation,
        )?;
        let empty_skip = HashSet::new();
        check_node_preconditions(
            self.conn,
            &input.run_id,
            preconditions,
            Some(input.node_id.as_str()),
            &empty_skip,
        )?;

        self.conn
            .execute(
                "INSERT INTO gate_traces (
                    run_id, node_id, claim_revision, contract_revision, execution_digest,
                    idempotency_key, command_id, command_line, action_id, test_target,
                    status, exit_code, tests_passed, tests_failed, tests_ignored,
                    passed_tests_json, failed_tests_json, check_results_json,
                    candidate_tree_before, candidate_tree_after, log_relpath, started_at, finished_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5,
                    ?6, ?7, ?8, ?9, ?10,
                    'running', NULL, 0, 0, 0,
                    '[]', '[]', '[]',
                    ?11, NULL, ?12, ?13, NULL
                )",
                params![
                    input.run_id.as_str(),
                    input.node_id,
                    i64::from(input.claim_revision),
                    i64::from(input.contract_revision),
                    input.execution_digest,
                    input.idempotency_key,
                    input.command_id,
                    input.command_line,
                    input.action_id,
                    input.test_target,
                    input.candidate_tree_before.as_str(),
                    input.log_relpath,
                    input.started_at,
                ],
            )
            .map_err(|err| map_write_err(err, "start_trace insert gate_traces"))?;
        let trace_row_id = self.conn.last_insert_rowid();
        bump_write_seq(self.conn)?;

        Ok(GateTraceRecord {
            trace_row_id: Some(trace_row_id),
            run_id: input.run_id.clone(),
            node_id: input.node_id.clone(),
            claim_revision: input.claim_revision,
            contract_revision: input.contract_revision,
            execution_digest: input.execution_digest.clone(),
            idempotency_key: input.idempotency_key.clone(),
            command_id: input.command_id.clone(),
            command_line: input.command_line.clone(),
            action_id: input.action_id.clone(),
            test_target: input.test_target.clone(),
            status: TraceStatus::Running,
            exit_code: None,
            tests_passed: 0,
            tests_failed: 0,
            tests_ignored: 0,
            passed_tests: Vec::new(),
            failed_tests: Vec::new(),
            check_results: Vec::new(),
            candidate_tree_before: input.candidate_tree_before.clone(),
            candidate_tree_after: None,
            log_relpath: input.log_relpath.clone(),
            started_at: input.started_at.clone(),
            finished_at: None,
        })
    }

    fn finish_trace(&mut self, input: &TraceFinishInput) -> Result<(), StateError> {
        if input.status == TraceStatus::Running {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "finish_trace requires a non-running terminal TraceStatus",
            ));
        }
        let passed_tests_json = serialize_json(&input.passed_tests, "passed_tests_json")?;
        let failed_tests_json = serialize_json(&input.failed_tests, "failed_tests_json")?;
        let check_results_json = serialize_json(&input.check_results, "check_results_json")?;

        let changed = self
            .conn
            .execute(
                "UPDATE gate_traces SET
                    status = ?1,
                    exit_code = ?2,
                    tests_passed = ?3,
                    tests_failed = ?4,
                    tests_ignored = ?5,
                    passed_tests_json = ?6,
                    failed_tests_json = ?7,
                    check_results_json = ?8,
                    candidate_tree_after = ?9,
                    finished_at = ?10
                 WHERE trace_row_id = ?11 AND status = 'running'",
                params![
                    input.status.as_str(),
                    input.exit_code,
                    i64::from(input.tests_passed),
                    i64::from(input.tests_failed),
                    i64::from(input.tests_ignored),
                    passed_tests_json,
                    failed_tests_json,
                    check_results_json,
                    input.candidate_tree_after.as_ref().map(|t| t.as_str()),
                    input.finished_at,
                    input.trace_row_id,
                ],
            )
            .map_err(|err| map_write_err(err, "finish_trace update gate_traces"))?;
        if changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "finish_trace failed: trace_row_id {} is missing or not in 'running' state",
                    input.trace_row_id
                ),
            ));
        }
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn mark_nodes_stale(
        &mut self,
        run_id: &RunId,
        preconditions: &StoragePreconditions,
        node_ids: &[String],
        now_utc: &str,
    ) -> Result<(), StateError> {
        let expected_gen = i64::try_from(preconditions.expected_run_generation).map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "expected_run_generation overflow: {}",
                    preconditions.expected_run_generation
                ),
            )
        })?;
        let expected_versions: HashMap<&str, u64> = preconditions
            .expected_node_versions
            .iter()
            .map(|e| (e.node_id.as_str(), e.expected_version))
            .collect();
        let target_set: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
        check_node_preconditions(self.conn, run_id, preconditions, None, &target_set)?;

        let run_changed = self
            .conn
            .execute(
                "UPDATE runs SET generation = generation + 1 WHERE run_id = ?1 AND generation = ?2",
                params![run_id.as_str(), expected_gen],
            )
            .map_err(|err| map_write_err(err, "mark_nodes_stale bump runs.generation"))?;
        if run_changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("mark_nodes_stale CAS conflict on runs.generation for '{run_id}'"),
            ));
        }

        for node_id in node_ids {
            let patch = NodeStatusPatch {
                worker_id: Some(None),
                session_fingerprint: Some(None),
                active_span_id: Some(None),
                ..NodeStatusPatch::status_only(node_id.clone(), NodeStatus::Stale)
            };
            apply_node_patch(
                self.conn,
                run_id,
                &patch,
                expected_versions.get(node_id.as_str()).copied(),
            )?;

            self.conn
                .execute(
                    "UPDATE gate_claims
                     SET outcome = 'reopened', ended_at = ?3
                     WHERE run_id = ?1 AND node_id = ?2 AND outcome = 'active'",
                    params![run_id.as_str(), node_id, now_utc],
                )
                .map_err(|err| map_write_err(err, "mark_nodes_stale close active claim"))?;
        }

        self.significant_dirty = true;
        bump_write_seq(self.conn)?;
        Ok(())
    }


    fn apply_transition_plan(
        &mut self,
        run_id: &RunId,
        plan: &TransitionPlan,
    ) -> Result<(), StateError> {
        let expected_versions: HashMap<&str, u64> = plan
            .preconditions
            .expected_node_versions
            .iter()
            .map(|e| (e.node_id.as_str(), e.expected_version))
            .collect();
        let patched_nodes: HashSet<&str> = plan
            .node_patches
            .iter()
            .map(|p| p.node_id.as_str())
            .collect();
        let primary_node = plan
            .preconditions
            .expected_node_versions
            .first()
            .map(|e| e.node_id.as_str())
            .or_else(|| plan.node_patches.first().map(|p| p.node_id.as_str()));

        check_node_preconditions(
            self.conn,
            run_id,
            &plan.preconditions,
            primary_node,
            &patched_nodes,
        )?;

        // Check (and optionally update/bump) runs.generation atomically.
        let needs_run_update = plan.bump_run_generation
            || plan.new_run_status.is_some()
            || plan.new_root_cycle_count.is_some()
            || plan.completed_at.is_some();

        if needs_run_update {
            let expected_gen =
                i64::try_from(plan.preconditions.expected_run_generation).map_err(|_| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!(
                            "expected_run_generation overflow: {}",
                            plan.preconditions.expected_run_generation
                        ),
                    )
                })?;
            let mut sets: Vec<&'static str> = Vec::new();
            let mut values: Vec<rusqlite::types::Value> = Vec::new();
            if plan.bump_run_generation {
                sets.push("generation = generation + 1");
            }
            if let Some(status) = plan.new_run_status {
                sets.push("status = ?");
                values.push(status.as_str().to_owned().into());
            }
            if let Some(cycle_count) = plan.new_root_cycle_count {
                sets.push("root_cycle_count = ?");
                values.push(i64::from(cycle_count).into());
            }
            if let Some(ref completed_at) = plan.completed_at {
                sets.push("completed_at = ?");
                values.push(completed_at.clone().into());
            }
            let sql = format!(
                "UPDATE runs SET {} WHERE run_id = ? AND generation = ?",
                sets.join(", ")
            );
            values.push(run_id.as_str().to_owned().into());
            values.push(expected_gen.into());
            let changed = self
                .conn
                .execute(&sql, rusqlite::params_from_iter(values.iter()))
                .map_err(|err| map_write_err(err, "apply_transition_plan update runs"))?;
            if changed != 1 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "apply_transition_plan CAS conflict on runs.generation for '{run_id}'"
                    ),
                ));
            }
        } else {
            check_run_generation(
                self.conn,
                run_id,
                plan.preconditions.expected_run_generation,
            )?;
        }

        for patch in &plan.node_patches {
            apply_node_patch(
                self.conn,
                run_id,
                patch,
                expected_versions.get(patch.node_id.as_str()).copied(),
            )?;
        }

        for dep in &plan.dependency_patches {
            if let Some(ref dependent) = dep.dependent_task_ref {
                self.conn
                    .execute(
                        "UPDATE task_dependencies
                         SET satisfied = ?1
                         WHERE run_id = ?2 AND task_ref = ?3 AND depends_on_run_id = ?2 AND depends_on_ref = ?4",
                        params![
                            i64::from(dep.satisfied),
                            run_id.as_str(),
                            dependent.as_str(),
                            dep.depends_on_ref.as_str(),
                        ],
                    )
                    .map_err(|err| map_write_err(err, "apply_transition_plan dependency_patch"))?;
            } else {
                self.conn
                    .execute(
                        "UPDATE task_dependencies
                         SET satisfied = ?1
                         WHERE run_id = ?2 AND depends_on_run_id = ?2 AND depends_on_ref = ?3",
                        params![
                            i64::from(dep.satisfied),
                            run_id.as_str(),
                            dep.depends_on_ref.as_str(),
                        ],
                    )
                    .map_err(|err| map_write_err(err, "apply_transition_plan dependency_patch"))?;
            }
        }

        for cp in &plan.claim_patches {
            let changed = self
                .conn
                .execute(
                    "UPDATE gate_claims
                     SET outcome = ?1, ended_at = ?2
                     WHERE run_id = ?3 AND node_id = ?4 AND claim_revision = ?5",
                    params![
                        cp.outcome.as_str(),
                        cp.ended_at,
                        run_id.as_str(),
                        cp.node_id,
                        i64::from(cp.claim_revision),
                    ],
                )
                .map_err(|err| map_write_err(err, "apply_transition_plan claim_patch"))?;
            if changed != 1 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "claim_patch target missing for '{}/{}/rev-{}'",
                        run_id, cp.node_id, cp.claim_revision
                    ),
                ));
            }
        }

        for art in &plan.new_artifacts {
            insert_artifact(self.conn, art)?;
        }

        for bb in &plan.new_blackboard {
            insert_blackboard_row(self.conn, bb)?;
        }

        for bbp in &plan.blackboard_patches {
            let changed = self
                .conn
                .execute(
                    "UPDATE blackboard SET status = ?1 WHERE run_id = ?2 AND id = ?3",
                    params![bbp.status.as_str(), run_id.as_str(), bbp.id],
                )
                .map_err(|err| map_write_err(err, "apply_transition_plan blackboard_patch"))?;
            if changed != 1 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!("blackboard entry {} missing in run '{}'", bbp.id, run_id),
                ));
            }
        }

        for subtask in &plan.new_subtasks {
            upsert_subtask_row(self.conn, subtask)?;
        }

        if plan.significant_transition {
            self.significant_dirty = true;
        }
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn apply_sync_plan(
        &mut self,
        run_id: &RunId,
        plan: &SyncPlan,
        new_profile: Option<(&str, &str)>,
    ) -> Result<(), StateError> {
        self.apply_transition_plan(run_id, &plan.transition)?;

        if let Some((profile_id, profile_json)) = new_profile {
            let changed = self
                .conn
                .execute(
                    "UPDATE runs SET spec_digest = ?1, profile_id = ?2, profile_json = ?3 WHERE run_id = ?4",
                    params![plan.spec_digest, profile_id, profile_json, run_id.as_str()],
                )
                .map_err(|err| map_write_err(err, "apply_sync_plan update runs with profile"))?;
            if changed != 1 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!("apply_sync_plan run '{run_id}' missing"),
                ));
            }
        } else {
            let changed = self
                .conn
                .execute(
                    "UPDATE runs SET spec_digest = ?1 WHERE run_id = ?2",
                    params![plan.spec_digest, run_id.as_str()],
                )
                .map_err(|err| map_write_err(err, "apply_sync_plan update runs.spec_digest"))?;
            if changed != 1 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!("apply_sync_plan run '{run_id}' missing"),
                ));
            }
        }

        if !plan.added_nodes.is_empty() {
            let mut stmt = self
                .conn
                .prepare("SELECT node_id FROM gate_nodes WHERE run_id = ?1")
                .map_err(|err| map_write_err(err, "apply_sync_plan load existing node_ids"))?;
            let existing_rows = stmt
                .query_map(params![run_id.as_str()], |row| row.get::<_, String>(0))
                .map_err(|err| map_write_err(err, "apply_sync_plan query existing node_ids"))?;
            let mut existing_parents = HashSet::new();
            for row in existing_rows {
                existing_parents.insert(
                    row.map_err(|err| map_write_err(err, "apply_sync_plan read existing node_id"))?,
                );
            }
            drop(stmt);

            for node in order_nodes_parent_first_with_existing(&plan.added_nodes, &existing_parents)?
            {
                insert_gate_node(self.conn, node)?;
            }
        }

        if !plan.seq_order_updates.is_empty() {
            for (node_id, seq_order) in &plan.seq_order_updates {
                let changed = self
                    .conn
                    .execute(
                        "UPDATE gate_nodes SET seq_order = ?1 WHERE run_id = ?2 AND node_id = ?3",
                        params![i64::from(*seq_order), run_id.as_str(), node_id],
                    )
                    .map_err(|err| map_write_err(err, "apply_sync_plan update seq_order"))?;
                if changed != 1 {
                    return Err(StateError::new(
                        ErrorCode::BackendConflict,
                        format!("apply_sync_plan seq_order target '{run_id}/{node_id}' missing"),
                    ));
                }
            }
        }

        for dep in &plan.removed_dependencies {
            self.conn
                .execute(
                    "DELETE FROM task_dependencies
                     WHERE run_id = ?1
                       AND task_ref = ?2
                       AND depends_on_workspace = ?3
                       AND depends_on_run_id = ?4
                       AND depends_on_ref = ?5",
                    params![
                        dep.run_id.as_str(),
                        dep.task_ref.as_str(),
                        dep.depends_on_workspace,
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str(),
                    ],
                )
                .map_err(|err| map_write_err(err, "apply_sync_plan delete task_dependencies"))?;
        }

        for dep in &plan.added_dependencies {
            self.conn
                .execute(
                    "INSERT INTO task_dependencies (
                        run_id, task_ref, depends_on_workspace, depends_on_run_id, depends_on_ref, satisfied
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ON CONFLICT(run_id, task_ref, depends_on_workspace, depends_on_run_id, depends_on_ref)
                    DO UPDATE SET satisfied = excluded.satisfied",
                    params![
                        dep.run_id.as_str(),
                        dep.task_ref.as_str(),
                        dep.depends_on_workspace,
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str(),
                        i64::from(dep.satisfied),
                    ],
                )
                .map_err(|err| map_write_err(err, "apply_sync_plan insert task_dependencies"))?;
        }

        self.significant_dirty = true;
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn update_run_spec_id(
        &mut self,
        run_id: &RunId,
        spec_id: Option<&str>,
    ) -> Result<(), StateError> {
        let changed = self
            .conn
            .execute(
                "UPDATE runs SET spec_id = ?1 WHERE run_id = ?2",
                params![spec_id, run_id.as_str()],
            )
            .map_err(|err| map_write_err(err, "update runs.spec_id"))?;
        if changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("update_run_spec_id run '{run_id}' missing"),
            ));
        }
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn insert_blackboard(&mut self, item: &BlackboardRecord) -> Result<BlackboardRecord, StateError> {
        let stored = insert_blackboard_row(self.conn, item)?;
        bump_write_seq(self.conn)?;
        Ok(stored)
    }

    fn update_blackboard_status(
        &mut self,
        run_id: &RunId,
        patch: &BlackboardStatusPatch,
    ) -> Result<(), StateError> {
        let changed = self
            .conn
            .execute(
                "UPDATE blackboard SET status = ?1 WHERE run_id = ?2 AND id = ?3",
                params![patch.status.as_str(), run_id.as_str(), patch.id],
            )
            .map_err(|err| map_write_err(err, "update_blackboard_status"))?;
        if changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("blackboard entry {} missing in run '{}'", patch.id, run_id),
            ));
        }
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn upsert_subtask(&mut self, subtask: &SubtaskRecord) -> Result<(), StateError> {
        upsert_subtask_row(self.conn, subtask)?;
        bump_write_seq(self.conn)?;
        Ok(())
    }

    fn archive_and_prune_run(
        &mut self,
        preconditions: &StoragePreconditions,
        tombstone: &RunTombstoneRecord,
    ) -> Result<(), StateError> {
        check_run_generation(
            self.conn,
            &tombstone.run_id,
            preconditions.expected_run_generation,
        )?;
        let empty_skip = HashSet::new();
        check_node_preconditions(
            self.conn,
            &tombstone.run_id,
            preconditions,
            None,
            &empty_skip,
        )?;

        let cleanup_manifest_json = serialize_json(
            &tombstone.cleanup_manifest,
            "run_tombstones.cleanup_manifest_json",
        )?;
        let tomb_gen = i64::try_from(tombstone.generation).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("tombstone generation overflow: {}", tombstone.generation),
            )
        })?;

        self.conn
            .execute(
                "INSERT INTO run_tombstones (
                    run_id, run_name, collection, workspace, spec_id, title, profile_id,
                    is_ephemeral, generation, terminal_status, target_base, target_base_commit,
                    archive_path, closed_commit, cleanup_pending, cleanup_manifest_json,
                    summary_json, metrics_json, closed_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16,
                    ?17, ?18, ?19
                )",
                params![
                    tombstone.run_id.as_str(),
                    tombstone.run_name.as_str(),
                    tombstone.collection,
                    tombstone.workspace,
                    tombstone.spec_id,
                    tombstone.title,
                    tombstone.profile_id,
                    i64::from(tombstone.is_ephemeral),
                    tomb_gen,
                    tombstone.terminal_status.as_str(),
                    tombstone.target_base,
                    tombstone.target_base_commit.as_ref().map(|c| c.as_str()),
                    tombstone.archive_path,
                    tombstone.closed_commit.as_ref().map(|c| c.as_str()),
                    i64::from(tombstone.cleanup_pending),
                    cleanup_manifest_json,
                    tombstone.summary_json,
                    tombstone.metrics_json,
                    tombstone.closed_at,
                ],
            )
            .map_err(|err| map_write_err(err, "archive_and_prune_run insert run_tombstones"))?;

        let expected_gen = i64::try_from(preconditions.expected_run_generation).map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "expected_run_generation overflow: {}",
                    preconditions.expected_run_generation
                ),
            )
        })?;
        let deleted = self
            .conn
            .execute(
                "DELETE FROM runs WHERE run_id = ?1 AND generation = ?2",
                params![tombstone.run_id.as_str(), expected_gen],
            )
            .map_err(|err| map_write_err(err, "archive_and_prune_run delete runs"))?;
        if deleted != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "archive_and_prune_run CAS conflict deleting run '{}'",
                    tombstone.run_id
                ),
            ));
        }

        self.significant_dirty = true;
        bump_write_seq(self.conn)?;
        Ok(())
    }


    fn mark_tombstone_cleaned(&mut self, run_id: &RunId) -> Result<(), StateError> {
        let changed = self
            .conn
            .execute(
                "UPDATE run_tombstones SET cleanup_pending = 0 WHERE run_id = ?1",
                params![run_id.as_str()],
            )
            .map_err(|err| map_write_err(err, "mark_tombstone_cleaned"))?;
        if changed != 1 {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!("run_tombstone '{run_id}' does not exist"),
            ));
        }
        bump_write_seq(self.conn)?;
        Ok(())
    }
}
