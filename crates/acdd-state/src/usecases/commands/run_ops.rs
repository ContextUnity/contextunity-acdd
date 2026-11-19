//! Run-level mutation handlers (`draft`, `sync`, `clear`).

use crate::git_ops::worktree::remove_worktree_and_branch;
use crate::machine::{
    evaluate_transition, SyncInput, TransitionInput,
    TransitionOutcome,
};
use crate::observability::build_completed_run_digest;
use crate::profile::inherit::resolve_profile_for_rebind;
use crate::spec::{
    extract_spec_number, format_next_spec_id, parse_spec,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArchiveCleanupEntry, RunTombstoneRecord, StoragePreconditions, TerminalStatus,
    ValidatedObservations,
};
use crate::usecases::dto::{
    ClearReq, ClearResp, DraftReq, DraftResp, SyncReq, SyncResp,
};
use crate::usecases::lifecycle::{
    apply_effective_isolation, build_tombstone_cleanup_manifest_from_snapshot, checkout_config,
    cleanup_tombstone, collect_external_satisfied_deps, effective_spec_collections,
    ensure_state_enabled, is_managed_run_branch, parse_profile_from_run, primary_workspace_root,
    read_confined_utf8, resolve_active_run, resolve_source_checkout,
    sorted_dir_paths,
    to_machine_profile, validate_profile_fail_closed, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::read_spec_readonly;
use crate::usecases::StateEngineConfig;
use acdd_core::config::Adapter;
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::path::Path;

use super::{build_machine_spec_input, sanitize_slug};

pub(crate) fn handle_draft_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: DraftReq,
) -> Result<DraftResp, StateError> {
    ensure_state_enabled(config)?;
    let collections = effective_spec_collections(config);
    let col = collections.get(&req.collection).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("unknown spec collection '{}'", req.collection),
        )
    })?;

    let slug = sanitize_slug(&req.slug)?;
    let draft_dir = col.default_draft_dir().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "spec collection '{}' has no literal non-glob directory for drafting",
                req.collection
            ),
        )
    })?;
    let primary = primary_workspace_root(config);

    let (spec_id, spec_rel) = if col.prefixes.is_empty() && req.prefix.is_none() {
        (slug.clone(), format!("{draft_dir}/{slug}.md"))
    } else {
        let prefix = match req.prefix {
            Some(ref p) => {
                if !col.prefixes.is_empty() && !col.prefixes.contains(p) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "prefix '{p}' is not allowed for collection '{}' (allowed: {:?})",
                            req.collection, col.prefixes
                        ),
                    ));
                }
                p.clone()
            }
            None => col
                .prefixes
                .first()
                .cloned()
                .unwrap_or_else(|| "SPEC".to_owned()),
        };

        let mut max_num = backend.max_spec_number(&req.collection, &prefix)?;
        let resolved_dirs = col.resolve_dirs(&primary).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to resolve directories for '{}': {err}", req.collection),
            )
        })?;
        for dir_abs in resolved_dirs {
            for scan_dir in [dir_abs.clone(), dir_abs.join("archive")] {
                if std::fs::symlink_metadata(&scan_dir)
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    continue;
                }
                for p in sorted_dir_paths(&scan_dir) {
                    if p.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if let Some(num) = extract_spec_number(stem, &prefix) {
                            max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
                        }
                    }
                    if let Ok(rel) = p.strip_prefix(&primary) {
                        if let Ok(raw) = read_confined_utf8(&primary, &rel.to_string_lossy()) {
                            if let Ok(doc) = parse_spec(&raw) {
                                if let Some(num) = extract_spec_number(&doc.id, &prefix) {
                                    max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
                                }
                            }
                        }
                    }
                }
            }
        }
        let spec_id = format_next_spec_id(&prefix, col.digits, max_num);
        let spec_rel = format!("{draft_dir}/{spec_id}-{slug}.md");
        (spec_id, spec_rel)
    };

    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| slug.replace('-', " "));
    let profile_line = req
        .profile
        .as_deref()
        .or(col.profile.as_deref())
        .map(|p| format!("profile: {p}\n"))
        .unwrap_or_default();

    let rendered = format!(
        "---\nid: {spec_id}\ntitle: \"{}\"\ndoc_type: contract\n{profile_line}status: planned\ninvariants: []\nacceptance_criteria: []\n---\n\n# {spec_id}: {title}\n\n### task: T1\n\n```yaml\ntarget: \"Describe task T1 target\"\nscope: []\ntags: [default]\ninvariants: []\nacceptance_criteria: []\n```\n",
        title.replace('"', "\\\"")
    );
    let _ = parse_spec(&rendered)?;

    match acdd_core::fs::write_new_confined(&primary, Path::new(&spec_rel), rendered.as_bytes()) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("draft target '{spec_rel}' already exists"),
            ));
        }
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to write drafted spec '{spec_rel}': {err}"),
            ));
        }
    }

    Ok(DraftResp {
        spec_id,
        spec_path: spec_rel,
        collection: req.collection,
    })
}

pub(crate) fn handle_sync_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SyncReq,
    source_override: Option<&Adapter>,
) -> Result<SyncResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        let caller = resolve_source_checkout(primary, layout, &config.worktree, false)?;
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let current_resolved = parse_profile_from_run(&snapshot.run)?;
        let current_machine = to_machine_profile(&current_resolved)?;

        let spec_rel = if let Some(ref sp) = snapshot.run.spec_path {
            sp.clone()
        } else {
            format!(".acdd/run/{}/spec.md", snapshot.run.run_id)
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

        let (new_machine_profile, new_profile_pair_owned, rebound_resolved) = if req.rebind_profile {
            let mut refreshed = resolve_profile_for_rebind(
                &snapshot.run.profile_id,
                Some(&source),
                Some(&config.modules),
                Some(&checkout.profiles_dirs),
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
        crate::validate::validate_spec_for_run(&checkout, backend, provider, &doc, effective_profile)?;

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
                rebind_profile: req.rebind_profile,
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
                let rel = format!(".acdd/run/{}/profile.resolved.yaml", snapshot.run.run_id);
                if let Ok(dest) = checked_child(primary, Path::new(&rel)) {
                    let _ = atomic_write(&dest, resolved_yaml.as_bytes(), false);
                }
            }
        }

        Ok(SyncResp {
            run_id: snapshot.run.run_id,
            spec_digest,
            rebound_profile: req.rebind_profile,
            added_nodes,
            updated_nodes,
        })
    })
}

pub(crate) fn handle_clear_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ClearReq,
) -> Result<ClearResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;

        // Never delete the user's non-ephemeral spec file on `clear`; only clean worktrees, branches (including `run.branch`), refs, and ephemeral spec.
        let mut cleanup_manifest = build_tombstone_cleanup_manifest_from_snapshot(config, &snapshot, primary);
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
                path_or_ref: format!(".acdd/run/{}/spec.md", snapshot.run.run_id),
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

        exec_tx_with(backend, |tx| tx.archive_and_prune_run(&preconditions, &tombstone))?;

        cleaned_items.extend(cleanup_tombstone(config, backend, &tombstone, layout)?);
        let final_tombstone = backend.load_tombstone(&tombstone.run_id)?.unwrap_or(tombstone);
        Ok(ClearResp {
            tombstone: final_tombstone,
            cleaned_items,
        })
    })
}
