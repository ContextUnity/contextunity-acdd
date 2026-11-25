//! Pure `CompletedRunDigest`, `summary_json`, and `metrics_json` builders for `run_tombstones`.

use crate::spec::ClosureSummary;
use crate::types::{
    ClaimOutcome, CompletedRunDigest, NodeKind, NodeStatus, RunGraphSnapshot, RunTombstoneRecord,
};
use acdd_core::errors::StateError;
use acdd_core::format::canonical_json_string;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Build `(CompletedRunDigest, summary_json, metrics_json)` for a completed or cleared run.
pub fn build_completed_run_digest(
    graph: &RunGraphSnapshot,
    tombstone_seed: &RunTombstoneRecord,
    closure: Option<&ClosureSummary>,
) -> Result<(CompletedRunDigest, String, String), StateError> {
    let closed_commit = tombstone_seed
        .closed_commit
        .clone()
        .or_else(|| closure.and_then(|c| c.closed_commit.clone()));

    let digest = CompletedRunDigest {
        run_id: tombstone_seed.run_id.clone(),
        run_name: tombstone_seed.run_name.clone(),
        collection: tombstone_seed.collection.clone(),
        spec_id: tombstone_seed.spec_id.clone(),
        title: tombstone_seed.title.clone(),
        terminal_status: tombstone_seed.terminal_status,
        archive_path: tombstone_seed.archive_path.clone(),
        closed_commit: closed_commit.clone(),
        closed_at: tombstone_seed.closed_at.clone(),
    };

    let task_nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .collect();
    let tasks_total = task_nodes.len();
    let tasks_completed = task_nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Completed)
        .count();
    let tasks_cancelled = task_nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Cancelled)
        .count();

    let (
        applicable_invariants,
        verified_invariants,
        decisions_count,
        deferred_count,
        review_summary,
    ) = if let Some(c) = closure {
        (
            c.rollup.verified_invariants.applicable.clone(),
            c.rollup.verified_invariants.verified.clone(),
            c.rollup.decisions.len(),
            c.rollup.deferred_findings.len(),
            c.rollup.review_summary.clone(),
        )
    } else {
        let mut applicable = BTreeSet::new();
        let mut verified = BTreeSet::new();
        for node in &graph.nodes {
            applicable.extend(node.invariants.applicable.iter().cloned());
            verified.extend(node.invariants.verified.iter().cloned());
        }
        (
            applicable.into_iter().collect(),
            verified.into_iter().collect(),
            0usize,
            0usize,
            None,
        )
    };

    let summary_val = json!({
        "run_id": digest.run_id,
        "run_name": digest.run_name,
        "collection": digest.collection,
        "spec_id": digest.spec_id,
        "title": digest.title,
        "profile_id": tombstone_seed.profile_id,
        "is_ephemeral": tombstone_seed.is_ephemeral,
        "isolation": graph.run.isolation,
        "terminal_status": digest.terminal_status,
        "archive_path": digest.archive_path,
        "closed_commit": digest.closed_commit,
        "closed_at": digest.closed_at,
        "tasks_total": tasks_total,
        "tasks_completed": tasks_completed,
        "tasks_cancelled": tasks_cancelled,
        "task_receipts_count": closure.map_or(0, |c| c.task_receipts.len()),
        "verified_invariants": {
            "applicable": applicable_invariants,
            "verified": verified_invariants,
        },
        "decisions_count": decisions_count,
        "deferred_findings_count": deferred_count,
        "review_summary": review_summary,
    });

    let mut nodes_by_status: BTreeMap<String, usize> = BTreeMap::new();
    for node in &graph.nodes {
        *nodes_by_status
            .entry(node.status.as_str().to_owned())
            .or_insert(0) += 1;
    }

    let mut claims_by_outcome: BTreeMap<String, usize> = BTreeMap::new();
    let mut rejections_total = 0usize;
    for claim in &graph.claims {
        *claims_by_outcome
            .entry(claim.outcome.as_str().to_owned())
            .or_insert(0) += 1;
        if claim.outcome == ClaimOutcome::Rejected {
            rejections_total += 1;
        }
    }

    let mut traces_by_status: BTreeMap<String, usize> = BTreeMap::new();
    for trace in &graph.traces {
        *traces_by_status
            .entry(trace.status.as_str().to_owned())
            .or_insert(0) += 1;
    }

    let metrics_val = json!({
        "nodes_total": graph.nodes.len(),
        "nodes_by_status": nodes_by_status,
        "claims_total": graph.claims.len(),
        "claims_by_outcome": claims_by_outcome,
        "rejections_total": rejections_total,
        "traces_total": graph.traces.len(),
        "traces_by_status": traces_by_status,
        "subtasks_total": graph.subtasks.len(),
        "artifacts_total": graph.artifacts.len(),
        "root_cycle_count": graph.run.root_cycle_count,
        "generation": graph.run.generation,
    });

    let summary_json = canonical_json_string(&summary_val);
    let metrics_json = canonical_json_string(&metrics_val);

    Ok((digest, summary_json, metrics_json))
}
