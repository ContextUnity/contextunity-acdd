//! Pure archive Markdown input construction and closure rollup rendering.

use crate::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, BlackboardTopic, GateNodeRecord, NodeKind,
    NodeStatus, RunGraphSnapshot, TraceStatus,
};
use crate::usecases::dto::SubmitReq;
use acdd_core::errors::AcddError;
use acdd_core::ids::{CommitOid, GateId, NodeCursor, TreeOid};
use acdd_spec::{
    ArchiveRenderInput, ArchiveTaskUpdate, ClosureSummary, InvariantsState, Receipt, ReceiptRollup,
    SpecStatus, Subtask,
};
use serde_json::json;
use std::collections::BTreeMap;

pub(super) fn build_provisional_receipt(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    current_tree: &TreeOid,
    committed_oid: Option<CommitOid>,
    now_utc: &str,
    req: &SubmitReq,
) -> Result<Receipt, AcddError> {
    Ok(Receipt {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: NodeCursor::from_node_id(&node.node_id)?,
        gate_id: GateId::new(&node.gate_id)?,
        worker_id: req.worker_id.clone(),
        claim_revision: node.claim_revision,
        contract_revision: node.contract_revision,
        candidate_tree: current_tree.clone(),
        committed_oid,
        accepted_at: now_utc.to_owned(),
        need_proofs: BTreeMap::new(),
        rollup: Some(ReceiptRollup {
            verified_invariants: InvariantsState {
                applicable: req.verified_invariants.clone(),
                verified: req.verified_invariants.clone(),
            },
            decisions: Vec::new(),
            deferred_findings: Vec::new(),
            review_summary: req.summary.clone(),
        }),
    })
}

fn push_unique_string(target: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if !trimmed.is_empty() && !target.iter().any(|existing| existing == trimmed) {
        target.push(trimmed.to_owned());
    }
}

fn collect_rollup_notes(
    rollup: Option<&ReceiptRollup>,
    decisions: &mut Vec<String>,
    deferred_findings: &mut Vec<String>,
) {
    if let Some(rollup) = rollup {
        rollup
            .decisions
            .iter()
            .for_each(|d| push_unique_string(decisions, d));
        rollup
            .deferred_findings
            .iter()
            .for_each(|d| push_unique_string(deferred_findings, d));
    }
}

pub(super) fn build_archive_render_input(
    snapshot: &RunGraphSnapshot,
    closing_receipt: Option<&Receipt>,
    blackboard_items: &[BlackboardRecord],
    closed_commit: Option<CommitOid>,
    now_utc: &str,
    summary: Option<&str>,
) -> ArchiveRenderInput {
    let mut tasks = BTreeMap::new();
    let mut task_receipts = BTreeMap::new();
    let mut all_applicable = Vec::new();
    let mut all_verified = Vec::new();
    let mut decisions = Vec::new();
    let mut deferred_findings = Vec::new();

    for node in &snapshot.nodes {
        if node.kind != NodeKind::Task {
            continue;
        }
        let Some(ref tref) = node.task_ref else {
            continue;
        };
        let status = match node.status {
            NodeStatus::Completed => SpecStatus::Completed,
            NodeStatus::Cancelled => SpecStatus::Cancelled,
            NodeStatus::InProgress => SpecStatus::Active,
            _ => SpecStatus::Planned,
        };
        for inv in &node.invariants.applicable {
            if !all_applicable.contains(inv) {
                all_applicable.push(inv.clone());
            }
        }
        for inv in &node.invariants.verified {
            if !all_verified.contains(inv) {
                all_verified.push(inv.clone());
            }
        }
        let subtasks: Vec<Subtask> = snapshot
            .subtasks
            .iter()
            .filter(|s| s.node_id == node.node_id)
            .map(|s| Subtask {
                subtask_ref: s.subtask_ref.clone(),
                title: s.title.clone(),
                status: s.status,
                evidence: s.evidence.clone(),
            })
            .collect();

        let prefix = format!("task:{tref}:");
        let task_gates: BTreeMap<&str, &GateNodeRecord> = snapshot
            .nodes
            .iter()
            .filter(|g| {
                g.kind == NodeKind::TaskGate
                    && (g.parent_id.as_deref() == Some(node.node_id.as_str())
                        || g.node_id.starts_with(&prefix))
            })
            .map(|g| (g.node_id.as_str(), g))
            .collect();
        let task_receipt = snapshot
            .artifacts
            .iter()
            .enumerate()
            .filter_map(|(idx, a)| {
                if a.kind != ArtifactKind::Receipt || !a.node_id.starts_with(&prefix) {
                    return None;
                }
                let seq_order = if let Some(gate_node) = task_gates.get(a.node_id.as_str()) {
                    if !matches!(
                        gate_node.status,
                        NodeStatus::Completed | NodeStatus::InProgress
                    ) || (gate_node.claim_revision > 0
                        && a.claim_revision < gate_node.claim_revision)
                    {
                        return None;
                    }
                    gate_node.seq_order
                } else if task_gates.is_empty() && node.status == NodeStatus::Completed {
                    0
                } else {
                    return None;
                };
                let rec = serde_json::from_str::<Receipt>(&a.payload_json).ok()?;
                Some(((seq_order, a.claim_revision, a.created_at.as_str(), idx), rec))
            })
            .max_by(|(k_a, _), (k_b, _)| k_a.cmp(k_b))
            .map(|(_, rec)| rec);
        if let Some(ref rec) = task_receipt {
            collect_rollup_notes(rec.rollup.as_ref(), &mut decisions, &mut deferred_findings);
            task_receipts.insert(tref.clone(), rec.clone());
        }

        tasks.insert(
            tref.clone(),
            ArchiveTaskUpdate {
                status,
                extended_scope: node.extended_scope.clone(),
                subtasks,
                receipt: task_receipt,
            },
        );
    }

    if let Some(closing) = closing_receipt {
        collect_rollup_notes(
            closing.rollup.as_ref(),
            &mut decisions,
            &mut deferred_findings,
        );
    }

    for item in blackboard_items {
        if item.topic == BlackboardTopic::Decisions {
            push_unique_string(&mut decisions, &item.payload);
        } else if item.topic == BlackboardTopic::Deferred
            && matches!(
                item.status,
                BlackboardStatus::Open | BlackboardStatus::CarriedOver
            )
        {
            push_unique_string(&mut deferred_findings, &item.payload);
        }
    }

    let tasks_total = tasks.len();
    let tasks_completed = tasks
        .values()
        .filter(|t| t.status == SpecStatus::Completed)
        .count();
    let tasks_cancelled = tasks
        .values()
        .filter(|t| t.status == SpecStatus::Cancelled)
        .count();
    let total_review_cycles: u64 = snapshot
        .nodes
        .iter()
        .map(|n| u64::from(n.cycle_count))
        .sum::<u64>()
        + u64::from(snapshot.run.root_cycle_count);
    let passed_traces = snapshot
        .traces
        .iter()
        .filter(|t| t.status == TraceStatus::Passed)
        .count();
    let failed_traces = snapshot
        .traces
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                TraceStatus::Failed | TraceStatus::TimedOut | TraceStatus::ScopeViolation
            )
        })
        .count();

    let mut metrics_summary = BTreeMap::new();
    metrics_summary.insert("tasks_total".to_owned(), json!(tasks_total));
    metrics_summary.insert("tasks_completed".to_owned(), json!(tasks_completed));
    metrics_summary.insert("tasks_cancelled".to_owned(), json!(tasks_cancelled));
    metrics_summary.insert("task_receipts_count".to_owned(), json!(task_receipts.len()));
    metrics_summary.insert("total_claims".to_owned(), json!(snapshot.claims.len()));
    metrics_summary.insert("total_traces".to_owned(), json!(snapshot.traces.len()));
    metrics_summary.insert("passed_traces".to_owned(), json!(passed_traces));
    metrics_summary.insert("failed_traces".to_owned(), json!(failed_traces));
    metrics_summary.insert("total_review_cycles".to_owned(), json!(total_review_cycles));
    metrics_summary.insert("decisions_count".to_owned(), json!(decisions.len()));
    metrics_summary.insert(
        "deferred_findings_count".to_owned(),
        json!(deferred_findings.len()),
    );

    ArchiveRenderInput {
        closure: ClosureSummary {
            status: SpecStatus::Completed,
            closed_at: now_utc.to_owned(),
            target_base: snapshot.run.target_base.clone(),
            target_base_commit: snapshot.run.target_base_commit.clone(),
            closed_commit,
            task_receipts,
            rollup: ReceiptRollup {
                verified_invariants: InvariantsState {
                    applicable: all_applicable,
                    verified: all_verified,
                },
                decisions,
                deferred_findings,
                review_summary: summary.map(str::to_owned),
            },
            metrics_summary,
        },
        tasks,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{GateArtifactRecord, IsolationMode, RunRecord, RunStatus, SubtaskRecord};
    use acdd_core::ids::{GateId, RunId, RunName, SubtaskRef, TaskRef, WorkerId};
    use acdd_spec::{ReceiptRollup, SubtaskStatus};

    pub(crate) fn sample_snapshot(run_id: &RunId) -> RunGraphSnapshot {
        RunGraphSnapshot {
            run: RunRecord {
                run_id: run_id.clone(),
                run_name: RunName::new("submit-test").expect("run_name"),
                collection: None,
                workspace: "/tmp/ws".to_owned(),
                profile_id: "code".to_owned(),
                profile_json: "{}".to_owned(),
                spec_path: None,
                spec_id: None,
                spec_digest: "dig".to_owned(),
                is_ephemeral: false,
                isolation: IsolationMode::Worktree,
                target_base: Some("main".to_owned()),
                target_base_commit: None,
                branch: Some("run/submit-test".to_owned()),
                worktree: Some("/tmp/wt".to_owned()),
                trace_id: "tr".to_owned(),
                status: RunStatus::Active,
                cancellation_reason: None,
                root_cycle_count: 1,
                generation: 1,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                completed_at: None,
            },
            nodes: vec![
                GateNodeRecord {
                    run_id: run_id.clone(),
                    node_id: "task:T1".to_owned(),
                    parent_id: None,
                    kind: NodeKind::Task,
                    task_ref: Some(TaskRef::new("T1").expect("task_ref")),
                    gate_id: "__task__".to_owned(),
                    seq_order: 1,
                    status: NodeStatus::Completed,
                    node_version: 1,
                    isolation: IsolationMode::Worktree,
                    attempt: 1,
                    cycle_count: 2,
                    max_attempts: 3,
                    claim_revision: 1,
                    contract_revision: 1,
                    worker_id: Some(WorkerId::new("w1").expect("worker_id")),
                    session_fingerprint: None,
                    branch: None,
                    worktree: None,
                    baseline_commit: None,
                    claim_tree: None,
                    accepted_tree: None,
                    accepted_definition_digest: None,
                    tags: vec![],
                    invariants: InvariantsState::default(),
                    scope: vec![],
                    extended_scope: vec![],
                    active_span_id: None,
                },
                GateNodeRecord {
                    run_id: run_id.clone(),
                    node_id: "task:T1:deliver".to_owned(),
                    parent_id: Some("task:T1".to_owned()),
                    kind: NodeKind::TaskGate,
                    task_ref: Some(TaskRef::new("T1").expect("task_ref")),
                    gate_id: "deliver".to_owned(),
                    seq_order: 2,
                    status: NodeStatus::InProgress,
                    node_version: 1,
                    isolation: IsolationMode::Worktree,
                    attempt: 1,
                    cycle_count: 0,
                    max_attempts: 3,
                    claim_revision: 1,
                    contract_revision: 1,
                    worker_id: Some(WorkerId::new("w1").expect("worker_id")),
                    session_fingerprint: None,
                    branch: None,
                    worktree: None,
                    baseline_commit: None,
                    claim_tree: None,
                    accepted_tree: None,
                    accepted_definition_digest: None,
                    tags: vec![],
                    invariants: InvariantsState::default(),
                    scope: vec![],
                    extended_scope: vec![],
                    active_span_id: None,
                },
            ],
            dependencies: vec![],
            claims: vec![],
            traces: vec![],
            artifacts: vec![GateArtifactRecord {
                run_id: run_id.clone(),
                node_id: "task:T1:deliver".to_owned(),
                claim_revision: 1,
                kind: ArtifactKind::Receipt,
                payload_json: serde_json::to_string(&Receipt {
                    run_id: run_id.clone(),
                    node_cursor: NodeCursor::task(
                        TaskRef::new("T1").expect("task_ref"),
                        vec![GateId::new("deliver").expect("gate_id")],
                    ),
                    gate_id: GateId::new("deliver").expect("gate_id"),
                    worker_id: WorkerId::new("w1").expect("worker_id"),
                    claim_revision: 1,
                    contract_revision: 1,
                    candidate_tree: TreeOid::new("0".repeat(40)).expect("tree"),
                    committed_oid: None,
                    accepted_at: "2026-01-01T00:01:00Z".to_owned(),
                    need_proofs: BTreeMap::new(),
                    rollup: Some(ReceiptRollup {
                        verified_invariants: InvariantsState::default(),
                        decisions: vec!["D-receipt".to_owned()],
                        deferred_findings: vec!["F-receipt".to_owned()],
                        review_summary: None,
                    }),
                })
                .expect("receipt json"),
                snapshot_commit: None,
                created_at: "2026-01-01T00:01:00Z".to_owned(),
            }],
            subtasks: vec![SubtaskRecord {
                run_id: run_id.clone(),
                node_id: "task:T1".to_owned(),
                subtask_ref: SubtaskRef::new("S1").expect("subtask_ref"),
                seq_order: 1,
                title: "Sub".to_owned(),
                status: SubtaskStatus::Completed,
                evidence: Some("done".to_owned()),
                updated_at: "2026-01-01T00:01:00Z".to_owned(),
            }],
        }
    }

    #[test]
    fn closure_rollup_collects_decisions_deferred_and_metrics() {
        let run_id = RunId::new("run-submit-unit").expect("run_id");
        let snapshot = sample_snapshot(&run_id);

        // Closure rollup populates decisions, deferred_findings, and metrics_summary.
        let bb = vec![
            BlackboardRecord {
                id: Some(1),
                run_id: run_id.clone(),
                node_id: Some("task:T1".to_owned()),
                recipient_node_id: None,
                subtask_ref: None,
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Decisions,
                status: BlackboardStatus::Open,
                payload: "D-blackboard".to_owned(),
                created_at: "2026-01-01T00:02:00Z".to_owned(),
            },
            BlackboardRecord {
                id: Some(2),
                run_id: run_id.clone(),
                node_id: None,
                recipient_node_id: None,
                subtask_ref: None,
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Deferred,
                status: BlackboardStatus::CarriedOver,
                payload: "F-deferred".to_owned(),
                created_at: "2026-01-01T00:03:00Z".to_owned(),
            },
        ];
        let archive = build_archive_render_input(
            &snapshot,
            None,
            &bb,
            None,
            "2026-01-01T00:05:00Z",
            Some("LGTM"),
        );
        assert_eq!(
            archive.closure.rollup.decisions,
            vec!["D-receipt".to_owned(), "D-blackboard".to_owned()]
        );
        assert_eq!(
            archive.closure.rollup.deferred_findings,
            vec!["F-receipt".to_owned(), "F-deferred".to_owned()]
        );
        assert_eq!(archive.closure.metrics_summary["tasks_completed"], 1);
        assert_eq!(archive.closure.metrics_summary["total_review_cycles"], 3);
    }
}
