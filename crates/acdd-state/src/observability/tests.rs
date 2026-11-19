use super::*;
use crate::harness::RunHarness;
use crate::spec::{ClosureSummary, Receipt, ReceiptRollup, SpecStatus};
use crate::types::{
    ClaimOutcome, GateClaimRecord, GateTraceRecord, InvariantsState, NodeKind, NodeStatus,
    ObservabilitySnapshot, RunTombstoneRecord, TerminalStatus, TraceStatus,
};
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, RunName, TaskRef, TreeOid, WorkerId};
use serde_json::Value;
use std::collections::BTreeMap;

#[test]
fn prometheus_004_golden_format_and_label_escaping() {
    let mut nodes_by_status = BTreeMap::new();
    nodes_by_status.insert("completed".to_owned(), 3);
    nodes_by_status.insert("ready".to_owned(), 2);

    let mut traces_by_status = BTreeMap::new();
    traces_by_status.insert("failed".to_owned(), 1);
    traces_by_status.insert("passed".to_owned(), 4);
    traces_by_status.insert("custom\"esc\\line\nstatus".to_owned(), 7);

    let snapshot = ObservabilitySnapshot {
        active_runs: 2,
        completed_runs: 5,
        cleared_runs: 1,
        nodes_by_status,
        active_claims: 1,
        traces_by_status,
        open_blackboard_items: 4,
    };

    let rendered = render_prometheus_text(&snapshot);
    let expected = concat!(
        "# HELP acdd_runs_active Number of active ACDD runs\n",
        "# TYPE acdd_runs_active gauge\n",
        "acdd_runs_active 2\n",
        "# HELP acdd_runs_completed_total Total completed ACDD runs\n",
        "# TYPE acdd_runs_completed_total counter\n",
        "acdd_runs_completed_total 5\n",
        "# HELP acdd_runs_cleared_total Total cleared ACDD runs\n",
        "# TYPE acdd_runs_cleared_total counter\n",
        "acdd_runs_cleared_total 1\n",
        "# HELP acdd_gate_nodes Current gate nodes by status\n",
        "# TYPE acdd_gate_nodes gauge\n",
        "acdd_gate_nodes{status=\"completed\"} 3\n",
        "acdd_gate_nodes{status=\"ready\"} 2\n",
        "# HELP acdd_gate_claims_active Number of currently active gate claims\n",
        "# TYPE acdd_gate_claims_active gauge\n",
        "acdd_gate_claims_active 1\n",
        "# HELP acdd_gate_traces_total Total gate traces by execution status\n",
        "# TYPE acdd_gate_traces_total counter\n",
        "acdd_gate_traces_total{status=\"custom\\\"esc\\\\line\\nstatus\"} 7\n",
        "acdd_gate_traces_total{status=\"failed\"} 1\n",
        "acdd_gate_traces_total{status=\"passed\"} 4\n",
        "# HELP acdd_blackboard_open_items Number of open blackboard items\n",
        "# TYPE acdd_blackboard_open_items gauge\n",
        "acdd_blackboard_open_items 4\n",
    );
    assert_eq!(rendered, expected);
}

#[test]
fn completed_run_digest_builds_canonical_summary_and_metrics_matrix() {
    let mut graph = RunHarness::build_sample_graph(
        "run-obs-1",
        "obs-run",
        "/tmp/ws",
        Some("/tmp/ws"),
        &["t1", "t2"],
    )
    .unwrap();

    for node in &mut graph.nodes {
        if node.kind == NodeKind::Task {
            node.status = if node.task_ref.as_ref().map(|r| r.as_str()) == Some("t1") {
                NodeStatus::Completed
            } else {
                NodeStatus::Cancelled
            };
        }
    }
    let tree = TreeOid::new("a".repeat(40)).unwrap();
    let commit = CommitOid::new("b".repeat(40)).unwrap();
    graph.claims.push(GateClaimRecord {
        claim_id: Some(1),
        run_id: graph.run.run_id.clone(),
        node_id: "task:t1:build".to_owned(),
        claim_revision: 1,
        worker_id: WorkerId::new("w1").unwrap(),
        session_fingerprint: None,
        worktree: "/tmp/ws".to_owned(),
        span_id: "1".repeat(16),
        claim_tree: tree.clone(),
        claimed_at: "2026-11-17T10:00:00.000Z".to_owned(),
        ended_at: Some("2026-11-17T10:05:00.000Z".to_owned()),
        outcome: ClaimOutcome::Rejected,
    });
    graph.traces.push(GateTraceRecord {
        trace_row_id: Some(1),
        run_id: graph.run.run_id.clone(),
        node_id: "task:t1:build".to_owned(),
        claim_revision: 1,
        contract_revision: 1,
        execution_digest: "c".repeat(64),
        idempotency_key: None,
        command_id: Some("build_check".to_owned()),
        command_line: Some("cargo test".to_owned()),
        action_id: None,
        test_target: None,
        status: TraceStatus::Passed,
        exit_code: Some(0),
        passed_tests: vec!["t_one".to_owned()],
        failed_tests: Vec::new(),
        check_results: Vec::new(),
        candidate_tree_before: tree.clone(),
        candidate_tree_after: Some(tree.clone()),
        log_relpath: "logs/t1-build-rev1.log".to_owned(),
        started_at: "2026-11-17T10:01:00.000Z".to_owned(),
        finished_at: Some("2026-11-17T10:02:00.000Z".to_owned()),
    });

    let mut receipts = BTreeMap::new();
    receipts.insert(
        TaskRef::new("t1").unwrap(),
        Receipt {
            run_id: RunId::new("run-obs-1").unwrap(),
            node_cursor: NodeCursor::task(
                TaskRef::new("t1").unwrap(),
                vec![GateId::new("deliver").unwrap()],
            ),
            gate_id: GateId::new("deliver").unwrap(),
            worker_id: WorkerId::new("w-rev").unwrap(),
            claim_revision: 1,
            contract_revision: 1,
            candidate_tree: tree,
            committed_oid: Some(commit.clone()),
            accepted_at: "2026-11-17T10:05:00.000Z".to_owned(),
            need_proofs: BTreeMap::new(),
            rollup: None,
        },
    );

    let closure = ClosureSummary {
        status: SpecStatus::Completed,
        closed_at: "2026-11-17T10:10:00.000Z".to_owned(),
        target_base: Some("main".to_owned()),
        target_base_commit: None,
        closed_commit: Some(commit.clone()),
        task_receipts: receipts,
        rollup: ReceiptRollup {
            verified_invariants: InvariantsState {
                applicable: vec!["INV-1".to_owned()],
                verified: vec!["INV-1".to_owned()],
            },
            decisions: vec!["DEC-1".to_owned()],
            deferred_findings: Vec::new(),
            review_summary: Some("All clean".to_owned()),
        },
        metrics_summary: BTreeMap::new(),
    };

    let seed_completed = RunTombstoneRecord {
        run_id: graph.run.run_id.clone(),
        run_name: RunName::new("obs-run").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "/tmp/ws".to_owned(),
        spec_id: Some("AC-005".to_owned()),
        title: "Obs".to_owned(),
        profile_id: "code".to_owned(),
        is_ephemeral: false,
        generation: 2,
        terminal_status: TerminalStatus::Completed,
        target_base: Some("main".to_owned()),
        target_base_commit: None,
        archive_path: Some("docs/milestones/archive/AC-005.md".to_owned()),
        closed_commit: None,
        cleanup_pending: true,
        cleanup_manifest: Vec::new(),
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-17T10:10:00.000Z".to_owned(),
    };

    let (digest, summary_json, metrics_json) =
        build_completed_run_digest(&graph, &seed_completed, Some(&closure)).unwrap();
    assert_eq!(digest.terminal_status, TerminalStatus::Completed);
    assert_eq!(digest.closed_commit, Some(commit));
    assert_eq!(
        digest.archive_path.as_deref(),
        Some("docs/milestones/archive/AC-005.md")
    );

    let summary: Value = serde_json::from_str(&summary_json).unwrap();
    assert_eq!(summary["tasks_total"], 2);
    assert_eq!(summary["tasks_completed"], 1);
    assert_eq!(summary["tasks_cancelled"], 1);
    assert_eq!(summary["task_receipts_count"], 1);
    assert_eq!(summary["decisions_count"], 1);
    assert_eq!(summary["review_summary"], "All clean");

    let metrics: Value = serde_json::from_str(&metrics_json).unwrap();
    assert_eq!(metrics["rejections_total"], 1);
    assert_eq!(metrics["traces_total"], 1);

    // Cleared run without closure still succeeds cleanly.
    let mut seed_cleared = seed_completed;
    seed_cleared.terminal_status = TerminalStatus::Cleared;
    seed_cleared.archive_path = None;
    let (cleared_digest, cleared_summary, _) =
        build_completed_run_digest(&graph, &seed_cleared, None).unwrap();
    assert_eq!(cleared_digest.terminal_status, TerminalStatus::Cleared);
    let cleared_val: Value = serde_json::from_str(&cleared_summary).unwrap();
    assert_eq!(cleared_val["terminal_status"], "cleared");
    assert_eq!(cleared_val["task_receipts_count"], 0);
}
