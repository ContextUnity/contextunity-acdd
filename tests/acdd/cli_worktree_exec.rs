use super::support::{git_cmd, init_git_repo_on_branch};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::WorkerId;
use acdd_state::store::sqlite::SqliteStateReader;
use acdd_state::store::traits::StateReader;
use acdd_state::usecases::{StateCommandOutput, StateQueryOutput};
use contextunity_acdd::state_bridge::{resolve_exec_target, ExecResolveInput};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const EXEC_TEST_WORKFLOW: &str = r#"version: 2
inherit: none
save: none
roles:
  builder:
    mode: subagent
commands:
  ok_check: "echo verified"
git:
  isolation: worktree
  worktrees: manual
gates:
  - id: build
    role: builder
    command: ok_check
"#;

fn acdd_cmd(cwd: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut cmd)
        .current_dir(cwd)
        .args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn acdd")
}

fn acdd_ok(cwd: &Path, envs: &[(&str, &str)], args: &[&str]) -> String {
    let out = acdd_cmd(cwd, envs, args);
    assert!(
        out.status.success(),
        "acdd {:?} failed in {} (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        args,
        cwd.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn seed_exec_repo(ws: &Workspace) -> std::path::PathBuf {
    let primary = ws.root().canonicalize().unwrap();
    init_git_repo_on_branch(&primary, "main");
    git_cmd(&primary, &["config", "commit.gpgsign", "false"]);
    ws.write("acdd.yaml", "preset: full\n");
    ws.write(".acdd/workflows/default.yaml", EXEC_TEST_WORKFLOW);
    git_cmd(&primary, &["add", "acdd.yaml", ".acdd/workflows/default.yaml"]);
    git_cmd(&primary, &["commit", "-m", "seed acdd workflow"]);
    primary
}

fn start_worktree_run(primary: &Path, name: &str) -> (String, std::path::PathBuf) {
    let raw = acdd_ok(
        primary,
        &[],
        &[
            "--format",
            "json",
            "start",
            "--goal",
            &format!("goal for {name}"),
            "--name",
            name,
            "--worktree",
            "--scope",
            "src/lib.rs",
        ],
    );
    let StateCommandOutput::Start(resp) = serde_json::from_str(&raw).unwrap() else {
        panic!("expected start output: {raw}");
    };
    let wt = std::path::PathBuf::from(resp.run.worktree.expect("run worktree"))
        .canonicalize()
        .unwrap();
    (resp.run.run_id.to_string(), wt)
}

#[cfg(unix)]
fn probe_env_args() -> Vec<&'static str> {
    vec![
        "sh",
        "-c",
        "printf '%s|%s|%s|%s' \"$(pwd -P)\" \"${ACDD_WORKSPACE_ROOT:-}\" \"${ACDD_WORKTREE:-}\" \"${ACDD_RUN:-}\"",
    ]
}

#[cfg(windows)]
fn probe_env_args() -> Vec<&'static str> {
    vec![
        "cmd",
        "/C",
        "echo %CD%|%ACDD_WORKSPACE_ROOT%|%ACDD_WORKTREE%|%ACDD_RUN%",
    ]
}

#[test]
fn cli_dash_c_worktree_parity_and_env_inheritance() {
    let ws = Workspace::new();
    let primary = seed_exec_repo(&ws);
    let (run_a, wt_a) = start_worktree_run(&primary, "run-alpha");
    let wt_a_str = wt_a.to_str().unwrap();

    // Seed a file, index the worktree via `-C`, then delete the file on disk to test deleted-file delta.
    fs::write(wt_a.join("src/to_delete.rs"), "pub fn temp_fn() {}\n").unwrap();
    let _ = acdd_ok(&primary, &[], &["-C", wt_a_str, "build"]);
    fs::remove_file(wt_a.join("src/to_delete.rs")).unwrap();

    // Table-driven `-C <wt>` parity across code/docs and state commands without `--run`/`--worktree`.
    let code_and_docs_cases: &[&[&str]] = &[
        &["-C", "", "delta", "src/to_delete.rs"],
        &["-C", "", "delta", "src/lib.rs"],
        &["-C", "", "read", "src/lib.rs", "--start-line", "1", "--line-count", "5"],
        &["-C", "", "query", "search", "base"],
        &["-C", "", "ast", "grep", "fn $NAME() -> i32"],
        &["-C", "", "lint"],
        &["-C", "", "docs", "check"],
    ];
    for template in code_and_docs_cases {
        let mut args: Vec<&str> = template.to_vec();
        args[1] = wt_a_str;
        let _ = acdd_ok(&primary, &[], &args);
    }
    // Redundant matching positional `.` alongside `-C <wt_a>`, and non-inventory deleted file routes to delta error (not OS root canonicalize).
    let _ = acdd_ok(&wt_a, &[], &["-C", wt_a_str, "delta", ".", "src/lib.rs"]);
    let missing_inv = acdd_cmd(&primary, &[], &["-C", wt_a_str, "delta", "src/no-such-deleted-file.rs"]);
    assert!(!missing_inv.status.success());
    assert!(
        String::from_utf8_lossy(&missing_inv.stderr).contains("deletion was not in previous inventory"),
        "{}",
        String::from_utf8_lossy(&missing_inv.stderr)
    );

    let state_cases: &[(&[&str], &str)] = &[
        (&["status"], &run_a),
        (&["show"], &run_a),
        (&["next"], &run_a),
        (&["context", "build"], &run_a),
        (&["blackboard", "--action", "read"], &run_a),
        (
            &[
                "blackboard",
                "--action",
                "post",
                "--topic",
                "notes",
                "--payload",
                "parity check",
            ],
            &run_a,
        ),
        (&["claim", "build", "--worker", "w-parity"], &run_a),
        (&["verify", "build", "--worker", "w-parity"], &run_a),
        (
            &["submit", "build", "--action", "pass", "--worker", "w-parity"],
            &run_a,
        ),
    ];
    for (tail, expected_run) in state_cases {
        let mut args = vec!["--format", "json", "-C", wt_a_str];
        args.extend_from_slice(tail);
        let out = acdd_ok(&primary, &[], &args);
        assert!(
            out.contains(expected_run),
            "command {tail:?} with -C did not target {expected_run}: {out}"
        );
    }

    // `ACDD_WORKTREE` fallback (without `-C` or `ACDD_WORKSPACE_ROOT`) and `ACDD_RUN` inheritance.
    let (run_b, wt_b) = start_worktree_run(&primary, "run-beta");
    let wt_b_str = wt_b.to_str().unwrap();
    let env_status = acdd_ok(
        &primary,
        &[("ACDD_WORKTREE", wt_b_str), ("ACDD_RUN", &run_b)],
        &["--format", "json", "status"],
    );
    assert!(env_status.contains(&run_b), "{env_status}");

    // Explicit `-C` overrides ambient `ACDD_WORKTREE` and `ACDD_WORKSPACE_ROOT`.
    let override_status = acdd_ok(
        &primary,
        &[
            ("ACDD_WORKSPACE_ROOT", primary.to_str().unwrap()),
            ("ACDD_WORKTREE", wt_b_str),
        ],
        &["--format", "json", "-C", wt_a_str, "status"],
    );
    assert!(override_status.contains(&run_a), "{override_status}");

    // Explicit `-C` takes precedence over conflicting ambient `ACDD_RUN` across read & mutation seams,
    // while explicit `--run` retains precedence over `-C`.
    struct ConflictCase<'a> {
        label: &'a str,
        envs: &'a [(&'a str, &'a str)],
        args: &'a [&'a str],
        expected_run: &'a str,
    }
    let conflicting_envs = [
        ConflictCase {
            label: "dash-c-beats-nonexistent-env-status",
            envs: &[("ACDD_RUN", "nonexistent-run")],
            args: &["--format", "json", "-C", wt_b_str, "status"],
            expected_run: &run_b,
        },
        ConflictCase {
            label: "dash-c-beats-other-env-claim",
            envs: &[("ACDD_RUN", &run_a)],
            args: &["--format", "json", "-C", wt_b_str, "claim", "build", "--worker", "w-override"],
            expected_run: &run_b,
        },
        ConflictCase {
            label: "dash-c-beats-other-env-blackboard-post",
            envs: &[("ACDD_RUN", &run_a)],
            args: &[
                "--format",
                "json",
                "-C",
                wt_b_str,
                "blackboard",
                "--action",
                "post",
                "--topic",
                "notes",
                "--payload",
                "override ok",
            ],
            expected_run: &run_b,
        },
        ConflictCase {
            label: "explicit-run-flag-beats-dash-c",
            envs: &[("ACDD_RUN", "nonexistent-run")],
            args: &["--format", "json", "-C", wt_b_str, "status", "--run", &run_a],
            expected_run: &run_a,
        },
    ];
    for case in conflicting_envs {
        let out = acdd_ok(&primary, case.envs, case.args);
        assert!(out.contains(case.expected_run), "{}: {out}", case.label);
    }

    // `clear` run selection precedence: `--run` CLI flag > positional > `-C`/worktree > `ACDD_RUN`.
    let flag_beats_pos = acdd_cmd(
        &primary,
        &[("ACDD_RUN", &run_b)],
        &["clear", "nonexistent-pos", "--run", "nonexistent-flag", "--force"],
    );
    assert!(!flag_beats_pos.status.success());
    let flag_err = String::from_utf8_lossy(&flag_beats_pos.stderr);
    assert!(flag_err.contains("nonexistent-flag"), "{flag_err}");

    let pos_beats_env = acdd_ok(
        &primary,
        &[("ACDD_RUN", &run_b)],
        &["--format", "json", "clear", &run_a, "--force"],
    );
    assert!(pos_beats_env.contains(&run_a), "{pos_beats_env}");

    let dash_c_beats_env_clear = acdd_ok(
        &primary,
        &[("ACDD_RUN", "nonexistent-env")],
        &["--format", "json", "-C", wt_b_str, "clear", "--force"],
    );
    assert!(dash_c_beats_env_clear.contains(&run_b), "{dash_c_beats_env_clear}");

    // Linked worktree `validate` follows primary `state_db` when primary `acdd.yaml` is valid,
    // and still validates the worktree when primary holds a pre-cutover `WorkflowSchemaInvalid` `acdd.yaml`.
    let (run_v, wt_v) = start_worktree_run(&primary, "run-val");
    let wt_v_str = wt_v.to_str().unwrap();
    fs::create_dir_all(primary.join("docs/milestones")).unwrap();
    fs::write(
        primary.join("docs/milestones/AC-001-base.md"),
        "---\nid: AC-001\ntitle: Base\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: active\nscope: [src/]\n```\n",
    )
    .unwrap();
    fs::write(
        primary.join("acdd.yaml"),
        "preset: full\nstate_db: .acdd/primary-state.sqlite\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    )
    .unwrap();
    fs::write(
        primary.join(".acdd/workflows/task_wf.yaml"),
        "version: 2\ngates:\n  - tasks:\n      gates:\n        - id: build\n          command: build_check\n          proof: command\ncommands:\n  build_check: \"echo ok\"\n",
    )
    .unwrap();
    let _ = acdd_ok(
        &primary,
        &[],
        &[
            "start",
            "docs/milestones/AC-001-base.md",
            "--workflow",
            ".acdd/workflows/task_wf.yaml",
            "--in-place",
        ],
    );
    fs::create_dir_all(wt_v.join(".acdd/workflows")).unwrap();
    fs::write(
        wt_v.join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: code\ncommands:\n  build_check: \"echo ok\"\n  verify_all: \"echo ok\"\n",
    )
    .unwrap();
    fs::create_dir_all(wt_v.join("docs/milestones")).unwrap();
    fs::write(
        wt_v.join("docs/milestones/AC-001-base.md"),
        "---\nid: AC-001\ntitle: Base\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: active\nscope: [src/]\nstatus: active\n```\n",
    )
    .unwrap();
    fs::write(
        wt_v.join("acdd.yaml"),
        "preset: full\nstate_db: .acdd/wt-other.sqlite\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    )
    .unwrap();
    let _ = acdd_ok(&primary, &[], &["-C", wt_v_str, "validate"]);
    let _ = acdd_ok(&primary, &[], &["clear", "--force"]);
    fs::write(
        wt_v.join("docs/milestones/AC-001-base.md"),
        "---\nid: AC-001\ntitle: Base\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: active\nscope: [src/]\n```\n",
    )
    .unwrap();
    fs::write(primary.join("acdd.yaml"), "unknown_pre_cutover_key: 1\n").unwrap();
    let _ = acdd_ok(&primary, &[], &["-C", wt_v_str, "validate"]);
    let _ = run_v;
}

#[test]
fn cli_exec_resolution_env_injection_exit_codes_and_fail_closed() {
    // 1. Missing state.sqlite must fail closed without creating `.acdd/state.sqlite`,
    //    and `modules.state: false` must be respected.
    let cold_ws = Workspace::new();
    let cold_root = cold_ws.root().canonicalize().unwrap();
    init_git_repo_on_branch(&cold_root, "main");
    let cold_out = acdd_cmd(&cold_root, &[], &["exec", "--", "echo", "should-fail"]);
    assert!(!cold_out.status.success());
    assert!(!cold_root.join(".acdd/state.sqlite").exists());

    cold_ws.write("acdd.yaml", "modules:\n  state: false\n");
    let disabled_err_direct = resolve_exec_target(&ExecResolveInput {
        explicit_cli_root: None,
        env_workspace_root: None,
        env_worktree: None,
        explicit_config: None,
        explicit_state_db: None,
        worker: None,
        run_selector: None,
        run_from_cli: false,
        cwd: &cold_root,
    })
    .unwrap_err();
    assert_eq!(disabled_err_direct.code, ErrorCode::FeatureDisabled);
    let disabled_out = acdd_cmd(&cold_root, &[], &["exec", "--", "echo", "should-fail"]);
    assert!(!disabled_out.status.success());
    let disabled_err = String::from_utf8_lossy(&disabled_out.stderr);
    assert!(
        disabled_err.contains("FEATURE_DISABLED") || disabled_err.contains("disabled"),
        "{disabled_err}"
    );
    assert!(!cold_root.join(".acdd/state.sqlite").exists());
    // Explicit `-C` when `modules.state: false` still executes in checkout without touching state.sqlite.
    let explicit_disabled = acdd_ok(
        &cold_root,
        &[],
        &["-C", cold_root.to_str().unwrap(), "exec", "--", "echo", "ok"],
    );
    assert_eq!(explicit_disabled.trim(), "ok");
    assert!(!cold_root.join(".acdd/state.sqlite").exists());

    // 2. Single active run resolution, CWD inside linked worktree subdir, unbound `-C`, and exit codes.
    let ws = Workspace::new();
    let primary = seed_exec_repo(&ws);
    let (run_one, wt_one) = start_worktree_run(&primary, "exec-one");
    let wt_one_canon = acdd_core::fs::canonicalize_macos_var(&wt_one);
    let unbound_dir = ws.root().join("unbound-checkout");
    fs::create_dir_all(&unbound_dir).unwrap();
    let unbound_canon = acdd_core::fs::canonicalize_macos_var(&unbound_dir.canonicalize().unwrap());

    let probe = probe_env_args();
    let run_probe = |cwd: &Path, envs: &[(&str, &str)], prefix: &[&str]| {
        let mut args = prefix.to_vec();
        args.push("exec");
        args.push("--");
        args.extend(probe.iter().copied());
        acdd_ok(cwd, envs, &args)
    };

    let single_out = run_probe(&primary, &[], &[]);
    let expected_one = format!(
        "{}|{}|{}|{}",
        wt_one_canon.display(),
        wt_one_canon.display(),
        wt_one_canon.display(),
        run_one
    );
    assert_eq!(single_out.trim(), expected_one);

    // CWD already inside a linked worktree subdirectory resolves that worktree and bound run.
    let subdir_out = run_probe(&wt_one.join("src"), &[], &[]);
    assert_eq!(subdir_out.trim(), expected_one);

    // Explicit `-C` on an unbound directory strips any inherited `ACDD_RUN`.
    let unbound_out = run_probe(
        &primary,
        &[("ACDD_RUN", &run_one)],
        &["-C", unbound_canon.to_str().unwrap()],
    );
    let expected_unbound = format!(
        "{}|{}|{}|",
        unbound_canon.display(),
        unbound_canon.display(),
        unbound_canon.display()
    );
    assert_eq!(unbound_out.trim(), expected_unbound);

    // Exit-code propagation (non-zero status and Unix signal -> 128 + sig).
    #[cfg(unix)]
    {
        let exit_42 = acdd_cmd(&primary, &[], &["exec", "--", "sh", "-c", "exit 42"]);
        assert_eq!(exit_42.status.code(), Some(42));

        let sig_term = acdd_cmd(&primary, &[], &["exec", "--", "sh", "-c", "kill -TERM $$"]);
        assert_eq!(sig_term.status.code(), Some(128 + 15));
    }

    // 3. Multi-run disambiguation (`--run`, `ACDD_RUN`, `--worker`, explicit `-C`, and fail-closed errors).
    let (run_two, wt_two) = start_worktree_run(&primary, "exec-two");
    let wt_two_canon = acdd_core::fs::canonicalize_macos_var(&wt_two);
    let state_reader = SqliteStateReader::new(primary.join(".acdd/state.sqlite"));
    assert!(state_reader
        .active_worker_claims(&WorkerId::new("w-exec").unwrap())
        .unwrap()
        .is_empty());
    let _ = acdd_ok(
        &primary,
        &[],
        &["-C", wt_one.to_str().unwrap(), "claim", "build", "--worker", "w-exec"],
    );
    let one_claim = state_reader
        .active_worker_claims(&WorkerId::new("w-exec").unwrap())
        .unwrap();
    assert_eq!(one_claim.len(), 1);
    assert_eq!(one_claim[0].run_id.as_str(), run_one);

    let expected_two = format!(
        "{}|{}|{}|{}",
        wt_two_canon.display(),
        wt_two_canon.display(),
        wt_two_canon.display(),
        run_two
    );

    struct ExecCase<'a> {
        label: &'a str,
        envs: &'a [(&'a str, &'a str)],
        exec_args: &'a [&'a str],
        expected: Option<&'a str>,
        err_needle: Option<&'a str>,
    }
    let cases = [
        ExecCase {
            label: "ambiguous-no-selector",
            envs: &[],
            exec_args: &["exec"],
            expected: None,
            err_needle: Some("ambiguous"),
        },
        ExecCase {
            label: "explicit-run-flag",
            envs: &[],
            exec_args: &["exec", "--run", "exec-two"],
            expected: Some(&expected_two),
            err_needle: None,
        },
        ExecCase {
            label: "env-acdd-run",
            envs: &[("ACDD_RUN", &run_two)],
            exec_args: &["exec"],
            expected: Some(&expected_two),
            err_needle: None,
        },
        ExecCase {
            label: "explicit-worker",
            envs: &[],
            exec_args: &["exec", "--worker", "w-exec"],
            expected: Some(&expected_one),
            err_needle: None,
        },
        ExecCase {
            label: "unknown-worker-fails",
            envs: &[],
            exec_args: &["exec", "--worker", "missing-worker"],
            expected: None,
            err_needle: Some("no active in_progress gate"),
        },
        ExecCase {
            label: "explicit-dash-c-disambiguates",
            envs: &[],
            exec_args: &["-C", wt_two.to_str().unwrap(), "exec"],
            expected: Some(&expected_two),
            err_needle: None,
        },
    ];
    for case in cases {
        let mut args = case.exec_args.to_vec();
        args.push("--");
        args.extend(probe.iter().copied());
        let out = acdd_cmd(&primary, case.envs, &args);
        if let Some(exp) = case.expected {
            assert!(
                out.status.success(),
                "{}: {}",
                case.label,
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                exp,
                "{}",
                case.label
            );
        } else {
            assert!(!out.status.success(), "{} unexpectedly succeeded", case.label);
            let err = String::from_utf8_lossy(&out.stderr);
            if let Some(needle) = case.err_needle {
                assert!(err.contains(needle), "{}: {err}", case.label);
            }
        }
    }

    // Multiple active claims by the same worker must fail closed.
    let _ = acdd_ok(
        &primary,
        &[],
        &["-C", wt_two.to_str().unwrap(), "claim", "build", "--worker", "w-exec"],
    );
    let two_claims = state_reader
        .active_worker_claims(&WorkerId::new("w-exec").unwrap())
        .unwrap();
    assert_eq!(two_claims.len(), 2);
    let mut multi_claim_args = vec!["exec", "--worker", "w-exec", "--"];
    multi_claim_args.extend(probe.iter().copied());
    let multi_claim = acdd_cmd(&primary, &[], &multi_claim_args);
    assert!(!multi_claim.status.success());
    let multi_err = String::from_utf8_lossy(&multi_claim.stderr);
    assert!(multi_err.contains("2 active in_progress claims"), "{multi_err}");
}

#[test]
fn cli_show_artifact_context_detail_and_submit_evidence_routing_matrix() {
    let ws = Workspace::new();
    let root = ws.root().canonicalize().unwrap();
    init_git_repo_on_branch(&root, "main");
    git_cmd(&root, &["config", "commit.gpgsign", "false"]);
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(
        root.join(".acdd/workflows/ev_wf.yaml"),
        "version: 2\nroles:\n  architect: {}\n  reviewer: {}\ncontours:\n  arch:\n    description: Architecture\n    criteria: [Verify]\nproofs:\n  custom_rev_like:\n    schema:\n      type: object\n      required: [decision, summary, contours]\n      properties:\n        decision: { type: string }\n        summary: { type: string }\n        contours: { type: object }\ngates:\n  - id: bp\n    role: architect\n    proof: custom_rev_like\n    pass: rev\n  - id: rev\n    role: reviewer\n    proof: review\n    receipt: true\n    need: [bp]\n    contours: [arch]\n",
    )
    .unwrap();
    git_cmd(&root, &["add", ".acdd/workflows/ev_wf.yaml"]);
    git_cmd(&root, &["commit", "-m", "seed ev_wf"]);
    let start_out = acdd_ok(
        &root,
        &[],
        &[
            "--format",
            "json",
            "start",
            "--goal",
            "test evidence routing",
            "--name",
            "shared-ev-run",
            "--workflow",
            ".acdd/workflows/ev_wf.yaml",
            "--in-place",
        ],
    );
    let StateCommandOutput::Start(start_resp) = serde_json::from_str(&start_out).unwrap() else {
        panic!("expected Start output: {start_out}");
    };
    let run_id = start_resp.run.run_id.to_string();

    // 1. `context --detail full` sets `full_detail: true`, compact omits it.
    let ctx_compact_raw = acdd_ok(
        &root,
        &[],
        &["--format", "json", "context", "bp", "--run", &run_id, "--detail", "compact"],
    );
    let StateQueryOutput::Context(ctx_compact) = serde_json::from_str(&ctx_compact_raw).unwrap()
    else {
        panic!("expected Context output: {ctx_compact_raw}");
    };
    assert!(!ctx_compact.full_detail);
    let ctx_full_raw = acdd_ok(
        &root,
        &[],
        &["--format", "json", "context", "bp", "--run", &run_id, "--detail", "full"],
    );
    let StateQueryOutput::Context(ctx_full) = serde_json::from_str(&ctx_full_raw).unwrap()
    else {
        panic!("expected Context output: {ctx_full_raw}");
    };
    assert!(ctx_full.full_detail);

    // 2. Claim `bp`, submit CustomSchema JSON containing `decision`/`summary`/`contours` without being hijacked into ReviewEvidence.
    let _ = acdd_ok(&root, &[], &["claim", "bp", "--worker", "arch", "--run", &run_id]);
    let _ = acdd_ok(
        &root,
        &[],
        &[
            "submit",
            "bp",
            "--action",
            "pass",
            "--worker",
            "arch",
            "--run",
            &run_id,
            "--evidence",
            r#"{"decision":"pass","summary":"looks good","contours":{"arch":{"evidence":"ok"}}}"#,
        ],
    );

    // 3. `show --artifact proof` returns full parsed payload and omits `profile_json` by default.
    let show_default_raw = acdd_ok(
        &root,
        &[],
        &["--format", "json", "show", "bp", "--run", &run_id],
    );
    let StateQueryOutput::Show(show_default) = serde_json::from_str(&show_default_raw).unwrap()
    else {
        panic!("expected Show output: {show_default_raw}");
    };
    assert!(show_default.run.profile_json.is_empty());
    assert_eq!(show_default.gate_artifacts.len(), 1);
    assert_eq!(
        show_default.gate_artifacts[0].kind,
        acdd_state::types::ArtifactKind::Proof
    );
    assert!(show_default.gate_artifacts[0].payload.is_none());

    let show_proof_raw = acdd_ok(
        &root,
        &[],
        &[
            "--format",
            "json",
            "show",
            "bp",
            "--run",
            &run_id,
            "--artifact",
            "proof",
            "--include-profile",
        ],
    );
    let StateQueryOutput::Show(show_proof) = serde_json::from_str(&show_proof_raw).unwrap()
    else {
        panic!("expected Show output: {show_proof_raw}");
    };
    assert!(!show_proof.run.profile_json.is_empty());
    assert_eq!(
        show_proof.gate_artifacts[0].payload.as_ref().unwrap()["summary"],
        "looks good"
    );

    // 4. Claim `rev` (`proof: review`) and pass malformed ReviewEvidence -> exact serde error (never swallowed as "requires --evidence").
    let _ = acdd_ok(&root, &[], &["claim", "rev", "--worker", "reviewer", "--run", &run_id]);
    let bad_rev = acdd_cmd(
        &root,
        &[],
        &[
            "submit",
            "rev",
            "--action",
            "pass",
            "--worker",
            "reviewer",
            "--run",
            &run_id,
            "--evidence",
            r#"{"decision":"pass","summary":"oops"}"#,
        ],
    );
    assert!(!bad_rev.status.success());
    let err_txt = String::from_utf8_lossy(&bad_rev.stderr);
    assert!(
        err_txt.contains("invalid review evidence"),
        "expected serde error, got: {err_txt}"
    );
    assert!(!err_txt.contains("requires --evidence"), "{err_txt}");

    // 5. Complete run 1, start a distinct active worktree run, and verify `submit` without `--run` resolves active-only.
    let _ = acdd_ok(
        &root,
        &[],
        &[
            "submit",
            "rev",
            "--action",
            "pass",
            "--worker",
            "reviewer",
            "--run",
            &run_id,
            "--evidence",
            r#"{"decision":"pass","summary":"verified","contours":{"arch":{"applicable":true,"evidence":"ok"}}}"#,
        ],
    );
    let active_start = acdd_ok(
        &root,
        &[],
        &[
            "--format",
            "json",
            "start",
            "--goal",
            "active named run",
            "--name",
            "active-ev-run",
            "--workflow",
            ".acdd/workflows/ev_wf.yaml",
            "--worktree",
        ],
    );
    let StateCommandOutput::Start(active_resp) = serde_json::from_str(&active_start).unwrap() else {
        panic!("expected Start output: {active_start}");
    };
    let active_id = active_resp.run.run_id.to_string();
    let _ = acdd_ok(&root, &[], &["claim", "bp", "--worker", "arch", "--run", &active_id]);
    let done_submit = acdd_cmd(
        &root,
        &[],
        &[
            "submit",
            "bp",
            "--action",
            "pass",
            "--worker",
            "arch",
            "--run",
            &run_id,
            "--evidence",
            r#"{"decision":"pass","summary":"stale run","contours":{"arch":{"evidence":"ok"}}}"#,
        ],
    );
    assert!(!done_submit.status.success());
    let done_err = String::from_utf8_lossy(&done_submit.stderr);
    assert!(
        done_err.contains("RUN_SELECTOR_AMBIGUOUS"),
        "completed run must be rejected by active-only resolution, got: {done_err}"
    );
    let _ = acdd_ok(
        &root,
        &[],
        &[
            "submit",
            "bp",
            "--action",
            "pass",
            "--worker",
            "arch",
            "--evidence",
            r#"{"decision":"pass","summary":"active bp","contours":{"arch":{"evidence":"ok"}}}"#,
        ],
    );
}
