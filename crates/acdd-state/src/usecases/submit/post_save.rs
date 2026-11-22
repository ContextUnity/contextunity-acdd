//! Post-save Git persistence (`save.post`) and container blackboard cleanup.

use crate::git_ops::commit::{
    archive_spec_with_git, create_scoped_commit_with_expected, current_branch_name,
    current_head_commit, is_protected_main_branch, ArchiveCommitReq, ScopedCommitReq,
};
use crate::git_ops::snapshot::{save_task_snapshot, snapshot_prefix};
use crate::git_ops::worktree::{cleanup_locked_refs, lock_cleanup_repos, try_lock_secondary_repo, RepoWorktreeLayout};
use crate::machine::{plan_container_blackboard_cleanup, MachineGateDef};
use crate::spec::{is_safe_path_segment_id, render_archive_markdown};
use crate::store::traits::StateBackend;
use crate::types::{
    BlackboardListReq, BlackboardRecord, BlackboardStatus, BlackboardTopic, GateNodeRecord,
    RunGraphSnapshot, SaveAction, TransitionPlan,
};
use crate::usecases::archive_notes::{build_archive_render_input, build_provisional_receipt};
use crate::usecases::dto::SubmitReq;
use crate::usecases::lifecycle::{compute_archive_relpath, read_confined_utf8, snapshot_worktree_layout};
use crate::usecases::queries::extract_predecessor_gate_proofs;
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{atomic_write, checked_child, is_normalized_relative};
use acdd_core::ids::{CommitOid, NodeCursor, RunId, SubtaskRef, TreeOid};
use acdd_core::path_policy::{has_json_extension, WorkspacePathPolicy};
use acdd_core::subsystems::SubsystemId;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

pub(super) type PersistPostSaveOutcome = (Option<CommitOid>, Option<String>, Option<String>);

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_post_save_git(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    locked: Option<&RepoWorktreeLayout>,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    protected_paths: &[String],
    current_tree: &TreeOid,
    expected_scoped_tree: &TreeOid,
    now_utc: &str,
    req: &SubmitReq,
    policy: &WorkspacePathPolicy,
) -> Result<PersistPostSaveOutcome, StateError> {
    let should_commit = machine_gate.save.post == SaveAction::Commit;
    let should_snapshot = machine_gate.save.post == SaveAction::Snapshot;
    let snapshot_task = node.task_ref.as_ref().filter(|_| should_snapshot || machine_gate.receipt);
    let target = snapshot_task.map(|_| snapshot_worktree_layout(config, locked, target_worktree)).transpose()?;
    let repos = if snapshot_task.is_some() && machine_gate.receipt {
        Some(lock_cleanup_repos(locked, config.linked_workspaces.values())?)
    } else {
        None
    };
    let _guard = if repos.is_none() {
        target.as_ref().map(|layout| try_lock_secondary_repo(locked, layout)).transpose()?
    } else {
        None
    };
    let report_rel = machine_gate
        .save
        .report
        .as_deref()
        .map(|tpl| expand_report_relpath(tpl, snapshot, node, protected_paths, policy))
        .transpose()?;

    // Pre-validate report destination confinement and serialization BEFORE any Git mutation.
    if let Some(ref rel) = report_rel {
        let dest = checked_child(primary, Path::new(rel)).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid confined save.report path '{rel}': {err}"),
            )
        })?;
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("save.report target '{}' must be a regular file", dest.display()),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed inspecting save.report '{}': {err}", dest.display()),
                ));
            }
        }
        let _ = render_gate_report(
            rel,
            config,
            backend,
            snapshot,
            node,
            machine_gate,
            cursor,
            current_tree,
            None,
            now_utc,
            req,
        )?;
    }

    let (committed_oid, archive_path) = if machine_gate.save.archive && !snapshot.run.is_ephemeral {
        let spec_rel = snapshot.run.spec_path.as_deref().ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "non-ephemeral run missing spec_path for archive",
            )
        })?;
        let archive_rel =
            compute_archive_relpath(config, snapshot.run.collection.as_deref(), spec_rel)?;
        let original_raw = read_confined_utf8(target_worktree, spec_rel)
            .or_else(|_| read_confined_utf8(primary, spec_rel))?;

        let provisional_receipt = build_provisional_receipt(
            snapshot,
            node,
            machine_gate,
            cursor,
            current_tree,
            None,
            now_utc,
            req,
        );
        let blackboard_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
        let render_input = build_archive_render_input(
            snapshot,
            Some(&provisional_receipt),
            &blackboard_items,
            None,
            now_utc,
            req.summary.as_deref(),
        );
        let (rendered_md, rendered_digest) = render_archive_markdown(&original_raw, &render_input)?;

        if should_commit {
            let branch = current_branch_name(target_worktree)?;
            if is_protected_main_branch(branch.as_deref()) && !req.allow_main {
                return Err(StateError::new(
                    ErrorCode::MainBranchProtected,
                    format!(
                        "refusing archive commit onto protected branch '{}' without --allow-main",
                        branch.as_deref().unwrap_or("main")
                    ),
                ));
            }
        }

        let arch_resp = archive_spec_with_git(&ArchiveCommitReq {
            worktree: target_worktree.to_path_buf(),
            run_id: snapshot.run.run_id.clone(),
            node_id: node.node_id.clone(),
            claim_revision: node.claim_revision,
            run_base_head: snapshot.run.target_base_commit.clone(),
            spec_relpath: spec_rel.to_owned(),
            archive_relpath: archive_rel,
            rendered_archive_markdown: rendered_md,
            rendered_archive_digest: rendered_digest,
            should_commit,
            allow_main: req.allow_main,
            commit_message: format!(
                "acdd(archive): close {} ({})",
                snapshot.run.run_name, cursor
            ),
        })?;
        (arch_resp.committed_oid, Some(arch_resp.archive_path))
    } else if should_commit {
        let branch = current_branch_name(target_worktree)?;
        if is_protected_main_branch(branch.as_deref()) && !req.allow_main {
            return Err(StateError::new(
                ErrorCode::MainBranchProtected,
                format!(
                    "refusing scoped commit onto protected branch '{}' without --allow-main",
                    branch.as_deref().unwrap_or("main")
                ),
            ));
        }
        let base_head = match node.baseline_commit.clone() {
            Some(c) => c,
            None => current_head_commit(target_worktree)?,
        };
        let commit_scope = if admitted_scope.is_empty() {
            vec!["**".to_owned()]
        } else {
            admitted_scope.to_vec()
        };
        let commit_resp = create_scoped_commit_with_expected(
            &ScopedCommitReq {
                worktree: target_worktree.to_path_buf(),
                run_id: snapshot.run.run_id.clone(),
                node_id: node.node_id.clone(),
                claim_revision: node.claim_revision,
                base_head,
                admitted_scope: commit_scope,
                path_policy: policy.clone(),
                message: format!("acdd({cursor}): accept gate"),
                allow_main: req.allow_main,
            },
            Some(expected_scoped_tree),
        )?;
        (commit_resp.committed_oid, None)
    } else {
        (None, None)
    };

    let written_report = report_rel
        .as_deref()
        .map(|rel| {
            materialize_report_file(
                rel,
                config,
                backend,
                primary,
                snapshot,
                node,
                machine_gate,
                cursor,
                current_tree,
                committed_oid.as_ref(),
                now_utc,
                req,
            )
        })
        .transpose()?;

    if let Some(task) = snapshot_task {
        let prefix = snapshot_prefix(&snapshot.run.run_id, Some(task));
        if should_snapshot {
            let reference = format!("{prefix}r{}", node.claim_revision);
            save_task_snapshot(target_worktree, &reference, expected_scoped_tree, false)?;
        }
        if machine_gate.receipt {
            cleanup_locked_refs(repos.as_ref().expect("receipt repositories locked"), &snapshot.run.run_id, Some(prefix), None, None, None)?;
        }
    }

    if archive_path.is_some() {
        acdd_core::failpoint::hit("submit_archive_after_git");
    } else if should_commit {
        acdd_core::failpoint::hit("submit_commit_after_git");
    }

    Ok((committed_oid, archive_path, written_report))
}

fn expand_report_relpath(
    template: &str,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    protected_paths: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<String, StateError> {
    let spec_id = snapshot.run.spec_id.as_deref().unwrap_or("goal");
    for (label, value) in [
        ("run_id", snapshot.run.run_id.as_str()),
        ("run_name", snapshot.run.run_name.as_str()),
        ("gate_id", node.gate_id.as_str()),
        ("spec_id", spec_id),
    ] {
        if !is_safe_path_segment_id(value) || value.contains(['{', '}']) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "gate '{}' cannot expand unsafe save.report placeholder '{{{label}}}' = '{value}'",
                    node.gate_id
                ),
            ));
        }
    }
    let rendered = template
        .trim()
        .replace("{run_id}", snapshot.run.run_id.as_str())
        .replace("{run_name}", snapshot.run.run_name.as_str())
        .replace("{gate_id}", node.gate_id.as_str())
        .replace("{spec_id}", spec_id)
        .replace('\\', "/");
    if rendered.is_empty()
        || rendered.contains(['{', '}'])
        || !is_normalized_relative(Path::new(&rendered))
        || !policy.is_valid_report_path(&rendered, protected_paths)
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "gate '{}' rendered invalid or prohibited save.report path '{rendered}'",
                node.gate_id
            ),
        ));
    }
    Ok(rendered)
}

#[allow(clippy::too_many_arguments)]
fn materialize_report_file(
    report_rel: &str,
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    primary: &Path,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    committed_oid: Option<&CommitOid>,
    now_utc: &str,
    req: &SubmitReq,
) -> Result<String, StateError> {
    let dest = checked_child(primary, Path::new(report_rel)).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid confined save.report path '{report_rel}': {err}"),
        )
    })?;
    let bytes = render_gate_report(
        report_rel,
        config,
        backend,
        snapshot,
        node,
        machine_gate,
        cursor,
        current_tree,
        committed_oid,
        now_utc,
        req,
    )?;
    atomic_write(&dest, &bytes, false).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to write save.report '{}': {err}", dest.display()),
        )
    })?;
    Ok(report_rel.to_owned())
}

#[allow(clippy::too_many_arguments)]
fn render_gate_report(
    report_rel: &str,
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    committed_oid: Option<&CommitOid>,
    now_utc: &str,
    req: &SubmitReq,
) -> Result<Vec<u8>, StateError> {
    let proof_value = req.custom_evidence.clone().or_else(|| {
        req.review_evidence
            .as_ref()
            .and_then(|rev| serde_json::to_value(rev).ok())
    });
    let predecessor_proofs = extract_predecessor_gate_proofs(snapshot, node);
    let receipt = machine_gate.receipt.then(|| {
        build_provisional_receipt(
            snapshot,
            node,
            machine_gate,
            cursor,
            current_tree,
            committed_oid.cloned(),
            now_utc,
            req,
        )
    });
    let blackboard_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    let highlights: Vec<&BlackboardRecord> = blackboard_items
        .iter()
        .filter(|b| {
            matches!(
                b.topic,
                BlackboardTopic::Findings
                    | BlackboardTopic::Decisions
                    | BlackboardTopic::Deferred
                    | BlackboardTopic::Blockers
            ) && !b.payload.trim().is_empty()
                && !matches!(b.status, BlackboardStatus::Resolved)
        })
        .collect();

    if has_json_extension(report_rel) {
        let payload = json!({
            "schema_version": 1,
            "run_id": snapshot.run.run_id,
            "run_name": snapshot.run.run_name,
            "profile_id": snapshot.run.profile_id,
            "spec_id": snapshot.run.spec_id,
            "spec_path": snapshot.run.spec_path,
            "gate_id": node.gate_id,
            "node_cursor": cursor.to_string(),
            "claim_revision": node.claim_revision,
            "worker_id": req.worker_id,
            "accepted_at": now_utc,
            "candidate_tree": current_tree,
            "committed_oid": committed_oid,
            "report_path": report_rel,
            "summary": req.summary,
            "proof": proof_value,
            "predecessor_proofs": predecessor_proofs,
            "receipt": receipt,
            "verified_invariants": req.verified_invariants,
            "blackboard": highlights,
        });
        let text = serde_json::to_string_pretty(&payload).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize save.report JSON: {err}"),
            )
        })?;
        return Ok(format!("{text}\n").into_bytes());
    }

    let mut md = String::new();
    let _ = writeln!(md, "# Report: {} ({})\n", snapshot.run.run_name, cursor);
    let _ = writeln!(md, "- **Run ID:** `{}`", snapshot.run.run_id);
    let _ = writeln!(md, "- **Profile:** `{}`", snapshot.run.profile_id);
    if let Some(ref sid) = snapshot.run.spec_id {
        let _ = writeln!(md, "- **Spec ID:** `{sid}`");
    }
    if let Some(ref sp) = snapshot.run.spec_path {
        let _ = writeln!(md, "- **Spec Path:** `{sp}`");
    }
    let _ = writeln!(md, "- **Worker:** `{}`", req.worker_id);
    let _ = writeln!(md, "- **Accepted At:** `{now_utc}`");
    let _ = writeln!(md, "- **Candidate Tree:** `{current_tree}`");
    if let Some(oid) = committed_oid {
        let _ = writeln!(md, "- **Committed OID:** `{oid}`");
    }
    if !req.verified_invariants.is_empty() {
        let _ = writeln!(
            md,
            "- **Verified Invariants:** {}",
            req.verified_invariants
                .iter()
                .map(|i| format!("`{i}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if let Some(summary) = req.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        let _ = writeln!(md, "\n## Summary\n\n{summary}");
    }

    if let Some(ref proof) = proof_value {
        let _ = writeln!(md, "\n## Gate Proof (`{}`)\n", node.gate_id);
        if let Some(exec_sum) = proof.get("executive_summary").and_then(Value::as_str) {
            if !exec_sum.trim().is_empty() {
                let _ = writeln!(md, "{exec_sum}\n");
            }
        }
        let pretty = serde_json::to_string_pretty(proof).unwrap_or_default();
        let _ = writeln!(md, "```json\n{pretty}\n```");
    }

    if let Some(ref preds) = predecessor_proofs {
        let _ = writeln!(md, "\n## Predecessor Stage Proofs");
        for (pred_gate, pred_val) in preds {
            let pretty = serde_json::to_string_pretty(pred_val).unwrap_or_default();
            let _ = writeln!(md, "\n### `{pred_gate}`\n\n```json\n{pretty}\n```");
        }
    }

    if !highlights.is_empty() {
        let _ = writeln!(md, "\n## Blackboard Highlights\n");
        for item in highlights.iter().take(20) {
            let _ = writeln!(
                md,
                "- **[{} · {}]** `{}`: {}",
                item.topic.as_str(),
                item.status.as_str(),
                item.author,
                item.payload.trim()
            );
        }
    }

    Ok(md.into_bytes())
}

pub(super) fn list_all_blackboard_items(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    run_id: &RunId,
) -> Result<Vec<BlackboardRecord>, StateError> {
    if !config.modules.is_subsystem_enabled(SubsystemId::Blackboard) {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = backend.list_blackboard(
            run_id,
            &BlackboardListReq {
                limit: 100,
                offset,
                ..BlackboardListReq::default()
            },
        )?;
        let fetched = page.items.len();
        items.extend(page.items);
        if fetched == 0 || items.len() >= page.total {
            break;
        }
        offset += fetched;
    }
    Ok(items)
}

pub(super) fn attach_container_blackboard_cleanup(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    plan: &mut TransitionPlan,
) -> Result<(), StateError> {
    if !machine_gate.receipt || !config.modules.is_subsystem_enabled(SubsystemId::Blackboard) {
        return Ok(());
    }
    let Some(ref container_id) = node.parent_id else {
        return Ok(());
    };
    let all_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    let patches = plan_container_blackboard_cleanup(snapshot, container_id, &all_items);

    let mut seen_patch_ids: BTreeSet<i64> =
        plan.blackboard_patches.iter().map(|p| p.id).collect();
    for patch in patches {
        if seen_patch_ids.insert(patch.id) {
            plan.blackboard_patches.push(patch);
        }
    }

    // Track already-existing run-level CarriedOver Deferred payloads to prevent duplication on retry.
    let mut existing_carried: BTreeSet<(Option<SubtaskRef>, String)> = all_items
        .iter()
        .filter(|item| {
            item.node_id.is_none()
                && item.topic == BlackboardTopic::Deferred
                && item.status == BlackboardStatus::CarriedOver
        })
        .map(|item| (item.subtask_ref.clone(), item.payload.clone()))
        .collect();
    for bb in &plan.new_blackboard {
        if bb.node_id.is_none()
            && bb.topic == BlackboardTopic::Deferred
            && bb.status == BlackboardStatus::CarriedOver
        {
            existing_carried.insert((bb.subtask_ref.clone(), bb.payload.clone()));
        }
    }

    // Promote carried-over deferred items at run level by inserting carried-over notice if needed.
    for p in &plan.blackboard_patches {
        if p.status == BlackboardStatus::CarriedOver {
            if let Some(src) = all_items.iter().find(|i| i.id == Some(p.id)) {
                let key = (src.subtask_ref.clone(), src.payload.clone());
                if existing_carried.insert(key) {
                    plan.new_blackboard.push(BlackboardRecord {
                        id: None,
                        run_id: snapshot.run.run_id.clone(),
                        node_id: None,
                        recipient_node_id: None,
                        subtask_ref: src.subtask_ref.clone(),
                        gate: src.gate.clone(),
                        author: src.author.clone(),
                        topic: BlackboardTopic::Deferred,
                        status: BlackboardStatus::CarriedOver,
                        payload: src.payload.clone(),
                        created_at: acdd_core::telemetry::now_utc_iso_ms(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::MachineGateDef;
    use crate::store::memory::MemoryStateBackend;
    use crate::store::traits::exec_tx_with;
    use crate::types::StoragePreconditions;
    use crate::usecases::StateEngineConfig;
    use acdd_core::ids::{GateId, SubtaskRef};
    use acdd_core::response::ResponsePolicy;
    use acdd_core::subsystems::SubsystemRegistry;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn blackboard_cleanup_paginates_beyond_100_without_duplicating_carried_over() {
        let run_id = RunId::new("run-bb-page").expect("run_id");
        let snapshot = crate::usecases::archive_notes::tests::sample_snapshot(&run_id);
        let backend = MemoryStateBackend::new().expect("memory backend");
        let config = StateEngineConfig {
            workspace_root: Path::new("/tmp/ws").to_path_buf(),
            worktree: Path::new("/tmp/ws").to_path_buf(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: BTreeMap::new(),
            profiles_dirs: Vec::new(),
            linked_workspaces: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            response_policy: ResponsePolicy::default(),
            retain_traces: 5,
            max_snapshot_file_bytes: None,
            allowed_binary_extensions: None,
        };
        let s1 = SubtaskRef::new("S1").expect("subtask_ref");
        exec_tx_with(&backend, |tx| {
            tx.insert_run_graph(&snapshot)?;
            // Insert >100 container draft notes plus an already carried-over deferred item.
            for idx in 0..105 {
                tx.insert_blackboard(&BlackboardRecord {
                    id: None,
                    run_id: run_id.clone(),
                    node_id: Some("task:T1".to_owned()),
                    recipient_node_id: None,
                    subtask_ref: None,
                    gate: None,
                    author: "w1".to_owned(),
                    topic: BlackboardTopic::Draft,
                    status: BlackboardStatus::Open,
                    payload: format!("draft-{idx}"),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                })?;
            }
            tx.insert_blackboard(&BlackboardRecord {
                id: None,
                run_id: run_id.clone(),
                node_id: Some("task:T1".to_owned()),
                recipient_node_id: None,
                subtask_ref: Some(s1.clone()),
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Deferred,
                status: BlackboardStatus::Open,
                payload: "defer-once".to_owned(),
                created_at: "2026-01-01T00:00:01Z".to_owned(),
            })?;
            tx.insert_blackboard(&BlackboardRecord {
                id: None,
                run_id: run_id.clone(),
                node_id: None,
                recipient_node_id: None,
                subtask_ref: Some(s1),
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Deferred,
                status: BlackboardStatus::CarriedOver,
                payload: "defer-once".to_owned(),
                created_at: "2026-01-01T00:00:02Z".to_owned(),
            })?;
            Ok(())
        })
        .expect("seed >100 bb");

        let mut plan = TransitionPlan {
            preconditions: StoragePreconditions {
                expected_run_generation: 1,
                expected_node_versions: vec![],
                expected_claim_revision: None,
                expected_contract_revision: None,
                expected_worker_id: None,
                required_predecessor_trees: vec![],
            },
            bump_run_generation: false,
            new_run_status: None,
            new_root_cycle_count: None,
            completed_at: None,
            node_patches: vec![],
            dependency_patches: vec![],
            claim_patches: vec![],
            new_artifacts: vec![],
            new_blackboard: vec![],
            blackboard_patches: vec![],
            new_subtasks: vec![],
            significant_transition: false,
        };
        let deliver = &snapshot.nodes[1];
        let mut gate = MachineGateDef::minimal(GateId::new("deliver").expect("gate_id"));
        gate.receipt = true;

        attach_container_blackboard_cleanup(&config, &backend, &snapshot, deliver, &gate, &mut plan)
            .expect("cleanup pagination");

        // All 105 drafts + 1 deferred container item are patched (> 100!).
        assert_eq!(plan.blackboard_patches.len(), 106);
        // Existing run-level CarriedOver record prevented duplication.
        assert!(plan.new_blackboard.is_empty());
    }
}
