//! Read-only blueprint slice, dependency receipt, and specification loaders for context & submit paths.

use crate::spec::{parse_spec, Receipt, SpecDocument};
use crate::types::{ArtifactKind, GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::lifecycle::{primary_workspace_root, read_confined_utf8};
use acdd_core::config::ACDD_DIR;

/// Read and parse the specification Markdown file for `run` read-only, if present on disk.
pub(crate) fn read_spec_readonly(
    config: &StateEngineConfig,
    run: &crate::types::RunRecord,
) -> Option<SpecDocument> {
    let primary = primary_workspace_root(config);
    let rel = if let Some(ref sp) = run.spec_path {
        sp.clone()
    } else if run.is_ephemeral {
        format!("{ACDD_DIR}/run/{}/spec.md", run.run_id)
    } else {
        return None;
    };
    let raw = read_confined_utf8(&primary, &rel).ok()?;
    parse_spec(&raw).ok()
}

/// Collect `Receipt` artifacts from completed intra-run dependency tasks.
pub(crate) fn collect_dependency_receipts(
    graph: &RunGraphSnapshot,
    node: &GateNodeRecord,
) -> Vec<Receipt> {
    let Some(ref tref) = node.task_ref else {
        return Vec::new();
    };
    let dep_refs: Vec<&str> = graph
        .dependencies
        .iter()
        .filter(|d| &d.task_ref == tref && d.depends_on_run_id == graph.run.run_id)
        .map(|d| d.depends_on_ref.as_str())
        .collect();
    if dep_refs.is_empty() {
        return Vec::new();
    }

    let mut receipts = Vec::new();
    for dep_ref in dep_refs {
        let prefix = format!("task:{dep_ref}:");
        let latest = graph
            .artifacts
            .iter()
            .enumerate()
            .filter(|(_, art)| {
                art.kind == ArtifactKind::Receipt && art.node_id.starts_with(&prefix)
            })
            .max_by(|(idx_a, a), (idx_b, b)| {
                a.claim_revision
                    .cmp(&b.claim_revision)
                    .then_with(|| a.created_at.cmp(&b.created_at))
                    .then_with(|| idx_a.cmp(idx_b))
            })
            .map(|(_, art)| art);
        if let Some(art) = latest {
            if let Ok(rec) = serde_json::from_str::<Receipt>(&art.payload_json) {
                receipts.push(rec);
            }
        }
    }
    receipts
}

fn latest_pre_task_proof_value(graph: &RunGraphSnapshot) -> Option<serde_json::Value> {
    let completed_pre_nodes: std::collections::BTreeMap<&str, u32> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.status == NodeStatus::Completed
                && crate::machine::graph::is_pre_task_root_node(graph, n)
        })
        .map(|n| (n.node_id.as_str(), n.claim_revision))
        .collect();
    if completed_pre_nodes.is_empty() {
        return None;
    }

    let mut best: Option<(u32, &str, serde_json::Value)> = None;
    for art in &graph.artifacts {
        let Some(&node_claim_rev) = completed_pre_nodes.get(art.node_id.as_str()) else {
            continue;
        };
        if !matches!(art.kind, ArtifactKind::Proof | ArtifactKind::Receipt) {
            continue;
        }
        if node_claim_rev > 0 && art.claim_revision < node_claim_rev {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&art.payload_json) {
            if !val.get("tasks").is_some_and(serde_json::Value::is_object) {
                continue;
            }
            let replace = match best {
                None => true,
                Some((prev_rev, prev_at, _)) => {
                    (art.claim_revision, art.created_at.as_str()) > (prev_rev, prev_at)
                }
            };
            if replace {
                best = Some((art.claim_revision, art.created_at.as_str(), val));
            }
        }
    }
    best.map(|(_, _, val)| val)
}

/// Extract `tasks[<task_ref>]` slice (plus global boundaries/invariants if present) from the latest accepted pre-task root artifact.
pub(crate) fn extract_pre_task_slice(
    graph: &RunGraphSnapshot,
    task_ref: &str,
) -> Option<serde_json::Value> {
    let val = latest_pre_task_proof_value(graph)?;
    let mut slice = val.get("tasks").and_then(|t| t.get(task_ref))?.clone();
    if let serde_json::Value::Object(ref mut map) = slice {
        for shared_key in ["global_negative_boundaries", "cited_invariants"] {
            if !map.contains_key(shared_key) {
                if let Some(shared) = val
                    .get(shared_key)
                    .filter(|v| !matches!(v, serde_json::Value::Array(a) if a.is_empty()))
                {
                    map.insert(shared_key.to_owned(), shared.clone());
                }
            }
        }
    }
    Some(slice)
}

/// Extract global architectural boundaries from the accepted pre-task blueprint artifact for root closure/polish gates.
pub(crate) fn extract_global_blueprint_slice(
    graph: &RunGraphSnapshot,
) -> Option<serde_json::Value> {
    let val = latest_pre_task_proof_value(graph)?;
    let mut out = serde_json::Map::new();
    for key in [
        "cited_invariants",
        "global_negative_boundaries",
        "c2_boundary_impact",
    ] {
        if let Some(field) = val.get(key) {
            let non_empty = match field {
                serde_json::Value::Array(a) => !a.is_empty(),
                serde_json::Value::String(s) => !s.trim().is_empty(),
                serde_json::Value::Null => false,
                _ => true,
            };
            if non_empty {
                out.insert(key.to_owned(), field.clone());
            }
        }
    }
    (!out.is_empty()).then_some(serde_json::Value::Object(out))
}

/// Extract latest `ArtifactKind::Proof` payloads from completed predecessor sibling gates in the same container, ordered by `seq_order`.
pub(crate) fn extract_predecessor_gate_proofs(
    graph: &RunGraphSnapshot,
    node: &GateNodeRecord,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let node_is_pre_task = crate::machine::graph::is_pre_task_root_node(graph, node);
    let mut predecessors: Vec<&GateNodeRecord> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.parent_id == node.parent_id
                && n.seq_order < node.seq_order
                && n.status == NodeStatus::Completed
                && matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                && crate::machine::graph::is_pre_task_root_node(graph, n) == node_is_pre_task
        })
        .collect();
    predecessors.sort_by_key(|n| n.seq_order);

    let mut proofs = serde_json::Map::new();
    for pred in predecessors {
        let latest = graph
            .artifacts
            .iter()
            .enumerate()
            .filter(|(_, art)| {
                art.node_id == pred.node_id
                    && art.kind == ArtifactKind::Proof
                    && (pred.claim_revision == 0 || art.claim_revision >= pred.claim_revision)
            })
            .max_by(|(idx_a, a), (idx_b, b)| {
                a.claim_revision
                    .cmp(&b.claim_revision)
                    .then_with(|| a.created_at.cmp(&b.created_at))
                    .then_with(|| idx_a.cmp(idx_b))
            })
            .map(|(_, art)| art);
        let Some(art) = latest else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&art.payload_json) else {
            continue;
        };
        let non_empty = match &val {
            serde_json::Value::Object(m) => !m.is_empty(),
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::String(s) => !s.trim().is_empty(),
            serde_json::Value::Null => false,
            _ => true,
        };
        if non_empty {
            proofs.insert(pred.gate_id.clone(), val);
        }
    }
    (!proofs.is_empty()).then_some(proofs)
}

#[cfg(test)]
mod tests {
    use super::extract_pre_task_slice;
    use crate::types::{ArtifactKind, GateArtifactRecord, NodeKind, NodeStatus};
    use acdd_core::ids::RunId;

    #[test]
    fn extract_pre_task_slice_selects_latest_accepted_revision() {
        let run_id = RunId::new("run-slice").expect("run_id");
        let rev1 = GateArtifactRecord {
            run_id: run_id.clone(),
            node_id: "blueprint_synthesis.synthesize".to_owned(),
            claim_revision: 1,
            kind: ArtifactKind::Proof,
            payload_json: r#"{"tasks":{"T1":{"ver":"stale-rev-1"}}}"#.to_owned(),
            snapshot_commit: None,
            created_at: "2026-01-01T00:01:00Z".to_owned(),
        };
        let rev2 = GateArtifactRecord {
            run_id: run_id.clone(),
            node_id: "blueprint_synthesis.synthesize".to_owned(),
            claim_revision: 2,
            kind: ArtifactKind::Proof,
            payload_json: r#"{"tasks":{"T1":{"ver":"accepted-rev-2"}}}"#.to_owned(),
            snapshot_commit: None,
            created_at: "2026-01-01T00:02:00Z".to_owned(),
        };

        let mut rejected_snap = crate::usecases::archive_notes::tests::sample_snapshot(&run_id);
        rejected_snap.nodes[0].node_id = "blueprint_synthesis.synthesize".to_owned();
        rejected_snap.nodes[0].kind = NodeKind::RootGate;
        rejected_snap.nodes[0].task_ref = None;
        rejected_snap.nodes[0].gate_id = "synthesize".to_owned();
        rejected_snap.nodes[0].status = NodeStatus::Stale;
        rejected_snap.nodes[0].claim_revision = 1;
        rejected_snap.nodes[1].node_id = "tasks#1".to_owned();
        rejected_snap.nodes[1].parent_id = None;
        rejected_snap.nodes[1].kind = NodeKind::Container;
        rejected_snap.nodes[1].gate_id = "tasks".to_owned();
        rejected_snap.nodes[1].status = NodeStatus::Pending;
        rejected_snap.artifacts = vec![rev1.clone()];
        assert_eq!(extract_pre_task_slice(&rejected_snap, "T1"), None);

        let mut completed_snap = rejected_snap;
        completed_snap.nodes[0].status = NodeStatus::Completed;
        completed_snap.nodes[0].claim_revision = 2;
        completed_snap.artifacts = vec![rev1, rev2];
        let selected = extract_pre_task_slice(&completed_snap, "T1").expect("slice present");
        assert_eq!(selected["ver"], "accepted-rev-2");
    }
}
