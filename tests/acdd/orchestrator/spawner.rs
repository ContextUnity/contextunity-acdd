#![cfg(target_os = "linux")]

use crate::common::Workspace;
use acdd_tasks::{
    engine::orchestrator::{
        spawner::{
            monitor_runner_process, render_role_prompt, spawn_role_runner, RunnerError,
            RunnerOutcome, RunnerProcess, RunnerSessionPolicy, RunnerStream,
        },
        types::RunnerInvocationContext,
    },
    tasks::profile::{validate_profile_document, RunnerConfig},
};
use serde_json::{json, Value};
use std::{
    fs, io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn role_fixture_path(_root: &Path) -> String {
    crate::acdd::snapshot::role_helper_path()
        .to_string_lossy()
        .replace('\'', "'\\''")
}

fn runner_config(root: &Path, mode: &str) -> RunnerConfig {
    let fixture = role_fixture_path(root);
    let runner = format!(
        "'{fixture}' runner-fixture {mode} {{prompt}} \"{{model}}\" '{{reasoning}}' {{session_id}} '{{worktree}}'"
    );
    let profile = validate_profile_document(
        &serde_yaml::to_string(&json!({
            "agents": {"fixture": {"models": ["test-model:high"], "runner": runner}}
        }))
        .unwrap(),
    )
    .unwrap();
    RunnerConfig::from_role(&profile.roles["fixture"]).unwrap()
}

fn invocation(worktree: &Path, prompt: &str) -> RunnerInvocationContext {
    RunnerInvocationContext {
        task_id: "fixture/milestone:runner".into(),
        stage: "build".into(),
        role: "custom-role-without-name-heuristics".into(),
        model: "model {reasoning}".into(),
        reasoning: "reasoning {model}".into(),
        session_id: "retained-builder-session".into(),
        prompt: prompt.into(),
        worktree: worktree.into(),
    }
}

fn capture(
    process: RunnerProcess,
    timeout: Duration,
) -> Result<(RunnerOutcome, Vec<u8>, Vec<u8>), RunnerError> {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let outcome = monitor_runner_process(process, timeout, &mut |stream, bytes| {
        match stream {
            RunnerStream::Stdout => stdout.extend_from_slice(bytes),
            RunnerStream::Stderr => stderr.extend_from_slice(bytes),
        }
        Ok(())
    })?;
    Ok((outcome, stdout, stderr))
}

#[test]
fn unsupported_provider_resume_is_rejected_at_profile_and_spawn_boundaries() {
    let root = Workspace::new();
    let context = invocation(root.root(), "data");
    let cases = [
        "codex exec resume {session_id} {prompt}",
        "codex resume {session_id} {prompt}",
        "codex -C {worktree} exec -m {model} resume {session_id} {prompt}",
        "codex --cd {worktree} --json resume {session_id} {prompt}",
        "codex --remote unix://socket e --image file.png resume {session_id} {prompt}",
        "codex exec --resume {prompt}",
        "codex exec --continue {prompt}",
        "codex exec --resume=latest {prompt}",
        "codex exec {thread_id} {prompt}",
        "codex exec {provider_session_id} {prompt}",
        "codex exec {conversation_id} {prompt}",
    ];
    for runner in cases {
        let document = serde_yaml::to_string(&json!({"agents": {"fixture": {
            "models": ["test-model:high"], "reuse_on_reject": true, "runner": runner
        }}}))
        .unwrap();
        let error = validate_profile_document(&document).unwrap_err();
        assert!(
            format!("{error:#}").contains("provider conversation resumption is unsupported"),
            "{runner}: {error:#}"
        );
        let config = RunnerConfig {
            runner: runner.into(),
            prompt_template: None,
        };
        let error = spawn_role_runner(&config, &context, RunnerSessionPolicy::Resume).unwrap_err();
        assert!(
            error.to_string().contains("ACDD_AGENT_SESSION_ID"),
            "{runner}: {error}"
        );
    }
    let ordinary = serde_yaml::to_string(&json!({"agents": {"fixture": {
        "models": ["test-model:high"], "reuse_on_reject": true,
        "runner": "codex -m resume exec -C {worktree} {prompt}"
    }}}))
    .unwrap();
    assert!(validate_profile_document(&ordinary).is_ok());
    let author = include_str!("../../../docs/workflows/author-milestone.workflow.yaml");
    let profile = validate_profile_document(author).unwrap();
    assert!(profile.roles["architect"].effective_reuse_on_reject());
    assert!(profile.roles["architect"]
        .runner
        .as_ref()
        .unwrap()
        .starts_with("codex exec "));
}

#[test]
fn runner_reports_spawn_failure() {
    let root = Workspace::new();
    let config = RunnerConfig {
        runner: "/no/such/acdd-role-runner {prompt}".into(),
        prompt_template: None,
    };
    let result = spawn_role_runner(
        &config,
        &invocation(root.root(), "data"),
        RunnerSessionPolicy::Resume,
    );
    assert!(
        matches!(result, Err(RunnerError::Io(ref error)) if error.kind() == io::ErrorKind::NotFound),
        "{result:?}"
    );
}

#[test]
fn monitor_delivers_both_streams_before_child_exit() {
    let root = Workspace::new();
    let acknowledgment = root.path("streams-drained");
    let context = invocation(root.root(), acknowledgment.to_str().unwrap());
    let launched = spawn_role_runner(
        &runner_config(root.root(), "streams"),
        &context,
        RunnerSessionPolicy::Resume,
    );
    assert!(launched.is_ok(), "runner must launch: {launched:?}");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let result = monitor_runner_process(
        launched.unwrap(),
        Duration::from_secs(5),
        &mut |stream, bytes| {
            match stream {
                RunnerStream::Stdout => stdout.extend_from_slice(bytes),
                RunnerStream::Stderr => stderr.extend_from_slice(bytes),
            }
            if stdout.len() == 262_144 && stderr.len() == 262_144 {
                fs::write(&acknowledgment, b"drained while child is alive")?;
            }
            Ok(())
        },
    );
    assert!(result.is_ok(), "both streams must drain: {result:?}");
    let outcome = result.unwrap();
    assert!(!outcome.timed_out);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(stdout, vec![b'o'; 262_144]);
    assert_eq!(stderr, vec![b'e'; 262_144]);
    assert!(acknowledgment.exists());
}

#[test]
fn monitor_cleans_up_when_output_sink_fails() {
    let root = Workspace::new();
    let launched = spawn_role_runner(
        &runner_config(root.root(), "wait"),
        &invocation(root.root(), "data"),
        RunnerSessionPolicy::Resume,
    );
    assert!(launched.is_ok(), "runner must launch: {launched:?}");
    let process = launched.unwrap();
    let pid = process.id();
    let result = monitor_runner_process(process, Duration::from_secs(5), &mut |_, _| {
        Err(io::Error::other("log sink unavailable"))
    });
    assert!(matches!(result, Err(RunnerError::Io(_))), "{result:?}");
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn dropping_runner_reaps_its_child() {
    let root = Workspace::new();
    let launched = spawn_role_runner(
        &runner_config(root.root(), "wait"),
        &invocation(root.root(), "data"),
        RunnerSessionPolicy::Resume,
    );
    assert!(launched.is_ok(), "runner must launch: {launched:?}");
    let process = launched.unwrap();
    let pid = process.id();
    drop(process);
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn runner_resumes_builder_identity_and_isolates_fresh_sessions() {
    let root = Workspace::new();
    let context = invocation(root.root(), "data");
    let parent_identity = std::env::var_os("ACDD_AGENT_SESSION_ID");
    let mut observed_sessions = Vec::new();
    for policy in [
        RunnerSessionPolicy::Resume,
        RunnerSessionPolicy::Fresh,
        RunnerSessionPolicy::Fresh,
    ] {
        let launched = spawn_role_runner(&runner_config(root.root(), "inspect"), &context, policy);
        assert!(launched.is_ok(), "runner must launch: {launched:?}");
        let (outcome, stdout, _) = capture(launched.unwrap(), Duration::from_secs(5)).unwrap();
        let observed: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(observed["acdd_session"], outcome.session_fingerprint);
        assert_eq!(observed["argv"][3], outcome.session_fingerprint);
        assert!(!outcome.session_fingerprint.trim().is_empty());
        if policy == RunnerSessionPolicy::Resume {
            assert_eq!(outcome.session_fingerprint, context.session_id);
        } else {
            assert_ne!(outcome.session_fingerprint, context.session_id);
            assert_ne!(
                Some(std::ffi::OsString::from(&outcome.session_fingerprint)),
                parent_identity
            );
            assert!(!observed_sessions.contains(&outcome.session_fingerprint));
        }
        observed_sessions.push(outcome.session_fingerprint);
        assert_eq!(std::env::var_os("ACDD_AGENT_SESSION_ID"), parent_identity);
    }
}

#[test]
fn runner_argument_matrix() {
    let root = Workspace::new();
    let worktree = root.path("work tree");
    fs::create_dir(&worktree).unwrap();
    let fixture = role_fixture_path(root.root());
    let base = invocation(&worktree, "");
    let mut cases = Vec::new();
    for prompt in [
        "",
        "two words",
        "quotes '\" \\ newline\nUnicode: λ {session_id}",
        "$(touch injected); `touch injected`; & | > injected",
    ] {
        cases.push((
            runner_config(root.root(), "inspect"),
            prompt,
            json!([
                prompt,
                base.model,
                base.reasoning,
                base.session_id,
                worktree.to_string_lossy()
            ]),
        ));
    }
    let mut quoted = runner_config(root.root(), "inspect");
    quoted
        .runner
        .push_str(r#" '' "" joined' words' escaped\ space "back\slash" "quote\"end" '#'"#);
    cases.push((
        quoted,
        "literal {model}",
        json!([
            "literal {model}",
            base.model,
            base.reasoning,
            base.session_id,
            worktree.to_string_lossy(),
            "",
            "",
            "joined words",
            "escaped space",
            "back\\slash",
            "quote\"end",
            "#"
        ]),
    ));
    let codex = RunnerConfig {
        runner: format!("'{fixture}' runner-fixture inspect exec -C \"{{worktree}}\" -m \"{{model}}\" -c model_reasoning_effort=\"{{reasoning}}\" \"{{prompt}}\""),
        prompt_template: None,
    };
    let prompt = "quoted 'prompt' with spaces";
    cases.push((
        codex,
        prompt,
        json!([
            "exec",
            "-C",
            worktree.to_string_lossy(),
            "-m",
            base.model,
            "-c",
            format!("model_reasoning_effort={}", base.reasoning),
            prompt
        ]),
    ));
    for (config, prompt, expected_argv) in cases {
        let context = invocation(&worktree, prompt);
        let process = spawn_role_runner(&config, &context, RunnerSessionPolicy::Resume).unwrap();
        let pid = process.id();
        let session = process.session_fingerprint().to_owned();
        let (outcome, stdout, stderr) = capture(process, Duration::from_secs(5)).unwrap();
        let observed: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(observed["argv"], expected_argv);
        assert_eq!(observed["cwd"], json!(context.worktree.to_string_lossy()));
        assert_eq!(observed["pid"], pid);
        assert_eq!(observed["pgid"], pid);
        assert_ne!(observed["pgid"], unsafe { libc::getpgrp() });
        assert_eq!(observed["acdd_session"], context.session_id);
        assert_eq!(outcome.process_id, pid);
        assert_eq!(outcome.session_fingerprint, session);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.timed_out);
        assert!(stderr.is_empty());
        assert!(!worktree.join("injected").exists());
    }
}

#[test]
fn runner_configuration_matrix() {
    enum Expected {
        Argv(Value),
        Context,
        Command,
        Session,
    }
    let root = Workspace::new();
    let fixture = role_fixture_path(root.root());
    let base = invocation(root.root(), "quoted value {model}");
    let mut cases = Vec::new();
    for (template, model, reasoning, expected) in [
        ("'{prompt}'", "", "", Expected::Argv(json!([base.prompt]))),
        ("-m '{model}'", "", "high", Expected::Context),
        ("-m '{model}'", "   ", "high", Expected::Context),
        (
            "-c 'model_reasoning_effort={reasoning}'",
            "model",
            "",
            Expected::Context,
        ),
        (
            "-c 'model_reasoning_effort={reasoning}'",
            "model",
            "  ",
            Expected::Context,
        ),
        (
            "'{model}' '{reasoning}'",
            "model with spaces",
            "high",
            Expected::Argv(json!(["model with spaces", "high"])),
        ),
    ] {
        let mut context = base.clone();
        context.model = model.into();
        context.reasoning = reasoning.into();
        cases.push((
            RunnerConfig {
                runner: format!("'{fixture}' runner-fixture inspect {template}"),
                prompt_template: None,
            },
            context,
            expected,
        ));
    }
    for runner in [
        "",
        "   ",
        "''",
        "\"\"",
        "runner 'unterminated",
        "runner \"unterminated",
        "runner trailing\\",
        "runner nul\0argument",
    ] {
        cases.push((
            RunnerConfig {
                runner: runner.into(),
                prompt_template: None,
            },
            base.clone(),
            Expected::Command,
        ));
    }
    for session in ["", " \t\n", acdd_tasks::store::UNKNOWN_SESSION_FINGERPRINT] {
        let mut context = base.clone();
        context.session_id = session.into();
        cases.push((runner_config(root.root(), "inspect"), context, Expected::Session));
    }
    for (config, context, expected) in cases {
        let result = spawn_role_runner(&config, &context, RunnerSessionPolicy::Resume);
        match expected {
            Expected::Argv(argv) => {
                let (outcome, stdout, _) =
                    capture(result.unwrap(), Duration::from_secs(5)).unwrap();
                assert_eq!(outcome.exit_code, Some(0));
                let observed: Value = serde_json::from_slice(&stdout).unwrap();
                assert_eq!(observed["argv"], argv);
            }
            Expected::Context => assert!(
                matches!(result, Err(RunnerError::InvalidContext(_))),
                "{result:?}"
            ),
            Expected::Command => assert!(
                matches!(result, Err(RunnerError::InvalidCommand(_))),
                "{result:?}"
            ),
            Expected::Session => assert!(
                matches!(result, Err(RunnerError::InvalidSession(_))),
                "{result:?}"
            ),
        }
    }
}

#[test]
fn monitor_completion_matrix() {
    let root = Workspace::new();
    let bytes: Vec<u8> = (0..=255).collect();
    for (mode, exit_code, expected_streams) in [
        ("exit", Some(7), None),
        (
            "binary",
            Some(0),
            Some((
                bytes.clone(),
                bytes.iter().rev().copied().collect::<Vec<_>>(),
            )),
        ),
        ("signal", None, None),
    ] {
        let process = spawn_role_runner(
            &runner_config(root.root(), mode),
            &invocation(root.root(), "data"),
            RunnerSessionPolicy::Resume,
        )
        .unwrap();
        let (outcome, stdout, stderr) = capture(process, Duration::from_secs(5)).unwrap();
        assert_eq!(outcome.exit_code, exit_code, "{mode}");
        assert!(!outcome.timed_out, "{mode}");
        if let Some((expected_stdout, expected_stderr)) = expected_streams {
            assert_eq!(stdout, expected_stdout);
            assert_eq!(stderr, expected_stderr);
        }
    }
}

#[test]
fn monitor_deadline_matrix() {
    struct Case {
        mode: &'static str,
        descendant: bool,
        no_exit: bool,
        output: Option<&'static [u8]>,
        output_required: bool,
        reaped: bool,
    }
    let root = Workspace::new();
    for case in [
        Case {
            mode: "hang",
            descendant: true,
            no_exit: true,
            output: None,
            output_required: false,
            reaped: true,
        },
        Case {
            mode: "orphan",
            descendant: true,
            no_exit: false,
            output: None,
            output_required: false,
            reaped: false,
        },
        Case {
            mode: "flood",
            descendant: false,
            no_exit: false,
            output: None,
            output_required: true,
            reaped: true,
        },
        Case {
            mode: "move",
            descendant: false,
            no_exit: false,
            output: Some(b"moved\n"),
            output_required: false,
            reaped: true,
        },
    ] {
        let ready = root.path(format!("{}.pid", case.mode));
        let context = invocation(root.root(), ready.to_str().unwrap());
        let process = spawn_role_runner(
            &runner_config(root.root(), case.mode),
            &context,
            RunnerSessionPolicy::Resume,
        )
        .unwrap();
        let pid = process.id();
        let descendant = if case.descendant {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Ok(contents) = fs::read_to_string(&ready) {
                    if let Ok(pid) = contents.parse::<i32>() {
                        break Some(pid);
                    }
                }
                assert!(Instant::now() < deadline, "{}: child readiness", case.mode);
                thread::sleep(Duration::from_millis(10));
            }
        } else {
            None
        };
        let mut bytes_seen = 0;
        let mut stdout = Vec::new();
        let outcome =
            monitor_runner_process(process, Duration::from_millis(250), &mut |stream, bytes| {
                bytes_seen += bytes.len();
                if case.output.is_some() && matches!(stream, RunnerStream::Stdout) {
                    stdout.extend_from_slice(bytes);
                }
                Ok(())
            })
            .unwrap();
        assert!(outcome.timed_out, "{}", case.mode);
        if case.no_exit {
            assert_eq!(outcome.exit_code, None);
        }
        if case.output_required {
            assert!(bytes_seen > 0);
        }
        if let Some(expected) = case.output {
            assert_eq!(stdout, expected);
        }
        if let Some(descendant) = descendant {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let running = match fs::read_to_string(format!("/proc/{descendant}/stat")) {
                    Ok(record) => !record
                        .rsplit_once(')')
                        .expect("missing proc stat state")
                        .1
                        .trim_start()
                        .starts_with('Z'),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => panic!("{error}"),
                };
                if !running {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{}: descendant still running",
                    case.mode
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        if case.reaped {
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }
}

#[test]
fn role_prompt_matrix() {
    let base = json!({
        "task_id": "fixture/milestone:runner", "stage": "build",
        "goal": "Preserve inserted {prompt} literally", "allowed_write_scope": ["src/runner.rs"],
        "applicable_invariants": ["No parallel lifecycle authority"],
        "workflow_guidance": {
            "role_runner": {"rendered_prompt": "Already prepared: inserted {model} remains literal."},
            "expected_evidence_template": {"proof": {"command_proof": {"exit_code": 0}}},
            "rejection": {"findings": {"summary": "Repair stream cleanup"}}
        }
    });
    let mut cases = vec![(base.clone(), true)];
    for (parent, field) in [
        ("", "task_id"),
        ("", "goal"),
        ("", "allowed_write_scope"),
        ("/workflow_guidance", "expected_evidence_template"),
    ] {
        let mut claim = base.clone();
        claim
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        cases.push((claim, false));
    }
    for (pointer, value) in [
        (
            "/workflow_guidance/role_runner/rendered_prompt",
            Value::Null,
        ),
        ("/task_id", json!(" ")),
        ("/goal", json!(false)),
        ("/allowed_write_scope", json!([null])),
        ("/allowed_write_scope", json!({})),
        ("/workflow_guidance/expected_evidence_template", json!([])),
        ("/workflow_guidance/role_runner/rendered_prompt", json!("")),
        ("/byte_limited", json!(true)),
        ("/truncated", json!(true)),
        ("/omitted_fields", json!(["/context_bundle"])),
        ("/omitted_field_count", json!(1)),
    ] {
        let mut claim = base.clone();
        if let Some(field) = claim.pointer_mut(pointer) {
            *field = value;
        } else {
            claim[pointer.trim_start_matches('/')] = value;
        }
        cases.push((claim, false));
    }
    for (claim, valid) in cases {
        let result = render_role_prompt(&claim);
        if !valid {
            assert!(
                matches!(result, Err(RunnerError::InvalidContext(_))),
                "{result:?}"
            );
            continue;
        }
        let rendered = result.unwrap();
        let (prepared, context) = rendered.split_once("\n\nGate context:\n").unwrap();
        assert_eq!(
            prepared,
            claim["workflow_guidance"]["role_runner"]["rendered_prompt"]
        );
        assert_eq!(serde_json::from_str::<Value>(context).unwrap(), claim);
    }
}
