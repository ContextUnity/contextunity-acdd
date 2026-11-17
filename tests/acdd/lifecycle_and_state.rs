use super::support::{base_engine_config, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{SubtaskRef, TaskRef, WorkerId};
use acdd_state::harness::RunHarness;
use acdd_state::types::{RunStatus, SubmitAction, SubtaskStatus};
use acdd_state::usecases::dto::{
    ClaimReq, ClearReq, DraftReq, ExtendScopeReq, ListReq, NextReq, ObservabilityReq, ReopenReq,
    ShowReq, StartReq, StatusReq, SubmitReq, SubtaskReq, SyncReq,
};
use acdd_state::usecases::{
    execute_on, queries, StateCommand, StateCommandOutput, StateQuery, StateQueryEngine,
    StateQueryOutput,
};
use std::collections::BTreeSet;
use std::fs;

const TWO_GATE_PROFILE: &str = r#"version: 2
inherit: none
save: none
roles:
  architect: {}
gates:
  - id: pre_gate
    role: architect
    proof:
      scheme:
        type: object
        required: [tasks]
        properties:
          tasks: { type: object }
  - tasks:
      gates:
        - id: work
          pass: finish
        - id: finish
          need: [work]
          receipt: true
  - id: post_gate
"#;

#[test]
fn in_memory_run_lifecycle_claims_subtasks_scope_reopen_sync_and_clear_matrix() {
    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "dev");
    ws.write(".acdd/profiles/two-gate.yaml", TWO_GATE_PROFILE);
    ws.write(
        "docs/milestones/001-branching.md",
        include_str!("fixtures/milestones/branching_deps.md"),
    );
    ws.write(
        "docs/milestones/archive/005-old.md",
        &include_str!("fixtures/milestones/archived_after_sync.md").replace("m-prior", "AC-005"),
    );
    super::support::git_cmd(ws.root(), &["add", "."]);
    super::support::git_cmd(ws.root(), &["commit", "-m", "seed branching spec"]);

    let cfg = base_engine_config(ws.root());
    let providers = TestProvider::full_composite();
    let harness = RunHarness::in_memory().unwrap();

    // 1. Draft numbering scans existing active/archive files + DB tombstones.
    let StateCommandOutput::Draft(drafted) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "new-feature".to_owned(),
            prefix: None,
            title: Some("New Feature".to_owned()),
            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
        }),
    )
    .unwrap()
    else {
        panic!("expected DraftResp");
    };
    assert_eq!(drafted.spec_id, "AC-006");
    assert!(ws.path(&drafted.spec_path).is_file());
    super::support::git_cmd(ws.root(), &["add", &drafted.spec_path]);
    super::support::git_cmd(ws.root(), &["commit", "-m", "commit drafted spec"]);

    // 2. Start from migrated branching_deps.md fixture + pre_task root gate progression.
    let StateCommandOutput::Start(started) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/001-branching.md".to_owned()),
            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
            ..StartReq::default()
        }),
    )
    .unwrap()
    else {
        panic!("expected StartResp");
    };
    let rid = started.run.run_id.to_string();
    assert_eq!(started.ready_nodes, vec!["pre_gate".to_owned()]);

    let w1 = WorkerId::new("worker-1").unwrap();
    let w2 = WorkerId::new("worker-2").unwrap();

    // Attempting to claim a task gate while pre-task root gate `pre_gate` is pending emits next-gate hint.
    let pre_err = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("first.work".to_owned()),
            worker_id: w1.clone(),
            session_fingerprint: Some("s1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap_err();
    assert_eq!(pre_err.code, ErrorCode::GateVerificationFailed);
    assert!(
        pre_err.message.contains("Pre-task gate 'pre_gate' must complete"),
        "{pre_err:?}"
    );
    assert_eq!(pre_err.hint.as_deref(), Some("claim 'pre_gate' next"));

    // Claim & pass pre_gate -> unlocks task `first`.
    execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: None, // auto-select pre_gate
            worker_id: w1.clone(),
            session_fingerprint: Some("s1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap();
    let StateCommandOutput::Submit(pre_sub) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "pre_gate".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w1.clone(),
            claim_revision: Some(1),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: Some(serde_json::json!({"tasks": {"first": {}}})),
            verified_invariants: Vec::new(),
            summary: Some("pre done".to_owned()),
            allow_main: true,
            worktree: None,
        })),
    )
    .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert!(pre_sub.unblocked_nodes.contains(&"first.work".to_owned()));

    // 3. Adding a new task via spec edit + Sync after pre_gate completed triggers PreTaskArtifactStale.
    let orig_spec = fs::read_to_string(ws.path("docs/milestones/001-branching.md")).unwrap();
    let added_task_spec = format!(
        "{orig_spec}\n### task: extra\n```yaml\ntask_ref: extra\ntarget: Extra\nscope: [src/extra.rs]\n```\n"
    );
    ws.write("docs/milestones/001-branching.md", &added_task_spec);
    let sync_stale = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Sync(SyncReq {
            run: Some(rid.clone()),
            rebind_profile: false,
        }),
    )
    .unwrap_err();
    assert_eq!(sync_stale.code, ErrorCode::PreTaskArtifactStale);
    ws.write("docs/milestones/001-branching.md", &orig_spec);

    // 4. Claim `first.work` -> idempotent reclaim OK -> second worker blocked (TaskAlreadyClaimed).
    execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("first.work".to_owned()),
            worker_id: w1.clone(),
            session_fingerprint: Some("s1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap();
    let steal_err = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("first.work".to_owned()),
            worker_id: w2.clone(),
            session_fingerprint: Some("s2".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap_err();
    assert_eq!(steal_err.code, ErrorCode::TaskAlreadyClaimed);

    // 5. Subtask lifecycle + mandatory evidence guard + ExtendScope (`contract_revision` bump).
    let blank_ev_err = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Subtask(SubtaskReq {
            run: Some(rid.clone()),
            task_ref: TaskRef::new("first").unwrap(),
            subtask_ref: SubtaskRef::new("sub-1").unwrap(),
            status: SubtaskStatus::Completed,
            evidence: Some("   ".to_owned()),
        }),
    )
    .unwrap_err();
    assert_eq!(blank_ev_err.code, ErrorCode::ProfileSchemaInvalid);

    execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Subtask(SubtaskReq {
            run: Some(rid.clone()),
            task_ref: TaskRef::new("first").unwrap(),
            subtask_ref: SubtaskRef::new("sub-1").unwrap(),
            status: SubtaskStatus::Completed,
            evidence: Some("verified in memory".to_owned()),
        }),
    )
    .unwrap();

    let StateCommandOutput::ExtendScope(ext_ok) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::ExtendScope(ExtendScopeReq {
            run: Some(rid.clone()),
            node: "first.work".to_owned(),
            paths: vec!["src/first_helper.rs".to_owned()],
            reason: "shared helper".to_owned(),
            worker_id: Some(w1.clone()),
        }),
    )
    .unwrap()
    else {
        panic!("expected ExtendScopeResp");
    };
    assert_eq!(ext_ok.contract_revision, 2);

    // Complete `first` (`work` -> `finish`) -> unlocks parallel wave (`second`, `third`).
    for gate in ["first.work", "first.finish"] {
        if gate == "first.finish" {
            execute_on(
                &cfg,
                &harness,
                &providers,
                StateCommand::Claim(ClaimReq {
                    run: Some(rid.clone()),
                    node: Some(gate.to_owned()),
                    worker_id: w1.clone(),
                    session_fingerprint: Some("s1".to_owned()),
                    force: false,
                    steal_remote: false,
                    worktree: None,
                }),
            )
            .unwrap();
        }
        execute_on(
            &cfg,
            &harness,
            &providers,
            StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: gate.to_owned(),
                action: SubmitAction::Pass,
                worker_id: w1.clone(),
                claim_revision: Some(1),
                test_target: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                summary: None,
                allow_main: true,
                worktree: None,
            })),
        )
        .unwrap();
    }

    let StateQueryOutput::Next(next_wave) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::Next(NextReq {
            run: Some(rid.clone()),
            limit: None,
        }),
    )
    .unwrap()
    else {
        panic!("expected NextResp");
    };
    assert_eq!(
        next_wave.ready_cursors,
        vec!["second.work".to_owned(), "third.work".to_owned()]
    );

    // Claim `second.work` and extend scope onto `src/third.rs` -> TaskScopeConflict when `third` is claimed,
    // or Claim `third.work` with overlapping scope -> TaskScopeConflict!
    execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("second.work".to_owned()),
            worker_id: w1.clone(),
            session_fingerprint: Some("s1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap();
    execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("third.work".to_owned()),
            worker_id: w2,
            session_fingerprint: Some("s2".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }),
    )
    .unwrap();
    let scope_conflict = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::ExtendScope(ExtendScopeReq {
            run: Some(rid.clone()),
            node: "second.work".to_owned(),
            paths: vec!["src/third.rs".to_owned()],
            reason: "conflict check".to_owned(),
            worker_id: Some(w1),
        }),
    )
    .unwrap_err();
    assert_eq!(scope_conflict.code, ErrorCode::TaskScopeConflict);

    // 6. Reopen completed `first` without --cascade fails with DependentTaskCompleted because `second` is in_progress;
    // with --cascade succeeds and stales `second`/`third`.
    let no_cascade = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Reopen(ReopenReq {
            run: Some(rid.clone()),
            node: "first".to_owned(),
            cascade: false,
            reason: None,
        }),
    )
    .unwrap_err();
    assert_eq!(no_cascade.code, ErrorCode::DependentTaskCompleted);

    let StateCommandOutput::Reopen(cascaded) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Reopen(ReopenReq {
            run: Some(rid.clone()),
            node: "first".to_owned(),
            cascade: true,
            reason: Some("contract fix".to_owned()),
        }),
    )
    .unwrap()
    else {
        panic!("expected ReopenResp");
    };
    assert!(cascaded.affected_nodes.iter().any(|n| n.contains("second")));

    // 7. Pagination generation pin check + Ephemeral --goal + Clear tombstone.
    let StateQueryOutput::Show(shown) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::Show(ShowReq {
            run: Some(rid.clone()),
            node: Some("first".to_owned()),
        }),
    )
    .unwrap()
    else {
        panic!("expected ShowResp");
    };
    assert_eq!(shown.subtasks.len(), 1);

    let StateQueryOutput::List(list_p1) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::List(ListReq {
            status: Some(RunStatus::Active),
            limit: Some(10),
            offset: 0,
            ..ListReq::default()
        }),
    )
    .unwrap()
    else {
        panic!("expected ListResp");
    };
    assert_eq!(list_p1.page.runs.len(), 1);
    let gen_err = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::List(ListReq {
            generation: Some(list_p1.page.runs[0].generation + 99),
            ..ListReq::default()
        }),
    )
    .unwrap_err();
    assert_eq!(gen_err.code, ErrorCode::BackendConflict);

    // Clear removes active run and records cleared tombstone.
    let StateCommandOutput::Clear(cleared) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Clear(ClearReq {
            run: Some(rid),
            reason: Some("abandoned".to_owned()),
            force: true,
        }),
    )
    .unwrap()
    else {
        panic!("expected ClearResp");
    };
    assert_eq!(cleared.tombstone.terminal_status.as_str(), "cleared");

    // Ephemeral --goal run creation works without spec_path.
    let StateCommandOutput::Start(ephemeral) = execute_on(
        &cfg,
        &harness,
        &providers,
        StateCommand::Start(StartReq {
            goal: Some("Quick fix".to_owned()),
            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
            scope: vec!["src/lib.rs".to_owned()],
            ..StartReq::default()
        }),
    )
    .unwrap()
    else {
        panic!("expected ephemeral StartResp");
    };
    assert!(ephemeral.run.is_ephemeral);
    assert!(ephemeral.run.spec_path.is_none());
}

#[test]
fn read_only_queries_never_create_db_and_modules_state_false_disables_all_state() {
    let ws = Workspace::new();
    let cfg = base_engine_config(ws.root());
    let providers = TestProvider::full_composite();

    // Read-only engine on missing `.acdd/state.sqlite` returns empty without creating file.
    let ro = StateQueryEngine::open_read_only(cfg.clone(), providers.clone()).unwrap();
    for query in [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Next(NextReq::default()),
        StateQuery::Observability(ObservabilityReq::default()),
    ] {
        let out = ro.query(query).expect("read-only query on missing DB succeeds");
        match out {
            StateQueryOutput::Status(s) => assert!(s.run.is_none()),
            StateQueryOutput::List(l) => assert!(l.page.runs.is_empty()),
            StateQueryOutput::Next(n) => assert!(n.run_id.is_none()),
            StateQueryOutput::Observability(o) => assert_eq!(o.snapshot.active_runs, 0),
            _ => {}
        }
    }
    assert!(!ws.path(".acdd/state.sqlite").exists());

    // `modules.state = false` fails closed with FeatureDisabled.
    let mut disabled_cfg = cfg;
    disabled_cfg.modules = SubsystemRegistry {
        disabled: BTreeSet::from(["state".to_owned()]),
        ..SubsystemRegistry::default()
    };
    let ro_disabled = StateQueryEngine::open_read_only(disabled_cfg.clone(), providers.clone()).unwrap();
    let err_ro = ro_disabled.query(StateQuery::Status(StatusReq::default())).unwrap_err();
    assert_eq!(err_ro.code, ErrorCode::FeatureDisabled);

    let harness = RunHarness::in_memory().unwrap();
    let err_cmd = execute_on(
        &disabled_cfg,
        &harness,
        &providers,
        StateCommand::Start(StartReq {
            goal: Some("blocked".to_owned()),
            ..StartReq::default()
        }),
    )
    .unwrap_err();
    assert_eq!(err_cmd.code, ErrorCode::FeatureDisabled);
}
