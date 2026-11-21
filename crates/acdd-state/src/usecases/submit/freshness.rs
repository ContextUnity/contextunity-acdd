//! Submit preflight, trace freshness verification, and `need:` drift stale-marking.

use crate::git_ops::commit::find_claim_commit_in_range;
use crate::git_ops::snapshot::{
    current_scope_tree, diff_scope_changed_paths,
};
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    compute_execution_digest, container_scope, evaluate_submit,
    ExecutionDigestInput, MachineGateDef, MachineGraphItem, MachineProfileInput, MachineProofKind,
    SubmitEvaluation, SubmitInput,
};
use crate::profile::ResolvedGateProfile;
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    GateNodeRecord, GateTraceRecord, NodeKind, NodeStatus,
    RunGraphSnapshot, SaveAction,
    SubmitAction, TraceStatus, ValidatedObservations,
};
use crate::usecases::dto::SubmitReq;
use crate::usecases::lifecycle::{
    compute_archive_relpath, load_active_run_graph,
    parse_cursor_for_snapshot, resolve_node_worktree,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::verify::{find_machine_gate, resolve_commands};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::checked_child;
use acdd_core::ids::{GateId, NodeCursor, TreeOid};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{CompositeDomainProvider, DomainCheckReq, FreshnessObservation};
use serde_json::json;
use std::path::Path;


pub(super) struct SubmitPreflight {
    pub(super) primary: std::path::PathBuf,
    pub(super) target_worktree: std::path::PathBuf,
    pub(super) snapshot: RunGraphSnapshot,
    pub(super) resolved_profile: ResolvedGateProfile,
    pub(super) node: GateNodeRecord,
    pub(super) machine_gate: MachineGateDef,
    pub(super) cursor: NodeCursor,
    pub(super) admitted_scope: Vec<String>,
    pub(super) current_tree: TreeOid,
    pub(super) policy: WorkspacePathPolicy,
    pub(super) existing_fresh: Option<GateTraceRecord>,
}

pub(super) fn inspect_submit_preflight(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    primary: &Path,
    req: &SubmitReq,
) -> Result<Option<SubmitPreflight>, StateError> {
    let loaded = load_active_run_graph(
        config,
        backend,
        req.run.as_deref(),
        req.worktree.as_deref(),
    )?;
    let cursor = parse_cursor_for_snapshot(&loaded.snapshot, &req.node)?;
    let node_id = cursor.to_node_id()?;
    let Some(node) = loaded.snapshot.nodes.iter().find(|n| n.node_id == node_id).cloned() else {
        return Ok(None);
    };
    if node.status != NodeStatus::InProgress
        || node.claim_revision == 0
        || node.worker_id.as_ref() != Some(&req.worker_id)
        || req
            .claim_revision
            .is_some_and(|expected| expected != node.claim_revision)
    {
        return Ok(None);
    }
    let machine_gate = find_machine_gate(&loaded.machine_profile, &node)?.clone();
    let target_worktree =
        resolve_node_worktree(config, &loaded.snapshot.run, &node, req.worktree.as_deref());

    if machine_gate.save.archive && !loaded.snapshot.run.is_ephemeral && machine_gate.save.post == SaveAction::Commit {
        if let Some(ref spec_rel) = loaded.snapshot.run.spec_path {
            let archive_rel =
                compute_archive_relpath(config, loaded.snapshot.run.collection.as_deref(), spec_rel)?;
            let trailer_needle = format!(
                "Acdd-Claim: {}/{}/{}",
                loaded.snapshot.run.run_id, node.node_id, node.claim_revision
            );
            if checked_child(&target_worktree, Path::new(&archive_rel)).is_ok_and(|p| p.is_file())
                && find_claim_commit_in_range(
                    &target_worktree,
                    loaded.snapshot.run.target_base_commit.as_ref(),
                    &trailer_needle,
                )?
                .is_some()
            {
                return Ok(None);
            }
        }
    }

    let needs_trace = matches!(
        machine_gate.proof,
        MachineProofKind::Command | MachineProofKind::Call
    ) || !machine_gate.checks.is_empty()
        || machine_gate.command.is_some()
        || machine_gate.call.is_some();
    if !needs_trace {
        return Ok(None);
    }

    let policy = loaded.path_policy(config, backend, Some(&target_worktree))?;
    let current_tree = current_scope_tree(&target_worktree, None, &policy)?;
    if machine_gate.save.immutable && node.claim_tree.as_ref() != Some(&current_tree) {
        return Ok(None);
    }

    let admitted_scope = container_scope(&loaded.snapshot, &node);
    let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();
    let changed_paths = diff_scope_changed_paths(
        &target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        &policy,
    )?;
    let pre_obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        changed_paths,
        path_policy: Some(policy.clone()),
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };
    if !detect_scope_violations(
        &loaded.snapshot,
        &loaded.machine_profile,
        &pre_obs,
        &admitted_scope,
        is_unscoped_root,
    )
    .is_empty()
    {
        return Ok(None);
    }

    let existing_fresh = select_fresh_candidate_trace(
        &loaded.snapshot,
        &node,
        TraceStatus::Passed,
        &current_tree,
    );

    Ok(Some(SubmitPreflight {
        primary: primary.to_path_buf(),
        target_worktree,
        snapshot: loaded.snapshot,
        resolved_profile: loaded.profile,
        node,
        machine_gate,
        cursor,
        admitted_scope,
        current_tree,
        policy,
        existing_fresh,
    }))
}

fn select_fresh_candidate_trace(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    required_status: TraceStatus,
    current_tree: &TreeOid,
) -> Option<GateTraceRecord> {
    snapshot
        .traces
        .iter()
        .filter(|t| {
            t.node_id == node.node_id
                && t.claim_revision == node.claim_revision
                && t.contract_revision == node.contract_revision
                && t.status == required_status
                && &t.candidate_tree_before == current_tree
                && t.candidate_tree_after.as_ref() == Some(current_tree)
        })
        .max_by_key(|t| t.trace_row_id.unwrap_or(0))
        .cloned()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_existing_trace_freshness(
    provider: &CompositeDomainProvider,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    resolved_profile: &ResolvedGateProfile,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
    req: &SubmitReq,
    tr: &GateTraceRecord,
) -> Result<Option<String>, StateError> {
    let gate_def = find_profile_gate_def(resolved_profile, cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let checks_params = serde_json::Value::Array(
        machine_gate
            .checks
            .iter()
            .map(|c| json!({ "name": c.qualified_name, "params": c.params }))
            .collect(),
    );
    let effective_test_target = req.test_target.as_deref().or(tr.test_target.as_deref());
    let trace_commands = resolve_commands(
        resolved_profile,
        &gate_def,
        admitted_scope,
        effective_test_target,
    )?;
    let live_changed_paths = diff_scope_changed_paths(
        target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        policy,
    )?;
    let check_req = DomainCheckReq {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: GateId::new(&gate_def.id)?,
        workspace_root: primary.to_path_buf(),
        worktree: target_worktree.to_path_buf(),
        admitted_scope: admitted_scope.to_vec(),
        changed_paths: live_changed_paths,
        test_target: effective_test_target.map(ToOwned::to_owned),
        path_policy: policy.clone(),
        baseline_ref: crate::context::baseline_ref_for_node(node),
    };
    let mut live_index_gens = Vec::new();
    let mut live_salts = Vec::new();
    for chk in &gate_def.checks {
        let (domain, check_name) = chk.check.split_once('.').ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("check '{}' must be qualified as '<domain>.<check>'", chk.check),
            )
        })?;
        match provider
            .observe_freshness(&chk.check, &chk.with, &check_req)
            .map_err(|err| {
                StateError::from_anyhow(&err).cloned().unwrap_or_else(|| {
                    StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("domain check '{}' freshness check errored: {err}", chk.check),
                    )
                })
            })?
        {
            FreshnessObservation::Passed {
                index_generation,
                execution_digest_salt,
            } => {
                if let Some(gen) = index_generation {
                    live_index_gens.push(format!("{domain}.{check_name}:{gen}"));
                }
                if let Some(salt) = execution_digest_salt {
                    live_salts.push(format!("{domain}.{check_name}:{salt}"));
                }
            }
            FreshnessObservation::Failed { .. } => return Ok(None),
        }
    }
    let index_generation = crate::usecases::lifecycle::combine_check_observations(&live_index_gens);
    let live_files_digest = crate::usecases::lifecycle::combine_check_observations(&live_salts);
    let digest_input = ExecutionDigestInput {
        contract_revision: node.contract_revision,
        gate_def_digest: machine_gate.definition_digest.clone(),
        admitted_scope: admitted_scope.to_vec(),
        resolved_commands: trace_commands,
        test_target: effective_test_target.map(ToOwned::to_owned),
        call_with: machine_gate.with_params.clone(),
        checks_params,
        index_generation,
        live_files_digest,
    };
    let expected_digest = compute_execution_digest(&digest_input)?;
    Ok((tr.execution_digest == expected_digest).then_some(expected_digest))
}

#[allow(clippy::too_many_arguments)]
fn mark_drifted_need_predecessors_stale(
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    changed_paths: &[String],
    policy: &WorkspacePathPolicy,
    req: &SubmitReq,
) -> Result<(), StateError> {
    let mut probe_profile = machine_profile.clone();
    for item in &mut probe_profile.gates {
        match item {
            MachineGraphItem::Gate(g) => {
                if g.id.as_str() == node.gate_id && node.kind == NodeKind::RootGate {
                    g.save.immutable = false;
                }
            }
            MachineGraphItem::TasksBlock(tb) => {
                if node.kind == NodeKind::TaskGate {
                    for g in &mut tb.gates {
                        if g.id.as_str() == node.gate_id {
                            g.save.immutable = false;
                        }
                    }
                }
            }
        }
    }
    let now_utc = acdd_core::telemetry::now_utc_iso_ms();
    let probe_obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        changed_paths: changed_paths.to_vec(),
        path_policy: Some(policy.clone()),
        now_utc: now_utc.clone(),
        ..ValidatedObservations::default()
    };
    let probe_input = SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        worker_id: req.worker_id.clone(),
        claim_revision: req.claim_revision,
        expected_execution_digest: None,
        review_evidence: req.review_evidence.clone(),
        rejection_findings: None,
        custom_evidence: req.custom_evidence.clone(),
        verified_invariants: req.verified_invariants.clone(),
        snapshot_commit: None,
        summary: req.summary.clone(),
    };
    match evaluate_submit(snapshot, &probe_profile, &probe_obs, &probe_input) {
        Ok(SubmitEvaluation::MarkStaleAndReject {
            stale_node_ids,
            preconditions,
            ..
        }) => {
            exec_tx_with(backend, |tx| {
                tx.mark_nodes_stale(&snapshot.run.run_id, &preconditions, &stale_node_ids, &now_utc)
            })?;
        }
        Err(err) if err.code == ErrorCode::OutOfScopeChanges => {
            return Err(err);
        }
        Ok(_) | Err(_) => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn ensure_fresh_trace_or_auto_verify(
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    resolved_profile: &ResolvedGateProfile,
    machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    _admitted_scope: &[String],
    current_tree: &TreeOid,
    changed_paths: &[String],
    policy: &WorkspacePathPolicy,
    req: &SubmitReq,
    verified_hint: Option<&(TreeOid, String)>,
) -> Result<Option<String>, StateError> {
    if machine_gate.save.immutable {
        if let Some(ref claim_tree) = node.claim_tree {
            if claim_tree != current_tree {
                mark_drifted_need_predecessors_stale(
                    backend,
                    snapshot,
                    machine_profile,
                    node,
                    cursor,
                    current_tree,
                    changed_paths,
                    policy,
                    req,
                )?;
                return Err(StateError::new(
                    ErrorCode::TaskCandidateMismatch,
                    format!(
                        "immutable gate '{}' working tree '{}' drifted from claim_tree '{}'",
                        node.node_id, current_tree, claim_tree
                    ),
                ));
            }
        }
    }

    let needs_trace = matches!(
        machine_gate.proof,
        MachineProofKind::Command | MachineProofKind::Call
    ) || !machine_gate.checks.is_empty()
        || machine_gate.command.is_some()
        || machine_gate.call.is_some();

    if !needs_trace {
        return Ok(None);
    }

    let _gate_def = find_profile_gate_def(resolved_profile, cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;

    let existing_fresh =
        select_fresh_candidate_trace(snapshot, node, TraceStatus::Passed, current_tree);

    if let (Some(ref tr), Some((hint_tree, hint_digest))) = (existing_fresh, verified_hint) {
        if hint_tree == current_tree && tr.execution_digest == *hint_digest {
            return Ok(Some(hint_digest.clone()));
        }
    }

    Err(StateError::new(
        ErrorCode::TaskCandidateMismatch,
        format!(
            "working tree changed or verify trace became stale for '{}' before submit lock acquisition",
            node.node_id
        ),
    ))
}
