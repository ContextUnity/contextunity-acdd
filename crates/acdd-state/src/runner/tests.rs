use super::command::execute_gate_commands_inner;
use super::spawner::spawn_role_runner_borrowed_inner;
use super::*;
use crate::profile::{CommandDef, ResourceLimits, RoleDef, RoleExecutionMode};
use crate::types::ClaimOutcome;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{GateId, NodeCursor, RunId, TaskRef, WorkerId};
use std::collections::BTreeMap;
use std::time::Duration;
use tempfile::TempDir;

fn test_run_id() -> RunId {
    RunId::new("run-runner-1").unwrap()
}

fn test_cursor() -> NodeCursor {
    NodeCursor::task(
        TaskRef::new("t1").unwrap(),
        vec![GateId::new("build").unwrap()],
    )
}

#[test]
fn command_and_spawner_timeout_kills_process_group_without_sleep() {
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();

    let cmd_req = CommandExecReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        claim_revision: 1,
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        command_id: "hang".to_owned(),
        command: CommandDef::Script("while :; do :; done".to_owned()),
        scope: vec!["src/**".to_owned()],
        profile_scope_args: BTreeMap::new(),
        test_target: None,
        limits: ResourceLimits::default(),
    };
    let out = execute_gate_commands_inner(&cmd_req, Some(Duration::from_millis(80))).unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);

    let mut cache = BTreeMap::new();
    let spawn_run_id = test_run_id();
    let spawn_cursor = test_cursor();
    let spawn_role_def = RoleDef {
        mode: RoleExecutionMode::Subagent,
        reuse_on_reject: true,
        runner: Some("while :; do :; done".to_owned()),
        ..RoleDef::default()
    };
    let spawn_worker = WorkerId::new("w1").unwrap();
    let spawn_state_db = dir.path().join("state.sqlite");
    let spawn_req = RoleSpawnBorrow {
        run_id: &spawn_run_id,
        node_cursor: &spawn_cursor,
        node_container_id: "task:t1",
        claim_revision: 1,
        role_name: "builder",
        role_def: &spawn_role_def,
        worker_id: &spawn_worker,
        worktree: &worktree,
        primary_worktree: &worktree,
        state_db_path: &spawn_state_db,
        code_db_path: None,
        trace_parent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        prompt_markdown: "# Prompt\n",
        timeout_seconds: 1,
        max_log_bytes: 65536,
        max_line_bytes: 65536,
    };
    let spawn_out =
        spawn_role_runner_borrowed_inner(&spawn_req, &mut cache, Some(Duration::from_millis(80)))
            .unwrap();
    assert!(spawn_out.timed_out);
    assert_eq!(spawn_out.exit_code, None);
    assert_eq!(
        verify_runner_submitted(&spawn_out, Some(ClaimOutcome::Passed))
            .unwrap_err()
            .code,
        ErrorCode::RunnerNoSubmission
    );

    // Exited group leader whose background descendant keeps stdout/stderr open must still trigger
    // process-group SIGKILL on timeout across Linux & macOS (waitid WNOWAIT, zero early try_wait reap).
    let descendant_req = CommandExecReq {
        command_id: "orphan-descendant".to_owned(),
        command: CommandDef::Script("(sleep 30) & exit 0".to_owned()),
        ..cmd_req
    };
    let start_ts = std::time::Instant::now();
    let desc_out =
        execute_gate_commands_inner(&descendant_req, Some(Duration::from_millis(80))).unwrap();
    assert!(desc_out.timed_out);
    assert_eq!(desc_out.exit_code, None);
    assert!(
        start_ts.elapsed() < Duration::from_secs(2),
        "timed-out process-group cleanup hung on lingering descendant"
    );
}
