use super::support::{base_engine_config, git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{GateId, NodeCursor, RunId, TaskRef, WorkerId};
use acdd_state::workflow::{RoleDef, RoleExecutionMode};
use acdd_state::runner::{spawn_role_runner_borrowed, verify_runner_submitted, RoleSpawnBorrow};
use acdd_state::types::ClaimOutcome;
use acdd_state::usecases::dto::{RunReq, StartReq};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use std::collections::BTreeMap;
use std::fs;

fn init_workflow_ws(profile_rel: &str, profile_body: &str) -> Workspace {
    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "dev");
    ws.write(
        "docs/milestones/minimal.md",
        include_str!("fixtures/milestones/minimal.md"),
    );
    ws.write(profile_rel, profile_body);
    git_cmd(ws.root(), &["add", "."]);
    git_cmd(ws.root(), &["commit", "-m", "seed workflow"]);
    ws
}

#[test]
fn workflow_runner_execution_retries_failures_timeouts_and_roles_matrix() {
    let providers = TestProvider::full_composite();

    // 1. Ordered workflow fixture executes gates in `pass:` order.
    {
        let ws = init_workflow_ws(
            ".acdd/workflows/ordered.yaml",
            include_str!("fixtures/workflows/ordered.yaml"),
        );
        let cfg = base_engine_config(ws.root());
        let engine = StateEngine::open(cfg, providers.clone()).unwrap();
        let StateCommandOutput::Start(s) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/minimal.md".to_owned()),
                workflow: Some(".acdd/workflows/ordered.yaml".to_owned()),
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let StateCommandOutput::Run(r) = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(s.run.run_id.to_string()),
                allow_main: true,
                ..RunReq::default()
            }))
            .unwrap()
        else {
            panic!("expected RunResp");
        };
        assert_eq!(r.report.gates_completed, 2);
        assert_eq!(
            fs::read_to_string(ws.path(".acdd/workflow-order.log")).unwrap(),
            "first\nsecond\n"
        );
    }

    // 2. Retry workflow fixture recovers on second attempt (`attempt` -> `fail: prepare` -> `attempt` passes).
    {
        let ws = init_workflow_ws(
            ".acdd/workflows/retry.yaml",
            include_str!("fixtures/workflows/retry.yaml"),
        );
        let cfg = base_engine_config(ws.root());
        let engine = StateEngine::open(cfg, providers.clone()).unwrap();
        let StateCommandOutput::Start(s) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/minimal.md".to_owned()),
                workflow: Some(".acdd/workflows/retry.yaml".to_owned()),
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        // Single `acdd run` recovers automatically: `prepare` -> `attempt` fails (creating
        // `retry-marker`, routing via `fail: prepare`) -> `prepare` -> `attempt` succeeds!
        let StateCommandOutput::Run(r) = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(s.run.run_id.to_string()),
                allow_main: true,
                ..RunReq::default()
            }))
            .unwrap()
        else {
            panic!("expected RunResp");
        };
        assert_eq!(r.report.gates_completed, 3);
        assert_eq!(
            fs::read_to_string(ws.path(".acdd/retry-result")).unwrap(),
            "retried\n"
        );
    }

    // 3. Failing workflow halts before running subsequent `forbidden` gate.
    {
        let ws = init_workflow_ws(
            ".acdd/workflows/failing.yaml",
            include_str!("fixtures/workflows/failing.yaml"),
        );
        let cfg = base_engine_config(ws.root());
        let engine = StateEngine::open(cfg, providers.clone()).unwrap();
        let StateCommandOutput::Start(s) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/minimal.md".to_owned()),
                workflow: Some(".acdd/workflows/failing.yaml".to_owned()),
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let err = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(s.run.run_id.to_string()),
                allow_main: true,
                ..RunReq::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateVerificationFailed);
        assert!(!ws.path("must-not-run").exists());
    }

    // 4. Timeout workflow (`timeout_seconds: 1`) kills process group cleanly.
    {
        let ws = init_workflow_ws(
            ".acdd/workflows/timeout.yaml",
            include_str!("fixtures/workflows/timeout.yaml"),
        );
        let cfg = base_engine_config(ws.root());
        let engine = StateEngine::open(cfg, providers.clone()).unwrap();
        let StateCommandOutput::Start(s) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/minimal.md".to_owned()),
                workflow: Some(".acdd/workflows/timeout.yaml".to_owned()),
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let err = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(s.run.run_id.to_string()),
                allow_main: true,
                ..RunReq::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateVerificationFailed);
    }

    // 5. Role without runner (`rejection_routing.yaml`) fails closed with WorkflowRunnerRequired.
    {
        let ws = init_workflow_ws(
            ".acdd/workflows/rej.yaml",
            include_str!("fixtures/workflows/rejection_routing.yaml"),
        );
        let cfg = base_engine_config(ws.root());
        let engine = StateEngine::open(cfg, providers).unwrap();
        let StateCommandOutput::Start(s) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/minimal.md".to_owned()),
                workflow: Some(".acdd/workflows/rej.yaml".to_owned()),
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let err = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(s.run.run_id.to_string()),
                allow_main: true,
                ..RunReq::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkflowRunnerRequired);
    }

    // 6. Role spawner delivers stdin prompt + env + session reuse + RUNNER_NO_SUBMISSION guard.
    {
        let ws = Workspace::new();
        let mut cache = BTreeMap::new();
        let run_id = RunId::new("run-role-1").unwrap();
        let cursor = NodeCursor::task(
            TaskRef::new("t1").unwrap(),
            vec![GateId::new("build").unwrap()],
        );
        let role_def = RoleDef {
            mode: RoleExecutionMode::Subagent,
            reuse_on_reject: true,
            runner: Some(
                "cat > prompt.md && printf 'SID:%s\n' \"$ACDD_AGENT_SESSION_ID\"".to_owned(),
            ),
            ..RoleDef::default()
        };
        let worker_id = WorkerId::new("w-role").unwrap();
        let state_db = ws.path(".acdd/state.sqlite");
        let req = RoleSpawnBorrow {
            run_id: &run_id,
            node_cursor: &cursor,
            node_container_id: "task:t1",
            claim_revision: 1,
            role_name: "builder",
            role_def: &role_def,
            worker_id: &worker_id,
            worktree: ws.root(),
            primary_worktree: ws.root(),
            state_db_path: &state_db,
            code_db_path: None,
            trace_parent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
            prompt_markdown: "# Prompt Body\n",
            timeout_seconds: 5,
            max_log_bytes: 65536,
            max_line_bytes: 65536,
        };
        let out = spawn_role_runner_borrowed(&req, &mut cache).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(
            fs::read_to_string(ws.path("prompt.md")).unwrap(),
            "# Prompt Body\n"
        );
        let borrowed_out = spawn_role_runner_borrowed(&req, &mut cache).unwrap();
        assert_eq!(borrowed_out.exit_code, Some(0));
        assert_eq!(borrowed_out.agent_session_id, out.agent_session_id);
        let no_sub = verify_runner_submitted(&out, Some(ClaimOutcome::Active)).unwrap_err();
        assert_eq!(no_sub.code, ErrorCode::RunnerNoSubmission);
    }
}
