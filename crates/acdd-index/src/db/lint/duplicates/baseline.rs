use super::types::{effective_stmt_hash, path_matches_probe, SymbolDuplicateReport};
use crate::models::{details::NodeDetailKey, tags::NodeKind, StatementFingerprint};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

type CallableFingerprintSeq = Vec<(u64, u64, usize)>;
type BaselineCallableEntry = (
    String,
    String,
    Option<String>,
    CallableFingerprintSeq,
    CallableFingerprintSeq,
);

fn statement_seq(stmts: &[StatementFingerprint], include_skeleton: bool) -> CallableFingerprintSeq {
    stmts
        .iter()
        .map(|s| {
            let skel = if include_skeleton {
                effective_stmt_hash(s, true)
            } else {
                0
            };
            (s.hash, skel, s.token_count)
        })
        .collect()
}

/// Computes the set of candidate indices in `diff_paths` that are newly introduced or
/// structurally modified compared to `baseline_ref` (immune to comments and line shifts).
pub(super) fn changed_candidates_vs_baseline(
    report: &SymbolDuplicateReport,
    diff_paths: &[String],
    baseline_ref: &str,
    workspace_root: Option<&Path>,
) -> HashSet<usize> {
    let diff_candidate_indices: Vec<usize> = report
        .candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, c)| path_matches_probe(&c.path, "", Some(diff_paths)).then_some(idx))
        .collect();
    let Some(root) = workspace_root else {
        return diff_candidate_indices.into_iter().collect();
    };
    if diff_candidate_indices.is_empty() {
        return HashSet::new();
    }
    let unique_paths: Vec<String> = diff_candidate_indices
        .iter()
        .map(|&idx| report.candidates[idx].path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let specs: Vec<(&str, &str)> = unique_paths
        .iter()
        .map(|p| (baseline_ref, p.as_str()))
        .collect();
    let Ok(blobs) = acdd_core::git::read_git_blobs_at_paths_batch(root, &specs) else {
        return diff_candidate_indices.into_iter().collect();
    };
    // Map path -> Option<HashSet<BaselineCallableEntry>>
    // None means the file did not exist at baseline_ref (new file).
    let mut baseline_by_path: HashMap<&str, Option<HashSet<BaselineCallableEntry>>> =
        HashMap::with_capacity(unique_paths.len());
    for (path, blob_res) in unique_paths.iter().zip(blobs) {
        let Some(source) = blob_res.ok().flatten() else {
            baseline_by_path.insert(path.as_str(), None);
            continue;
        };
        let Some(lang) = crate::languages::for_path(Path::new(path)) else {
            baseline_by_path.insert(path.as_str(), None);
            continue;
        };
        let Ok(typed) = crate::ast::extract_typed(path, lang.id(), &source) else {
            baseline_by_path.insert(path.as_str(), None);
            continue;
        };
        let mut fp_map: HashMap<&str, &[StatementFingerprint]> = HashMap::new();
        for entry in &typed.facts.statement_fingerprints {
            fp_map.insert(entry.node_id.as_str(), entry.statements.as_slice());
        }
        let impl_trait_by_owner: HashMap<&str, &str> = typed
            .facts
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Impl)
            .filter_map(|n| {
                n.details
                    .get(NodeDetailKey::IMPL_TRAIT)
                    .and_then(|v| v.as_str())
                    .map(|tr| (n.id.as_str(), tr))
            })
            .collect();
        let mut method_impl_trait: HashMap<&str, &str> = HashMap::new();
        for edge in &typed.facts.edges {
            if edge.kind == "contains" {
                if let Some(&tr) = impl_trait_by_owner.get(edge.src.as_str()) {
                    method_impl_trait.insert(edge.dst.as_str(), tr);
                }
            }
        }
        let mut callables = HashSet::new();
        for node in &typed.facts.nodes {
            if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                continue;
            }
            let stmts = fp_map.get(node.id.as_str()).copied().unwrap_or(&[]);
            let resolved_trait = node
                .details
                .get(NodeDetailKey::IMPL_TRAIT)
                .and_then(|v| v.as_str())
                .or_else(|| method_impl_trait.get(node.id.as_str()).copied())
                .map(str::to_owned);
            callables.insert((
                node.qualname.clone(),
                node.kind.as_str().to_owned(),
                resolved_trait,
                statement_seq(stmts, true),
                statement_seq(stmts, false),
            ));
        }
        baseline_by_path.insert(path.as_str(), Some(callables));
    }

    let mut changed = HashSet::new();
    for idx in diff_candidate_indices {
        let cand = &report.candidates[idx];
        let live_stmts = &report.candidate_statements[idx];
        let Some(Some(baseline_set)) = baseline_by_path.get(cand.path.as_str()) else {
            changed.insert(idx);
            continue;
        };
        let has_skeleton = live_stmts.iter().any(|s| s.skeleton_hash != 0);
        let live_seq = statement_seq(live_stmts, has_skeleton);
        let unchanged = baseline_set
            .iter()
            .any(|(qualname, kind, impl_trait, with_skel, no_skel)| {
                qualname == &cand.qualname
                    && kind == &cand.kind
                    && impl_trait == &cand.impl_trait
                    && if has_skeleton {
                        with_skel == &live_seq
                    } else {
                        no_skel == &live_seq
                    }
            });
        if !unchanged {
            changed.insert(idx);
        }
    }
    changed
}
