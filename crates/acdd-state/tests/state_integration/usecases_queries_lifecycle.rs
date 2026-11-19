use super::common::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_core::ports::DomainToolOutput;
use acdd_state::harness::RunHarness;
use acdd_state::store::traits::{StateBackend, StateReader};
use acdd_state::types::{
    BlackboardTopic, ClaimOutcome, GateClaimRecord, IsolationMode, NodeStatus, NodeStatusPatch, NodeVersionExpectation, StoragePreconditions,
    SubtaskStatus, TerminalStatus, TraceFinishInput, TraceStartInput, TraceStatus, TransitionPlan,
};
use acdd_state::usecases::dto::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, ClearReq, DraftReq, ExtendScopeReq,
    ListReq, NextReq, ObservabilityReq, ReopenReq, ShowReq, StartReq, StatusReq, SubtaskReq, SyncReq,
};
use acdd_state::usecases::{
    execute_on, queries, StateCommand, StateCommandOutput, StateEngine, StateQuery,
    StateQueryEngine, StateQueryOutput,
};
use serde_json::json;
use std::fs;

#[test]
fn seam_readonly_queries_matrix_and_zero_db_creation() {
    let empty_tmp = make_temp_dir("empty-ro");
    let cfg = base_config(empty_tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let ro = StateQueryEngine::open_read_only(cfg, provider.clone()).unwrap();

    let empty_cases = [
        (StateQuery::Status(StatusReq::default()), false),
        (StateQuery::List(ListReq::default()), false),
        (StateQuery::Show(ShowReq { run: None, node: None }), true),
        (StateQuery::Next(NextReq::default()), false),
        (StateQuery::BlackboardRead(BlackboardReadReq::default()), true),
        (StateQuery::Observability(ObservabilityReq { include_prometheus: true }), false),
    ];
    for (idx, (query, expect_ambiguous)) in empty_cases.into_iter().enumerate() {
        let res = ro.query(query);
        if expect_ambiguous {
            assert_eq!(res.unwrap_err().code, ErrorCode::RunSelectorAmbiguous, "query #{idx}");
        } else {
            assert!(res.is_ok(), "query #{idx} failed: {res:?}");
        }
    }
    assert!(!empty_tmp.path().join(".acdd/state.sqlite").exists());
    assert!(!empty_tmp.path().join(".acdd/state.bak.sqlite").exists());

    let harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
    let ws = harness.workspace_root().unwrap().to_path_buf();
    let mut cfg_pop = base_config(&ws);
    cfg_pop.state_db_override = Some(harness.db_path().unwrap().to_path_buf());
    harness.seed_run(&build_rich_graph(&ws, "run-ro-001", IsolationMode::InPlace)).unwrap();

    let ro_pop = StateQueryEngine::open_read_only(cfg_pop, provider).unwrap();
    match ro_pop.query(StateQuery::Status(StatusReq::default())).unwrap() {
        StateQueryOutput::Status(s) => {
            assert_eq!(s.run.unwrap().run_id.as_str(), "run-ro-001");
            assert!(s.ready_nodes.contains(&"t1.build".to_owned()));
        }
        other => panic!("unexpected Status output: {other:?}"),
    }
    match ro_pop.query(StateQuery::Next(NextReq::default())).unwrap() {
        StateQueryOutput::Next(n) => {
            assert_eq!(n.ready_cursors, vec!["t1.build".to_owned()]);
            assert_eq!(n.waves, vec![vec!["t1".to_owned()], vec!["t2".to_owned()]]);
        }
        other => panic!("unexpected Next output: {other:?}"),
    }
    match ro_pop
        .query(StateQuery::Show(ShowReq {
            run: Some("run-ro-001".to_owned()),
            node: Some("t1.build".to_owned()),
        }))
        .unwrap()
    {
        StateQueryOutput::Show(s) => {
            assert_eq!(s.run.run_id.as_str(), "run-ro-001");
            assert_eq!(s.node.unwrap().node_id, "task:t1:build");
            assert!(s.context_preview.is_some());
        }
        other => panic!("unexpected Show output: {other:?}"),
    }
    match ro_pop.query(StateQuery::Observability(ObservabilityReq { include_prometheus: true })).unwrap() {
        StateQueryOutput::Observability(o) => {
            assert_eq!(o.snapshot.active_runs, 1);
            assert!(o.prometheus_text.as_deref().is_some_and(|t| t.contains("acdd_runs_active 1")));
        }
        other => panic!("unexpected Observability output: {other:?}"),
    }
}

#[test]
fn seam_modules_matrix_enforces_feature_disabled_and_domain_capabilities() {
    let tmp = make_temp_dir("modules-matrix");
    let mut cfg = base_config(tmp.path());
    cfg.modules.disabled.insert("state".to_owned());
    let (_, provider) = build_provider_with_mock(&cfg.modules);

    assert_eq!(StateEngine::open(cfg.clone(), provider.clone()).err().unwrap().code, ErrorCode::FeatureDisabled);
    assert!(!tmp.path().join(".acdd/state.sqlite").exists());

    let ro = StateQueryEngine::open_read_only(cfg.clone(), provider.clone()).unwrap();
    for q in [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Next(NextReq::default()),
        StateQuery::BlackboardRead(BlackboardReadReq::default()),
        StateQuery::Observability(ObservabilityReq::default()),
    ] {
        assert_eq!(ro.query(q).unwrap_err().code, ErrorCode::FeatureDisabled);
    }

    let harness = RunHarness::in_memory().unwrap();
    assert_eq!(
        execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(), slug: "blocked".to_owned(), prefix: None, title: None, profile: None,
        })).unwrap_err().code,
        ErrorCode::FeatureDisabled
    );

    // #30: Stateless CallPluginTool remains usable with modules.state == false; run-scoped stays blocked.
    for (run, expect_ok) in [(None, true), (Some("run-x".to_owned()), false)] {
        let res = execute_on(&cfg, &harness, &provider, StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: "http_probe".to_owned(),
            args: json!({ "url": "https://example.invalid/stateless" }),
            run,
            node: None,
            worktree: None,
        }));
        if expect_ok {
            assert!(matches!(res, Ok(StateCommandOutput::CallPluginTool(_))));
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::FeatureDisabled);
        }
    }

    let mut bb_cfg = base_config(tmp.path());
    bb_cfg.modules.disabled.insert("blackboard".to_owned());
    let (_, bb_prov) = build_provider_with_mock(&bb_cfg.modules);
    assert_eq!(queries::execute_query(&bb_cfg, &harness, &bb_prov, StateQuery::BlackboardRead(BlackboardReadReq::default())).unwrap_err().code, ErrorCode::FeatureDisabled);
    assert!(queries::execute_query(&bb_cfg, &harness, &bb_prov, StateQuery::Status(StatusReq::default())).is_ok());
    assert_eq!(
        execute_on(&bb_cfg, &harness, &bb_prov, StateCommand::BlackboardPost(BlackboardPostReq {
            run: None, node: None, recipient_node: None, subtask_ref: None, gate: None,
            author: "builder".to_owned(), topic: BlackboardTopic::Notes, payload: "note".to_owned(),
        })).unwrap_err().code,
        ErrorCode::FeatureDisabled
    );

    let mut no_cd_cfg = base_config(tmp.path());
    no_cd_cfg.modules.disabled.insert("code".to_owned());
    no_cd_cfg.modules.disabled.insert("docs".to_owned());
    let (_, no_cd_prov) = build_provider_with_mock(&no_cd_cfg.modules);
    assert!(!no_cd_prov.is_domain_enabled("code") && !no_cd_prov.is_domain_enabled("docs") && no_cd_prov.is_domain_enabled("http"));
    assert_eq!(no_cd_prov.validate_check_ref("code.graph_fresh", &json!({})).unwrap_err().code, ErrorCode::DomainCapabilityUnavailable);

    let mut no_http_cfg = base_config(tmp.path());
    no_http_cfg.modules.domains.insert("http".to_owned(), false);
    let (_, no_http_prov) = build_provider_with_mock(&no_http_cfg.modules);
    assert_eq!(
        execute_on(&no_http_cfg, &harness, &no_http_prov, StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: "http_probe".to_owned(),
            args: json!({ "url": "https://example.invalid" }),
            run: None, node: None, worktree: None,
        })).unwrap_err().code,
        ErrorCode::DomainCapabilityUnavailable
    );
}

#[test]
fn seam_mock_domain_provider_plugin_tool_and_gate_tools_matrix() {
    let tmp = make_temp_dir("plugin-tool");
    let cfg = base_config(tmp.path());
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();

    for (name, tool_name, args, expect_err, delta) in [
        ("valid_probe", "http_probe", json!({ "url": "https://example.invalid/health" }), None, 1usize),
        ("missing_url", "http_probe", json!({}), Some(ErrorCode::ProfileSchemaInvalid), 0),
        ("extra_prop", "http_probe", json!({ "url": "https://x", "extra": 42 }), Some(ErrorCode::ProfileSchemaInvalid), 0),
        ("unknown_tool", "http_nonexistent", json!({}), Some(ErrorCode::DomainCapabilityUnavailable), 0),
    ] {
        let before = mock.tool_calls();
        let res = engine.execute(StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: tool_name.to_owned(),
            args,
            run: None,
            node: None,
            worktree: None,
        }));
        assert_eq!(mock.tool_calls() - before, delta, "delta for {name}");
        match expect_err {
            None => match res.unwrap() {
                StateCommandOutput::CallPluginTool(out) => assert_eq!(
                    out.output,
                    DomainToolOutput::Json(json!({ "tool": "probe", "url": "https://example.invalid/health", "status": 200 }))
                ),
                other => panic!("unexpected output for {name}: {other:?}"),
            },
            Some(code) => assert_eq!(res.unwrap_err().code, code, "case {name}"),
        }
    }

    let make_tool_prof = |tool: &str| format!(
        "version: 2\ninherit: base\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {{test}}\"\n  check:\n    command: \"true\"\n  lint:\n    command: \"true\"\ngates:\n  - id: probe_gate\n    proof: none\n    tools: [{tool}]\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n"
    );
    let bad_p = write_profile(tmp.path(), "plugin_bad.yaml", &make_tool_prof("http_missing_tool"));
    let ok_p = write_profile(tmp.path(), "plugin_ok.yaml", &make_tool_prof("http_probe"));
    assert_eq!(
        start_run(&engine, StartReq { goal: Some("Bad".to_owned()), profile: Some(bad_p), ..StartReq::default() }).unwrap_err().code,
        ErrorCode::DomainCapabilityUnavailable
    );
    let ok_started = start_run(&engine, StartReq { goal: Some("Ok".to_owned()), profile: Some(ok_p), in_place: true, ..StartReq::default() }).unwrap();
    clear_run(&engine, ok_started.run.run_id.as_str());

    // #1: In-flight mutating call Running trace inspects prev.idempotency_key without abort/re-dispatch,
    // even under out-of-scope drift, and respects recovery_only if finished concurrently.
    let mut_tmp = make_temp_dir("mut-call");
    let _ = init_git_repo(mut_tmp.path(), "main");
    let mut_p = write_profile(mut_tmp.path(), "mut_call.yaml", r#"version: 2
inherit: base
protected_paths: [Cargo.toml]
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
  check: { command: "true" }
  lint: { command: "true" }
gates:
  - id: deploy_gate
    proof: call
    call: http.dispatch
    with: { target: prod }
  - tasks:
      gates:
        - id: build
          proof: none
"#);
    let mut_cfg = base_config(mut_tmp.path());
    let (mut_mock, mut_prov) = build_provider_with_mock(&mut_cfg.modules);
    let mut_engine = StateEngine::open(mut_cfg, mut_prov).unwrap();
    let mut_start = start_run(&mut_engine, StartReq {
        goal: Some("Mutating call crash recovery".to_owned()),
        profile: Some(mut_p),
        in_place: true,
        ..StartReq::default()
    }).unwrap();
    let mut_rid = mut_start.run.run_id.clone();
    let mut_claim = claim_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, false, false, None).unwrap();
    let before_snap = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap();
    let deploy_node = before_snap.nodes.iter().find(|n| n.node_id == "root:deploy_gate").unwrap();
    let precond = StoragePreconditions {
        expected_run_generation: before_snap.run.generation,
        expected_node_versions: vec![NodeVersionExpectation {
            node_id: deploy_node.node_id.clone(),
            expected_version: deploy_node.node_version,
        }],
        expected_claim_revision: Some(mut_claim.claim_revision),
        expected_contract_revision: Some(deploy_node.contract_revision),
        expected_worker_id: deploy_node.worker_id.clone(),
        required_predecessor_trees: Vec::new(),
    };
    mut_engine.backend().exec_tx(|tx| {
        tx.start_trace(&precond, &TraceStartInput {
            run_id: mut_rid.clone(),
            node_id: deploy_node.node_id.clone(),
            claim_revision: mut_claim.claim_revision,
            contract_revision: deploy_node.contract_revision,
            execution_digest: "deadbeef".to_owned(),
            idempotency_key: Some("prior-crash-key".to_owned()),
            command_id: None,
            command_line: None,
            action_id: Some("http.dispatch".to_owned()),
            test_target: None,
            candidate_tree_before: mut_claim.candidate_tree.clone(),
            log_relpath: "logs/deploy.log".to_owned(),
            started_at: "2026-11-18T00:00:00.000Z".to_owned(),
        }).map(|_| ())
    }).unwrap();

    for (write_drift, expect_code, expect_lookup_delta) in [
        (false, ErrorCode::MutatingActionUncertainOutcome, 1usize),
        (true, ErrorCode::MutatingActionUncertainOutcome, 1),
    ] {
        if write_drift {
            fs::write(mut_tmp.path().join("Cargo.toml"), "[package]\nname = \"drift\"\n").unwrap();
        }
        let lb = mut_mock.lookup_calls();
        let ab = mut_mock.action_calls();
        let err = verify_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, Some(mut_claim.claim_revision)).unwrap_err();
        assert_eq!(err.code, expect_code);
        assert_eq!(mut_mock.lookup_calls() - lb, expect_lookup_delta);
        assert_eq!(mut_mock.action_calls() - ab, 0);
        let tr = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap().traces
            .into_iter().filter(|t| t.node_id == "root:deploy_gate").max_by_key(|t| t.trace_row_id.unwrap_or(0)).unwrap();
        assert_eq!(tr.status, TraceStatus::Running);
    }

    let tr_id = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap().traces
        .into_iter().filter(|t| t.node_id == "root:deploy_gate").max_by_key(|t| t.trace_row_id.unwrap_or(0)).unwrap().trace_row_id.unwrap();
    mut_engine.backend().exec_tx(|tx| tx.finish_trace(&TraceFinishInput {
        trace_row_id: tr_id, status: TraceStatus::Failed, exit_code: None,
        passed_tests: Vec::new(), failed_tests: Vec::new(), check_results: Vec::new(),
        candidate_tree_after: None, finished_at: "2026-11-18T00:01:00.000Z".to_owned(),
    })).unwrap();
    let ab = mut_mock.action_calls();
    assert_eq!(verify_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, Some(mut_claim.claim_revision)).unwrap_err().code, ErrorCode::OutOfScopeChanges);
    assert_eq!(mut_mock.action_calls() - ab, 0);
}

#[test]
fn seam_draft_sequential_numbering_and_archive_scan() {
    let tmp = make_temp_dir("draft");
    let milestones_dir = tmp.path().join("docs/milestones");
    fs::create_dir_all(milestones_dir.join("archive")).unwrap();
    fs::write(milestones_dir.join("AC-003-alpha.md"), "# AC-003\n").unwrap();
    fs::write(milestones_dir.join("archive/AC-007-omega.md"), "# AC-007\n").unwrap();

    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();

    match execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
        collection: "milestones".to_owned(), slug: "new-feature".to_owned(), prefix: None, title: Some("New Feature".to_owned()), profile: None,
    })).unwrap() {
        StateCommandOutput::Draft(d) => {
            assert_eq!(d.spec_id, "AC-008");
            assert_eq!(d.spec_path, "docs/milestones/AC-008-new-feature.md");
            assert!(tmp.path().join(&d.spec_path).is_file());
        }
        other => panic!("unexpected draft output: {other:?}"),
    }
    assert_eq!(
        execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(), slug: "---".to_owned(), prefix: None, title: None, profile: None,
        })).unwrap_err().code,
        ErrorCode::ProfileSchemaInvalid
    );
}

#[test]
fn seam_start_ephemeral_goal_and_cross_workspace_depends_on_matrix() {
    let root_tmp = make_temp_dir("start-main");
    let other_tmp = make_temp_dir("start-other");
    let mut cfg = base_config(root_tmp.path());
    cfg.linked_workspaces.insert("other".to_owned(), other_tmp.path().to_path_buf());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider.clone()).unwrap();

    let bound_p = write_profile(root_tmp.path(), "bound.yaml", "version: 2\ninherit: base\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n");
    assert_eq!(
        start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-001.md".to_owned()), goal: Some("Also".to_owned()), ..StartReq::default() }).unwrap_err().code,
        ErrorCode::ProfileSchemaInvalid
    );
    let eph = start_run(&engine, StartReq { goal: Some("Ship ephemeral run".to_owned()), profile: Some(bound_p.clone()), scope: vec!["crates/a/**".to_owned()], in_place: true, ..StartReq::default() }).unwrap();
    assert!(eph.run.is_ephemeral);
    assert!(root_tmp.path().join(eph.materialized_spec_path.unwrap()).is_file());
    assert!(root_tmp.path().join(format!(".acdd/run/{}/profile.resolved.yaml", eph.run.run_id)).is_file());
    clear_run(&engine, eph.run.run_id.as_str());

    fs::create_dir_all(root_tmp.path().join("docs/milestones")).unwrap();
    let dep_spec = "---\nid: AC-010\ntitle: Dependent Run\nstatus: planned\ndepends_on:\n  - \"other/AC-001:t1\"\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - Consumer works\n```\n";
    fs::write(root_tmp.path().join("docs/milestones/AC-010-dep.md"), dep_spec).unwrap();
    fs::write(root_tmp.path().join("docs/milestones/AC-010-bad-task.md"), dep_spec.replace("other/AC-001:t1", "other/AC-001:missing_task")).unwrap();

    let other_engine = StateEngine::open(base_config(other_tmp.path()), provider).unwrap();
    let og1 = build_rich_graph(other_tmp.path(), "run-other-001", IsolationMode::InPlace);
    let tomb1 = sample_tombstone("run-other-001", "other-cleared", Some("AC-001"), TerminalStatus::Cleared, None);
    other_engine.backend().exec_tx(|tx| { tx.insert_run_graph(&og1)?; tx.archive_and_prune_run(&empty_preconditions(1), &tomb1) }).unwrap();
    assert_eq!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()), profile: Some(bound_p.clone()), in_place: true, ..StartReq::default() }).unwrap_err().code, ErrorCode::GateVerificationFailed);

    let mut og2 = build_rich_graph(other_tmp.path(), "run-other-002", IsolationMode::InPlace);
    og2.run.run_name = RunName::new("other-completed").unwrap();
    let mut tomb2 = sample_tombstone("run-other-002", "other-completed", Some("AC-001"), TerminalStatus::Completed, Some("docs/milestones/archive/AC-001.md"));
    tomb2.closed_at = "2026-11-17T10:20:00.000Z".to_owned();
    fs::create_dir_all(other_tmp.path().join("docs/milestones/archive")).unwrap();
    fs::write(other_tmp.path().join("docs/milestones/archive/AC-001.md"), "---\nid: AC-001\ntitle: Producer Completed\nstatus: completed\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Producer task\nscope: [src/]\nstatus: completed\n```\n").unwrap();
    other_engine.backend().exec_tx(|tx| { tx.insert_run_graph(&og2)?; tx.archive_and_prune_run(&empty_preconditions(1), &tomb2) }).unwrap();

    assert_eq!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-bad-task.md".to_owned()), profile: Some(bound_p.clone()), in_place: true, ..StartReq::default() }).unwrap_err().code, ErrorCode::GateVerificationFailed);
    assert!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()), profile: Some(bound_p), in_place: true, ..StartReq::default() }).is_ok());
}

#[test]
fn seam_in_memory_lifecycle_subtask_blackboard_extend_scope_reopen_sync_clear() {
    let tmp = make_temp_dir("lifecycle");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let mut graph = build_rich_graph(tmp.path(), "run-life-001", IsolationMode::Worktree);

    if let Some(n) = graph.nodes.iter_mut().find(|n| n.node_id == "task:t2:build") {
        n.status = NodeStatus::InProgress;
        n.claim_revision = 1;
        n.worker_id = Some(WorkerId::new("worker-b").unwrap());
    }
    if let Some(n) = graph.nodes.iter_mut().find(|n| n.node_id == "task:t2") {
        n.status = NodeStatus::InProgress;
    }
    graph.claims.push(GateClaimRecord {
        claim_id: None, run_id: RunId::new("run-life-001").unwrap(), node_id: "task:t2:build".to_owned(), claim_revision: 1,
        worker_id: WorkerId::new("worker-b").unwrap(), session_fingerprint: Some("host:1:1".to_owned()),
        worktree: tmp.path().display().to_string(), span_id: "0123456789abcdef".to_owned(),
        claim_tree: TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
        claimed_at: "2026-11-17T10:01:00.000Z".to_owned(), ended_at: None, outcome: ClaimOutcome::Active,
    });
    harness.seed_run(&graph).unwrap();

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::Subtask(SubtaskReq {
        run: Some("run-life-001".to_owned()), task_ref: TaskRef::new("t1").unwrap(), subtask_ref: SubtaskRef::new("s1").unwrap(),
        status: SubtaskStatus::Completed, evidence: Some("   ".to_owned()),
    })).unwrap_err().code, ErrorCode::ProfileSchemaInvalid);
    assert!(matches!(execute_on(&cfg, &harness, &provider, StateCommand::Subtask(SubtaskReq {
        run: Some("run-life-001".to_owned()), task_ref: TaskRef::new("t1").unwrap(), subtask_ref: SubtaskRef::new("s1").unwrap(),
        status: SubtaskStatus::Completed, evidence: Some("implemented in crates/a/src/lib.rs".to_owned()),
    })).unwrap(), StateCommandOutput::Subtask(_)));

    let bb_post_out = execute_on(&cfg, &harness, &provider, StateCommand::BlackboardPost(BlackboardPostReq {
        run: Some("run-life-001".to_owned()), node: Some("t1.build".to_owned()), recipient_node: None, subtask_ref: None,
        gate: Some("build".to_owned()), author: "worker-a".to_owned(), topic: BlackboardTopic::Notes, payload: "shared note context".to_owned(),
    })).unwrap();
    assert_eq!(bb_post_out.into_untagged_json().unwrap()["entry"]["payload"], "shared note context");

    for (author, payload, expected_field) in [(" ", "non-empty", "author"), ("worker-a", " ", "payload")] {
        let err = execute_on(&cfg, &harness, &provider, StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some("run-life-001".to_owned()), node: None, recipient_node: None, subtask_ref: None, gate: None,
            author: author.to_owned(), topic: BlackboardTopic::Notes, payload: payload.to_owned(),
        })).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        assert!(err.message.contains(expected_field));
    }
    let bb_query_out = queries::execute_query(&cfg, &harness, &provider, StateQuery::BlackboardRead(BlackboardReadReq {
        run: Some("run-life-001".to_owned()), topic: Some(BlackboardTopic::Notes), ..BlackboardReadReq::default()
    })).unwrap();
    assert_eq!(bb_query_out.clone().into_untagged_json().unwrap()["total"], 1);
    match bb_query_out {
        StateQueryOutput::BlackboardRead(bb) => assert_eq!(bb.items[0].payload, "shared note context"),
        other => panic!("unexpected BlackboardRead output: {other:?}"),
    }

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::ExtendScope(ExtendScopeReq {
        run: Some("run-life-001".to_owned()), node: "t1".to_owned(), paths: vec!["crates/b/src/main.rs".to_owned()],
        reason: "needs sibling crate".to_owned(), worker_id: None,
    })).unwrap_err().code, ErrorCode::TaskScopeConflict);
    match execute_on(&cfg, &harness, &provider, StateCommand::ExtendScope(ExtendScopeReq {
        run: Some("run-life-001".to_owned()), node: "t1".to_owned(), paths: vec!["crates/c/**".to_owned()],
        reason: "shared test helper".to_owned(), worker_id: None,
    })).unwrap() {
        StateCommandOutput::ExtendScope(e) => {
            assert_eq!(e.contract_revision, 2);
            assert!(e.admitted_scope.contains(&"crates/c/**".to_owned()));
        }
        other => panic!("unexpected ExtendScope output: {other:?}"),
    }

    let snap = harness.load_run_graph(&RunId::new("run-life-001").unwrap()).unwrap().unwrap();
    harness.exec_tx(|tx| {
        let t1_n = snap.nodes.iter().find(|n| n.node_id == "task:t1").unwrap();
        let t1_b = snap.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap();
        let tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
        let mut p1 = NodeStatusPatch::status_only("task:t1", NodeStatus::Completed);
        p1.accepted_tree = Some(Some(tree.clone()));
        let mut p2 = NodeStatusPatch::status_only("task:t1:build", NodeStatus::Completed);
        p2.accepted_tree = Some(Some(tree));
        tx.apply_transition_plan(&snap.run.run_id, &TransitionPlan {
            preconditions: StoragePreconditions {
                expected_run_generation: snap.run.generation,
                expected_node_versions: vec![
                    NodeVersionExpectation { node_id: t1_n.node_id.clone(), expected_version: t1_n.node_version },
                    NodeVersionExpectation { node_id: t1_b.node_id.clone(), expected_version: t1_b.node_version },
                ],
                expected_claim_revision: None, expected_contract_revision: None, expected_worker_id: None, required_predecessor_trees: Vec::new(),
            },
            bump_run_generation: true, new_run_status: None, new_root_cycle_count: None, completed_at: None,
            node_patches: vec![p1, p2], dependency_patches: Vec::new(), claim_patches: Vec::new(),
            new_artifacts: Vec::new(), new_blackboard: Vec::new(), blackboard_patches: Vec::new(), new_subtasks: Vec::new(),
            significant_transition: true,
        })
    }).unwrap();

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::Reopen(ReopenReq {
        run: Some("run-life-001".to_owned()), node: "t1.build".to_owned(), cascade: false, reason: Some("fix".to_owned()),
    })).unwrap_err().code, ErrorCode::DependentTaskCompleted);
    assert!(matches!(execute_on(&cfg, &harness, &provider, StateCommand::Reopen(ReopenReq {
        run: Some("run-life-001".to_owned()), node: "t1.build".to_owned(), cascade: true, reason: Some("cascade".to_owned()),
    })).unwrap(), StateCommandOutput::Reopen(_)));

    fs::write(tmp.path().join("docs/milestones/AC-001-sample.md"), "---\nid: AC-001\ntitle: Sample Updated\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - One\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [crates/b/**]\ndepends_on: [t1]\nacceptance_criteria:\n  - Two\n```\n### task: t3\n```yaml\ntask_ref: t3\nscope: [crates/d/**]\nacceptance_criteria:\n  - Three\n```\n").unwrap();
    match execute_on(&cfg, &harness, &provider, StateCommand::Sync(SyncReq { run: Some("run-life-001".to_owned()), rebind_profile: false })).unwrap() {
        StateCommandOutput::Sync(s) => assert!(s.added_nodes >= 2),
        other => panic!("unexpected Sync output: {other:?}"),
    }
    match execute_on(&cfg, &harness, &provider, StateCommand::Clear(ClearReq { run: Some("run-life-001".to_owned()), reason: Some("cancelled".to_owned()), force: true })).unwrap() {
        StateCommandOutput::Clear(c) => {
            assert_eq!(c.tombstone.terminal_status, TerminalStatus::Cleared);
            assert!(harness.load_run_graph(&RunId::new("run-life-001").unwrap()).unwrap().is_none());
        }
        other => panic!("unexpected Clear output: {other:?}"),
    }
}

