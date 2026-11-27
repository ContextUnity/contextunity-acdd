//! Run-level mutation handlers (`draft`, `sync`, `clear`).

use crate::context::slices::read_spec_readonly;
use crate::git_ops::worktree::remove_worktree_and_branch;
use crate::machine::{evaluate_transition, SyncInput, TransitionInput, TransitionOutcome};
use crate::observability::build_completed_run_digest;
use crate::workflow::inherit::resolve_workflow_for_rebind;
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArchiveCleanupEntry, RunTombstoneRecord, StoragePreconditions, TerminalStatus,
    ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::collect_external_satisfied_deps;
use crate::usecases::dto::{ClearReq, ClearResp, DraftReq, DraftResp, SyncReq, SyncResp};
use crate::usecases::lifecycle::{
    apply_effective_isolation, build_tombstone_cleanup_manifest_from_snapshot, checkout_config,
    cleanup_tombstone, effective_spec_collections, ensure_state_enabled, is_managed_run_branch,
    load_active_run_graph, primary_workspace_root, read_confined_utf8, resolve_active_run,
    resolve_source_checkout, to_machine_profile, validate_profile_fail_closed,
    with_repo_lock_and_maintenance,
};
use acdd_core::config::{Adapter, ACDD_DIR};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::ports::CompositeDomainProvider;
use acdd_spec::parse_spec;
use serde_json::json;
use std::path::Path;

use super::common::build_machine_spec_input;

pub(crate) fn handle_draft_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: DraftReq,
) -> Result<DraftResp, AcddError> {
    ensure_state_enabled(config)?;
    let collections = effective_spec_collections(config);
    let primary = primary_workspace_root(config);
    let overlay = crate::usecases::overlay::BackendDraftOverlay(backend);
    acdd_spec::draft_spec(&primary, &collections, &req, &overlay)
}

pub(crate) fn handle_sync_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SyncReq,
    source_override: Option<&Adapter>,
) -> Result<SyncResp, AcddError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        let caller = resolve_source_checkout(primary, layout, &config.worktree, false)?;
        let loaded = load_active_run_graph(config, backend, req.run.as_deref(), None)?;
        let snapshot = loaded.snapshot;
        let current_resolved = loaded.profile;
        let current_machine = loaded.machine_profile;

        let spec_rel = if let Some(ref sp) = snapshot.run.spec_path {
            sp.clone()
        } else {
            format!("{ACDD_DIR}/run/{}/spec.md", snapshot.run.run_id)
        };
        let source = if snapshot.run.is_ephemeral {
            primary.to_path_buf()
        } else if let Some(path) = snapshot.run.worktree.as_deref() {
            resolve_source_checkout(primary, layout, Path::new(path), false)?
        } else {
            caller
        };
        let raw_spec = read_confined_utf8(&source, &spec_rel)?;
        let doc = parse_spec(&raw_spec)?;
        let checkout = checkout_config(config, primary, &source, source_override)?;

        let (new_machine_profile, new_profile_pair_owned, rebound_resolved) = if req.rebind_workflow
        {
            let mut refreshed = resolve_workflow_for_rebind(
                &snapshot.run.profile_id,
                Some(&source),
                Some(&config.modules),
                Some(&checkout.workflows_dirs),
            )?;
            apply_effective_isolation(
                &mut refreshed,
                snapshot.run.isolation,
                current_resolved.git.worktrees,
            );
            validate_profile_fail_closed(config, &refreshed, provider, true)?;
            let m = to_machine_profile(&refreshed)?;
            let pid = m.profile_id.clone();
            let pjson = m.profile_json.clone();
            (Some(m), Some((pid, pjson)), Some(refreshed))
        } else {
            (None, None, None)
        };

        let effective_profile = rebound_resolved.as_ref().unwrap_or(&current_resolved);
        crate::validate::validate_spec_for_run(
            &checkout,
            backend,
            provider,
            &doc,
            effective_profile,
        )?;

        let primary_str = primary.to_string_lossy().into_owned();
        let machine_spec =
            build_machine_spec_input(&doc, &primary_str, &snapshot.run.run_id, &checkout)?;
        let ext_deps = collect_external_satisfied_deps(&checkout, &snapshot)?;
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &snapshot,
            &current_machine,
            &obs,
            &TransitionInput::Sync(SyncInput {
                spec: machine_spec,
                new_profile: new_machine_profile,
                rebind_workflow: req.rebind_workflow,
            }),
        )?;
        let TransitionOutcome::Sync(sync_plan) = outcome else {
            unreachable!();
        };

        let added_nodes = sync_plan.added_nodes.len();
        let updated_nodes = sync_plan.transition.node_patches.len();
        let spec_digest = sync_plan.spec_digest.clone();
        let doc_id = doc.id.clone();

        exec_tx_with(backend, |tx| {
            let pair = new_profile_pair_owned
                .as_ref()
                .map(|(id, json)| (id.as_str(), json.as_str()));
            tx.apply_sync_plan(&snapshot.run.run_id, &sync_plan, pair)?;
            tx.update_run_spec_id(&snapshot.run.run_id, Some(doc_id.as_str()))
        })?;

        if let Some(ref rebound) = rebound_resolved {
            if let Ok(resolved_yaml) = serde_yaml::to_string(rebound) {
                let rel = format!(
                    "{ACDD_DIR}/run/{}/profile.resolved.yaml",
                    snapshot.run.run_id
                );
                if let Ok(dest) = checked_child(primary, Path::new(&rel)) {
                    let _ = atomic_write(&dest, resolved_yaml.as_bytes(), false);
                }
            }
        }

        Ok(SyncResp {
            run_id: snapshot.run.run_id,
            spec_digest,
            rebound_workflow: req.rebind_workflow,
            added_nodes,
            updated_nodes,
        })
    })
}

pub(crate) fn handle_clear_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ClearReq,
) -> Result<ClearResp, AcddError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| AcddError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;

        // Never delete the user's non-ephemeral spec file on `clear`; only clean worktrees, branches (including `run.branch`), refs, and ephemeral spec.
        let mut cleanup_manifest =
            build_tombstone_cleanup_manifest_from_snapshot(config, &snapshot, primary);
        cleanup_manifest.retain(|entry| entry.kind != "spec_file");
        if let Some(ref run_br) = snapshot.run.branch {
            if is_managed_run_branch(&snapshot.run, run_br)
                && snapshot.run.target_base.as_ref() != Some(run_br)
                && !cleanup_manifest
                    .iter()
                    .any(|e| e.kind == "branch" && e.path_or_ref == *run_br)
            {
                cleanup_manifest.push(ArchiveCleanupEntry {
                    kind: "branch".to_owned(),
                    path_or_ref: run_br.clone(),
                    workspace: None,
                    force: req.force,
                });
            }
        }
        if snapshot.run.is_ephemeral {
            cleanup_manifest.push(ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: format!("{ACDD_DIR}/run/{}/spec.md", snapshot.run.run_id),
                workspace: None,
                force: false,
            });
        }

        // Execute Git worktree/branch removals BEFORE pruning active SQLite run state so Git natively
        // rejects dirty worktrees or unmerged branches when `!req.force` without losing active state.
        let mut cleaned_items = Vec::new();
        for entry in cleanup_manifest.iter().filter(|e| e.kind == "worktree") {
            remove_worktree_and_branch(
                primary,
                Some(Path::new(&entry.path_or_ref)),
                None,
                req.force,
            )?;
            cleaned_items.push(entry.path_or_ref.clone());
        }
        for entry in cleanup_manifest.iter().filter(|e| e.kind == "branch") {
            remove_worktree_and_branch(primary, None, Some(&entry.path_or_ref), req.force)?;
            cleaned_items.push(entry.path_or_ref.clone());
        }

        let remaining_manifest: Vec<ArchiveCleanupEntry> = cleanup_manifest
            .into_iter()
            .filter(|e| !matches!(e.kind.as_str(), "worktree" | "branch"))
            .collect();

        let spec_doc = read_spec_readonly(config, &snapshot.run);
        let closed_at = acdd_core::telemetry::now_utc_iso_ms();
        let tombstone_seed = RunTombstoneRecord {
            run_id: snapshot.run.run_id.clone(),
            run_name: snapshot.run.run_name.clone(),
            collection: snapshot.run.collection.clone(),
            workspace: snapshot.run.workspace.clone(),
            spec_id: spec_doc.as_ref().map(|d| d.id.clone()),
            title: spec_doc
                .as_ref()
                .map(|d| d.title.clone())
                .unwrap_or_else(|| snapshot.run.run_name.as_str().to_owned()),
            profile_id: snapshot.run.profile_id.clone(),
            is_ephemeral: snapshot.run.is_ephemeral,
            generation: snapshot.run.generation,
            terminal_status: TerminalStatus::Cleared,
            target_base: snapshot.run.target_base.clone(),
            target_base_commit: snapshot.run.target_base_commit.clone(),
            archive_path: None,
            closed_commit: None,
            cleanup_pending: true,
            cleanup_manifest: remaining_manifest,
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at,
        };

        let (_, mut summary_json, metrics_json) =
            build_completed_run_digest(&snapshot, &tombstone_seed, None)?;
        if let Some(ref reason) = req.reason {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&summary_json) {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("cancellation_reason".to_owned(), json!(reason));
                    summary_json = v.to_string();
                }
            }
        }
        let tombstone = RunTombstoneRecord {
            summary_json,
            metrics_json,
            ..tombstone_seed
        };

        let preconditions = StoragePreconditions {
            expected_run_generation: snapshot.run.generation,
            expected_node_versions: Vec::new(),
            expected_claim_revision: None,
            expected_contract_revision: None,
            expected_worker_id: None,
            required_predecessor_trees: Vec::new(),
        };

        exec_tx_with(backend, |tx| {
            tx.archive_and_prune_run(&preconditions, &tombstone)
        })?;

        cleaned_items.extend(cleanup_tombstone(config, backend, &tombstone, layout)?);
        let final_tombstone = backend
            .load_tombstone(&tombstone.run_id)?
            .unwrap_or(tombstone);
        Ok(ClearResp {
            tombstone: final_tombstone,
            cleaned_items,
        })
    })
}
