use super::support::{base_engine_config, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_state::harness::RunHarness;
use acdd_state::types::{BlackboardTopic, SubmitAction};
use acdd_state::usecases::dto::{
    BlackboardPostReq, BlackboardReadReq, ClaimReq, StartReq, SubmitReq,
};
use acdd_state::usecases::{
    queries, StateCommand, StateCommandOutput, StateQuery, StateQueryOutput,
};

#[test]
fn blackboard_topics_routing_pagination_deferred_rollup_and_module_gating() {
    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "dev");
    ws.write(
        ".acdd/profiles/bb.yaml",
        "version: 2\ninherit: none\nsave: none\ngates:\n  - tasks:\n      gates:\n        - id: work\n          pass: finish\n        - id: finish\n          need: [work]\n          receipt: true\n",
    );
    ws.write(
        "docs/milestones/001-linear.md",
        include_str!("fixtures/milestones/linear.md"),
    );
    super::support::git_cmd(ws.root(), &["add", "."]);
    super::support::git_cmd(ws.root(), &["commit", "-m", "seed bb spec"]);

    let cfg = base_engine_config(ws.root());
    let providers = TestProvider::full_composite();
    let harness = RunHarness::in_memory().unwrap();

    let StateCommandOutput::Start(started) = harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/001-linear.md".to_owned()),
                profile: Some(".acdd/profiles/bb.yaml".to_owned()),
                ..StartReq::default()
            }),
        )
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let rid = started.run.run_id.to_string();

    // Post across all 6 topics, including targeted recipient routing and deferred finding.
    for (idx, (topic, recipient)) in [
        (BlackboardTopic::Draft, None),
        (BlackboardTopic::Notes, Some("first")),
        (BlackboardTopic::Findings, Some("first.work")),
        (BlackboardTopic::Blockers, Some("second")),
        (BlackboardTopic::Decisions, None),
        (BlackboardTopic::Deferred, Some("first")),
    ]
    .into_iter()
    .enumerate()
    {
        harness
            .execute(
                &cfg,
                &providers,
                StateCommand::BlackboardPost(BlackboardPostReq {
                    run: Some(rid.clone()),
                    node: Some("first.work".to_owned()),
                    recipient_node: recipient.map(str::to_owned),
                    subtask_ref: None,
                    gate: Some("work".to_owned()),
                    author: format!("agent-{idx}"),
                    topic,
                    payload: format!("payload-{idx}"),
                }),
            )
            .unwrap();
    }

    // Filter by topic + pagination (`limit`, `offset`, `generation`).
    let StateQueryOutput::BlackboardRead(page1) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some(rid.clone()),
            limit: Some(2),
            offset: 0,
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap() else {
        panic!("expected BlackboardReadResp");
    };
    assert_eq!(page1.total, 6);
    assert_eq!(page1.items.len(), 2);

    let StateQueryOutput::BlackboardRead(targeted) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some(rid.clone()),
            recipient_node: Some("second".to_owned()),
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap() else {
        panic!("expected BlackboardReadResp");
    };
    assert_eq!(targeted.items.len(), 1);
    assert_eq!(targeted.items[0].topic, BlackboardTopic::Blockers);

    let gen_err = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some(rid.clone()),
            generation: Some(page1.generation + 50),
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap_err();
    assert_eq!(gen_err.code, ErrorCode::BackendConflict);

    // Completing task `first` closes temporary `notes`/`draft` on task and promotes `deferred` to run level.
    let worker = acdd_core::ids::WorkerId::new("w1").unwrap();
    for gate in ["first.work", "first.finish"] {
        harness
            .execute(
                &cfg,
                &providers,
                StateCommand::Claim(ClaimReq {
                    run: Some(rid.clone()),
                    node: Some(gate.to_owned()),
                    worker_id: worker.clone(),
                    session_fingerprint: None,
                    force: false,
                    steal_remote: false,
                    worktree: None,
                    detail: Default::default(),
                }),
            )
            .unwrap();
        harness
            .execute(
                &cfg,
                &providers,
                StateCommand::Submit(Box::new(SubmitReq {
                    run: Some(rid.clone()),
                    node: gate.to_owned(),
                    action: SubmitAction::Pass,
                    worker_id: worker.clone(),
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

    let StateQueryOutput::BlackboardRead(deferred_after) = queries::execute_query(
        &cfg,
        &harness,
        &providers,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some(rid.clone()),
            topic: Some(BlackboardTopic::Deferred),
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap() else {
        panic!("expected BlackboardReadResp");
    };
    assert!(
        !deferred_after.items.is_empty(),
        "deferred items must remain queryable after task completion"
    );
}
