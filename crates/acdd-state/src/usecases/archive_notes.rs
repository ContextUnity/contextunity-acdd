//! Pure archive Markdown input construction and Git commit-note rendering.

use crate::git_ops::commit::{cap_note_body, write_git_note, GitNoteReq};
use crate::profile::{
    GitCommitNoteFormat, GitCommitNotePolicy, GitCommitNoteSection, ResolvedGateProfile,
};
use crate::spec::{
    ArchiveRenderInput, ArchiveTaskUpdate, ClosureSummary, Receipt, ReceiptRollup, SpecStatus,
    Subtask,
};
use crate::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, BlackboardTopic, GateNodeRecord,
    InvariantsState, NodeKind, NodeStatus, RunGraphSnapshot, TraceStatus,
};
use crate::usecases::dto::SubmitReq;
use crate::machine::MachineGateDef;
use acdd_core::ids::{CommitOid, NodeCursor, TreeOid};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub(super) fn build_provisional_receipt(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    committed_oid: Option<CommitOid>,
    now_utc: &str,
    req: &SubmitReq,
) -> Receipt {
    Receipt {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: machine_gate.id.clone(),
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
    }
}

fn push_unique_string(target: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if !trimmed.is_empty() && !target.iter().any(|existing| existing == trimmed) {
        target.push(trimmed.to_owned());
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
        let task_receipt = snapshot
            .artifacts
            .iter()
            .filter(|a| a.kind == ArtifactKind::Receipt && a.node_id.starts_with(&prefix))
            .max_by_key(|a| a.claim_revision)
            .and_then(|a| serde_json::from_str::<Receipt>(&a.payload_json).ok());
        if let Some(ref rec) = task_receipt {
            if let Some(ref rollup) = rec.rollup {
                for dec in &rollup.decisions {
                    push_unique_string(&mut decisions, dec);
                }
                for def in &rollup.deferred_findings {
                    push_unique_string(&mut deferred_findings, def);
                }
            }
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
        if let Some(ref rollup) = closing.rollup {
            for dec in &rollup.decisions {
                push_unique_string(&mut decisions, dec);
            }
            for def in &rollup.deferred_findings {
                push_unique_string(&mut deferred_findings, def);
            }
        }
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
                TraceStatus::Failed
                    | TraceStatus::TimedOut
                    | TraceStatus::CompileError
                    | TraceStatus::ScopeViolation
            )
        })
        .count();

    let mut metrics_summary = BTreeMap::new();
    metrics_summary.insert("tasks_total".to_owned(), json!(tasks_total));
    metrics_summary.insert("tasks_completed".to_owned(), json!(tasks_completed));
    metrics_summary.insert("tasks_cancelled".to_owned(), json!(tasks_cancelled));
    metrics_summary.insert(
        "task_receipts_count".to_owned(),
        json!(task_receipts.len()),
    );
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

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_best_effort_git_note(
    worktree: &Path,
    profile: &ResolvedGateProfile,
    note_key: &str,
    commit_oid: &CommitOid,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
    changed_paths: &[String],
    now_utc: &str,
) {
    let Some(policy) = profile.git_commit_notes.get(note_key) else {
        return;
    };
    let body = render_git_commit_note(policy, snapshot, node, cursor, changed_paths, now_utc);
    let _ = write_git_note(&GitNoteReq {
        worktree: worktree.to_path_buf(),
        commit_oid: commit_oid.clone(),
        policy: policy.clone(),
        body,
    });
}

fn elapsed_seconds_between(start_rfc3339: &str, end_rfc3339: &str) -> Option<i64> {
    let start = chrono::DateTime::parse_from_rfc3339(start_rfc3339).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(end_rfc3339).ok()?;
    Some((end.timestamp() - start.timestamp()).max(0))
}

pub(super) fn render_git_commit_note(
    policy: &GitCommitNotePolicy,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
    changed_paths: &[String],
    now_utc: &str,
) -> String {
    let tasks_total = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .count();
    let tasks_completed = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
        .count();
    let total_cycles: u64 = snapshot
        .nodes
        .iter()
        .map(|n| u64::from(n.cycle_count))
        .sum::<u64>()
        + u64::from(snapshot.run.root_cycle_count);
    let latest_trace = snapshot
        .traces
        .iter()
        .filter(|t| t.node_id == node.node_id && t.claim_revision == node.claim_revision)
        .max_by_key(|t| t.trace_row_id.unwrap_or(0));
    let elapsed_secs = elapsed_seconds_between(&snapshot.run.created_at, now_utc);

    let raw = match policy.format {
        GitCommitNoteFormat::Json => {
            let mut obj = serde_json::Map::new();
            obj.insert("run_id".to_owned(), json!(snapshot.run.run_id.as_str()));
            obj.insert("run_name".to_owned(), json!(snapshot.run.run_name.as_str()));
            obj.insert("node_cursor".to_owned(), json!(cursor.to_string()));
            obj.insert("claim_revision".to_owned(), json!(node.claim_revision));
            obj.insert("attempt".to_owned(), json!(node.attempt));
            obj.insert("isolation".to_owned(), json!(snapshot.run.isolation.as_str()));
            for section in &policy.sections {
                match section {
                    GitCommitNoteSection::Duration => {
                        obj.insert(
                            "duration".to_owned(),
                            json!({
                                "created_at": snapshot.run.created_at,
                                "recorded_at": now_utc,
                                "elapsed_seconds": elapsed_secs,
                            }),
                        );
                    }
                    GitCommitNoteSection::TasksSummary => {
                        obj.insert(
                            "tasks_summary".to_owned(),
                            json!({
                                "total": tasks_total,
                                "completed": tasks_completed,
                            }),
                        );
                    }
                    GitCommitNoteSection::ReviewCycles => {
                        obj.insert(
                            "review_cycles".to_owned(),
                            json!({
                                "node_cycles": node.cycle_count,
                                "root_cycles": snapshot.run.root_cycle_count,
                                "total_cycles": total_cycles,
                            }),
                        );
                    }
                    GitCommitNoteSection::DiffBreakdown => {
                        obj.insert(
                            "diff_breakdown".to_owned(),
                            json!({
                                "changed_files_count": changed_paths.len(),
                                "changed_paths": changed_paths,
                            }),
                        );
                    }
                    GitCommitNoteSection::TestSuite => {
                        if let Some(tr) = latest_trace {
                            obj.insert(
                                "test_suite".to_owned(),
                                json!({
                                    "status": tr.status.as_str(),
                                    "passed": tr.tests_passed,
                                    "failed": tr.tests_failed,
                                    "ignored": tr.tests_ignored,
                                }),
                            );
                        } else {
                            obj.insert(
                                "test_suite".to_owned(),
                                json!({
                                    "passed": 0,
                                    "failed": 0,
                                    "ignored": 0,
                                }),
                            );
                        }
                    }
                    GitCommitNoteSection::Topology => {
                        obj.insert(
                            "topology".to_owned(),
                            json!({
                                "isolation": snapshot.run.isolation.as_str(),
                                "branch": node.branch.as_ref().or(snapshot.run.branch.as_ref()),
                                "worktree": node.worktree.as_ref().or(snapshot.run.worktree.as_ref()),
                                "target_base": snapshot.run.target_base,
                            }),
                        );
                    }
                }
            }
            Value::Object(obj).to_string()
        }
        GitCommitNoteFormat::Markdown => {
            let mut md = format!(
                "# ACDD Gate Commit Note\n\n- Run: `{}` (`{}`)\n- Node: `{}` (rev {})\n- Isolation: `{}`\n",
                snapshot.run.run_name,
                snapshot.run.run_id,
                cursor,
                node.claim_revision,
                snapshot.run.isolation
            );
            for section in &policy.sections {
                match section {
                    GitCommitNoteSection::Duration => {
                        if let Some(secs) = elapsed_secs {
                            md.push_str(&format!(
                                "- Duration: {secs}s (since `{}`)\n",
                                snapshot.run.created_at
                            ));
                        } else {
                            md.push_str(&format!(
                                "- Duration: started `{}`\n",
                                snapshot.run.created_at
                            ));
                        }
                    }
                    GitCommitNoteSection::TasksSummary => {
                        md.push_str(&format!("- Tasks: {tasks_completed}/{tasks_total} completed\n"));
                    }
                    GitCommitNoteSection::ReviewCycles => {
                        md.push_str(&format!(
                            "- Review cycles: node={}, total={}\n",
                            node.cycle_count, total_cycles
                        ));
                    }
                    GitCommitNoteSection::DiffBreakdown => {
                        md.push_str(&format!(
                            "- Diff breakdown: {} changed path(s)\n",
                            changed_paths.len()
                        ));
                    }
                    GitCommitNoteSection::TestSuite => {
                        if let Some(tr) = latest_trace {
                            md.push_str(&format!(
                                "- Test suite ({}): {} passed, {} failed, {} ignored\n",
                                tr.status, tr.tests_passed, tr.tests_failed, tr.tests_ignored
                            ));
                        } else {
                            md.push_str("- Test suite: no trace recorded\n");
                        }
                    }
                    GitCommitNoteSection::Topology => {
                        md.push_str(&format!(
                            "- Topology: isolation=`{}`, branch=`{}`, target_base=`{}`\n",
                            snapshot.run.isolation,
                            node.branch
                                .as_deref()
                                .or(snapshot.run.branch.as_deref())
                                .unwrap_or("-"),
                            snapshot.run.target_base.as_deref().unwrap_or("-")
                        ));
                    }
                }
            }
            md
        }
    };
    cap_note_body(&raw, policy.max_bytes, policy.format)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::profile::GitCommitNotePolicy;
    use crate::spec::ReceiptRollup;
    use crate::types::{
        GateArtifactRecord, IsolationMode, RunRecord, RunStatus, SubtaskRecord, SubtaskStatus,
    };
    use acdd_core::ids::{GateId, RunId, RunName, SubtaskRef, TaskRef, WorkerId};

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
                snapshot_ref: None,
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
    fn commit_note_sections_truncation_and_closure_rollup() {
        let run_id = RunId::new("run-submit-unit").expect("run_id");
        let snapshot = sample_snapshot(&run_id);
        let node = &snapshot.nodes[1];
        let cursor = NodeCursor::task(
            TaskRef::new("T1").expect("task_ref"),
            vec![GateId::new("deliver").expect("gate_id")],
        );

        let json_policy = GitCommitNotePolicy {
            ref_name: "refs/notes/acdd".to_owned(),
            format: GitCommitNoteFormat::Json,
            max_bytes: 4096,
            sections: vec![
                GitCommitNoteSection::Duration,
                GitCommitNoteSection::TasksSummary,
                GitCommitNoteSection::ReviewCycles,
                GitCommitNoteSection::DiffBreakdown,
                GitCommitNoteSection::TestSuite,
                GitCommitNoteSection::Topology,
            ],
        };
        let rendered_json = render_git_commit_note(
            &json_policy,
            &snapshot,
            node,
            &cursor,
            &["crates/a.rs".to_owned()],
            "2026-01-01T00:05:00Z",
        );
        let parsed: Value = serde_json::from_str(&rendered_json).expect("valid json note");
        assert_eq!(parsed["duration"]["elapsed_seconds"], 300);
        assert_eq!(parsed["tasks_summary"]["completed"], 1);
        assert_eq!(parsed["review_cycles"]["total_cycles"], 3);
        assert_eq!(parsed["diff_breakdown"]["changed_files_count"], 1);
        assert!(parsed.get("test_suite").is_some());
        assert_eq!(parsed["topology"]["target_base"], "main");

        // Tight max_bytes still yields valid UTF-8 and valid JSON!
        let capped_policy = GitCommitNotePolicy {
            max_bytes: 48,
            ..json_policy
        };
        let capped_json = render_git_commit_note(
            &capped_policy,
            &snapshot,
            node,
            &cursor,
            &["crates/a.rs".to_owned()],
            "2026-01-01T00:05:00Z",
        );
        assert!(capped_json.len() <= 48);
        assert!(serde_json::from_str::<Value>(&capped_json).is_ok());

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
