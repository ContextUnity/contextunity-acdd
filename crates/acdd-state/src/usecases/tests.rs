//! Compact unit tests for crate-internal `usecases` helpers (`check_post_runner_completion`,
//! `execute_gate_node`, `resume_pending_tombstone_cleanups_dyn`, `resolve_commands`,
//! `is_context_reset_required`, and legacy `v0.6.0` trace compat).

use super::{StateCommand, StateCommandOutput, StateEngine, StateEngineConfig};
use crate::harness::RunHarness;
use crate::profile::{CommandDef, ResolvedGateProfile};
use crate::store::traits::{StateBackend, StateReader};
use crate::types::{
    ArchiveCleanupEntry, FindingItem, FindingSeverity, RejectionFindings, RejectionKind,
    RunGraphSnapshot, RunTombstoneRecord, StoragePreconditions, SubmitAction, TerminalStatus,
};
use crate::usecases::dto::{ClaimReq, StartReq, StartResp, SubmitReq, SubmitResp};
use acdd_core::config::default_specs;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::git::git_command;
use acdd_core::ids::{RunId, RunName, WorkerId};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::response::ResponsePolicy;
use acdd_core::subsystems::SubsystemRegistry;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

fn make_temp_dir(label: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(&format!("acdd-usecases-{label}-"))
        .tempdir()
        .expect("create tempdir")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git_command(dir, args).expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    git_ok(dir, &["init", "-b", "main"]);
    git_ok(dir, &["config", "user.email", "test@contextunity.local"]);
    git_ok(dir, &["config", "user.name", "ACDD Test"]);
    fs::write(dir.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\n").unwrap();
    fs::write(dir.join(".gitignore"), ".acdd/\n.worktrees/\n").unwrap();
    git_ok(dir, &["add", "."]);
    git_ok(dir, &["commit", "-m", "init"]);
}

fn unit_config(root: &Path) -> StateEngineConfig {
    StateEngineConfig {
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        state_db_override: None,
        code_db_path: None,
        spec_collections: default_specs(),
        profiles_dirs: Vec::new(),
        linked_workspaces: BTreeMap::new(),
        modules: SubsystemRegistry::default(),
        response_policy: ResponsePolicy::default(),
        retain_traces: 5,
        max_snapshot_file_bytes: None,
        allowed_binary_extensions: None,
    }
}

fn empty_preconditions(expected_run_generation: u64) -> StoragePreconditions {
    StoragePreconditions {
        expected_run_generation,
        expected_node_versions: Vec::new(),
        expected_claim_revision: None,
        expected_contract_revision: None,
        expected_worker_id: None,
        required_predecessor_trees: Vec::new(),
    }
}

fn exec_start(eng: &StateEngine, spec_path: &str, profile: &str) -> StartResp {
    match eng
        .execute(StateCommand::Start(StartReq {
            spec_path: Some(spec_path.to_owned()),
            profile: Some(profile.to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    {
        StateCommandOutput::Start(resp) => *resp,
        other => panic!("expected StartResp, got {other:?}"),
    }
}

fn exec_claim(eng: &StateEngine, rid: &str, node: &str, worker: &str) -> u32 {
    match eng
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.to_owned()),
            node: Some(node.to_owned()),
            worker_id: WorkerId::new(worker).unwrap(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(resp) => resp.claim_revision,
        other => panic!("expected ClaimResp, got {other:?}"),
    }
}

fn exec_submit(
    eng: &StateEngine,
    rid: &str,
    node: &str,
    worker: &str,
    rev: Option<u32>,
    action: SubmitAction,
    findings: Option<RejectionFindings>,
) -> Result<SubmitResp, StateError> {
    match eng.execute(StateCommand::Submit(Box::new(SubmitReq {
        run: Some(rid.to_owned()),
        node: node.to_owned(),
        action,
        worker_id: WorkerId::new(worker).unwrap(),
        claim_revision: rev,
        test_target: None,
        review_evidence: None,
        rejection_findings: findings,
        custom_evidence: None,
        verified_invariants: Vec::new(),
        summary: None,
        allow_main: true,
        worktree: None,
    })))? {
        StateCommandOutput::Submit(resp) => Ok(*resp),
        other => panic!("expected SubmitResp, got {other:?}"),
    }
}

fn write_profile(root: &Path, name: &str, yaml: &str) -> String {
    let dir = root.join(".acdd/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), yaml).unwrap();
    format!(".acdd/profiles/{name}")
}

fn empty_provider() -> Arc<CompositeDomainProvider> {
    Arc::new(CompositeDomainProvider::default())
}

fn seed_minimal_graph(rid: &str, rname: &str, ws: &str) -> RunGraphSnapshot {
    RunHarness::build_sample_graph(rid, rname, ws, None, &["t1"]).unwrap()
}

#[test]
fn check_post_runner_completion_tombstone_matrix() {
    let tmp = make_temp_dir("post-runner");
    let harness = RunHarness::in_memory().unwrap();
    let spawn_ok = crate::runner::RoleSpawnOutcome {
        exit_code: Some(0),
        timed_out: false,
        agent_session_id: "sess-1".to_owned(),
        log_relpath: "logs/run.log".to_owned(),
    };
    for (idx, (rid_str, rname, st, expect_ok)) in [
        (
            "run-pruned-done",
            "AC-DONE",
            TerminalStatus::Completed,
            true,
        ),
        (
            "run-pruned-cleared",
            "AC-CLR",
            TerminalStatus::Cleared,
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let tomb = RunTombstoneRecord {
            run_id: RunId::new(rid_str).unwrap(),
            run_name: RunName::new(rname).unwrap(),
            collection: Some("milestones".to_owned()),
            workspace: tmp.path().to_string_lossy().into_owned(),
            spec_id: None,
            title: format!("{rname} title"),
            profile_id: "default".to_owned(),
            is_ephemeral: false,
            generation: 1,
            terminal_status: st,
            target_base: None,
            target_base_commit: None,
            archive_path: None,
            closed_commit: None,
            cleanup_pending: false,
            cleanup_manifest: Vec::new(),
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: "2026-11-17T10:00:00.000Z".to_owned(),
        };
        let mut g = seed_minimal_graph(rid_str, rname, tmp.path().to_str().unwrap());
        g.run.worktree = Some(format!("{}-{idx}", tmp.path().display()));
        harness.seed_run(&g).unwrap();
        harness
            .exec_tx(|tx| tx.archive_and_prune_run(&empty_preconditions(g.run.generation), &tomb))
            .unwrap();
        let res = crate::usecases::run::check_post_runner_completion(
            &harness,
            &tomb.run_id,
            "root:handoff",
            1,
            &spawn_ok,
        );
        if expect_ok {
            assert!(res.unwrap());
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::RunnerNoSubmission);
        }
    }
}

#[test]
fn execute_gate_node_session_cache_reuses_defect_and_rotates_on_drift() {
    let wt_tmp = make_temp_dir("drift-reset");
    let dr_root = wt_tmp.path().join("drift_reset_repo");
    let dr_sessions_log = wt_tmp.path().join("dr_sessions.log");
    let dr_prompts_log = wt_tmp.path().join("dr_prompts.log");
    init_repo(&dr_root);
    let dr_p = write_profile(
        &dr_root,
        "drift_reset.yaml",
        &format!(
            "version: 2\ninherit: code\ngit: {{ isolation: in_place, worktrees: manual }}\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {{test}}\"\n  build_check: {{ command: \"true\" }}\n  verify_all: {{ command: \"true\" }}\nroles:\n  builder:\n    mode: subagent\n    reuse_on_reject: true\n    runner: \"sh -c 'echo $ACDD_AGENT_SESSION_ID >> {}; cat >> {}'\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n          pass: review\n        - id: review\n          role: independent_reviewer\n          proof: none\n          independent_from: [build]\n          pass: deliver\n          fail: build\n          fail_drift: rearchitect\n        - id: deliver\n          role: independent_reviewer\n          independent_from: [build]\n          need: [build, review]\n          receipt: true\n          fail: build\n        - id: rearchitect\n          role: architect\n          pass: build\n",
            dr_sessions_log.display(),
            dr_prompts_log.display(),
        ),
    );
    fs::create_dir_all(dr_root.join("docs/milestones")).unwrap();
    fs::write(
        dr_root.join("docs/milestones/AC-DR.md"),
        "---\nid: AC-DR\ntitle: Drift Session Reset\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\n```\n",
    )
    .unwrap();
    git_ok(&dr_root, &["add", "."]);
    git_ok(&dr_root, &["commit", "-m", "seed drift reset"]);
    let dr_cfg = unit_config(&dr_root);
    let dr_prov = empty_provider();
    let dr_eng = StateEngine::open(dr_cfg.clone(), dr_prov.clone()).unwrap();
    let dr_start = exec_start(&dr_eng, "docs/milestones/AC-DR.md", &dr_p);
    let dr_rid = dr_start.run.run_id.clone();
    let session_cache = std::sync::Mutex::new(std::collections::BTreeMap::new());

    // Helper closure triggers builder execution via `execute_gate_node` (returns RunnerNoSubmission after spawn).
    let run_builder_step = || {
        let snap = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
        let node = snap
            .nodes
            .iter()
            .find(|node| node.node_id == "task:t1:build")
            .unwrap();
        let err = crate::usecases::run::execute_gate_node(
            &dr_cfg,
            dr_eng.backend(),
            &dr_prov,
            &snap,
            node,
            true,
            &session_cache,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::RunnerNoSubmission);
    };

    // Pass 1: fresh builder session created & cached.
    run_builder_step();
    let snap_b1 = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
    let rev_b1 = snap_b1
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1:build")
        .unwrap()
        .claim_revision;
    exec_submit(
        &dr_eng,
        dr_rid.as_str(),
        "t1.build",
        "runner-builder",
        Some(rev_b1),
        SubmitAction::Pass,
        None,
    )
    .unwrap();

    // Reject review with `defect` -> cached session MUST be reused on next builder step.
    let rc1 = exec_claim(&dr_eng, dr_rid.as_str(), "t1.review", "rev-1");
    exec_submit(
        &dr_eng,
        dr_rid.as_str(),
        "t1.review",
        "rev-1",
        Some(rc1),
        SubmitAction::Reject,
        Some(RejectionFindings {
            kind: RejectionKind::Defect,
            summary: "minor defect".to_owned(),
            blackboard_message_id: None,
            items: vec![FindingItem {
                severity: FindingSeverity::Low,
                path: Some("src/lib.rs".to_owned()),
                message: "typo".to_owned(),
                remediation: "fix typo".to_owned(),
            }],
        }),
    )
    .unwrap();
    run_builder_step();
    let snap_b2 = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
    let rev_b2 = snap_b2
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1:build")
        .unwrap()
        .claim_revision;
    exec_submit(
        &dr_eng,
        dr_rid.as_str(),
        "t1.build",
        "runner-builder",
        Some(rev_b2),
        SubmitAction::Pass,
        None,
    )
    .unwrap();

    // Reject review with `architectural_drift` -> triggers `rearchitect` & invalidates poisoned session.
    let rc2 = exec_claim(&dr_eng, dr_rid.as_str(), "t1.review", "rev-2");
    exec_submit(
        &dr_eng,
        dr_rid.as_str(),
        "t1.review",
        "rev-2",
        Some(rc2),
        SubmitAction::Reject,
        Some(RejectionFindings {
            kind: RejectionKind::ArchitecturalDrift,
            summary: "layer violation".to_owned(),
            blackboard_message_id: None,
            items: vec![FindingItem {
                severity: FindingSeverity::High,
                path: Some("src/lib.rs".to_owned()),
                message: "bypassed canonical owner".to_owned(),
                remediation: "route through owner".to_owned(),
            }],
        }),
    )
    .unwrap();
    let arch_c = exec_claim(&dr_eng, dr_rid.as_str(), "t1.rearchitect", "arch-1");
    exec_submit(
        &dr_eng,
        dr_rid.as_str(),
        "t1.rearchitect",
        "arch-1",
        Some(arch_c),
        SubmitAction::Pass,
        None,
    )
    .unwrap();

    run_builder_step();
    let sessions_raw = fs::read_to_string(&dr_sessions_log).unwrap();
    let sessions: Vec<&str> = sessions_raw.lines().collect();
    assert_eq!(sessions.len(), 3);
    assert_eq!(
        sessions[0], sessions[1],
        "defect rejection must reuse builder session"
    );
    assert_ne!(
        sessions[1], sessions[2],
        "architectural_drift must rotate poisoned builder session"
    );
    let prompts_raw = fs::read_to_string(&dr_prompts_log).unwrap();
    assert!(prompts_raw.contains("CONTEXT RESET REQUIRED"));
}

#[test]
fn resume_pending_tombstone_cleanups_handles_linked_and_missing_worktree() {
    let cskip_tmp = make_temp_dir("tombstone-cleanup");
    let cs_root = cskip_tmp.path();
    init_repo(cs_root);
    fs::create_dir_all(cs_root.join("docs/milestones/archive")).unwrap();
    let cskip_p = write_profile(
        cs_root,
        "cskip.yaml",
        "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual }\ngates:\n  - id: handoff\n    proof: none\n",
    );
    let cs_cfg = unit_config(cs_root);
    let cs_eng = StateEngine::open(cs_cfg.clone(), empty_provider()).unwrap();
    let linked_spec = cs_root.join("linked-wt/docs/milestones/AC-LNK.md");
    fs::create_dir_all(linked_spec.parent().unwrap()).unwrap();
    fs::write(&linked_spec, "# linked spec\n").unwrap();
    fs::write(
        cs_root.join("docs/milestones/AC-LNK.md"),
        "---\nid: AC-LNK\ntitle: Linked Spec Replay\nstatus: planned\n---\n# Empty\n",
    )
    .unwrap();
    let linked_wt_abs = cs_root.join("linked-wt").canonicalize().unwrap();
    let lnk_s = exec_start(&cs_eng, "docs/milestones/AC-LNK.md", &cskip_p);
    let link_tomb = RunTombstoneRecord {
        run_id: lnk_s.run.run_id.clone(),
        run_name: lnk_s.run.run_name.clone(),
        collection: Some("milestones".to_owned()),
        workspace: linked_wt_abs.to_string_lossy().into_owned(),
        spec_id: Some("AC-LNK".to_owned()),
        title: "AC-LNK".to_owned(),
        profile_id: "default".to_owned(),
        is_ephemeral: false,
        generation: lnk_s.run.generation + 1,
        terminal_status: TerminalStatus::Completed,
        target_base: None,
        target_base_commit: None,
        archive_path: Some("docs/milestones/archive/AC-LNK.md".to_owned()),
        closed_commit: None,
        cleanup_pending: true,
        cleanup_manifest: vec![
            ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: "docs/milestones/AC-LNK.md".to_owned(),
                workspace: Some(linked_wt_abs.to_string_lossy().into_owned()),
                force: false,
            },
            ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: "docs/milestones/AC-LNK.md".to_owned(),
                workspace: Some(
                    cs_root
                        .parent()
                        .unwrap()
                        .join("removed-wt-gone")
                        .to_string_lossy()
                        .into_owned(),
                ),
                force: false,
            },
        ],
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-17T10:00:00.000Z".to_owned(),
    };
    cs_eng
        .backend()
        .exec_tx(|tx| {
            tx.archive_and_prune_run(&empty_preconditions(lnk_s.run.generation), &link_tomb)
        })
        .unwrap();
    let cleaned_spec = crate::usecases::lifecycle::resume_pending_tombstone_cleanups_dyn(
        &cs_cfg,
        cs_eng.backend(),
        None,
    )
    .unwrap();
    assert!(cleaned_spec
        .iter()
        .any(|p| p == "docs/milestones/AC-LNK.md"));
    assert!(
        !linked_spec.exists()
            && !cs_eng
                .backend()
                .load_tombstone(&lnk_s.run.run_id)
                .unwrap()
                .unwrap()
                .cleanup_pending
    );
}

#[test]
fn resolve_commands_context_reset_and_v060_compat_unit() {
    let loc_tmp = make_temp_dir("resolve-reset-v60");
    let loc_root = loc_tmp.path();
    init_repo(loc_root);
    fs::create_dir_all(loc_root.join("docs/milestones")).unwrap();
    let loc_p = write_profile(
        loc_root,
        "loc.yaml",
        "version: 2\ninherit: code\ngit: { isolation: in_place, worktrees: manual }\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n        - id: review\n          role: independent_reviewer\n          proof: none\n",
    );
    fs::write(
        loc_root.join("docs/milestones/AC-LOC.md"),
        "---\nid: AC-LOC\ntitle: Unit\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\n```\n",
    )
    .unwrap();
    git_ok(loc_root, &["add", "."]);
    git_ok(loc_root, &["commit", "-m", "seed"]);
    let loc_cfg = unit_config(loc_root);
    let loc_eng = StateEngine::open(loc_cfg.clone(), empty_provider()).unwrap();
    let loc_start = exec_start(&loc_eng, "docs/milestones/AC-LOC.md", &loc_p);
    let loc_rid = loc_start.run.run_id.clone();
    // `{scope_target}` changes rendered command lines inside `resolve_commands` (`execution_digest` sensitivity).
    let mut scope_prof: ResolvedGateProfile =
        serde_json::from_str(&loc_start.run.profile_json).unwrap();
    scope_prof.commands.insert(
        "scoped".to_owned(),
        CommandDef::Detailed(Box::new(crate::profile::schema::DetailedCommandDef {
            command: "cargo check {scope_target}".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::from([("src/**".to_owned(), "--lib".to_owned())]),
        })),
    );
    let scope_gate = crate::profile::GateDef {
        id: "scoped_gate".to_owned(),
        command: Some("scoped".to_owned()),
        ..Default::default()
    };
    let cmds_src = crate::usecases::verify::resolve_commands(
        &scope_prof,
        &scope_gate,
        &["src/lib.rs".to_owned()],
        None,
    )
    .unwrap();
    let cmds_docs = crate::usecases::verify::resolve_commands(
        &scope_prof,
        &scope_gate,
        &["docs/readme.md".to_owned()],
        None,
    )
    .unwrap();
    assert_ne!(cmds_src, cmds_docs);

    // Custom non-`builder` role context reset uses persisted `reset_gate_id` when present, falling back to `builder` only when absent.
    let mut reset_snap = loc_eng.backend().load_run_graph(&loc_rid).unwrap().unwrap();
    let impl_node = reset_snap
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1:build")
        .unwrap()
        .clone();
    let mut impl_gate = crate::profile::GateDef {
        id: "build".to_owned(),
        role: Some("implementer".to_owned()),
        ..Default::default()
    };
    reset_snap.artifacts.push(crate::types::GateArtifactRecord {
        run_id: loc_rid.clone(),
        node_id: "task:t1:review".to_owned(),
        claim_revision: 9,
        kind: crate::types::ArtifactKind::Findings,
        payload_json: json!({
            "kind": "architectural_drift",
            "reset_worker_context": true,
            "reset_gate_id": "build",
            "summary": "custom role drift",
            "items": [{ "severity": "high", "message": "drift", "remediation": "rebuild" }]
        })
        .to_string(),
        snapshot_commit: None,
        created_at: "2099-01-01T00:00:00Z".to_owned(),
    });
    assert!(crate::usecases::commands::is_context_reset_required(
        &reset_snap,
        &impl_node,
        &impl_gate
    ));
    reset_snap.artifacts.last_mut().unwrap().payload_json = json!({
        "kind": "architectural_drift",
        "reset_worker_context": true,
        "summary": "legacy without reset_gate_id",
        "items": [{ "severity": "high", "message": "drift", "remediation": "rebuild" }]
    })
    .to_string();
    assert!(!crate::usecases::commands::is_context_reset_required(
        &reset_snap,
        &impl_node,
        &impl_gate
    ));
    impl_gate.role = Some("builder".to_owned());
    assert!(crate::usecases::commands::is_context_reset_required(
        &reset_snap,
        &impl_node,
        &impl_gate
    ));
}
