use super::support::{base_engine_config, git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::WorkerId;
use acdd_state::harness::RunHarness;
use acdd_state::types::SubmitAction;
use acdd_state::usecases::dto::{ClaimReq, DraftReq, StartReq, SubmitReq, SyncReq};
use acdd_state::usecases::{StateCommand, StateCommandOutput};
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
    ws.write(
        ".worktrees/source/overrides/chosen.yaml",
        "version: 2\ninherit: parent\n",
    );
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
        let spec = if name == "configured" {
            "alternate-work/AC-004.md"
        } else {
            spec
        };
        let run_cli = |options: &[&str], args: &[&str], trailing: &[&str]| {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
            crate::common::isolate_acdd_env(&mut cmd)
                .current_dir(cwd)
                .args(["--format", "json"])
                .args(options)
                .args(args)
                .args(trailing);
            let result = cmd.output().unwrap();
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
            assert!(matches!(
                run_cli(
                    &["--root", linked_arg],
                    &[
                        "claim",
                        "handoff",
                        "--run",
                        started.run.run_id.as_str(),
                        "--worker",
                        "trace-worker"
                    ],
                    &[]
                ),
                StateCommandOutput::Claim(_)
            ));
            let traces = primary.join(format!(".acdd/run/{}/traces.jsonl", started.run.run_id));
            let before = fs::read_to_string(&traces).unwrap().lines().count();
            let mut status_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
            let status = crate::common::isolate_acdd_env(&mut status_cmd)
                .current_dir(&linked)
                .args(["status", "--run", started.run.run_id.as_str()])
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
            assert!(fs::read_to_string(&traces).unwrap().lines().count() > before);
            let before = fs::read(&traces).unwrap();
            let keep = primary.join(".acdd/run/retention-sentinel");
            fs::create_dir_all(&keep).unwrap();
            fs::write(keep.join("traces.jsonl"), "sentinel\n").unwrap();
            fs::write(primary.join("acdd.yaml"), "state_db: [invalid\n").unwrap();
            let mut failed_status_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
            let status = crate::common::isolate_acdd_env(&mut failed_status_cmd)
                .current_dir(&linked)
                .args(["status", "--run", started.run.run_id.as_str()])
                .output()
                .unwrap();
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
fn cli_snapshot_round_trip() {
    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "dev");
    ws.write("acdd.yaml", "profile: full\nmodules: {state: true}\n");
    ws.write(".acdd/profile.yaml", "version: 2\ngit: {isolation: in_place, worktrees: manual}\nroles: {worker: {mode: subagent}}\ngates:\n  - tasks:\n      gates:\n        - {id: build, role: worker, proof: none}\n");
    ws.write("docs/milestones/010-snapshot.md", "---\nid: m-snapshot\ntitle: Snapshot\ndoc_type: milestone\nstatus: active\n---\n# Snapshot\n\n### task: snap-task\n```yaml\ntask_ref: snap-task\ntarget: Snapshot\nscope: [src/lib.rs]\n```\n");
    let r1 = "pub fn base() -> i32 { 1 }\n";
    let r2 = "pub fn base() -> i32 { 2 }\n";
    let refs = || git_cmd(ws.root(), &["for-each-ref", "refs/acdd/snapshots"]);
    let run = |args: &[&str]| -> Result<String, String> {
        let out = crate::common::cli::run_cli(&ws, args);
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    };
    run(&[
        "start",
        "docs/milestones/010-snapshot.md",
        "--in-place",
        "--name",
        "snapshot-run",
    ])
    .expect("start");
    ws.write("src/lib.rs", r1);
    let saved_r1 = run(&[
        "snapshot",
        "save",
        "snap-task",
        "--rev",
        "1",
        "--run",
        "snapshot-run",
    ])
    .expect("save r1");
    assert!(saved_r1.contains("/snap-task/r1"), "{saved_r1}");
    ws.write("src/lib.rs", r2);
    let saved_r2 = run(&[
        "snapshot",
        "save",
        "snap-task",
        "--rev",
        "2",
        "--run",
        "snapshot-run",
    ])
    .expect("save r2");
    assert!(saved_r2.contains("/snap-task/r2"), "{saved_r2}");
    assert!(
        refs().contains("/snap-task/r1") && refs().contains("/snap-task/r2"),
        "{}",
        refs()
    );
    let unrelated = "refs/acdd/snapshots/unrelated/snap-task/r1";
    git_cmd(ws.root(), &["update-ref", unrelated, "HEAD"]);
    ws.write("src/lib.rs", "dirty\n");
    let restored = run(&[
        "snapshot",
        "restore",
        "snap-task",
        "--rev",
        "1",
        "--run",
        "snapshot-run",
    ])
    .expect("restore r1");
    assert!(
        restored.contains("/snap-task/r1") && !restored.contains("/snap-task/r2"),
        "{restored}"
    );
    assert_eq!(fs::read_to_string(ws.path("src/lib.rs")).unwrap(), r1);
    let cleared =
        run(&["snapshot", "clear", "--run", "snapshot-run", "--rev", "1"]).expect("clear revision");
    assert!(
        cleared.contains("/snap-task/r1") && !cleared.contains("/snap-task/r2"),
        "{cleared}"
    );
    assert_eq!(
        fs::read_to_string(ws.path("src/lib.rs")).unwrap(),
        r1,
        "clear must not restore"
    );
    assert!(
        refs().contains("/snap-task/r2")
            && refs().contains(unrelated)
            && !refs().contains("snapshot-run/snap-task/r1"),
        "{}",
        refs()
    );
    assert!(
        run(&[
            "snapshot",
            "restore",
            "snap-task",
            "--rev",
            "1",
            "--run",
            "snapshot-run"
        ])
        .is_err(),
        "cleared r1 must be unrestorable"
    );
    assert_eq!(
        fs::read_to_string(ws.path("src/lib.rs")).unwrap(),
        r1,
        "failed restore after task clear must preserve the worktree"
    );
    ws.write("src/lib.rs", r2);
    run(&[
        "snapshot",
        "save",
        "snap-task",
        "--rev",
        "1",
        "--run",
        "snapshot-run",
    ])
    .expect("save before --all");
    ws.write("src/lib.rs", "dirty-all\n");
    let cleared_all =
        run(&["snapshot", "clear", "--all", "--run", "snapshot-run"]).expect("clear --all");
    assert!(cleared_all.contains("/snap-task/r1"), "{cleared_all}");
    assert!(
        !refs().contains("refs/acdd/snapshots/snapshot-run/"),
        "{}",
        refs()
    );
    assert!(refs().contains(unrelated), "{}", refs());
    assert!(
        run(&[
            "snapshot",
            "restore",
            "snap-task",
            "--rev",
            "1",
            "--run",
            "snapshot-run"
        ])
        .is_err(),
        "run-wide clear must make r1 unrestorable"
    );
    assert_eq!(
        fs::read_to_string(ws.path("src/lib.rs")).unwrap(),
        "dirty-all\n",
        "failed restore after run-wide clear must preserve the worktree"
    );
    let bare = Workspace::new();
    init_git_repo_on_branch(bare.root(), "dev");
    bare.write("src/lib.rs", "pub fn base() -> i32 { 9 }\n");
    let bare_root = bare.root().to_str().unwrap();
    let run_at = |root: &str, args: &[&str]| -> Result<String, String> {
        let mut argv = vec![
            std::ffi::OsString::from("--root"),
            std::ffi::OsString::from(root),
        ];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let out = crate::common::cli::run_cli_args(&argv);
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    };
    let saved = run_at(
        bare_root,
        &[
            "snapshot",
            "save",
            "note",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("adhoc save");
    assert!(saved.contains("_adhoc/note/r1"), "{saved}");
    assert!(!bare.root().join(".acdd").exists());
    let tree = git_cmd(bare.root(), &["write-tree"]);
    let oid = std::process::Command::new("git")
        .current_dir(bare.root())
        .env("GIT_AUTHOR_DATE", "1000000000 +0000")
        .env("GIT_COMMITTER_DATE", "1000000000 +0000")
        .args(["commit-tree", &tree, "-m", "backdated"])
        .output()
        .unwrap();
    assert!(
        oid.status.success(),
        "{}",
        String::from_utf8_lossy(&oid.stderr)
    );
    git_cmd(
        bare.root(),
        &[
            "update-ref",
            "refs/acdd/snapshots/_adhoc/note/r2",
            String::from_utf8(oid.stdout).unwrap().trim(),
        ],
    );
    let descendant = "refs/acdd/snapshots/_adhoc/descendant/child/r1";
    git_cmd(
        bare.root(),
        &[
            "update-ref",
            descendant,
            &git_cmd(
                bare.root(),
                &["rev-parse", "refs/acdd/snapshots/_adhoc/note/r2"],
            ),
        ],
    );
    let listed = run_at(
        bare_root,
        &["snapshot", "list", "note", "--worktree", bare_root],
    )
    .expect("list");
    assert!(listed.contains("/r1") && listed.contains("/r2"), "{listed}");
    let listed =
        run_at(bare_root, &["snapshot", "list", "--worktree", bare_root]).expect("targetless list");
    assert!(
        listed.contains("/note/r1") && listed.contains("/note/r2") && !listed.contains(descendant),
        "{listed}"
    );
    bare.write("src/lib.rs", "dirty-adhoc\n");
    run_at(
        bare_root,
        &[
            "snapshot",
            "restore",
            "note",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("restore");
    assert_eq!(
        fs::read_to_string(bare.path("src/lib.rs")).unwrap(),
        "pub fn base() -> i32 { 9 }\n"
    );
    run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "note",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("delete rev");
    run_at(
        bare_root,
        &[
            "snapshot",
            "save",
            "note",
            "--rev",
            "3",
            "--worktree",
            bare_root,
        ],
    )
    .expect("save 3");
    let filtered_rev = run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "note",
            "--rev",
            "3",
            "--older-than",
            "1d",
            "--worktree",
            bare_root,
        ],
    )
    .expect("filter fresh revision");
    assert!(!filtered_rev.contains("/note/r3"), "{filtered_rev}");
    assert!(git_cmd(
        bare.root(),
        &["for-each-ref", "refs/acdd/snapshots/_adhoc/note"]
    )
    .contains("/note/r3"));
    assert!(run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "note",
            "--rev",
            "3",
            "--older-than",
            "bad",
            "--worktree",
            bare_root
        ]
    )
    .is_err());
    let filtered = run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "--older-than",
            "1d",
            "--worktree",
            bare_root,
        ],
    )
    .expect("filter old revisions");
    assert!(filtered.contains("/note/r2"), "{filtered}");
    let left = git_cmd(bare.root(), &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(
        left.contains("/note/r3") && !left.contains("/note/r2") && left.contains(descendant),
        "{left}"
    );
    run_at(
        bare_root,
        &["snapshot", "clear", "--rev", "1", "--worktree", bare_root],
    )
    .expect("targetless revision clear");
    assert!(git_cmd(bare.root(), &["for-each-ref", descendant]).contains(descendant));
    run_at(
        bare_root,
        &["snapshot", "--delete", "--all", "--worktree", bare_root],
    )
    .expect("delete all");
    let left = git_cmd(bare.root(), &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(
        left.contains(descendant) && !left.contains("/note/"),
        "{left}"
    );
    let s1 = run_at(
        bare_root,
        &["snapshot", "save", "auto", "--worktree", bare_root],
    )
    .expect("auto r1");
    assert!(s1.contains("_adhoc/auto/r1"), "{s1}");
    let auto_prefix = "refs/acdd/snapshots/_adhoc/auto";
    git_cmd(
        bare.root(),
        &["update-ref", &format!("{auto_prefix}/r01"), "HEAD"],
    );
    git_cmd(
        bare.root(),
        &["update-ref", &format!("{auto_prefix}/r999/child"), "HEAD"],
    );
    bare.write("src/lib.rs", "pub fn base() -> i32 { 10 }\n");
    let s2 = run_at(
        bare_root,
        &["snapshot", "save", "auto", "--worktree", bare_root],
    )
    .expect("auto r2");
    assert!(s2.contains("_adhoc/auto/r2"), "{s2}");
    let listed = run_at(
        bare_root,
        &["snapshot", "list", "auto", "--worktree", bare_root],
    )
    .expect("list canonical revisions");
    assert!(
        listed.contains("/r1")
            && listed.contains("/r2")
            && !listed.contains("r01")
            && !listed.contains("nested")
            && !listed.contains("child"),
        "{listed}"
    );
    bare.write("src/lib.rs", "dirty-auto\n");
    run_at(
        bare_root,
        &["snapshot", "restore", "auto", "--worktree", bare_root],
    )
    .expect("restore latest canonical revision");
    assert_eq!(
        fs::read_to_string(bare.path("src/lib.rs")).unwrap(),
        "pub fn base() -> i32 { 10 }\n"
    );
    let zero_prefix = "refs/acdd/snapshots/_adhoc/zero";
    git_cmd(
        bare.root(),
        &["update-ref", &format!("{zero_prefix}/r01"), "HEAD"],
    );
    let zero_err = run_at(
        bare_root,
        &["snapshot", "restore", "zero", "--worktree", bare_root],
    )
    .unwrap_err();
    assert!(zero_err.contains("no snapshot ref found"), "{zero_err}");
    let zero = run_at(
        bare_root,
        &["snapshot", "save", "zero", "--worktree", bare_root],
    )
    .expect("ignore zero-padded auto revision");
    assert!(zero.contains("_adhoc/zero/r1"), "{zero}");
    let zero_list = run_at(
        bare_root,
        &["snapshot", "list", "zero", "--worktree", bare_root],
    )
    .expect("list zero");
    assert!(
        zero_list.contains("/r1") && !zero_list.contains("r01"),
        "{zero_list}"
    );
    bare.write("src/lib.rs", "dirty-zero\n");
    let zero_restore = run_at(
        bare_root,
        &["snapshot", "restore", "zero", "--worktree", bare_root],
    )
    .expect("restore latest zero");
    assert!(zero_restore.contains("_adhoc/zero/r1"), "{zero_restore}");
    assert_eq!(
        fs::read_to_string(bare.path("src/lib.rs")).unwrap(),
        "pub fn base() -> i32 { 10 }\n"
    );
    run_at(
        bare_root,
        &[
            "snapshot",
            "save",
            "other",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("other r1");
    let del = run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("delete rev");
    assert!(
        del.contains("/auto/r1") && del.contains("/zero/r1") && del.contains("/other/r1"),
        "{del}"
    );
    let auto_refs = git_cmd(bare.root(), &["for-each-ref", auto_prefix]);
    assert!(
        auto_refs.contains("/r2")
            && auto_refs.contains("/r01")
            && auto_refs.contains("/r999/child")
            && !auto_refs.lines().any(|name| name.ends_with("/auto/r1")),
        "{auto_refs}"
    );
    assert!(git_cmd(bare.root(), &["for-each-ref", descendant]).contains(descendant));
    run_at(
        bare_root,
        &[
            "snapshot",
            "save",
            "list",
            "--rev",
            "1",
            "--worktree",
            bare_root,
        ],
    )
    .expect("save list");
    run_at(
        bare_root,
        &["snapshot", "--delete", "list", "--worktree", bare_root],
    )
    .expect("delete list");
    assert!(!git_cmd(
        bare.root(),
        &["for-each-ref", "refs/acdd/snapshots/_adhoc/list"]
    )
    .contains("refs/acdd"));
    assert!(run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "--older-than",
            "1д",
            "--worktree",
            bare_root
        ]
    )
    .is_err());
    assert!(run_at(
        bare_root,
        &[
            "snapshot",
            "--delete",
            "--older-than",
            "9999999999w",
            "--worktree",
            bare_root
        ]
    )
    .is_err());
    let parsed:
 contextunity_acdd::mcp::tools::inputs::StateInput = serde_json::from_value(serde_json::json!({
        "action": "snapshot", "delete": true, "name": "note", "older_than": "2d", "snapshot_action": "list",
        "worktree": bare_root
    })).unwrap();
    assert_eq!(parsed.delete, Some(true));
    assert_eq!(parsed.name.as_deref(), Some("note"));
    assert_eq!(parsed.older_than.as_deref(), Some("2d"));
    assert!(matches!(
        parsed.snapshot_action,
        Some(acdd_state::usecases::SnapshotAction::List)
    ));
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
    let StateCommandOutput::Draft(drafted) = harness
        .execute(
            &cfg,
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
    let StateCommandOutput::Start(started) = harness
        .execute(
            &cfg,
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
    let pre_err = harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("first.work".to_owned()),
                worker_id: w1.clone(),
                session_fingerprint: Some("s1".to_owned()),
                force: false,
                steal_remote: false,
                worktree: None,
                detail: Default::default(),
            }),
        )
        .unwrap_err();
    assert_eq!(pre_err.code, ErrorCode::GateVerificationFailed);
    assert!(
        pre_err
            .message
            .contains("Pre-task gate 'pre_gate' must complete"),
        "{pre_err:?}"
    );
    assert_eq!(pre_err.hint.as_deref(), Some("claim 'pre_gate' next"));

    // Claim & pass pre_gate -> unlocks task `first`.
    harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: None, // auto-select pre_gate
                worker_id: w1.clone(),
                session_fingerprint: Some("s1".to_owned()),
                force: false,
                steal_remote: false,
                worktree: None,
                detail: Default::default(),
            }),
        )
        .unwrap();
    let StateCommandOutput::Submit(pre_sub) = harness
        .execute(
            &cfg,
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
    let sync_stale = harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Sync(SyncReq {
                run: Some(rid.clone()),
                rebind_profile: false,
            }),
        )
        .unwrap_err();
    assert_eq!(sync_stale.code, ErrorCode::PreTaskArtifactStale);

    // 4. `spec_planning` `--goal` run without explicit scope derives `docs/**` + specs + profiles scope
    //    (excluding `src/**`) and injects `docs.workspace_config` on `synthesize` claim.
    harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Clear(acdd_state::usecases::ClearReq {
                run: Some(rid),
                ..Default::default()
            }),
        )
        .unwrap();
    ws.write(
        "acdd.yaml",
        "docs:\n  - docs\n  - ARCHITECTURE.md\nmodules:\n  code: true\n  docs: true\n  state: true\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    );
    ws.write("ARCHITECTURE.md", "# Architecture\n");
    ws.write("ROADMAP.md", "# Roadmap\n");
    let StateCommandOutput::Start(sp_started) = harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Start(StartReq {
                goal: Some("Plan new milestone".to_owned()),
                profile: Some("spec_planning".to_owned()),
                scope: Vec::new(),
                ..StartReq::default()
            }),
        )
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let StateCommandOutput::Claim(sp_claim) = harness
        .execute(
            &cfg,
            &providers,
            StateCommand::Claim(ClaimReq {
                run: Some(sp_started.run.run_id.to_string()),
                node: Some("synthesize".to_owned()),
                worker_id: w1,
                session_fingerprint: Some("sp-1".to_owned()),
                force: false,
                steal_remote: false,
                worktree: None,
                detail: Default::default(),
            }),
        )
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let scope_vals: Vec<&str> = sp_claim.contract["scope"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(scope_vals.contains(&"docs/**"), "{scope_vals:?}");
    assert!(scope_vals.contains(&"docs/milestones/**"), "{scope_vals:?}");
    assert!(scope_vals.contains(&"ARCHITECTURE.md"), "{scope_vals:?}");
    assert!(scope_vals.contains(&"ROADMAP.md"), "{scope_vals:?}");
    assert!(scope_vals.contains(&".acdd/profiles/**"), "{scope_vals:?}");
    assert!(
        !scope_vals.iter().any(|p| p.starts_with("src")),
        "{scope_vals:?}"
    );
    assert!(
        sp_claim
            .context_lenses
            .contains_key("docs.workspace_config"),
        "expected docs.workspace_config lens: {:?}",
        sp_claim.context_lenses.keys().collect::<Vec<_>>()
    );
}
