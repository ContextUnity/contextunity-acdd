use super::command::execute_gate_commands_inner;
use super::spawner::spawn_role_runner_inner;
use super::*;
use crate::harness::RunHarness;
use crate::machine::{MachineGitPolicy, MachineGraphItem, MachineProfileInput, MachineTasksBlockDef};
use crate::profile::{
    CommandCountsSpec, CommandDef, CommandOutputSpec, CompileErrorsFilter, CompileErrorsSpec,
    DetailedCommandDef, OutputTestsSpec, PatternCommandEntry, ResourceLimits, RoleDef,
    RoleExecutionMode, TestFilterConfig,
};
use crate::types::{ClaimOutcome, IsolationMode, NodeStatus};
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{GateId, NodeCursor, RunId, TaskRef, WorkerId};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::Command;
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
fn command_escaping_output_parsing_and_line_bounds_matrix() {
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();

    // Case 1: POSIX single-quote escaping for `{test}` and `{test_filter}` executes safely in `sh`.
    let escape_cmd = CommandDef::Detailed(Box::new(DetailedCommandDef {
        command: "printf '%s\n' {test_filter}".to_owned(),
        description: None,
        timeout_seconds: None,
        test_filter: Some(TestFilterConfig::Template("--exact {test}".to_owned())),
        scope_args_by_pattern: BTreeMap::new(),
        output: None,
    }));
    for target in [
        "suite::it_works",
        "it's_a_test$(echo pwned)",
        "a'b; echo injected",
    ] {
        let req = CommandExecReq {
            run_id: test_run_id(),
            node_cursor: test_cursor(),
            claim_revision: 1,
            worktree: worktree.clone(),
            primary_worktree: worktree.clone(),
            command_id: "test".to_owned(),
            command: escape_cmd.clone(),
            scope: vec!["src/**".to_owned()],
            test_target: Some(target.to_owned()),
            limits: ResourceLimits::default(),
            trace_parent: None,
        };
        let out = execute_gate_commands(&req).expect("escaped command runs");
        assert_eq!(out.exit_code, Some(0));
        let log_text = fs::read_to_string(
            worktree
                .join(".acdd/run")
                .join(req.run_id.as_str())
                .join(&out.log_relpath),
        )
        .unwrap();
        assert!(
            log_text.contains(target),
            "expected literal target {target:?} in log, got:\n{log_text}"
        );
        assert!(
            !log_text.contains("injected") || target.contains("injected"),
            "unexpected command injection"
        );
    }

    // Case 2: Chained command without `{test_filter}` fails closed with CommandTestFilterPlaceholderRequired.
    let chained_req = CommandExecReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        claim_revision: 2,
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        command_id: "chained".to_owned(),
        command: CommandDef::Script("echo a && echo b".to_owned()),
        scope: vec!["src/**".to_owned()],
        test_target: Some("t::x".to_owned()),
        limits: ResourceLimits::default(),
        trace_parent: None,
    };
    let err = execute_gate_commands(&chained_req).expect_err("must reject unplaceholdered chain");
    assert_eq!(err.code, ErrorCode::CommandTestFilterPlaceholderRequired);

    // Case 3: max_line_bytes, max_log_bytes, noise stripping, compile_errors include/exclude, counts & tests extraction.
    let output_spec = CommandOutputSpec {
        counts: Some(CommandCountsSpec {
            passed: r"(\d+) passed".to_owned(),
            failed: r"(\d+) failed".to_owned(),
        }),
        require_counts: true,
        noise: vec![r"^Compiling ".to_owned(), r"^NOISE:".to_owned()],
        compile_errors: CompileErrorsSpec::IncludeExclude(CompileErrorsFilter {
            include: vec![r"^error(\[E\d+\])?:".to_owned(), r"could not compile".to_owned()],
            exclude: vec![r"^error: test failed".to_owned()],
        }),
        tests: Some(OutputTestsSpec {
            target: Some(r"^Running (\S+)".to_owned()),
            passed: r"^test (\S+) \.\.\. ok$".to_owned(),
            failed: Some(r"^test (\S+) \.\.\. FAILED$".to_owned()),
            ignored: Some(r"^test (\S+) \.\.\. ignored$".to_owned()),
        }),
    };

    let script = concat!(
        "echo 'Compiling acdd-state v0.6.0' >&2; ",
        "echo 'NOISE: progress' >&2; ",
        "echo 'error: test failed, to rerun pass' >&2; ",
        "echo 'error[E0425]: cannot find value' >&2; ",
        "printf '%s\\n' 'Running tests/core.rs'; ",
        "printf '%s\\n' 'test alpha ... ok'; ",
        "printf '%s\\n' 'test beta ... FAILED'; ",
        "printf '%s\\n' 'test gamma ... ignored'; ",
        "printf '%s\\n' 'test result: FAILED. 1 passed; 1 failed; 1 ignored'; ",
        "printf '%0200d\\n' 7 >&2"
    );

    let parse_req = CommandExecReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        claim_revision: 3,
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        command_id: "build".to_owned(),
        command: CommandDef::Detailed(Box::new(DetailedCommandDef {
            command: script.to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::new(),
            output: Some(output_spec),
        })),
        scope: vec!["src/**".to_owned()],
        test_target: None,
        limits: ResourceLimits {
            max_line_bytes: 64,
            max_log_bytes: 512,
            max_capture_bytes: 4096,
            ..ResourceLimits::default()
        },
        trace_parent: None,
    };

    let outcome = execute_gate_commands(&parse_req).expect("output parsing runs");
    assert_eq!(outcome.tests_passed, 1);
    assert_eq!(outcome.tests_failed, 1);
    assert_eq!(outcome.tests_ignored, 1);
    assert_eq!(outcome.passed_tests, vec!["tests/core.rs::alpha"]);
    assert_eq!(outcome.failed_tests, vec!["tests/core.rs::beta"]);
    assert_eq!(outcome.ignored_tests, vec!["tests/core.rs::gamma"]);
    // `error: test failed` must be excluded, leaving only `error[E0425]`.
    assert_eq!(outcome.compile_error_count, 1);
    assert!(!outcome.stderr_tail.contains("Compiling acdd-state"));
    assert!(!outcome.stderr_tail.contains("NOISE: progress"));
    assert!(outcome.stderr_tail.contains("error[E0425]"));
    for line in outcome.stderr_tail.lines() {
        assert!(
            line.len() <= 64,
            "line exceeded max_line_bytes=64: len={}",
            line.len()
        );
    }
    let full_log_bytes = fs::read(
        worktree
            .join(".acdd/run")
            .join(parse_req.run_id.as_str())
            .join(&outcome.log_relpath),
    )
    .unwrap();
    assert!(full_log_bytes.len() <= 512);

    // Case 4: Pattern specificity + scope_args_by_pattern.
    let pat_cmd = CommandDef::Pattern(vec![
        PatternCommandEntry {
            pattern: "**".to_owned(),
            command: "echo fallback {scope_target}".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::new(),
            output: None,
        },
        PatternCommandEntry {
            pattern: "crates/acdd-state/**".to_owned(),
            command: "echo specific {scope_target}".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::from([(
                "crates/acdd-state/**".to_owned(),
                "-p acdd-state".to_owned(),
            )]),
            output: None,
        },
    ]);
    let pat_req = CommandExecReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        claim_revision: 4,
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        command_id: "pat".to_owned(),
        command: pat_cmd,
        scope: vec!["crates/acdd-state/src/runner/**".to_owned()],
        test_target: None,
        limits: ResourceLimits::default(),
        trace_parent: None,
    };
    let pat_out = execute_gate_commands(&pat_req).unwrap();
    assert_eq!(pat_out.command_line, "echo specific -p acdd-state");
}

#[test]
fn sanitize_command_process_scrubs_orchestrator_and_all_git_vars() {
    let dir = TempDir::new().unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(
        "for k in TRACEPARENT ACDD_CONFIG ACDD_PROFILE ACDD_STATE_DB ACDD_CODE_DB ACDD_WORKSPACE_ROOT ACDD_AGENT_SESSION_ID GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_AUTHOR_NAME; do eval v=\\$$k; if [ -n \"$v\" ]; then echo \"LEAK:$k=$v\"; fi; done; echo \"KEEP:$ACDD_KEEP_OK\""
    );
    cmd.current_dir(dir.path());
    cmd.env("TRACEPARENT", "00-1111-2222-01")
        .env("ACDD_CONFIG", "/x/acdd.yaml")
        .env("ACDD_PROFILE", "code")
        .env("ACDD_STATE_DB", "/x/state.sqlite")
        .env("ACDD_CODE_DB", "/x/code-map.sqlite")
        .env("ACDD_WORKSPACE_ROOT", "/x")
        .env("ACDD_AGENT_SESSION_ID", "sess-1")
        .env("GIT_DIR", "/x/.git")
        .env("GIT_WORK_TREE", "/x")
        .env("GIT_INDEX_FILE", "/x/.git/index")
        .env("GIT_OBJECT_DIRECTORY", "/x/.git/objects")
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "/y/.git/objects")
        .env("GIT_COMMON_DIR", "/x/.git")
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("ACDD_KEEP_OK", "yes");

    sanitize_command_process(&mut cmd);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("LEAK:"), "unexpected env leak: {stdout}");
    assert!(stdout.contains("KEEP:yes"));
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
        test_target: None,
        limits: ResourceLimits::default(),
        trace_parent: None,
    };
    let out = execute_gate_commands_inner(&cmd_req, Some(Duration::from_millis(80))).unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);

    let mut cache = BTreeMap::new();
    let spawn_req = RoleSpawnReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        node_container_id: "task:t1".to_owned(),
        claim_revision: 1,
        role_name: "builder".to_owned(),
        role_def: RoleDef {
            mode: RoleExecutionMode::Subagent,
            reuse_on_reject: true,
            runner: Some("while :; do :; done".to_owned()),
            ..RoleDef::default()
        },
        worker_id: WorkerId::new("w1").unwrap(),
        worktree: worktree.clone(),
        primary_worktree: worktree,
        state_db_path: dir.path().join("state.sqlite"),
        code_db_path: None,
        trace_parent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned(),
        prompt_markdown: "# Prompt\n".to_owned(),
        timeout_seconds: 1,
        max_log_bytes: 65536,
        max_line_bytes: 65536,
    };
    let spawn_out = spawn_role_runner_inner(&spawn_req, &mut cache, Some(Duration::from_millis(80)))
        .unwrap();
    assert!(spawn_out.timed_out);
    assert_eq!(spawn_out.exit_code, None);
    assert_eq!(
        verify_runner_submitted(&spawn_out, Some(ClaimOutcome::Passed)).unwrap_err().code,
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

#[test]
fn spawner_stdin_env_session_reuse_and_submission_verification_matrix() {
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();
    let mut cache = BTreeMap::new();

    // Case 1: Missing RoleDef.runner -> WorkflowRunnerRequired.
    let missing_runner = RoleSpawnReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        node_container_id: "task:t1".to_owned(),
        claim_revision: 1,
        role_name: "builder".to_owned(),
        role_def: RoleDef::default(),
        worker_id: WorkerId::new("w1").unwrap(),
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        state_db_path: worktree.join("state.sqlite"),
        code_db_path: None,
        trace_parent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned(),
        prompt_markdown: "hi".to_owned(),
        timeout_seconds: 5,
        max_log_bytes: 1_048_576,
        max_line_bytes: 65_536,
    };
    let err = spawn_role_runner(&missing_runner, &mut cache).expect_err("must require runner");
    assert_eq!(err.code, ErrorCode::WorkflowRunnerRequired);

    // Case 2: Large stdin prompt (> 130KB) + full ACDD_* env injection + reuse_on_reject matrix.
    let big_prompt = format!("# Gate Claim\n\n{}", "payload-line\n".repeat(12_000));
    assert!(big_prompt.len() > 130_000);

    let builder_cmd = "wc -c; printf 'ENV:%s|%s|%s|%s|%s|%s|%s\\n' \"$ACDD_RUN_ID\" \"$ACDD_NODE_CURSOR\" \"$ACDD_CLAIM_REVISION\" \"$ACDD_WORKER_ID\" \"$ACDD_AGENT_SESSION_ID\" \"$ACDD_CODE_DB\" \"$TRACEPARENT\"";
    let mut builder_req = RoleSpawnReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        node_container_id: "task:t1".to_owned(),
        claim_revision: 1,
        role_name: "builder".to_owned(),
        role_def: RoleDef {
            mode: RoleExecutionMode::Subagent,
            reuse_on_reject: true,
            runner: Some(builder_cmd.to_owned()),
            ..RoleDef::default()
        },
        worker_id: WorkerId::new("w-build").unwrap(),
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        state_db_path: worktree.join("state.sqlite"),
        code_db_path: Some(worktree.join("code-map.sqlite")),
        trace_parent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned(),
        prompt_markdown: big_prompt.clone(),
        timeout_seconds: 5,
        max_log_bytes: 1_048_576,
        max_line_bytes: 65_536,
    };
    let b1 = spawn_role_runner(&builder_req, &mut cache).unwrap();
    assert_eq!(b1.exit_code, Some(0));
    let log1 = fs::read_to_string(
        worktree
            .join(".acdd/run")
            .join(builder_req.run_id.as_str())
            .join(&b1.log_relpath),
    )
    .unwrap();
    assert!(log1.contains(&big_prompt.len().to_string()));
    assert!(log1.contains("ENV:run-runner-1|t1.build|1|w-build|"));
    assert!(log1.contains(&b1.agent_session_id));

    // Re-spawning builder in the same (run_id, node_container_id, role) with reuse_on_reject=true reuses session ID.
    builder_req.claim_revision = 2;
    let b2 = spawn_role_runner(&builder_req, &mut cache).unwrap();
    assert_eq!(b2.agent_session_id, b1.agent_session_id);

    // Independent reviewer (`reuse_on_reject: false`) always gets a fresh session ID.
    let mut reviewer_req = builder_req.clone();
    reviewer_req.role_name = "independent_reviewer".to_owned();
    reviewer_req.role_def.reuse_on_reject = false;
    let r1 = spawn_role_runner(&reviewer_req, &mut cache).unwrap();
    let r2 = spawn_role_runner(&reviewer_req, &mut cache).unwrap();
    assert_ne!(r1.agent_session_id, b1.agent_session_id);
    assert_ne!(r1.agent_session_id, r2.agent_session_id);

    // Case 3: verify_runner_submitted fail-closed matrix.
    let ok_outcome = RoleSpawnOutcome {
        exit_code: Some(0),
        timed_out: false,
        agent_session_id: "s1".to_owned(),
        log_relpath: "logs/x.log".to_owned(),
    };
    let nonzero_outcome = RoleSpawnOutcome {
        exit_code: Some(1),
        ..ok_outcome.clone()
    };
    let cases: &[(&RoleSpawnOutcome, Option<ClaimOutcome>, Result<(), ErrorCode>)] = &[
        (&ok_outcome, Some(ClaimOutcome::Passed), Ok(())),
        (&ok_outcome, Some(ClaimOutcome::Rejected), Ok(())),
        (
            &ok_outcome,
            Some(ClaimOutcome::Active),
            Err(ErrorCode::RunnerNoSubmission),
        ),
        (
            &ok_outcome,
            None,
            Err(ErrorCode::RunnerNoSubmission),
        ),
        (
            &nonzero_outcome,
            Some(ClaimOutcome::Passed),
            Err(ErrorCode::RunnerNoSubmission),
        ),
    ];
    for &(out, claim_after, expected) in cases {
        let res = verify_runner_submitted(out, claim_after);
        match expected {
            Ok(()) => assert!(res.is_ok(), "expected Ok for {claim_after:?}"),
            Err(code) => assert_eq!(res.unwrap_err().code, code),
        }
    }
}

#[test]
fn wave_scheduler_respects_dag_scope_and_forces_in_place_n_equals_1() {
    let mut graph = RunHarness::build_sample_graph(
        "run-waves-1",
        "wave-run",
        "/tmp/ws",
        None,
        &["t1", "t2", "t3"],
    )
    .unwrap();
    // Make t1 and t2 disjoint scopes, and t3 depend on t1.
    for node in &mut graph.nodes {
        match node.task_ref.as_ref().map(|r| r.as_str()) {
            Some("t1") => node.scope = vec!["src/a/**".to_owned()],
            Some("t2") => node.scope = vec!["src/b/**".to_owned()],
            Some("t3") => node.scope = vec!["src/a/**".to_owned()],
            _ => {}
        }
        if node.kind == crate::types::NodeKind::Task {
            node.status = NodeStatus::Ready;
        }
    }
    graph.dependencies.push(crate::types::TaskDependencyRecord {
        run_id: graph.run.run_id.clone(),
        task_ref: TaskRef::new("t3").unwrap(),
        depends_on_workspace: "/tmp/ws".to_owned(),
        depends_on_run_id: graph.run.run_id.clone(),
        depends_on_ref: TaskRef::new("t1").unwrap(),
        satisfied: false,
    });

    let mut profile = MachineProfileInput {
        profile_id: "default".to_owned(),
        profile_json: "{}".to_owned(),
        git: MachineGitPolicy::default(),
        default_save: crate::types::SaveAction::Commit,
        protected_paths: Vec::new(),
        tags: BTreeSet::from(["default".to_owned()]),
        proof_schemas: BTreeMap::new(),
        gates: vec![MachineGraphItem::TasksBlock(MachineTasksBlockDef {
            when: None,
            gates: Vec::new(),
        })],
    };

    // Worktree isolation with jobs=4 schedules [t1, t2] in wave 1 and [t3] in wave 2.
    let wt_waves = compute_ready_waves(&graph, &profile, 4);
    assert_eq!(
        wt_waves,
        vec![
            vec!["task:t1".to_owned(), "task:t2".to_owned()],
            vec!["task:t3".to_owned()],
        ]
    );

    // InPlace isolation forces N=1 even when requested jobs=4.
    graph.run.isolation = IsolationMode::InPlace;
    profile.git.isolation = IsolationMode::InPlace;
    let ip_waves = compute_ready_waves(&graph, &profile, 4);
    assert_eq!(
        ip_waves,
        vec![
            vec!["task:t1".to_owned()],
            vec!["task:t2".to_owned()],
            vec!["task:t3".to_owned()],
        ]
    );

    // Dry-run executes 0 waves.
    let dry_cfg = WaveRunConfig {
        run_id: graph.run.run_id.clone(),
        jobs: 4,
        allow_main: false,
        dry_run: true,
    };
    let dry_rep = run_wave_scheduler(&dry_cfg, &graph, &profile, &mut |_| Ok(true)).unwrap();
    assert_eq!(dry_rep.waves_executed, 0);
    assert_eq!(dry_rep.halted_reason.as_deref(), Some("dry_run"));

    // Live run executes all 3 forced-serial waves and halts early on step returning Ok(false).
    let live_cfg = WaveRunConfig {
        dry_run: false,
        ..dry_cfg
    };
    let mut visited = Vec::new();
    let live_rep = run_wave_scheduler(&live_cfg, &graph, &profile, &mut |id| {
        visited.push(id.to_owned());
        Ok(true)
    })
    .unwrap();
    assert_eq!(live_rep.waves_executed, 3);
    assert_eq!(live_rep.gates_completed, 3);
    assert_eq!(visited, vec!["task:t1", "task:t2", "task:t3"]);

    let halt_rep = run_wave_scheduler(&live_cfg, &graph, &profile, &mut |id| {
        Ok(id != "task:t2")
    })
    .unwrap();
    assert_eq!(halt_rep.waves_executed, 2);
    assert_eq!(halt_rep.gates_completed, 1);
    assert!(halt_rep.halted_reason.is_some());
}

#[test]
fn scope_args_pattern_expands_file_stem_and_path_skipping_globs() {
    let mut map = BTreeMap::new();
    map.insert("tests/*.rs".to_owned(), "--test {file_stem}".to_owned());
    map.insert("src/**/*.rs".to_owned(), "--file {path}".to_owned());

    let cases = [
        (
            vec!["tests/integration_a.rs", "tests/*.rs"],
            "cargo test {scope_target} {test_filter}",
            "cargo test --test 'integration_a' {test_filter}",
        ),
        (
            vec!["tests/*.rs"],
            "cargo test {scope_target}",
            "cargo test",
        ),
        (
            vec!["src/lib.rs", "tests/api.rs"],
            "tool {scope_target}",
            "tool --file 'src/lib.rs' --test 'api'",
        ),
        (
            vec!["tests/my file $(touch pwned).rs"],
            "cargo test {scope_target}",
            "cargo test --test 'my file $(touch pwned)'",
        ),
        (
            vec!["src/dir with spaces/a'b$(touch pwned2);echo inj.rs"],
            "tool {scope_target}",
            "tool --file 'src/dir with spaces/a'\\''b$(touch pwned2);echo inj.rs'",
        ),
        // #9: Empty args remove only `{scope_target}` + one adjacent space (preserving `a  b`),
        // and non-empty args expand EVERY active `{scope_target}` placeholder.
        (
            vec!["tests/*.rs"],
            "echo {scope_target} a  b",
            "echo a  b",
        ),
        (
            vec!["tests/*.rs"],
            "a  b {scope_target}",
            "a  b",
        ),
        (
            vec!["src/lib.rs"],
            "a {scope_target} && b {scope_target}",
            "a --file 'src/lib.rs' && b --file 'src/lib.rs'",
        ),
        (
            vec!["tests/*.rs"],
            "left {scope_target}right",
            "left right",
        ),
        (
            vec!["tests/*.rs"],
            "left{scope_target} right",
            "left right",
        ),
        (
            vec!["tests/*.rs"],
            "x{scope_target} {scope_target}y",
            "x y",
        ),
    ];

    for (raw_paths, tpl, expected) in cases {
        let paths: Vec<String> = raw_paths.into_iter().map(str::to_owned).collect();
        let resolved = crate::profile::render_command_with_scope_target(tpl, &map, &paths)
            .expect("valid template should expand");
        assert_eq!(resolved, expected);
    }

    // Reject quoted/escaped placeholders or unbalanced quotes in command and scope_args_by_pattern templates.
    for (bad_cmd, bad_arg) in [
        ("echo '{scope_target}'", "--file {path}"),
        ("echo \"{scope_target}\"", "--file {path}"),
        ("echo \\{scope_target}", "--file {path}"),
        ("echo 'unclosed {scope_target}", "--file {path}"),
        ("echo {scope_target}", "--test '{file_stem}'"),
        ("echo {scope_target}", "\"--file {path}\""),
        ("echo {scope_target}", "--file \\{path}"),
        ("echo {scope_target}", "--file 'unclosed {path}"),
        ("echo # trailing comment omits placeholder", "--file {path}"),
        ("echo # trailing newline comment\n", "--file {path}"),
    ] {
        let bad_map = BTreeMap::from([("src/*.rs".to_owned(), bad_arg.to_owned())]);
        let err = crate::profile::render_command_with_scope_target(
            bad_cmd,
            &bad_map,
            &["src/lib.rs".to_owned()],
        )
        .expect_err("must reject unsafe shell template");
        assert_eq!(
            err.code,
            ErrorCode::ProfileSchemaInvalid,
            "cmd={bad_cmd} arg={bad_arg}"
        );
    }

    // Verify shell execution keeps spaces intact and does NOT execute embedded `$()` or `;`.
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();
    let exec_req = CommandExecReq {
        run_id: test_run_id(),
        node_cursor: test_cursor(),
        claim_revision: 5,
        worktree: worktree.clone(),
        primary_worktree: worktree.clone(),
        command_id: "scope-quote".to_owned(),
        command: CommandDef::Detailed(Box::new(DetailedCommandDef {
            command: "printf 'ARG:%s\\n' {scope_target}".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: map,
            output: None,
        })),
        scope: vec![
            "tests/my file $(touch pwned).rs".to_owned(),
            "src/dir with spaces/a'b$(touch pwned2);echo inj.rs".to_owned(),
        ],
        test_target: None,
        limits: ResourceLimits::default(),
        trace_parent: None,
    };
    let out = execute_gate_commands(&exec_req).expect("quoted scope command runs");
    assert_eq!(out.exit_code, Some(0));
    assert!(!worktree.join("pwned").exists(), "$(touch pwned) must not execute");
    assert!(!worktree.join("pwned2").exists(), "$(touch pwned2) must not execute");
    let log_text = fs::read_to_string(
        worktree
            .join(".acdd/run")
            .join(exec_req.run_id.as_str())
            .join(&out.log_relpath),
    )
    .unwrap();
    assert!(log_text.contains("ARG:my file $(touch pwned)\n"));
    assert!(log_text.contains("ARG:src/dir with spaces/a'b$(touch pwned2);echo inj.rs\n"));
}
