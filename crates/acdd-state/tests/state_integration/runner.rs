use acdd_core::errors::ErrorCode;
use acdd_core::ids::{GateId, NodeCursor, RunId, TaskRef, WorkerId};
use acdd_state::runner::*;
use acdd_state::types::ClaimOutcome;
use acdd_state::workflow::{
    schema::{DetailedCommandDef, PatternCommandEntry, TestFilterConfig},
    CommandDef, ResourceLimits, RoleDef, RoleExecutionMode,
};
use std::collections::BTreeMap;
use std::fs;
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
fn command_escaping_and_line_bounds_matrix() {
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();

    // Case 1: POSIX single-quote escaping for `{test}` and `{test_filter}` executes safely in `sh`.
    let escape_cmd = CommandDef::Detailed(Box::new(DetailedCommandDef {
        command: "printf '%s\n' {test_filter}".to_owned(),
        description: None,
        timeout_seconds: None,
        test_filter: Some(TestFilterConfig::Template("--exact {test}".to_owned())),
        scope_args_by_pattern: BTreeMap::new(),
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
            profile_scope_args: BTreeMap::new(),
            test_target: Some(target.to_owned()),
            limits: ResourceLimits::default(),
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
        profile_scope_args: BTreeMap::new(),
        test_target: Some("t::x".to_owned()),
        limits: ResourceLimits::default(),
    };
    let err = execute_gate_commands(&chained_req).expect_err("must reject unplaceholdered chain");
    assert_eq!(err.code, ErrorCode::CommandTestFilterPlaceholderRequired);

    // Case 3: max_line_bytes, max_log_bytes, and bounded stderr tail capture.
    let script = concat!(
        "echo 'stdout progress line'; ",
        "echo 'error[E0425]: cannot find value' >&2; ",
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
        })),
        scope: vec!["src/**".to_owned()],
        profile_scope_args: BTreeMap::new(),
        test_target: None,
        limits: ResourceLimits {
            max_line_bytes: 64,
            max_log_bytes: 512,
            max_capture_bytes: 4096,
            ..ResourceLimits::default()
        },
    };

    let outcome = execute_gate_commands(&parse_req).expect("bounded execution runs");
    assert_eq!(outcome.exit_code, Some(0));
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
            post_always: false,
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
            post_always: false,
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
        profile_scope_args: BTreeMap::new(),
        test_target: None,
        limits: ResourceLimits::default(),
    };
    let pat_out = execute_gate_commands(&pat_req).unwrap();
    assert_eq!(pat_out.command_line, "echo specific -p acdd-state");
}

#[test]
fn spawner_stdin_env_session_reuse_and_submission_verification_matrix() {
    let dir = TempDir::new().unwrap();
    let worktree = dir.path().to_path_buf();
    let mut cache = BTreeMap::new();

    // Case 1: Missing RoleDef.runner -> WorkflowRunnerRequired.
    let run_id = test_run_id();
    let cursor = test_cursor();
    let default_role = RoleDef::default();
    let w1 = WorkerId::new("w1").unwrap();
    let state_db = worktree.join("state.sqlite");
    let code_db = worktree.join("code-map.sqlite");
    let trace_parent = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
    let missing_runner = RoleSpawnBorrow {
        run_id: &run_id,
        node_cursor: &cursor,
        node_container_id: "task:t1",
        claim_revision: 1,
        role_name: "builder",
        role_def: &default_role,
        worker_id: &w1,
        worktree: &worktree,
        primary_worktree: &worktree,
        state_db_path: &state_db,
        code_db_path: None,
        trace_parent,
        prompt_markdown: "hi",
        timeout_seconds: 5,
        max_log_bytes: 1_048_576,
        max_line_bytes: 65_536,
    };
    let err =
        spawn_role_runner_borrowed(&missing_runner, &mut cache).expect_err("must require runner");
    assert_eq!(err.code, ErrorCode::WorkflowRunnerRequired);

    // Case 2: Large stdin prompt (> 130KB) + full ACDD_* env injection + reuse_on_reject matrix.
    let big_prompt = format!("# Gate Claim\n\n{}", "payload-line\n".repeat(12_000));
    assert!(big_prompt.len() > 130_000);

    let builder_cmd = "wc -c; printf 'ENV:%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$ACDD_RUN_ID\" \"$ACDD_NODE_CURSOR\" \"$ACDD_CLAIM_REVISION\" \"$ACDD_WORKER_ID\" \"$ACDD_AGENT_SESSION_ID\" \"$ACDD_CODE_DB\" \"$TRACEPARENT\" \"$ACDD_ROLE_MODE\" \"$ACDD_ROLE_MODELS\" \"$ACDD_MODEL\" \"$ACDD_REASONING\"";
    let builder_role = RoleDef {
        mode: RoleExecutionMode::Subagent,
        reuse_on_reject: true,
        models: vec![
            acdd_state::workflow::schema::ModelSpec {
                model: "m1".to_owned(),
                reasoning: Some("high".to_owned()),
            },
            acdd_state::workflow::schema::ModelSpec {
                model: "m2".to_owned(),
                reasoning: None,
            },
        ],
        runner: Some(builder_cmd.to_owned()),
        ..RoleDef::default()
    };
    let w_build = WorkerId::new("w-build").unwrap();
    let mut builder_req = RoleSpawnBorrow {
        run_id: &run_id,
        node_cursor: &cursor,
        node_container_id: "task:t1",
        claim_revision: 1,
        role_name: "builder",
        role_def: &builder_role,
        worker_id: &w_build,
        worktree: &worktree,
        primary_worktree: &worktree,
        state_db_path: &state_db,
        code_db_path: Some(&code_db),
        trace_parent,
        prompt_markdown: &big_prompt,
        timeout_seconds: 5,
        max_log_bytes: 1_048_576,
        max_line_bytes: 65_536,
    };
    let b1 = spawn_role_runner_borrowed(&builder_req, &mut cache).unwrap();
    assert_eq!(b1.exit_code, Some(0));
    let log1 = fs::read_to_string(
        worktree
            .join(".acdd/run")
            .join(run_id.as_str())
            .join(&b1.log_relpath),
    )
    .unwrap();
    assert!(log1.contains(&big_prompt.len().to_string()));
    assert!(log1.contains("ENV:run-runner-1|t1.build|1|w-build|"));
    assert!(log1.contains(&b1.agent_session_id));
    assert!(log1.contains("|subagent|m1:high,m2|m1|high\n"), "{log1}");

    // Inline role with empty models clears optional model/reasoning vars cleanly.
    let mut empty_model_role = RoleDef {
        mode: RoleExecutionMode::Inline,
        models: Vec::new(),
        ..builder_role.clone()
    };
    let r_empty = spawn_role_runner_borrowed(
        &RoleSpawnBorrow {
            role_name: "coordinator",
            role_def: &empty_model_role,
            ..builder_req
        },
        &mut cache,
    )
    .unwrap();
    let log_empty = fs::read_to_string(
        worktree
            .join(".acdd/run")
            .join(run_id.as_str())
            .join(&r_empty.log_relpath),
    )
    .unwrap();
    assert!(log_empty.contains("|inline|||\n"), "{log_empty}");
    empty_model_role.models = vec![acdd_state::workflow::schema::ModelSpec {
        model: "m3".to_owned(),
        reasoning: None,
    }];
    let r_no_reasoning = spawn_role_runner_borrowed(
        &RoleSpawnBorrow {
            role_name: "coordinator",
            role_def: &empty_model_role,
            ..builder_req
        },
        &mut cache,
    )
    .unwrap();
    let log_no_reasoning = fs::read_to_string(
        worktree
            .join(".acdd/run")
            .join(run_id.as_str())
            .join(&r_no_reasoning.log_relpath),
    )
    .unwrap();
    assert!(
        log_no_reasoning.contains("|inline|m3|m3|\n"),
        "{log_no_reasoning}"
    );

    // Re-spawning builder in the same (run_id, node_container_id, role) with reuse_on_reject=true reuses session ID.
    builder_req.claim_revision = 2;
    let b2 = spawn_role_runner_borrowed(&builder_req, &mut cache).unwrap();
    assert_eq!(b2.agent_session_id, b1.agent_session_id);

    // Independent reviewer (`reuse_on_reject: false`) always gets a fresh session ID.
    let reviewer_role = RoleDef {
        reuse_on_reject: false,
        ..builder_role.clone()
    };
    let reviewer_req = RoleSpawnBorrow {
        role_name: "independent_reviewer",
        role_def: &reviewer_role,
        ..builder_req
    };
    let r1 = spawn_role_runner_borrowed(&reviewer_req, &mut cache).unwrap();
    let r2 = spawn_role_runner_borrowed(&reviewer_req, &mut cache).unwrap();
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
    let cases: &[(
        &RoleSpawnOutcome,
        Option<ClaimOutcome>,
        Result<(), ErrorCode>,
    )] = &[
        (&ok_outcome, Some(ClaimOutcome::Passed), Ok(())),
        (&ok_outcome, Some(ClaimOutcome::Rejected), Ok(())),
        (
            &ok_outcome,
            Some(ClaimOutcome::Active),
            Err(ErrorCode::RunnerNoSubmission),
        ),
        (&ok_outcome, None, Err(ErrorCode::RunnerNoSubmission)),
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
fn scope_args_pattern_expands_file_stem_and_path_skipping_globs() {
    let directory_map = BTreeMap::from([
        (
            "crates/acdd-index/**/*.rs".to_owned(),
            "--package acdd-index --lib".to_owned(),
        ),
        (
            "tests/linker/**/*.rs".to_owned(),
            "--test linker".to_owned(),
        ),
    ]);
    for (path, expected) in [
        (
            "crates/acdd-index/src/linker/",
            "cargo test --package acdd-index --lib",
        ),
        (
            "crates/acdd-index/src/languages",
            "cargo test --package acdd-index --lib",
        ),
        (
            "crates/acdd-index/src/languages/**",
            "cargo test --package acdd-index --lib",
        ),
        ("tests/linker/", "cargo test --test linker"),
        ("tests/linker", "cargo test --test linker"),
    ] {
        assert_eq!(
            acdd_state::workflow::render_command_with_scope_target(
                "cargo test {scope_target}",
                &directory_map,
                &[path.to_owned()]
            )
            .unwrap(),
            vec![expected],
            "directory scope failed for {path}"
        );
    }
    let mut map = BTreeMap::new();
    map.insert("tests/*.rs".to_owned(), "--test {file_stem}".to_owned());
    map.insert("src/**/*.rs".to_owned(), "--file {path}".to_owned());

    let cases: &[(&[&str], &str, &[&str])] = &[
        (
            &["tests/integration_a.rs", "tests/*.rs"],
            "cargo test {scope_target} {test_filter}",
            &["cargo test --test 'integration_a' {test_filter}"],
        ),
        (
            &["tests/*.rs"],
            "cargo test {scope_target}",
            &["cargo test"],
        ),
        (
            &["src/lib.rs", "tests/api.rs"],
            "tool {scope_target}",
            &["tool --file 'src/lib.rs'", "tool --test 'api'"],
        ),
        (
            &["tests/my file $(touch pwned).rs"],
            "cargo test {scope_target}",
            &["cargo test --test 'my file $(touch pwned)'"],
        ),
        (
            &["src/dir with spaces/a'b$(touch pwned2);echo inj.rs"],
            "tool {scope_target}",
            &["tool --file 'src/dir with spaces/a'\\''b$(touch pwned2);echo inj.rs'"],
        ),
        // #9: Empty args remove only `{scope_target}` + one adjacent space (preserving `a  b`),
        // and non-empty args expand EVERY active `{scope_target}` placeholder.
        (&["tests/*.rs"], "echo {scope_target} a  b", &["echo a  b"]),
        (&["tests/*.rs"], "a  b {scope_target}", &["a  b"]),
        (
            &["src/lib.rs"],
            "a {scope_target} && b {scope_target}",
            &["a --file 'src/lib.rs' && b --file 'src/lib.rs'"],
        ),
        (&["tests/*.rs"], "left {scope_target}right", &["left right"]),
        (&["tests/*.rs"], "left{scope_target} right", &["left right"]),
        (&["tests/*.rs"], "x{scope_target} {scope_target}y", &["x y"]),
    ];

    for &(raw_paths, tpl, expected) in cases {
        let paths: Vec<String> = raw_paths.iter().copied().map(str::to_owned).collect();
        let resolved = acdd_state::workflow::render_command_with_scope_target(tpl, &map, &paths)
            .expect("valid template should expand");
        let expected_vec: Vec<String> = expected.iter().copied().map(str::to_owned).collect();
        assert_eq!(resolved, expected_vec);
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
        let err = acdd_state::workflow::render_command_with_scope_target(
            bad_cmd,
            &bad_map,
            &["src/lib.rs".to_owned()],
        )
        .expect_err("must reject unsafe shell template");
        assert_eq!(
            err.code,
            ErrorCode::WorkflowSchemaInvalid,
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
        })),
        scope: vec![
            "tests/my file $(touch pwned).rs".to_owned(),
            "src/dir with spaces/a'b$(touch pwned2);echo inj.rs".to_owned(),
        ],
        profile_scope_args: BTreeMap::new(),
        test_target: None,
        limits: ResourceLimits::default(),
    };
    let out = execute_gate_commands(&exec_req).expect("quoted scope command runs");
    assert_eq!(out.exit_code, Some(0));
    assert!(
        !worktree.join("pwned").exists(),
        "$(touch pwned) must not execute"
    );
    assert!(
        !worktree.join("pwned2").exists(),
        "$(touch pwned2) must not execute"
    );
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
