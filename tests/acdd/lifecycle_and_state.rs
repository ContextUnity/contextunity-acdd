use super::support::{base_engine_config, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::WorkerId;
use acdd_state::harness::RunHarness;
use acdd_state::types::SubmitAction;
use acdd_state::usecases::dto::{ClaimReq, DraftReq, StartReq, SubmitReq, SyncReq};
use acdd_state::usecases::{execute_on, StateCommand, StateCommandOutput};
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
fn cli_start_and_sync_share_state_from_linked_checkout() {
    use super::support::git_cmd;
    use std::process::Command;

    let ws = Workspace::new();
    let primary = ws.root().canonicalize().unwrap();
    init_git_repo_on_branch(&primary, "primary");
    ws.write("acdd.yaml", "state_db: .acdd/repo.sqlite\n");
    let linked = primary.join(".worktrees/source");
    let linked_arg = linked.to_str().unwrap();
    git_cmd(&primary, &["worktree", "add", "-b", "source", linked_arg]);
    let spec = "work/AC-001.md";
    ws.write(
        ".worktrees/source/work/AC-001.md",
        "---\nid: AC-001\ntitle: Linked\nstatus: active\n---\n",
    );
    ws.write(
        ".worktrees/source/.acdd/profile.yaml",
        "version: 2\ninherit: none\nsave: none\ngates:\n  - id: handoff\n",
    );
    ws.write(
        ".worktrees/source/acdd.yaml",
        "state_db: .acdd/linked.sqlite\nspecs:\n  custom: {path: work}\n",
    );
    ws.write(
        ".worktrees/source/alternate.yaml",
        "state_db: .acdd/alternate.sqlite\nprofiles_dirs: [overrides]\nspecs:\n  override: {path: alternate-work}\n",
    );
    ws.write(
        ".worktrees/source/alternate-work/AC-004.md",
        "---\nid: AC-004\ntitle: Alternate collection\nstatus: active\ndepends_on: [AC-005]\n---\n",
    );
    ws.write(
        ".worktrees/source/alternate-work/archive/AC-005.md",
        "---\nid: AC-005\ntitle: Alternate archive\nstatus: completed\n---\n",
    );
    git_cmd(&linked, &["add", "alternate-work/archive"]);
    git_cmd(&linked, &["commit", "-m", "seed alternate archive"]);
    ws.write(".worktrees/source/overrides/chosen.yaml", "version: 2\ninherit: parent\n");
    ws.write(
        ".worktrees/source/overrides/parent.yaml",
        "version: 2\ninherit: none\nsave: none\ngates:\n  - id: handoff\n",
    );

    let cases = [
        ("cwd", linked.as_path(), vec![], vec![]),
        (
            "root",
            primary.as_path(),
            vec!["--root", linked_arg],
            vec![],
        ),
        (
            "configured",
            primary.as_path(),
            vec!["--root", linked_arg, "--config", "alternate.yaml"],
            vec!["--profile", "chosen"],
        ),
        (
            "explicit",
            primary.as_path(),
            vec!["--profile", ".acdd/profile.yaml"],
            vec!["--worktree-path", ".worktrees/source"],
        ),
    ];
    for case in cases {
        let (name, cwd, options, start_options) = case;
        let spec = if name == "configured" { "alternate-work/AC-004.md" } else { spec };
        let run_cli = |options: &[&str], args: &[&str], trailing: &[&str]| {
            let result = Command::new(env!("CARGO_BIN_EXE_acdd"))
                .current_dir(cwd)
                .args(["--format", "json"])
                .args(options)
                .args(args)
                .args(trailing)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{name} {args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            serde_json::from_slice::<StateCommandOutput>(&result.stdout).unwrap()
        };
        let StateCommandOutput::Start(started) =
            run_cli(&options, &["start", spec, "--name", name], &start_options)
        else {
            panic!("expected start output");
        };
        let (collection, profile) = if name == "configured" {
            ("override", "overrides/chosen.yaml")
        } else {
            ("custom", ".acdd/profile.yaml")
        };
        assert_eq!(started.run.collection.as_deref(), Some(collection));
        assert_eq!(started.run.profile_id, profile);
        assert_eq!(started.run.worktree.as_deref(), Some(linked_arg));
        let sync_options = if name == "explicit" {
            &[][..]
        } else {
            &options
        };
        assert!(matches!(run_cli(sync_options,
            &["sync", "--run", started.run.run_id.as_str(), "--rebind-profile"], &[]),
            StateCommandOutput::Sync(output) if output.rebound_profile));
        if name == "explicit" {
            assert!(matches!(run_cli(&["--root", linked_arg],
                &["claim", "handoff", "--run", started.run.run_id.as_str(), "--worker", "trace-worker"], &[]),
                StateCommandOutput::Claim(_)));
            let traces = primary.join(format!(".acdd/run/{}/traces.jsonl", started.run.run_id));
            let before = fs::read_to_string(&traces).unwrap().lines().count();
            let status = Command::new(env!("CARGO_BIN_EXE_acdd"))
                .current_dir(&linked)
                .args(["status", "--run", started.run.run_id.as_str()])
                .output().unwrap();
            assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
            assert!(fs::read_to_string(&traces).unwrap().lines().count() > before);
            let before = fs::read(&traces).unwrap();
            let keep = primary.join(".acdd/run/retention-sentinel");
            fs::create_dir_all(&keep).unwrap();
            fs::write(keep.join("traces.jsonl"), "sentinel\n").unwrap();
            fs::write(primary.join("acdd.yaml"), "state_db: [invalid\n").unwrap();
            let status = Command::new(env!("CARGO_BIN_EXE_acdd"))
                .current_dir(&linked)
                .args(["status", "--run", started.run.run_id.as_str()])
                .output().unwrap();
            assert!(!status.status.success());
            assert_eq!(fs::read(&traces).unwrap(), before);
            assert!(keep.exists());
            fs::write(primary.join("acdd.yaml"), "state_db: .acdd/repo.sqlite\n").unwrap();
        }
        if name != "explicit" {
            assert!(matches!(
                run_cli(
                    sync_options,
                    &["clear", "--run", started.run.run_id.as_str()],
                    &[]
                ),
                StateCommandOutput::Clear(_)
            ));
        }
    }
    assert!(primary.join(".acdd/repo.sqlite").is_file());
    for name in ["state", "linked", "alternate"] {
        for root in [&primary, &linked] {
            assert!(!root.join(format!(".acdd/{name}.sqlite")).exists());
        }
    }
}

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
}
