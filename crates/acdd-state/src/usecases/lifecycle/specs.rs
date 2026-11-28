//! Spec collections, archive relpaths, profile-to-machine conversion, cursor resolution, and archive reconciliation.

use super::cleanup::{build_tombstone_cleanup_manifest, CleanupManifestInput};
use crate::machine::{
    compute_schedule, MachineCheckDef, MachineGateDef, MachineGitPolicy, MachineGraphItem,
    MachineProfileInput, MachineProofKind, MachineSavePolicy, MachineTasksBlockDef,
};
use crate::workflow::{GateDef, ProofDef, ProofKind, ResolvedWorkflow};
use crate::store::sqlite::backup::RestoredActiveRun;
use crate::types::{
    GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, RunTombstoneRecord,
    SaveAction, TerminalStatus, ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::config::SpecConfig;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::fs::checked_child;
use acdd_core::git::{git_common_dir, git_stdout, read_git_blob_utf8};
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, RunName, WorkerId};
use acdd_core::schema::SchemaDef;
use acdd_spec::{
    effective_spec_collections as spec_effective_collections, match_spec_collection,
    parse_spec_with_collections, SpecDocument, SpecStatus,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Return configured spec collections or the default `milestones` + `plans` map.
pub fn effective_spec_collections(
    config: &StateEngineConfig,
) -> BTreeMap<String, SpecConfig> {
    spec_effective_collections(&config.spec_collections)
}

/// Resolve the owning collection `(key, &SpecConfig)` from an explicit hint or workspace-relative path.
pub(crate) fn owning_collection_for<'a>(
    collections: &'a BTreeMap<String, SpecConfig>,
    collection_hint: Option<&str>,
    rel_path: Option<&str>,
) -> Option<(&'a str, &'a SpecConfig)> {
    let key = collection_hint
        .filter(|k| collections.contains_key(*k))
        .map(str::to_owned)
        .or_else(|| rel_path.and_then(|p| match_spec_collection(collections, p)))?;
    collections
        .get_key_value(&key)
        .map(|(k, v)| (k.as_str(), v))
}

/// Parse raw spec content using `collection_hint` / `rel_path` owning collection resolution.
pub(crate) fn parse_spec_in_collections(
    raw: &str,
    collections: &BTreeMap<String, SpecConfig>,
    collection_hint: Option<&str>,
    rel_path: Option<&str>,
) -> Result<SpecDocument, AcddError> {
    let owning = owning_collection_for(collections, collection_hint, rel_path);
    parse_spec_with_collections(raw, owning, collections)
}

/// Derive the archive relative path for an active run's `spec_relpath`.
pub(crate) fn compute_archive_relpath(
    config: &StateEngineConfig,
    run_collection: Option<&str>,
    spec_relpath: &str,
) -> Result<String, AcddError> {
    let collections = effective_spec_collections(config);
    let col_key = run_collection
        .map(str::to_owned)
        .or_else(|| match_spec_collection(&collections, spec_relpath));

    let file_name = Path::new(spec_relpath)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("spec_path '{spec_relpath}' has no file name"),
            )
        })?;

    if let Some(key) = col_key {
        if let Some(cfg) = collections.get(&key) {
            if let Some(root_dir) = cfg.matched_concrete_root(spec_relpath) {
                return Ok(format!("{root_dir}/archive/{file_name}"));
            }
            if let Some(dir) = cfg.default_draft_dir() {
                return Ok(format!("{dir}/archive/{file_name}"));
            }
        }
    }
    let parent = Path::new(spec_relpath)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');
    if parent.is_empty() {
        Ok(format!("archive/{file_name}"))
    } else {
        Ok(format!("{parent}/archive/{file_name}"))
    }
}

/// Parse a `ResolvedWorkflow` from `runs.profile_json`.
pub fn parse_profile_from_run(run: &RunRecord) -> Result<ResolvedWorkflow, AcddError> {
    serde_json::from_str(&run.profile_json).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("corrupt runs.profile_json for '{}': {err}", run.run_id),
        )
    })
}

/// Convert a [`ResolvedWorkflow`] into the pure state machine's [`MachineProfileInput`],
/// expanding explicit `specs.<name>` and `paths.<name>` entries in gate `scope:`.
pub fn to_machine_profile(
    profile: &ResolvedWorkflow,
    spec_collections: Option<&BTreeMap<String, SpecConfig>>,
) -> Result<MachineProfileInput, AcddError> {
    let empty_specs = BTreeMap::new();
    let effective_specs =
        spec_effective_collections(spec_collections.unwrap_or(&empty_specs));
    let mut proof_schemas: BTreeMap<String, SchemaDef> = profile
        .proofs
        .iter()
        .map(|(k, v)| (k.clone(), v.schema.clone()))
        .collect();

    let last_top_root_idx = profile
        .gates
        .iter()
        .rposition(|g| !g.is_tasks_block() && g.gates.is_empty());

    let mut gates = Vec::with_capacity(profile.gates.len());
    for (idx, item) in profile.gates.iter().enumerate() {
        if let Some(ref tb) = item.tasks {
            let mut task_gates = Vec::with_capacity(tb.gates.len());
            for g in &tb.gates {
                task_gates.push(convert_gate_def(
                    profile,
                    g,
                    false,
                    &effective_specs,
                    &mut proof_schemas,
                )?);
            }
            gates.push(MachineGraphItem::TasksBlock(MachineTasksBlockDef {
                when: tb.when.clone(),
                gates: task_gates,
            }));
        } else {
            let is_terminal_root = last_top_root_idx == Some(idx) && item.pass.is_none();
            gates.push(MachineGraphItem::Gate(Box::new(convert_gate_def(
                profile,
                item,
                is_terminal_root,
                &effective_specs,
                &mut proof_schemas,
            )?)));
        }
    }

    Ok(MachineProfileInput {
        profile_id: profile.workflow_id.clone(),
        profile_json: profile.to_canonical_json()?,
        git: MachineGitPolicy {
            isolation: profile.git.isolation,
            worktrees: profile.git.worktrees,
        },
        default_save: profile.save,
        protected_paths: profile.protected_paths.clone(),
        tags: profile.tags.clone(),
        proof_schemas,
        gates,
    })
}

fn expand_gate_scope(
    raw_scope: &[String],
    profile: &ResolvedWorkflow,
    spec_collections: &BTreeMap<String, SpecConfig>,
    gate_id: &str,
) -> Result<Vec<String>, AcddError> {
    if raw_scope.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push_unique = |pat: String| {
        if seen.insert(pat.clone()) {
            out.push(pat);
        }
    };
    for entry in raw_scope {
        if let Some(name) = entry.strip_prefix("specs.") {
            let col = spec_collections.get(name).ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{gate_id}': unknown spec reference '{entry}' in scope:"),
                )
            })?;
            let patterns = col.patterns();
            if patterns.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{gate_id}': spec reference '{entry}' resolves to empty patterns"),
                ));
            }
            for pat in patterns {
                push_unique(pat.to_owned());
            }
        } else if let Some(name) = entry.strip_prefix("paths.") {
            let globs = profile.paths.get(name).ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{gate_id}': unknown path reference '{entry}' in scope:"),
                )
            })?;
            if globs.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{gate_id}': path reference '{entry}' in scope: resolves to empty globs"),
                ));
            }
            for glob in globs {
                push_unique(glob.clone());
            }
        } else {
            push_unique(entry.clone());
        }
    }
    Ok(out)
}

fn convert_gate_def(
    profile: &ResolvedWorkflow,
    gate: &GateDef,
    is_terminal_root: bool,
    spec_collections: &BTreeMap<String, SpecConfig>,
    proof_schemas: &mut BTreeMap<String, SchemaDef>,
) -> Result<MachineGateDef, AcddError> {
    let id = GateId::new(&gate.id)?;
    let proof = match &gate.proof {
        ProofDef::Kind(ProofKind::None) => MachineProofKind::None,
        ProofDef::Kind(ProofKind::Command) => MachineProofKind::Command,
        ProofDef::Kind(ProofKind::Call) => MachineProofKind::Call,
        ProofDef::Kind(ProofKind::Review) => MachineProofKind::Review,
        ProofDef::Named(name) | ProofDef::ResolvedNamed { name, .. } => {
            MachineProofKind::CustomSchema(name.clone())
        }
        ProofDef::Scheme { scheme } => {
            let dig = acdd_core::format::canonical_json_sha256(
                &serde_json::to_value(scheme.as_ref())
                    .map_err(|e| AcddError::new(ErrorCode::WorkflowSchemaInvalid, e.to_string()))?,
            );
            let synthetic = format!("__inline_{}", &dig[..12]);
            proof_schemas.insert(synthetic.clone(), *scheme.clone());
            MachineProofKind::CustomSchema(synthetic)
        }
    };

    let mut children = Vec::with_capacity(gate.gates.len());
    for child in &gate.gates {
        children.push(convert_gate_def(
            profile,
            child,
            false,
            spec_collections,
            proof_schemas,
        )?);
    }

    let effective_post = gate.save.post.unwrap_or(if is_terminal_root {
        profile.save
    } else {
        SaveAction::None
    });

    let expanded_scope = expand_gate_scope(&gate.scope, profile, spec_collections, &gate.id)?;

    Ok(MachineGateDef {
        id,
        role: gate.role.clone(),
        proof,
        command: gate.command.clone(),
        call: gate.call.clone(),
        with_params: gate
            .with
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default())),
        checks: gate
            .checks
            .iter()
            .map(|c| MachineCheckDef {
                qualified_name: c.check.clone(),
                params: c.with.clone(),
            })
            .collect(),
        contours: gate.contours.clone(),
        independent_from: gate.independent_from.clone(),
        need: gate.need.clone(),
        pass: gate.pass.as_ref().map(|g| g.as_str().to_owned()),
        fail: gate.fail.as_ref().map(|g| g.as_str().to_owned()),
        fail_drift: gate.fail_drift.as_ref().map(|g| g.as_str().to_owned()),
        max_attempts: gate.max_attempts.max(1),
        receipt: gate.receipt,
        save: MachineSavePolicy {
            post: effective_post,
            immutable: gate.save.immutable,
            archive: gate.save.archive,
            prune: gate.save.prune,
            report: gate.save.report.clone(),
        },
        scope: expanded_scope,
        when: gate.when.clone(),
        children,
        definition_digest: gate.definition_digest()?,
    })
}

/// Parse a user-supplied node cursor string against the tasks present in `snapshot`.
pub fn parse_cursor_for_snapshot(
    snapshot: &RunGraphSnapshot,
    raw: &str,
) -> Result<NodeCursor, AcddError> {
    let trimmed = raw.trim();
    NodeCursor::parse_with_task_lookup(trimmed, |seg| {
        snapshot.nodes.iter().any(|n| {
            n.kind == NodeKind::Task && n.task_ref.as_ref().is_some_and(|t| t.as_str() == seg)
        })
    })
    .map_err(AcddError::from)
}

/// Auto-select the next ready gate cursor when `ClaimReq.node` is omitted.
pub fn auto_select_ready_cursor(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> Result<NodeCursor, AcddError> {
    let schedule = compute_schedule(snapshot, obs)?;
    if let Some(root_node_id) = schedule.ready_root_nodes.first() {
        return NodeCursor::from_node_id(root_node_id).map_err(AcddError::from);
    }
    for task_ref in &schedule.ready_tasks {
        let parent_id = format!("task:{task_ref}");
        if let Some(gate_node) = snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.parent_id.as_deref() == Some(parent_id.as_str())
                    && matches!(n.status, NodeStatus::Ready | NodeStatus::InProgress)
            })
            .min_by_key(|n| n.seq_order)
        {
            return NodeCursor::from_node_id(&gate_node.node_id).map_err(AcddError::from);
        }
    }
    if let Some(in_prog) = snapshot
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                && n.status == NodeStatus::Ready
        })
        .min_by_key(|n| n.seq_order)
    {
        return NodeCursor::from_node_id(&in_prog.node_id).map_err(AcddError::from);
    }
    Err(AcddError::new(
        ErrorCode::GateVerificationFailed,
        format!(
            "no ready gate nodes available to claim in run '{}'",
            snapshot.run.run_id
        ),
    ))
}

/// Auto-select the single active claimed or ready gate when `ContextReq.node` is omitted.
pub fn auto_select_context_cursor(
    snapshot: &RunGraphSnapshot,
    worker_id: Option<&WorkerId>,
) -> Result<NodeCursor, AcddError> {
    let is_gate = |n: &GateNodeRecord| matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate);
    let matches_worker =
        |n: &GateNodeRecord| worker_id.is_none_or(|w| n.worker_id.as_ref() == Some(w));
    let to_cursors = |nodes: &[&GateNodeRecord]| -> Result<Vec<String>, AcddError> {
        nodes
            .iter()
            .map(|n| NodeCursor::from_node_id(&n.node_id).map(|c| c.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(AcddError::from)
    };

    let mut claimed: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| is_gate(n) && n.status == NodeStatus::InProgress && matches_worker(n))
        .collect();
    claimed.sort_by_key(|n| n.seq_order);
    match claimed.len() {
        1 => return NodeCursor::from_node_id(&claimed[0].node_id).map_err(AcddError::from),
        n if n > 1 => {
            let cursors = to_cursors(&claimed)?;
            return Err(AcddError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "multiple active claimed gates match context request: {}; specify `node`",
                    cursors.join(", ")
                ),
            ));
        }
        _ => {}
    }

    let mut ready: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| is_gate(n) && n.status == NodeStatus::Ready && matches_worker(n))
        .collect();
    ready.sort_by_key(|n| n.seq_order);
    match ready.len() {
        1 => NodeCursor::from_node_id(&ready[0].node_id).map_err(AcddError::from),
        n if n > 1 => {
            let cursors = to_cursors(&ready)?;
            Err(AcddError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "multiple ready gates match context request: {}; specify `node`",
                    cursors.join(", ")
                ),
            ))
        }
        _ => Err(AcddError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "no active claimed or ready gate found for context in run '{}'",
                snapshot.run.run_id
            ),
        )),
    }
}

fn candidate_archive_relpaths(spec_path: &str, spec_collection_paths: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    let normalized = spec_path.trim().replace('\\', "/");
    let norm_spec = normalized.strip_prefix("./").unwrap_or(&normalized);
    let path = Path::new(norm_spec);
    let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
        return candidates;
    };

    let parent = path
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');

    let mut matched_col: Option<String> = None;
    let mut matched_len = 0usize;
    for col_path in spec_collection_paths {
        let trimmed = col_path.trim().replace('\\', "/");
        let trimmed = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if !trimmed.is_empty()
            && path.starts_with(Path::new(trimmed))
            && trimmed.len() > matched_len
        {
            matched_len = trimmed.len();
            matched_col = Some(trimmed.to_owned());
        }
    }

    if !parent.is_empty() && parent != "." && parent != "archive" && !parent.ends_with("/archive") {
        candidates.push(format!("{parent}/archive/{file_name}"));
        if let Some(col) = matched_col {
            if col != parent {
                let col_rel = format!("{col}/archive/{file_name}");
                if !candidates.contains(&col_rel) {
                    candidates.push(col_rel);
                }
            }
        }
        return candidates;
    }

    // Absent or non-standard parent fallback (e.g., bare filename or already under archive/).
    for col_path in spec_collection_paths {
        let trimmed = col_path.trim().replace('\\', "/");
        let trimmed = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if !trimmed.is_empty() {
            let rel = format!("{trimmed}/archive/{file_name}");
            if !candidates.contains(&rel) {
                candidates.push(rel);
            }
        }
    }
    let root_rel = format!("archive/{file_name}");
    if !candidates.contains(&root_rel) {
        candidates.push(root_rel);
    }
    candidates
}

fn git_refs_to_probe(row: &RestoredActiveRun) -> Vec<String> {
    let mut refs = vec!["HEAD".to_owned()];
    if let Some(ref br) = row.branch {
        let trimmed = br.trim();
        if !trimmed.is_empty() && trimmed != "HEAD" {
            let full_ref = if trimmed.starts_with("refs/") {
                trimmed.to_owned()
            } else {
                format!("refs/heads/{trimmed}")
            };
            if !refs.contains(&full_ref) {
                refs.push(full_ref);
            }
        }
    }
    refs
}

fn archive_matches_run(doc: &SpecDocument, row: &RestoredActiveRun) -> bool {
    if doc.status != SpecStatus::Completed {
        return false;
    }
    if let Some(ref expected_spec_id) = row.spec_id {
        if &doc.id != expected_spec_id {
            return false;
        }
    }
    if let Some(ref closure) = doc.closure {
        if closure.status != SpecStatus::Completed {
            return false;
        }
        if closure
            .task_receipts
            .values()
            .any(|r| r.run_id.as_str() != row.run_id)
        {
            return false;
        }
    }
    !doc.tasks
        .iter()
        .filter_map(|t| t.receipt.as_ref())
        .any(|r| r.run_id.as_str() != row.run_id)
}

fn find_committed_matching_archive(
    ws_root: &Path,
    is_git_repo: bool,
    row: &RestoredActiveRun,
    candidates: &[String],
    collections: &BTreeMap<String, SpecConfig>,
) -> Option<(String, Option<String>, SpecDocument)> {
    let refs = git_refs_to_probe(row);
    for archive_rel in candidates {
        let Ok(child_path) = checked_child(ws_root, Path::new(archive_rel)) else {
            continue;
        };
        if is_git_repo {
            for git_ref in &refs {
                let Ok(Some(content)) = read_git_blob_utf8(ws_root, git_ref, archive_rel) else {
                    continue;
                };
                let Ok(doc) = parse_spec_in_collections(
                    &content,
                    collections,
                    row.collection.as_deref(),
                    Some(archive_rel),
                ) else {
                    continue;
                };
                if archive_matches_run(&doc, row) {
                    return Some((archive_rel.clone(), Some(git_ref.clone()), doc));
                }
            }
        } else if child_path.is_file() {
            let Ok(content) = acdd_core::fs::read_confined_utf8(ws_root, Path::new(archive_rel))
            else {
                continue;
            };
            let Ok(doc) = parse_spec_in_collections(
                &content,
                collections,
                row.collection.as_deref(),
                Some(archive_rel),
            ) else {
                continue;
            };
            if archive_matches_run(&doc, row) {
                return Some((archive_rel.clone(), None, doc));
            }
        }
    }
    None
}

/// Reconcile restored active non-ephemeral runs against committed `<collection>/archive/*.md`,
/// producing terminal `completed` tombstones for runs whose archive already landed.
pub(crate) fn reconcile_restored_runs_with_archives(
    active_runs: &[RestoredActiveRun],
    workspace_root: Option<&Path>,
    spec_collection_paths: &[String],
    spec_collections: &BTreeMap<String, SpecConfig>,
) -> Result<Vec<RunTombstoneRecord>, AcddError> {
    let effective_collections = spec_effective_collections(spec_collections);
    let mut tombstones = Vec::new();
    for row in active_runs {
        if row.is_ephemeral {
            continue;
        }
        let Some(ref spec_path) = row.spec_path else {
            continue;
        };
        let ws_root = workspace_root.unwrap_or_else(|| Path::new(&row.workspace));
        let is_git_repo = ws_root.join(".git").exists() || git_common_dir(ws_root).is_ok();
        let candidates = candidate_archive_relpaths(spec_path, spec_collection_paths);
        let Some((archive_rel, matched_ref, doc)) = find_committed_matching_archive(
            ws_root,
            is_git_repo,
            row,
            &candidates,
            &effective_collections,
        ) else {
            continue;
        };

        let spec_id = Some(doc.id).or_else(|| row.spec_id.clone());
        let title = if doc.title.trim().is_empty() {
            row.run_name.clone()
        } else {
            doc.title
        };

        let closed_commit = if let Some(ref git_ref) = matched_ref {
            git_stdout(
                ws_root,
                &["log", "-n", "1", "--format=%H", git_ref, "--", &archive_rel],
                "GIT_LOG_FAILED",
            )
            .ok()
            .and_then(|oid| CommitOid::new(oid).ok())
        } else {
            None
        };

        let task_refs: Vec<(Option<&str>, Option<&str>)> = row
            .task_worktrees_and_branches
            .iter()
            .map(|(wt, br)| (wt.as_deref(), br.as_deref()))
            .collect();
        let cleanup_manifest = build_tombstone_cleanup_manifest(
            &CleanupManifestInput {
                spec_path: Some(spec_path.as_str()),
                run_worktree: row.worktree.as_deref(),
                run_branch: row.branch.as_deref(),
                task_worktrees_and_branches: &task_refs,
            },
            ws_root,
        );

        let run_id = RunId::new(row.run_id.clone())?;
        let run_name = RunName::new(row.run_name.clone())?;
        let generation = u64::try_from(row.generation).map_err(|_| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("negative generation on restored run '{}'", row.run_id),
            )
        })?;
        let target_base_commit = row
            .target_base_commit
            .clone()
            .map(CommitOid::new)
            .transpose()?;

        tombstones.push(RunTombstoneRecord {
            run_id,
            run_name,
            collection: row.collection.clone(),
            workspace: row.workspace.clone(),
            spec_id,
            title,
            profile_id: row.profile_id.clone(),
            is_ephemeral: false,
            generation,
            terminal_status: TerminalStatus::Completed,
            target_base: row.target_base.clone(),
            target_base_commit,
            archive_path: Some(archive_rel),
            closed_commit,
            cleanup_pending: true,
            cleanup_manifest,
            summary_json: r#"{"reconciled_from_archive":true}"#.to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: acdd_core::telemetry::now_utc_iso_ms(),
        });
    }
    Ok(tombstones)
}
