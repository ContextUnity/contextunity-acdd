use super::common::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{RunId, RunName};
use acdd_state::harness::RunHarness;
use acdd_state::profile::ResolvedGateProfile;
use acdd_state::store::traits::{StateBackend, StateReader};
use acdd_state::types::{
    BlackboardTopic, FindingItem, FindingSeverity, IsolationMode, NodeStatus, RejectionFindings,
    RejectionKind, ReviewContourResult, ReviewEvidence, RunStatus, SubmitAction, TerminalStatus,
    TraceStartInput, WorktreeLifecycle,
};
use acdd_state::usecases::dto::{
    BlackboardPostReq, ListReq, ReopenReq, RunReq, StartReq, StatusReq, SubmitReq,
};
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateQuery, StateQueryEngine, StateQueryOutput,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[test]
fn seam_run_workflow_runner_required_and_dry_run_in_place() {
    let tmp = make_temp_dir("run-cmd");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    harness
        .seed_run(&build_rich_graph(
            tmp.path(),
            "run-auto-001",
            IsolationMode::InPlace,
        ))
        .unwrap();

    match harness
        .execute(
            &cfg,
            &provider,
            StateCommand::Run(RunReq {
                run: Some("run-auto-001".to_owned()),
                jobs: Some(4),
                allow_main: false,
                dry_run: true,
            }),
        )
        .unwrap()
    {
        StateCommandOutput::Run(resp) => {
            assert_eq!(resp.report.waves_executed, 0);
            assert!(resp
                .report
                .halted_reason
                .as_deref()
                .is_some_and(|s| s.contains("jobs=1")));
        }
        other => panic!("unexpected Run dry_run output: {other:?}"),
    }
    assert_eq!(
        harness
            .execute(
                &cfg,
                &provider,
                StateCommand::Run(RunReq {
                    run: Some("run-auto-001".to_owned()),
                    jobs: Some(1),
                    allow_main: false,
                    dry_run: false
                })
            )
            .unwrap_err()
            .code,
        ErrorCode::WorkflowRunnerRequired
    );
    let t1_build = harness
        .load_run_graph(&RunId::new("run-auto-001").unwrap())
        .unwrap()
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.node_id == "task:t1:build")
        .unwrap();
    assert_eq!(t1_build.status, NodeStatus::Ready);
    assert_eq!(t1_build.claim_revision, 0);

    // #10: managed worktree RoleDef.runner executes inside `claim_resp.worktree`, not primary.
    let wt_tmp = make_temp_dir("run-wt-cwd");
    let wt_root = wt_tmp.path();
    let _ = init_git_repo(wt_root, "main");
    let wt_p = write_profile(wt_root, "wt_runner.yaml", "version: 2\ninherit: code\ngit:\n  isolation: worktree\n  worktrees: managed\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  check: { command: \"true\" }\n  lint: { command: \"true\" }\nroles:\n  builder:\n    mode: subagent\n    runner: \"pwd > .acdd_runner_cwd\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n");
    let wt_cfg = base_config(wt_root);
    let (_, wt_prov) = build_provider_with_mock(&wt_cfg.modules);
    let wt_engine = StateEngine::open(wt_cfg, wt_prov).unwrap();
    let wt_start = start_run(
        &wt_engine,
        StartReq {
            goal: Some("Run inside managed task worktree".to_owned()),
            profile: Some(wt_p),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    assert_eq!(
        wt_engine
            .execute(StateCommand::Run(RunReq {
                run: Some(wt_start.run.run_id.to_string()),
                jobs: Some(1),
                allow_main: false,
                dry_run: false
            }))
            .unwrap_err()
            .code,
        ErrorCode::RunnerNoSubmission
    );
    let task_wt_dir = wt_root
        .join(".worktrees")
        .join(format!("{}-goal", wt_start.run.run_name));
    assert!(task_wt_dir.join(".acdd_runner_cwd").is_file());
    assert!(!wt_root.join(".acdd_runner_cwd").exists());
}

#[test]
fn seam_git_e2e_claim_verify_submit_and_archive() {
    // 1. Full Git E2E run: pre_verify (call+check+recovery), build, immutable, reject, need: stale, archive, landing, depends_on.
    let tmp = make_temp_dir("git-e2e");
    let repo = tmp.path();
    let _initial_head = init_git_repo(repo, "feat/phase2-e2e");
    let e2e_p = write_profile(
        repo,
        "e2e.yaml",
        r#"version: 2
inherit: code
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
  build_check: { command: "true" }
  verify_all: { command: "true" }
  ok_cmd:
    command: "true"
  test_cmd:
    command: "sh -c 'exit 0' -- {test_filter}"
    test_filter: "--filter {test}"
git:
  isolation: worktree
  worktrees: managed
gates:
  - id: pre_verify
    proof: command
    command: ok_cmd
    checks: [{ check: http.headers }]
    call: http.dispatch
    with: { target: prod-api }
    context: [http.endpoint_summary]
    save: { post: none }
  - tasks:
      gates:
        - id: build
          role: builder
          proof: command
          command: test_cmd
          save: { immutable: true }
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          independent_from: [build]
          need: [build]
          contours: [claims]
          receipt: true
          save: { post: commit }
          fail: build
  - id: handoff
    proof: none
    need: [pre_verify]
    receipt: true
    save: { post: commit, archive: true, prune: true }
"#,
    );
    fs::create_dir_all(repo.join("docs/milestones")).unwrap();
    fs::write(repo.join("docs/milestones/AC-020-e2e.md"), "---\nid: AC-020\ntitle: Full Phase 2 E2E Run\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\nacceptance_criteria:\n  - Function returns 42\n```\n").unwrap();

    let cfg = base_config(repo);
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg.clone(), provider.clone()).unwrap();
    let start_resp = start_run(
        &engine,
        StartReq {
            spec_path: Some("docs/milestones/AC-020-e2e.md".to_owned()),
            profile: Some(e2e_p),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let rid = start_resp.run.run_id.to_string();
    assert_eq!(start_resp.ready_nodes, vec!["pre_verify".to_owned()]);

    let unclaimed = pass_req(&rid, "pre_verify", "worker-pre", None, false);
    assert_eq!(
        submit_req(&engine, unclaimed).unwrap_err().code,
        ErrorCode::StaleClaimRevision
    );

    let pre_claim = claim_node(
        &engine,
        &rid,
        "pre_verify",
        "worker-pre",
        Some("host-a:999999:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert!(pre_claim
        .context_lenses
        .contains_key("http.endpoint_summary"));
    assert_eq!(
        claim_node(
            &engine,
            &rid,
            "pre_verify",
            "worker-pre",
            Some("host-a:999999:1"),
            false,
            false,
            None
        )
        .unwrap()
        .claim_revision,
        pre_claim.claim_revision
    );
    assert_eq!(
        claim_node(
            &engine,
            &rid,
            "pre_verify",
            "thief",
            Some("host-b:111:1"),
            true,
            false,
            None
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskAlreadyClaimed
    );

    let wrong_worker = pass_req(
        &rid,
        "pre_verify",
        "thief",
        Some(pre_claim.claim_revision),
        false,
    );
    assert_eq!(
        submit_req(&engine, wrong_worker).unwrap_err().code,
        ErrorCode::TaskAlreadyClaimed
    );
    let stale_rev = pass_req(
        &rid,
        "pre_verify",
        "worker-pre",
        Some(pre_claim.claim_revision + 1),
        false,
    );
    assert_eq!(
        submit_req(&engine, stale_rev).unwrap_err().code,
        ErrorCode::StaleClaimRevision
    );
    let no_trace = pass_req(
        &rid,
        "pre_verify",
        "worker-pre",
        Some(pre_claim.claim_revision),
        false,
    );
    let err = submit_req(&engine, no_trace).unwrap_err();
    assert_eq!(err.code, ErrorCode::TaskProofUnobserved);
    assert!(err.message.contains("run verify"));
    assert_eq!(mock.action_calls(), 0);

    let pre_verify_resp = verify_node(
        &engine,
        &rid,
        "pre_verify",
        "worker-pre",
        None,
        Some(pre_claim.claim_revision),
    )
    .unwrap();
    assert!(
        pre_verify_resp.passed
            && pre_verify_resp.check_results.len() == 1
            && mock.action_calls() == 1
    );

    // Inject interrupted Running trace and verify idempotency recovery.
    let pre_backend = backend_for(&cfg);
    let pre_snap = pre_backend
        .load_run_graph(&start_resp.run.run_id)
        .unwrap()
        .unwrap();
    let pre_node = pre_snap
        .nodes
        .iter()
        .find(|n| n.node_id == "root:pre_verify")
        .unwrap()
        .clone();
    pre_backend
        .exec_tx(|tx| {
            tx.start_trace(
                &acdd_state::machine::graph::build_preconditions(
                    &pre_snap,
                    &[pre_node.node_id.as_str()],
                    Some(&pre_node),
                ),
                &TraceStartInput {
                    run_id: start_resp.run.run_id.clone(),
                    node_id: pre_node.node_id.clone(),
                    claim_revision: pre_node.claim_revision,
                    contract_revision: pre_node.contract_revision,
                    execution_digest: pre_verify_resp.execution_digest.clone(),
                    idempotency_key: Some(format!(
                        "{}:{}:{}:{}",
                        start_resp.run.run_id,
                        pre_node.node_id,
                        pre_node.claim_revision,
                        pre_verify_resp.execution_digest
                    )),
                    command_id: Some("ok_cmd".to_owned()),
                    command_line: None,
                    action_id: Some("http.dispatch".to_owned()),
                    test_target: None,
                    candidate_tree_before: pre_node.claim_tree.clone().unwrap(),
                    log_relpath: "logs/crash.log".to_owned(),
                    started_at: "2026-11-17T10:02:00.000Z".to_owned(),
                },
            )
            .map(|_| ())
        })
        .unwrap();
    let action_before = mock.action_calls();
    assert!(
        verify_node(
            &engine,
            &rid,
            "pre_verify",
            "worker-pre",
            None,
            Some(pre_claim.claim_revision)
        )
        .unwrap()
        .passed
    );
    assert_eq!(
        (mock.action_calls(), mock.lookup_calls()),
        (action_before, 1)
    );

    let mut pre_pass = pass_req(
        &rid,
        "pre_verify",
        "worker-pre",
        Some(pre_claim.claim_revision),
        false,
    );
    pre_pass.summary = Some("pre verified".to_owned());
    assert!(submit_req(&engine, pre_pass)
        .unwrap()
        .committed_oid
        .is_none());

    let t1_claim = claim_node(
        &engine,
        &rid,
        "t1.build",
        "builder-1",
        Some("host-a:200:1"),
        false,
        false,
        None,
    )
    .unwrap();
    let task_wt = PathBuf::from(&t1_claim.worktree);
    assert!(task_wt.is_dir());
    fs::write(task_wt.join("README.md"), "# Out of scope edit\n").unwrap();
    assert_eq!(
        verify_node(
            &engine,
            &rid,
            "t1.build",
            "builder-1",
            Some("t1_tests"),
            Some(t1_claim.claim_revision)
        )
        .unwrap_err()
        .code,
        ErrorCode::OutOfScopeChanges
    );

    fs::write(task_wt.join("README.md"), "# Test Repo\n").unwrap();

    let t1_c2 = claim_node(
        &engine,
        &rid,
        "t1.build",
        "builder-1",
        Some("host-a:200:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert!(
        verify_node(
            &engine,
            &rid,
            "t1.build",
            "builder-1",
            Some("t1_tests"),
            Some(t1_c2.claim_revision)
        )
        .unwrap()
        .passed
    );
    fs::write(task_wt.join("src/lib.rs"), "pub fn feat() -> i32 { 42 }\n").unwrap();
    let mut immut_req = pass_req(
        &rid,
        "t1.build",
        "builder-1",
        Some(t1_c2.claim_revision),
        false,
    );
    immut_req.test_target = Some("t1_tests".to_owned());
    assert_eq!(
        submit_req(&engine, immut_req).unwrap_err().code,
        ErrorCode::TaskCandidateMismatch
    );

    let t1_c3 = claim_node(
        &engine,
        &rid,
        "t1.build",
        "builder-1",
        Some("host-a:200:2"),
        true,
        false,
        None,
    )
    .unwrap();
    assert!(t1_c3.claim_revision > t1_c2.claim_revision);
    assert!(
        verify_node(
            &engine,
            &rid,
            "t1.build",
            "builder-1",
            Some("t1_tests"),
            Some(t1_c3.claim_revision)
        )
        .unwrap()
        .passed
    );
    let mut b_pass = pass_req(
        &rid,
        "t1.build",
        "builder-1",
        Some(t1_c3.claim_revision),
        false,
    );
    b_pass.summary = Some("build contract verified".to_owned());
    submit_req(&engine, b_pass).unwrap();

    // Independence check -> force steal reject -> rework -> review pass -> integrate -> need drift -> reopen -> handoff archive.
    let same_rev_c = claim_node(
        &engine,
        &rid,
        "t1.review",
        "builder-1",
        Some("host-a:200:2"),
        false,
        false,
        None,
    )
    .unwrap();
    let contours = BTreeMap::from([(
        "claims".to_owned(),
        ReviewContourResult {
            applicable: true,
            evidence: "verified feat() returns 42".to_owned(),
        },
    )]);
    let same_worker_req = SubmitReq {
        review_evidence: Some(ReviewEvidence {
            decision: SubmitAction::Pass,
            contours: contours.clone(),
            summary: "self pass".to_owned(),
            notes: None,
        }),
        ..pass_req(
            &rid,
            "t1.review",
            "builder-1",
            Some(same_rev_c.claim_revision),
            false,
        )
    };
    assert_eq!(
        submit_req(&engine, same_worker_req).unwrap_err().code,
        ErrorCode::TaskReviewNotIndependent
    );

    let rev_c = claim_node(
        &engine,
        &rid,
        "t1.review",
        "reviewer-1",
        Some("host-a:201:1"),
        true,
        true,
        None,
    )
    .unwrap();
    submit_req(
        &engine,
        SubmitReq {
            action: SubmitAction::Reject,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::Defect,
                summary: "need cleaner docs comment".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Medium,
                    path: Some("src/lib.rs".to_owned()),
                    message: "missing doc comment".to_owned(),
                    remediation: "add /// doc comment".to_owned(),
                }],
            }),
            ..pass_req(
                &rid,
                "t1.review",
                "reviewer-1",
                Some(rev_c.claim_revision),
                false,
            )
        },
    )
    .unwrap();

    let b2 = claim_node(
        &engine,
        &rid,
        "t1.build",
        "builder-1",
        Some("host-a:200:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        b2.unresolved_findings
            .as_ref()
            .and_then(|f| f["findings"]["summary"].as_str()),
        Some("need cleaner docs comment")
    );
    verify_node(
        &engine,
        &rid,
        "t1.build",
        "builder-1",
        Some("t1_tests"),
        Some(b2.claim_revision),
    )
    .unwrap();
    let mut b2_pass = pass_req(
        &rid,
        "t1.build",
        "builder-1",
        Some(b2.claim_revision),
        false,
    );
    b2_pass.test_target = Some("t1_tests".to_owned());
    submit_req(&engine, b2_pass).unwrap();

    let r2 = claim_node(
        &engine,
        &rid,
        "t1.review",
        "reviewer-1",
        Some("host-a:201:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert!(
        r2.unresolved_findings.is_none(),
        "expected resolved findings cleared after passing build, got: {:?}",
        r2.unresolved_findings
    );
    let rev_pass = submit_req(
        &engine,
        SubmitReq {
            review_evidence: Some(ReviewEvidence {
                decision: SubmitAction::Pass,
                contours,
                summary: "LGTM".to_owned(),
                notes: None,
            }),
            summary: Some("review passed".to_owned()),
            ..pass_req(
                &rid,
                "t1.review",
                "reviewer-1",
                Some(r2.claim_revision),
                false,
            )
        },
    )
    .unwrap();
    assert!(rev_pass.committed_oid.is_some());

    assert_eq!(
        claim_node(
            &engine,
            &rid,
            "handoff",
            "handoff-1",
            Some("host-a:202:1"),
            false,
            false,
            None
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskNotIntegrated
    );
    let run_wt = PathBuf::from(start_resp.run.worktree.as_deref().unwrap());
    let t1_branch = git_ok(&task_wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git_ok(&run_wt, &["merge", "--ff-only", t1_branch.trim()]);

    let h_claim = claim_node(
        &engine,
        &rid,
        "handoff",
        "handoff-1",
        Some("host-a:202:1"),
        false,
        false,
        None,
    )
    .unwrap();
    let mut drift_req = pass_req(
        &rid,
        "handoff",
        "handoff-1",
        Some(h_claim.claim_revision),
        false,
    );
    drift_req.summary = Some("should detect pre_verify drift".to_owned());
    assert_eq!(
        submit_req(&engine, drift_req).unwrap_err().code,
        ErrorCode::TaskCandidateMismatch
    );
    assert_eq!(
        reader_for(&cfg)
            .load_run_graph(&start_resp.run.run_id)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.node_id == "root:pre_verify")
            .unwrap()
            .status,
        NodeStatus::Stale
    );

    engine
        .execute(StateCommand::Reopen(ReopenReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            cascade: false,
            reason: Some("reopen stale".to_owned()),
        }))
        .unwrap();
    let pre2 = claim_node(
        &engine,
        &rid,
        "pre_verify",
        "worker-pre",
        Some("host-a:999999:2"),
        false,
        false,
        None,
    )
    .unwrap();
    verify_node(
        &engine,
        &rid,
        "pre_verify",
        "worker-pre",
        None,
        Some(pre2.claim_revision),
    )
    .unwrap();
    let mut pre2_pass = pass_req(
        &rid,
        "pre_verify",
        "worker-pre",
        Some(pre2.claim_revision),
        false,
    );
    pre2_pass.summary = Some("re-verified".to_owned());
    submit_req(&engine, pre2_pass).unwrap();

    let mut h_pass = pass_req(
        &rid,
        "handoff",
        "handoff-1",
        Some(h_claim.claim_revision),
        false,
    );
    h_pass.summary = Some("all tasks shipped".to_owned());
    let h_sub = submit_req(&engine, h_pass).unwrap();
    assert_eq!(h_sub.run_status, RunStatus::Completed);
    assert_eq!(
        h_sub.archive_path.as_deref(),
        Some("docs/milestones/archive/AC-020-e2e.md")
    );
    let closed_oid = h_sub.committed_oid.expect("handoff committed_oid");
    assert!(!PathBuf::from(start_resp.run.worktree.as_deref().unwrap()).exists());
    assert!(!repo.join("docs/milestones/archive/AC-020-e2e.md").exists());
    assert!(!repo.join("docs/milestones/AC-020-e2e.md").exists());
    assert_eq!(
        git_ok(repo, &["rev-parse", "--verify", "refs/heads/run/AC-020"]),
        closed_oid.as_str()
    );
    assert!(git_ok(
        repo,
        &[
            "show",
            &format!(
                "{}:docs/milestones/archive/AC-020-e2e.md",
                closed_oid.as_str()
            )
        ]
    )
    .contains("id: AC-020"));

    let pre_land_doc = acdd_state::spec::parse_spec("---\nid: AC-021\ntitle: Dependent Before Landing\nstatus: planned\ndepends_on: [\"AC-020:t1\"]\n---\n### task: t1\n```yaml\ntarget: consume AC-020\nscope: [src/]\n```\n").unwrap();
    acdd_state::usecases::deps::verify_run_level_depends_on(&cfg, &reader_for(&cfg), &pre_land_doc)
        .expect("closed_commit archive satisfies depends_on before landing");
    git_ok(repo, &["merge", "--ff-only", "refs/heads/run/AC-020"]);
    assert!(repo.join("docs/milestones/archive/AC-020-e2e.md").is_file());
    assert!(!repo.join("docs/milestones/AC-020-e2e.md").exists());
    match query_with(
        &cfg,
        &provider,
        StateQuery::List(ListReq {
            include_tombstones: true,
            ..ListReq::default()
        }),
    )
    .unwrap()
    {
        StateQueryOutput::List(l) => {
            assert_eq!((l.page.total_runs, l.page.total_tombstones), (0, 1));
            assert_eq!(l.page.tombstones[0].spec_id.as_deref(), Some("AC-020"));
            assert_eq!(
                l.page.tombstones[0].terminal_status,
                TerminalStatus::Completed
            );
            assert!(!l.page.tombstones[0].cleanup_pending);
        }
        other => panic!("unexpected List output: {other:?}"),
    }
    assert!(repo.join(format!(".acdd/run/{rid}/traces.jsonl")).is_file());

    // 2. Unsalted check + fail-closed no-commit archive recovery matrix.
    let nocommit_tmp = make_temp_dir("nocommit-rec");
    let nroot = nocommit_tmp.path();
    init_git_repo(nroot, "main");
    fs::create_dir_all(nroot.join("docs/milestones/archive")).unwrap();
    let nc_p = write_profile(
        nroot,
        "nocommit.yaml",
        r#"version: 2
inherit: none
git: { isolation: in_place, worktrees: manual }
commands:
  build_cmd:
    command: "sh -c 'printf \"test %s ... ok\\n\" \"$2\"; exit 0' -- {test_filter}"
    test_filter: "--filter {test}"
gates:
  - id: pre
    proof: command
    command: build_cmd
    checks: [{ check: http.headers }]
    save: { post: none }
  - tasks:
      gates:
        - id: prep
          proof: none
          pass: work
        - id: work
          proof: none
          need: [prep]
          receipt: true
  - id: handoff
    proof: none
    need: [pre]
    receipt: true
    save: { post: none, immutable: true, archive: true, prune: true }
"#,
    );
    fs::write(nroot.join("docs/milestones/AC-300.md"), "---\nid: AC-300\ntitle: NoCommit Recovery\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: work\nscope: [src/lib.rs]\n```\n").unwrap();
    git_ok(nroot, &["add", "."]);
    git_ok(nroot, &["commit", "-m", "seed nocommit"]);
    let ncfg = base_config(nroot);
    let (nmock, nprov) = build_provider_with_mock(&ncfg.modules);
    let neng = StateEngine::open(ncfg, nprov).unwrap();
    let nstart = start_run(
        &neng,
        StartReq {
            spec_path: Some("docs/milestones/AC-300.md".to_owned()),
            profile: Some(nc_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let nrid = nstart.run.run_id.to_string();

    let pre_c = claim_node(&neng, &nrid, "pre", "w-nc", None, false, false, None).unwrap();
    nmock.set_unsalted_check(true);
    assert!(
        verify_node(
            &neng,
            &nrid,
            "pre",
            "w-nc",
            Some("t_unsalted"),
            Some(pre_c.claim_revision)
        )
        .unwrap()
        .passed
    );
    nmock.set_fail_check(true);
    let mut unsalted_req = pass_req(&nrid, "pre", "w-nc", Some(pre_c.claim_revision), true);
    unsalted_req.test_target = Some("t_unsalted".to_owned());
    assert_eq!(
        submit_req(&neng, unsalted_req).unwrap_err().code,
        ErrorCode::TaskProofUnobserved
    );
    nmock.set_fail_check(false);
    nmock.set_unsalted_check(false);

    verify_node(
        &neng,
        &nrid,
        "pre",
        "w-nc",
        Some("t_ok"),
        Some(pre_c.claim_revision),
    )
    .unwrap();
    let mut pre_ok_req = pass_req(&nrid, "pre", "w-nc", Some(pre_c.claim_revision), true);
    pre_ok_req.test_target = Some("t_ok".to_owned());
    submit_req(&neng, pre_ok_req).unwrap();
    let t1_p = claim_node(&neng, &nrid, "t1.prep", "w-nc", None, false, false, None).unwrap();
    submit_req(
        &neng,
        pass_req(&nrid, "t1.prep", "w-nc", Some(t1_p.claim_revision), true),
    )
    .unwrap();
    let t1_c = claim_node(&neng, &nrid, "t1.work", "w-nc", None, false, false, None).unwrap();
    submit_req(
        &neng,
        pass_req(&nrid, "t1.work", "w-nc", Some(t1_c.claim_revision), true),
    )
    .unwrap();
    let h_nc = claim_node(&neng, &nrid, "handoff", "w-nc", None, false, false, None).unwrap();
    fs::remove_file(nroot.join("docs/milestones/AC-300.md")).unwrap();

    let base_line = nstart
        .run
        .target_base
        .as_deref()
        .map(|b| format!("  target_base: {b}\n"))
        .unwrap_or_default();
    let commit_line = nstart
        .run
        .target_base_commit
        .as_ref()
        .map(|c| format!("  target_base_commit: {c}\n"))
        .unwrap_or_default();
    let mismatched_md = format!(
        "---\nid: AC-300\ntitle: NoCommit Recovery\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n{base_line}{commit_line}---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: work\nscope: [src/lib.rs]\n```\n"
    );
    for (label, content, drift_lib, expect_code) in [
        ("bogus", "not valid spec frontmatter", false, ErrorCode::ExternalGitDrift),
        ("wrong_id", "---\nid: AC-999\ntitle: Wrong\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n---\n# Wrong\n", false, ErrorCode::ExternalGitDrift),
        ("mismatched_closure", mismatched_md.as_str(), false, ErrorCode::ExternalGitDrift),
        ("immutable_drift", mismatched_md.as_str(), true, ErrorCode::TaskCandidateMismatch),
    ] {
        fs::write(nroot.join("docs/milestones/archive/AC-300.md"), content).unwrap();
        if drift_lib {
            fs::write(nroot.join("src/lib.rs"), "pub fn drifted_after_claim() {}\n").unwrap();
        }
        assert_eq!(submit_req(&neng, pass_req(&nrid, "handoff", "w-nc", Some(h_nc.claim_revision), true)).unwrap_err().code, expect_code, "{label}");
    }

    // 3. P7 seams (#25-state, #13, #15, #7, #16-usecases, #5, #2) on shared Git repo.
    let p7_tmp = make_temp_dir("p7-seams");
    let p7_root = p7_tmp.path();
    let _ = init_git_repo(p7_root, "feat/p7");
    fs::write(p7_root.join("src/a.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
    fs::write(p7_root.join("src/b.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
    git_ok(p7_root, &["add", "."]);
    git_ok(p7_root, &["commit", "-m", "add src a and b"]);
    let p7_p = write_profile(p7_root, "p7_base.yaml", "version: 2\ninherit: code\ngit: { isolation: worktree, worktrees: manual }\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  check: { command: \"true\" }\n  lint: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: command\n          command: test\n          pass: review\n        - id: review\n          proof: none\n          need: [build]\n          save: { immutable: true }\n");
    let p7_managed_p = write_profile(p7_root, "p7_managed.yaml", "version: 2\ninherit: none\ngit: { isolation: worktree, worktrees: managed }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n");
    fs::create_dir_all(p7_root.join("docs/milestones")).unwrap();
    fs::write(p7_root.join("docs/milestones/AC-700-seams.md"), "---\nid: AC-700\ntitle: P7 Seams\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/a.rs]\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [src/b.rs]\n```\n").unwrap();

    let p7_cfg = base_config(p7_root);
    let (_, p7_prov) = build_provider_with_mock(&p7_cfg.modules);
    let p7_eng = StateEngine::open(p7_cfg.clone(), p7_prov.clone()).unwrap();

    let wt_s = start_run(
        &p7_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()),
            profile: Some(p7_p.clone()),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let disk_prof: ResolvedGateProfile = serde_yaml::from_str(
        &fs::read_to_string(
            p7_root
                .join(".acdd/run")
                .join(wt_s.run.run_id.as_str())
                .join("profile.resolved.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (disk_prof.git.isolation, disk_prof.git.worktrees),
        (IsolationMode::Worktree, WorktreeLifecycle::Managed)
    );
    assert!(claim_node(
        &p7_eng,
        wt_s.run.run_id.as_str(),
        "t1.build",
        "w-wt",
        None,
        false,
        false,
        None
    )
    .unwrap()
    .worktree
    .contains(".worktrees/"));
    clear_run(&p7_eng, wt_s.run.run_id.as_str());

    let ip_s = start_run(
        &p7_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()),
            profile: Some(p7_p),
            ..StartReq::default()
        },
    )
    .unwrap();
    let ip_rid = ip_s.run.run_id.clone();

    let mut unbound_cfg = p7_cfg.clone();
    unbound_cfg.worktree = p7_root.join(".worktrees/unbound-checkout");
    fs::create_dir_all(&unbound_cfg.worktree).unwrap();
    assert_eq!(
        StateQueryEngine::open_read_only(unbound_cfg, p7_prov.clone())
            .unwrap()
            .query(StateQuery::Status(StatusReq::default()))
            .unwrap_err()
            .code,
        ErrorCode::RunSelectorAmbiguous
    );

    let mut tiny_cfg = p7_cfg.clone();
    tiny_cfg.response_policy.max_output_bytes = 64;
    let budget_eng = StateEngine::open(tiny_cfg, p7_prov.clone()).unwrap();
    assert_eq!(
        claim_node(
            &budget_eng,
            ip_rid.as_str(),
            "t1.build",
            "w-budget",
            None,
            false,
            false,
            None
        )
        .unwrap_err()
        .code,
        ErrorCode::GuidanceBudgetExceeded
    );
    let t1b_after_budget = reader_for(&p7_cfg)
        .load_run_graph(&ip_rid)
        .unwrap()
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.node_id == "task:t1:build")
        .unwrap();
    assert_eq!(
        (t1b_after_budget.status, t1b_after_budget.claim_revision),
        (NodeStatus::Ready, 0)
    );

    let c1 = claim_node(
        &p7_eng,
        ip_rid.as_str(),
        "t1.build",
        "w-1",
        None,
        false,
        false,
        None,
    )
    .unwrap();

    verify_node(
        &p7_eng,
        ip_rid.as_str(),
        "t1.build",
        "w-1",
        None,
        Some(c1.claim_revision),
    )
    .unwrap();
    submit_req(
        &p7_eng,
        pass_req(
            ip_rid.as_str(),
            "t1.build",
            "w-1",
            Some(c1.claim_revision),
            true,
        ),
    )
    .unwrap();
    let r1 = claim_node(
        &p7_eng,
        ip_rid.as_str(),
        "t1.review",
        "w-rev1",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    fs::write(p7_root.join("src/a.rs"), "pub fn a() -> i32 { 999 }\n").unwrap();
    assert_eq!(
        submit_req(
            &p7_eng,
            pass_req(
                ip_rid.as_str(),
                "t1.review",
                "w-rev1",
                Some(r1.claim_revision),
                true
            )
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskCandidateMismatch
    );
    assert_eq!(
        reader_for(&p7_cfg)
            .load_run_graph(&ip_rid)
            .unwrap()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.node_id == "task:t1:build")
            .unwrap()
            .status,
        NodeStatus::Stale
    );
    clear_run(&p7_eng, ip_rid.as_str());

    let mgr_s = start_run(
        &p7_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()),
            profile: Some(p7_managed_p.clone()),
            ..StartReq::default()
        },
    )
    .unwrap();
    let collide_wt = p7_root.join(format!(".worktrees/{}-t1", mgr_s.run.run_name));
    fs::create_dir_all(&collide_wt).unwrap();
    assert_eq!(
        claim_node(
            &p7_eng,
            mgr_s.run.run_id.as_str(),
            "t1.build",
            "w-col",
            None,
            false,
            false,
            Some(collide_wt)
        )
        .unwrap_err()
        .code,
        ErrorCode::ExternalGitDrift
    );
    clear_run(&p7_eng, mgr_s.run.run_id.as_str());

    let start_col_wt = p7_root.join(".worktrees/start-col-run");
    fs::create_dir_all(&start_col_wt).unwrap();
    assert_eq!(
        start_run(
            &p7_eng,
            StartReq {
                spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()),
                run_name: Some(RunName::new("start-col-run").unwrap()),
                profile: Some(p7_managed_p),
                worktree_path: Some(start_col_wt),
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ExternalGitDrift
    );

    // 4. Commit-mode preflight skip (#3), linked-worktree tombstone spec cleanup, and prune without archive.
    let cskip_tmp = make_temp_dir("commit-preflight-skip");
    let cs_root = cskip_tmp.path();
    init_git_repo(cs_root, "main");
    fs::create_dir_all(cs_root.join("docs/milestones/archive")).unwrap();
    let cskip_p = write_profile(cs_root, "cskip.yaml", "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual }\ncommands:\n  fail_cmd: { command: \"false\" }\ngates:\n  - id: handoff\n    proof: command\n    command: fail_cmd\n    save: { post: commit, archive: true, prune: true }\n");
    fs::write(
        cs_root.join("docs/milestones/AC-310.md"),
        "---\nid: AC-310\ntitle: Commit Preflight Skip\nstatus: planned\n---\n# Empty\n",
    )
    .unwrap();
    git_ok(cs_root, &["add", "."]);
    git_ok(cs_root, &["commit", "-m", "seed AC-310"]);
    let cs_cfg = base_config(cs_root);
    let (_, cs_prov) = build_provider_with_mock(&cs_cfg.modules);
    let cs_eng = StateEngine::open(cs_cfg.clone(), cs_prov).unwrap();
    let cs_s = start_run(
        &cs_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-310.md".to_owned()),
            profile: Some(cskip_p.clone()),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let cs_c = claim_node(
        &cs_eng,
        cs_s.run.run_id.as_str(),
        "handoff",
        "w-cs",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    fs::write(cs_root.join("docs/milestones/archive/AC-310.md"), "---\nid: AC-310\ntitle: Commit Preflight Skip\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    assert_eq!(
        submit_req(
            &cs_eng,
            pass_req(
                cs_s.run.run_id.as_str(),
                "handoff",
                "w-cs",
                Some(cs_c.claim_revision),
                true
            )
        )
        .unwrap_err()
        .code,
        ErrorCode::ExternalGitDrift
    );
    clear_run(&cs_eng, cs_s.run.run_id.as_str());

    let prune_p = write_profile(cs_root, "prune_only.yaml", "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual }\ngates:\n  - id: handoff\n    proof: none\n    save: { post: none, archive: false, prune: true }\n");
    fs::write(
        cs_root.join("docs/milestones/AC-NP.md"),
        "---\nid: AC-NP\ntitle: Prune Without Archive\nstatus: planned\n---\n# Keep\n",
    )
    .unwrap();
    let np_linked_wt = cs_root.join("np-linked-wt");
    git_ok(
        cs_root,
        &[
            "worktree",
            "add",
            "-b",
            "feat/ac-np",
            np_linked_wt.to_str().unwrap(),
        ],
    );
    let np_linked_spec = np_linked_wt.join("docs/milestones/AC-NP.md");
    fs::create_dir_all(np_linked_spec.parent().unwrap()).unwrap();
    fs::write(
        &np_linked_spec,
        "---\nid: AC-NP\ntitle: Prune Without Archive\nstatus: planned\n---\n# Keep Linked\n",
    )
    .unwrap();
    let np_s = start_run(
        &cs_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-NP.md".to_owned()),
            profile: Some(prune_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let np_c = claim_node(
        &cs_eng,
        np_s.run.run_id.as_str(),
        "handoff",
        "w-np",
        None,
        false,
        false,
        Some(np_linked_wt.clone()),
    )
    .unwrap();
    let mut np_pass = pass_req(
        np_s.run.run_id.as_str(),
        "handoff",
        "w-np",
        Some(np_c.claim_revision),
        true,
    );
    np_pass.worktree = Some(np_linked_wt);
    submit_req(&cs_eng, np_pass).unwrap();
    let np_tomb = reader_for(&cs_cfg)
        .load_tombstone(&np_s.run.run_id)
        .unwrap()
        .unwrap();
    assert!(!np_tomb
        .cleanup_manifest
        .iter()
        .any(|e| e.kind == "spec_file"));
    assert!(cs_root.join("docs/milestones/AC-NP.md").exists() && np_linked_spec.exists());

    // 5. Mechanical `max_net_loc` budget check (custom pre-task container, untracked UTF-8 path, explicit override decisions),
    //    `{scope_target}` digest sensitivity, custom builder role drift reset, and legacy `v0.6.0` trace compatibility.
    let loc_tmp = make_temp_dir("max-net-loc");
    let loc_root = loc_tmp.path();
    init_git_repo(loc_root, "main");
    fs::create_dir_all(loc_root.join("tests")).unwrap();
    fs::write(loc_root.join("tests/t1_test.rs"), "#[test] fn t0() {}\n").unwrap();
    fs::create_dir_all(loc_root.join("docs/milestones")).unwrap();
    let loc_p = write_profile(
        loc_root,
        "loc.yaml",
        "version: 2\ninherit: code\ngit: { isolation: in_place, worktrees: manual }\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - id: design\n    gates:\n      - id: synthesize\n        role: architect\n        proof: blueprint\n        pass: review\n      - id: review\n        role: independent_reviewer\n        proof: none\n        independent_from: [synthesize]\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n          pass: review\n        - id: review\n          role: independent_reviewer\n          proof: none\n          independent_from: [build]\n          need: [build]\n          receipt: true\n          fail: build\n",
    );
    fs::write(
        loc_root.join("docs/milestones/AC-LOC.md"),
        "---\nid: AC-LOC\ntitle: Max Net LOC\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs, src/é.rs, tests/t1_test.rs]\n```\n",
    )
    .unwrap();
    git_ok(loc_root, &["add", "."]);
    git_ok(loc_root, &["commit", "-m", "seed loc"]);
    let loc_cfg = base_config(loc_root);
    let (_, loc_prov) = build_provider_with_mock(&loc_cfg.modules);
    let loc_eng = StateEngine::open(loc_cfg.clone(), loc_prov.clone()).unwrap();
    let loc_start = start_run(
        &loc_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-LOC.md".to_owned()),
            profile: Some(loc_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let loc_rid = loc_start.run.run_id.clone();

    let syn_c = claim_node(
        &loc_eng,
        loc_rid.as_str(),
        "design.synthesize",
        "arch-1",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    let mut syn_pass = pass_req(
        loc_rid.as_str(),
        "design.synthesize",
        "arch-1",
        Some(syn_c.claim_revision),
        true,
    );
    syn_pass.custom_evidence = Some(json!({
        "cited_invariants": ["INV-SINGLE-OWNER"],
        "cited_symbols": ["base"],
        "global_negative_boundaries": ["none"],
        "blackboard_decision_id": "bb-1",
        "tasks": {
            "t1": {
                "layer": "core",
                "summary": "extend base",
                "exported_interfaces": ["base"],
                "negative_boundaries": ["no shadow helpers"],
                "incoming_seams": [],
                "outgoing_seams": [],
                "canonical_owners": ["src/lib.rs::base"],
                "mechanism_rules": ["keep net prod delta <= 2 lines"],
                "max_net_loc": 2
            }
        }
    }));
    submit_req(&loc_eng, syn_pass).unwrap();
    let syn_r = claim_node(
        &loc_eng,
        loc_rid.as_str(),
        "design.review",
        "rev-bp",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    submit_req(
        &loc_eng,
        pass_req(
            loc_rid.as_str(),
            "design.review",
            "rev-bp",
            Some(syn_r.claim_revision),
            true,
        ),
    )
    .unwrap();

    let b_loc = claim_node(
        &loc_eng,
        loc_rid.as_str(),
        "t1.build",
        "builder-loc",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    // Test additions (+10 lines) excluded; +2 tracked (`src/lib.rs`) + +2 untracked UTF-8 (`src/é.rs`) -> fails with +4 vs max_net_loc=2.
    fs::write(
        loc_root.join("tests/t1_test.rs"),
        (0..11)
            .map(|i| format!("#[test] fn t{i}() {{}}\n"))
            .collect::<String>(),
    )
    .unwrap();
    fs::write(
        loc_root.join("src/lib.rs"),
        "pub fn base() -> i32 { 1 }\npub fn f1() {}\npub fn f2() {}\n",
    )
    .unwrap();
    fs::write(
        loc_root.join("src/é.rs"),
        "pub fn u1() {}\npub fn u2() {}\n",
    )
    .unwrap();
    let over_err = submit_req(
        &loc_eng,
        pass_req(
            loc_rid.as_str(),
            "t1.build",
            "builder-loc",
            Some(b_loc.claim_revision),
            true,
        ),
    )
    .unwrap_err();
    assert_eq!(over_err.code, ErrorCode::GateVerificationFailed);
    assert!(
        over_err.message.contains("+4") && over_err.message.contains("max_net_loc=2"),
        "unexpected error: {}",
        over_err.message
    );

    // Removing untracked `src/é.rs` lowers net prod delta to +2 -> passes immediately.
    fs::remove_file(loc_root.join("src/é.rs")).unwrap();
    submit_req(
        &loc_eng,
        pass_req(
            loc_rid.as_str(),
            "t1.build",
            "builder-loc",
            Some(b_loc.claim_revision),
            true,
        ),
    )
    .unwrap();

    // Reject review back to build, exceed again (+4 via `src/é.rs`), verify explicit decision table vs denied/insufficient strings.
    let r_loc = claim_node(
        &loc_eng,
        loc_rid.as_str(),
        "t1.review",
        "rev-loc",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    submit_req(
        &loc_eng,
        SubmitReq {
            action: SubmitAction::Reject,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::Defect,
                summary: "expand slightly".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Low,
                    path: Some("src/lib.rs".to_owned()),
                    message: "needs f3/f4".to_owned(),
                    remediation: "add u1/u2 with explicit budget override".to_owned(),
                }],
            }),
            ..pass_req(
                loc_rid.as_str(),
                "t1.review",
                "rev-loc",
                Some(r_loc.claim_revision),
                true,
            )
        },
    )
    .unwrap();
    let b_loc2 = claim_node(
        &loc_eng,
        loc_rid.as_str(),
        "t1.build",
        "builder-loc",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    fs::write(
        loc_root.join("src/é.rs"),
        "pub fn u1() {}\npub fn u2() {}\n",
    )
    .unwrap();
    for (author, decision_payload, expect_ok) in [
        ("builder-loc", "Budget exception denied.", false),
        ("builder-loc", "{\"approved_max_net_loc\": 2}", false),
        ("builder-loc", "BUDGET_OVERRIDE: self-approved", false),
        (
            "architect",
            "BUDGET_OVERRIDE: expanded error handling",
            true,
        ),
    ] {
        loc_eng
            .execute(StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some(loc_rid.to_string()),
                node: Some("t1.build".to_owned()),
                recipient_node: None,
                subtask_ref: None,
                gate: Some("build".to_owned()),
                author: author.to_owned(),
                topic: BlackboardTopic::Decisions,
                payload: decision_payload.to_owned(),
            }))
            .unwrap();
        let res = submit_req(
            &loc_eng,
            pass_req(
                loc_rid.as_str(),
                "t1.build",
                "builder-loc",
                Some(b_loc2.claim_revision),
                true,
            ),
        );
        if expect_ok {
            res.expect(decision_payload);
        } else {
            assert_eq!(
                res.unwrap_err().code,
                ErrorCode::GateVerificationFailed,
                "author={author} payload={decision_payload}"
            );
        }
    }

    // 7. Audit pipeline (`recon -> analyze -> triage`): predecessor_proofs propagation, `save.report` (.md/.json), and `--in-place`.
    let audit_tmp = make_temp_dir("audit-report-e2e");
    let audit_root = audit_tmp.path();
    let _ = init_git_repo(audit_root, "main");
    fs::create_dir_all(audit_root.join("docs/plans")).unwrap();
    let plan_commit_p = write_profile(
        audit_root,
        "audit_json.yaml",
        "version: 2\ninherit: audit\ngit: { isolation: worktree, worktrees: managed }\nsave: commit\ngates_patch:\n  - id: triage\n    save:\n      post: commit\n      report: \".acdd/reports/{spec_id}-{gate_id}.json\"\n",
    );
    fs::write(
        audit_root.join("docs/plans/PLAN-001.md"),
        format!("---\nid: PLAN-001\ntitle: In-Place Audit Plan\nstatus: planned\nprofile: {plan_commit_p}\n---\nAudit auth boundaries.\n"),
    )
    .unwrap();
    let mut audit_cfg = base_config(audit_root);
    audit_cfg.modules.disabled.insert("code".to_owned());
    audit_cfg.modules.disabled.insert("docs".to_owned());
    audit_cfg.spec_collections.insert(
        "plans".to_owned(),
        acdd_core::config::SpecCollectionConfig {
            path: Some("docs/plans".to_owned()),
            paths: Vec::new(),
            profile: None,
            prefixes: vec!["PLAN".to_owned()],
            digits: 3,
        },
    );
    let (_, audit_prov) = build_provider_with_mock(&audit_cfg.modules);
    let audit_eng = StateEngine::open(audit_cfg.clone(), audit_prov).unwrap();

    // 7a. Built-in `audit` goal run writes Markdown report `.acdd/reports/{run_name}.md` and propagates `predecessor_proofs`.
    let a_start = start_run(
        &audit_eng,
        StartReq {
            goal: Some("Audit auth entrypoints".to_owned()),
            profile: Some("audit".to_owned()),
            scope: vec!["src/auth.rs".to_owned()],
            run_name: Some(RunName::new("audit-md").unwrap()),
            ..StartReq::default()
        },
    )
    .unwrap();
    assert_eq!(a_start.run.isolation, IsolationMode::InPlace);
    let a_rid = a_start.run.run_id.as_str();
    let recon_c = claim_node(
        &audit_eng,
        a_rid,
        "recon",
        "auditor-1",
        Some("host-a:301:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert_eq!(recon_c.contract["owners"], json!(["src/auth.rs"]));
    let mut recon_sub = pass_req(
        a_rid,
        "recon",
        "auditor-1",
        Some(recon_c.claim_revision),
        false,
    );
    recon_sub.custom_evidence = Some(json!({
        "scope_summary": "auth endpoints",
        "entrypoints": ["login_handler"],
        "sensitive_sinks": ["sql_query"],
        "invariants_in_play": ["INV-AUTH"]
    }));
    submit_req(&audit_eng, recon_sub).unwrap();

    let analyze_c = claim_node(
        &audit_eng,
        a_rid,
        "analyze",
        "auditor-1",
        Some("host-a:301:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        analyze_c
            .pre_task_slice
            .as_ref()
            .and_then(|s| s["predecessor_proofs"]["recon"]["scope_summary"].as_str()),
        Some("auth endpoints")
    );
    let mut analyze_sub = pass_req(
        a_rid,
        "analyze",
        "auditor-1",
        Some(analyze_c.claim_revision),
        false,
    );
    analyze_sub.custom_evidence = Some(json!({
        "summary": "1 candidate traced",
        "candidates": [{
            "id": "F-01", "category": "injection", "severity": "high",
            "location": "src/auth.rs:42", "trace": ["login_handler -> sql_query"],
            "violated_invariant_or_cwe": "CWE-89", "remediation": "Use bound parameters"
        }]
    }));
    submit_req(&audit_eng, analyze_sub).unwrap();

    let triage_c = claim_node(
        &audit_eng,
        a_rid,
        "triage",
        "reviewer-9",
        Some("host-a:302:1"),
        false,
        false,
        None,
    )
    .unwrap();
    assert!(triage_c
        .pre_task_slice
        .as_ref()
        .and_then(|s| s["predecessor_proofs"]["recon"].as_object())
        .is_some());
    assert!(triage_c
        .pre_task_slice
        .as_ref()
        .and_then(|s| s["predecessor_proofs"]["analyze"].as_object())
        .is_some());
    let mut triage_sub = pass_req(
        a_rid,
        "triage",
        "reviewer-9",
        Some(triage_c.claim_revision),
        false,
    );
    triage_sub.summary = Some("Audit triage finished".to_owned());
    triage_sub.custom_evidence = Some(json!({
        "verdict": "findings",
        "summary": "Confirmed F-01",
        "confirmed_findings": [{
            "id": "F-01", "category": "injection", "severity": "high",
            "location": "src/auth.rs:42", "trace": ["login_handler -> sql_query"],
            "violated_invariant_or_cwe": "CWE-89", "remediation": "Use bound parameters"
        }],
        "dismissed_count": 0
    }));
    let triage_resp = submit_req(&audit_eng, triage_sub).unwrap();
    assert_eq!(
        triage_resp.report_path.as_deref(),
        Some(".acdd/reports/audit-md.md")
    );
    let md_report = fs::read_to_string(audit_root.join(".acdd/reports/audit-md.md")).unwrap();
    assert!(
        md_report.contains("# Report: audit-md (triage)")
            && md_report.contains("Audit triage finished")
            && md_report.contains("Confirmed F-01")
            && md_report.contains("### `recon`")
    );

    // 7b. `--in-place` neutralizes `save: commit` -> `none` and materializes `.json` `save.report`.
    let p_start = start_run(
        &audit_eng,
        StartReq {
            spec_path: Some("docs/plans/PLAN-001.md".to_owned()),
            run_name: Some(RunName::new("plan-json").unwrap()),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    assert_eq!(p_start.run.isolation, IsolationMode::InPlace);
    let resolved_plan: ResolvedGateProfile =
        serde_json::from_str(&p_start.run.profile_json).unwrap();
    assert_eq!(resolved_plan.git.isolation, IsolationMode::InPlace);
    assert_eq!(resolved_plan.git.worktrees, WorktreeLifecycle::Manual);
    assert_eq!(resolved_plan.save, acdd_state::types::SaveAction::None);
    assert_eq!(
        resolved_plan
            .gates
            .iter()
            .find(|g| g.id == "triage")
            .unwrap()
            .save
            .post,
        Some(acdd_state::types::SaveAction::None)
    );

    let p_rid = p_start.run.run_id.as_str();
    let pr_c = claim_node(
        &audit_eng,
        p_rid,
        "recon",
        "auditor-1",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    let mut pr_s = pass_req(
        p_rid,
        "recon",
        "auditor-1",
        Some(pr_c.claim_revision),
        false,
    );
    pr_s.custom_evidence = Some(
        json!({ "scope_summary": "plan scope", "entrypoints": [], "sensitive_sinks": [], "invariants_in_play": [] }),
    );
    submit_req(&audit_eng, pr_s).unwrap();
    let pa_c = claim_node(
        &audit_eng,
        p_rid,
        "analyze",
        "auditor-1",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    let mut pa_s = pass_req(
        p_rid,
        "analyze",
        "auditor-1",
        Some(pa_c.claim_revision),
        false,
    );
    pa_s.custom_evidence = Some(json!({ "summary": "clean", "candidates": [] }));
    submit_req(&audit_eng, pa_s).unwrap();
    let pt_c = claim_node(
        &audit_eng,
        p_rid,
        "triage",
        "reviewer-9",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    let mut pt_s = pass_req(
        p_rid,
        "triage",
        "reviewer-9",
        Some(pt_c.claim_revision),
        false,
    );
    pt_s.custom_evidence = Some(
        json!({ "verdict": "pass", "summary": "No issues", "confirmed_findings": [], "dismissed_count": 0 }),
    );
    let pt_resp = submit_req(&audit_eng, pt_s).unwrap();
    assert!(pt_resp.committed_oid.is_none());
    assert_eq!(
        pt_resp.report_path.as_deref(),
        Some(".acdd/reports/PLAN-001-triage.json")
    );
    let json_report: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(audit_root.join(".acdd/reports/PLAN-001-triage.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json_report["spec_id"], "PLAN-001");
    assert_eq!(json_report["proof"]["verdict"], "pass");

    // 8. Wave 1 hardening contracts on the shared audit/git repo:
    //    (a) Conflicting `--in-place` + `--worktree` rejected;
    //    (b) YAML `git.isolation: in_place` neutralizes `save: commit` -> `none` & `sync --rebind-profile` preserves it;
    //    (c) `clear` without `--force` refuses dirty managed worktree BEFORE pruning active run;
    //    (d) `claim` atomicity (budget failure rolls back managed task worktree), `--force` re-claim updates parent_task,
    //        and `call_plugin_tool` resolves node/task worktree;
    //    (e) `index_generation` change invalidates cached verify trace.
    assert_eq!(
        start_run(
            &audit_eng,
            StartReq {
                spec_path: Some("docs/plans/PLAN-001.md".to_owned()),
                in_place: true,
                worktree: true,
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ProfileSchemaInvalid
    );

    let yaml_ip = write_profile(
        audit_root,
        "yaml_ip.yaml",
        "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: managed }\nsave: commit\ngates:\n  - id: g1\n    proof: none\n    save: { post: commit }\n",
    );
    let yip_s = start_run(
        &audit_eng,
        StartReq {
            spec_path: Some("docs/plans/PLAN-001.md".to_owned()),
            run_name: Some(RunName::new("yaml-ip").unwrap()),
            profile: Some(yaml_ip),
            ..StartReq::default()
        },
    )
    .unwrap();
    let yip_prof: ResolvedGateProfile = serde_json::from_str(&yip_s.run.profile_json).unwrap();
    assert_eq!(
        (
            yip_prof.git.isolation,
            yip_prof.git.worktrees,
            yip_prof.save
        ),
        (
            IsolationMode::InPlace,
            WorktreeLifecycle::Manual,
            acdd_state::types::SaveAction::None
        )
    );
    assert_eq!(
        yip_prof.gates[0].save.post,
        Some(acdd_state::types::SaveAction::None)
    );
    audit_eng
        .execute(StateCommand::Sync(acdd_state::usecases::dto::SyncReq {
            run: Some(yip_s.run.run_id.to_string()),
            rebind_profile: true,
        }))
        .unwrap();
    let rebound_run = reader_for(&audit_cfg)
        .load_run_graph(&yip_s.run.run_id)
        .unwrap()
        .unwrap()
        .run;
    let rebound_prof: ResolvedGateProfile =
        serde_json::from_str(&rebound_run.profile_json).unwrap();
    assert_eq!(
        (rebound_prof.git.isolation, rebound_prof.save),
        (IsolationMode::InPlace, acdd_state::types::SaveAction::None)
    );
    assert_eq!(
        rebound_prof.gates[0].save.post,
        Some(acdd_state::types::SaveAction::None)
    );
    clear_run(&audit_eng, yip_s.run.run_id.as_str());

    // (c)-(d) Managed worktree: dirty non-force clear preserves run, rejected claim leaks no worktree,
    // `--force` re-claim updates parent_task.worktree/branch, and plugin tool receives task worktree.
    let w1_p = write_profile(
        audit_root,
        "w1_mgr.yaml",
        "version: 2\ninherit: none\ngit: { isolation: worktree, worktrees: managed }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n          checks: [{ check: http.headers }]\n          save: { post: none }\n",
    );
    fs::create_dir_all(audit_root.join("docs/milestones")).unwrap();
    fs::write(
        audit_root.join("docs/milestones/AC-W1.md"),
        "---\nid: AC-W1\ntitle: Wave1 Hardening\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\n```\n",
    )
    .unwrap();
    git_ok(audit_root, &["add", "."]);
    git_ok(audit_root, &["commit", "-m", "seed w1 hardening"]);
    let (w1_mock, w1_prov) = build_provider_with_mock(&audit_cfg.modules);
    let w1_eng = StateEngine::open(audit_cfg.clone(), w1_prov.clone()).unwrap();
    let w1_s = start_run(
        &w1_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-W1.md".to_owned()),
            run_name: Some(RunName::new("w1-mgr").unwrap()),
            profile: Some(w1_p),
            ..StartReq::default()
        },
    )
    .unwrap();
    let w1_rid = w1_s.run.run_id.as_str();
    let mut w1_tiny = audit_cfg.clone();
    w1_tiny.response_policy.max_output_bytes = 32;
    let w1_tiny_eng = StateEngine::open(w1_tiny, w1_prov.clone()).unwrap();
    assert_eq!(
        claim_node(
            &w1_tiny_eng,
            w1_rid,
            "t1.build",
            "w-tiny",
            None,
            false,
            false,
            None
        )
        .unwrap_err()
        .code,
        ErrorCode::GuidanceBudgetExceeded
    );
    assert!(
        !audit_root.join(".worktrees/w1-mgr-t1").exists(),
        "failed claim preflight/assemble must roll back managed task worktree"
    );

    let w1_c = claim_node(&w1_eng, w1_rid, "t1.build", "w-1", None, false, false, None).unwrap();
    let task_wt_path = PathBuf::from(&w1_c.worktree);
    assert!(task_wt_path.is_dir());

    // Plugin tool with `node: Some("t1.build")` resolves task worktree rather than run.worktree.
    match w1_eng
        .execute(StateCommand::CallPluginTool(
            acdd_state::usecases::dto::CallPluginToolReq {
                tool_name: "http_probe".to_owned(),
                args: json!({ "url": "https://example.test" }),
                run: Some(w1_rid.to_owned()),
                node: Some("t1.build".to_owned()),
                worktree: None,
            },
        ))
        .unwrap()
    {
        StateCommandOutput::CallPluginTool(resp) => {
            let acdd_core::ports::DomainToolOutput::Json(val) = resp.output else {
                panic!("expected json output");
            };
            assert_eq!(val["worktree"].as_str(), Some(w1_c.worktree.as_str()));
        }
        other => panic!("unexpected CallPluginTool output: {other:?}"),
    }

    // (e) `index_generation` change between `verify` and `submit` invalidates cached trace.
    w1_mock.set_index_gen(1);
    let v_gen1 = verify_node(
        &w1_eng,
        w1_rid,
        "t1.build",
        "w-1",
        None,
        Some(w1_c.claim_revision),
    )
    .unwrap();
    w1_mock.set_index_gen(2);
    let stale = pass_req(w1_rid, "t1.build", "w-1", Some(w1_c.claim_revision), false);
    assert_eq!(
        submit_req(&w1_eng, stale).unwrap_err().code,
        ErrorCode::TaskProofUnobserved
    );
    let v_gen2 = verify_node(
        &w1_eng,
        w1_rid,
        "t1.build",
        "w-1",
        None,
        Some(w1_c.claim_revision),
    )
    .unwrap();
    assert_ne!(
        v_gen1.execution_digest, v_gen2.execution_digest,
        "changing FreshnessObservation::Passed.index_generation must change execution_digest"
    );

    // Non-force `clear` on dirty worktree fails BEFORE pruning active run or emitting a tombstone!
    fs::write(task_wt_path.join("dirty.txt"), "uncommitted\n").unwrap();
    assert_eq!(
        w1_eng
            .execute(StateCommand::Clear(acdd_state::usecases::dto::ClearReq {
                run: Some(w1_rid.to_owned()),
                reason: Some("try non-force".to_owned()),
                force: false,
            }))
            .unwrap_err()
            .code,
        ErrorCode::ExternalGitDrift
    );
    assert!(
        reader_for(&audit_cfg)
            .load_run_graph(&w1_s.run.run_id)
            .unwrap()
            .is_some(),
        "rejected non-force clear must NOT prune active run"
    );
    assert!(
        reader_for(&audit_cfg)
            .load_tombstone(&w1_s.run.run_id)
            .unwrap()
            .is_none(),
        "rejected non-force clear must NOT emit a pending tombstone"
    );
    assert!(task_wt_path.join("dirty.txt").exists());

    // `--force` re-claim with alternate worktree updates `parent_task.worktree` and `parent_task.branch`.
    let alt_wt = audit_root.join("alt-task-wt");
    git_ok(
        audit_root,
        &[
            "worktree",
            "add",
            "-b",
            "feat/alt-t1",
            alt_wt.to_str().unwrap(),
        ],
    );
    let _ = claim_node(
        &w1_eng,
        w1_rid,
        "t1.build",
        "w-1",
        None,
        true,
        false,
        Some(alt_wt.clone()),
    )
    .unwrap();
    let snap_after_force = reader_for(&audit_cfg)
        .load_run_graph(&w1_s.run.run_id)
        .unwrap()
        .unwrap();
    let parent_t1 = snap_after_force
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1")
        .unwrap();
    assert_eq!(parent_t1.branch.as_deref(), Some("feat/alt-t1"));
    assert_eq!(
        parent_t1
            .worktree
            .as_deref()
            .map(|p| acdd_core::fs::canonicalize_macos_var(std::path::Path::new(p))),
        Some(acdd_core::fs::canonicalize_macos_var(&alt_wt))
    );

    // Explicit `--force` clear cleans managed worktrees and prunes cleanly.
    clear_run(&w1_eng, w1_rid);
    git_ok(
        audit_root,
        &["worktree", "remove", "--force", alt_wt.to_str().unwrap()],
    );
    git_ok(audit_root, &["branch", "-D", "feat/alt-t1"]);
    let _ = acdd_state::git_ops::worktree::remove_worktree_and_branch(
        audit_root,
        Some(&task_wt_path),
        Some("task/w1-mgr-t1"),
        true,
    );

    // Multi-check verify -> submit preserves post-mutation `index_generation` from later checks.
    let mc_p = write_profile(
        audit_root,
        "mc_gen.yaml",
        "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n          checks: [{ check: http.headers }, { check: http.bump_gen }]\n          save: { post: none }\n",
    );
    let mc_s = start_run(
        &w1_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-W1.md".to_owned()),
            profile: Some(mc_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let mc_rid = mc_s.run.run_id.as_str();
    let mc_c = claim_node(
        &w1_eng, mc_rid, "t1.build", "w-mc", None, false, false, None,
    )
    .unwrap();
    w1_mock.set_index_gen(1);
    assert_eq!(w1_mock.index_gen(), 1);
    let mc_v = verify_node(
        &w1_eng,
        mc_rid,
        "t1.build",
        "w-mc",
        None,
        Some(mc_c.claim_revision),
    )
    .unwrap();
    assert!(mc_v.passed && mc_v.check_results.len() == 2);
    assert_eq!(w1_mock.index_gen(), 2);
    // Post-verify mutation must make submit recompute a newer generation and fail closed.
    w1_mock.set_index_gen(3);
    assert_eq!(
        submit_req(
            &w1_eng,
            pass_req(mc_rid, "t1.build", "w-mc", Some(mc_c.claim_revision), false)
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskProofUnobserved
    );
    w1_mock.set_index_gen(2);
    let mc_sub = submit_req(
        &w1_eng,
        pass_req(mc_rid, "t1.build", "w-mc", Some(mc_c.claim_revision), false),
    )
    .unwrap();
    assert_eq!(mc_sub.new_node_status, NodeStatus::Completed);
}
