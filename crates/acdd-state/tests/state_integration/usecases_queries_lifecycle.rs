use super::common::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_core::ports::DomainToolOutput;
use acdd_state::harness::RunHarness;
use acdd_state::store::traits::{StateBackend, StateReader};
use acdd_state::types::{
    ArtifactKind, BlackboardTopic, ClaimOutcome, GateArtifactRecord, GateClaimRecord,
    IsolationMode, NodeStatus, NodeStatusPatch, NodeVersionExpectation, StoragePreconditions,
    SubtaskStatus, TerminalStatus, TraceFinishInput, TraceStartInput, TraceStatus, TransitionPlan,
};
use acdd_state::usecases::dto::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, ClaimReq, ClearReq, ContextReq,
    DraftReq, ExtendScopeReq, ListReq, NextReq, ObservabilityReq, ReopenReq, ShowReq, StartReq,
    StatusReq, SubtaskReq, SyncReq,
};
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateQuery, StateQueryEngine, StateQueryOutput,
    execute_on, queries,
};
use serde_json::json;
use std::fs;

#[cfg(unix)]
#[test]
fn seam_snapshot_lifecycle_and_confinement_matrix() {
    use acdd_state::usecases::{SnapshotAction::*, SnapshotReq};
    use std::os::unix::fs::{PermissionsExt, symlink};

    let tmp = make_temp_dir("snapshots");
    let repo = tmp.path();
    let head = init_git_repo(repo, "main");
    let task = repo.join(".worktrees/checkpoint");
    git_ok(
        repo,
        &["worktree", "add", "--detach", task.to_str().unwrap()],
    );
    let index = git_ok(
        &task,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    );
    let profile = write_profile(
        repo,
        "checkpoints.yaml",
        r#"
version: 2
inherit: none
git: { isolation: worktree, worktrees: manual }
gates:
  - tasks:
      gates:
        - id: checkpoint
          proof: none
          save: snapshot
          pass: deliver
        - id: deliver
          proof: none
          need: [checkpoint]
          receipt: true
          save: none
  - id: handoff
    proof: none
    save: { post: none, prune: true }
"#,
    );
    let spec = "docs/milestones/AC-SNAPSHOT.md";
    fs::create_dir_all(repo.join("docs/milestones")).unwrap();
    fs::write(
        repo.join(spec),
        r#"---
id: AC-SNAPSHOT
title: Checkpoints
status: planned
---
### task: t1
```yaml
task_ref: t1
scope: [src/*.txt, src/z/f?le]
```
### task: t2
```yaml
task_ref: t2
scope: [src/lib.rs]
```
"#,
    )
    .unwrap();
    let cfg = base_config(repo);
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg.clone(), provider.clone()).unwrap();
    let start = || {
        start_run(
            &engine,
            StartReq {
                spec_path: Some(spec.to_owned()),
                profile: Some(profile.clone()),
                ..StartReq::default()
            },
        )
        .unwrap()
        .run
        .run_id
    };
    let rid = start();
    let pass = |node, override_worktree| {
        let claim = claim_node(
            &engine,
            rid.as_str(),
            node,
            "worker",
            None,
            false,
            false,
            override_worktree,
        )
        .unwrap();
        submit_req(
            &engine,
            pass_req(
                rid.as_str(),
                node,
                "worker",
                Some(claim.claim_revision),
                false,
            ),
        )
        .unwrap_or_else(|err| panic!("{node}: {err}"));
        claim.claim_revision
    };
    let snapshot = |action, task_ref: Option<&str>, all, revision| {
        engine.execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action,
            task_ref: task_ref.map(|t| TaskRef::new(t).unwrap()),
            all,
            revision,
            name: None,
            worktree: None,
            older_than: None,
        }))
    };
    let prefix = format!("refs/acdd/snapshots/{rid}");
    let refs = || git_ok(repo, &["for-each-ref", "--format=%(refname)", &prefix]);
    fs::create_dir_all(task.join("src/z")).unwrap();
    fs::write(task.join("src/a.txt"), "saved").unwrap();
    fs::write(task.join("src/z/file"), "saved").unwrap();
    let index_before = fs::read(&index).unwrap();
    let rev = pass("t1.checkpoint", Some(task.clone()));
    let sibling = repo.join(".worktrees/sibling");
    git_ok(
        repo,
        &["worktree", "add", "--detach", sibling.to_str().unwrap()],
    );
    let mismatch = engine
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: Save,
            task_ref: Some(TaskRef::new("t1").unwrap()),
            all: false,
            revision: None,
            name: None,
            worktree: Some(sibling),
            older_than: None,
        }))
        .unwrap_err();
    assert_eq!(mismatch.code, ErrorCode::ExternalGitDrift);
    assert_eq!(
        mismatch.message,
        "explicit worktree does not match assigned task worktree"
    );
    let auto_ref = format!("{prefix}/t1/r{rev}");
    assert_eq!(refs(), auto_ref);
    assert!(git_ok(repo, &["show", "-s", "--format=%P", &auto_ref]).is_empty());
    fs::write(task.join("src/a.txt"), "manual").unwrap();
    snapshot(Save, Some("t1"), false, Some(10)).unwrap();

    let outside = repo.join("src/z");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("file"), "outside").unwrap();
    fs::write(repo.join("src/a.txt"), "primary").unwrap();
    fs::write(task.join("README.md"), "task sentinel").unwrap();
    fs::create_dir_all(task.join(".acdd")).unwrap();
    fs::write(task.join(".acdd/sentinel"), "runtime").unwrap();

    let nested = task.join("src/z");
    let leaf = nested.join("file");
    let cases = [
        "symlink ancestor",
        "symlink leaf",
        "file ancestor",
        "directory leaf",
        "nested repository",
        "checkout failure",
        "deletion failure",
        "regular restore",
    ];
    for case in cases {
        fs::remove_dir_all(&nested).unwrap();
        fs::write(task.join("src/a.txt"), "dirty").unwrap();
        fs::write(task.join("src/extra.txt"), "delete me").unwrap();
        match case {
            "symlink ancestor" => symlink(&outside, &nested).unwrap(),
            "symlink leaf" => {
                fs::create_dir(&nested).unwrap();
                symlink(outside.join("file"), &leaf).unwrap();
            }
            "file ancestor" => fs::write(&nested, "collision").unwrap(),
            "directory leaf" => {
                fs::create_dir_all(&leaf).unwrap();
                fs::write(leaf.join("unrelated"), "keep").unwrap();
            }
            "nested repository" => {
                fs::create_dir(&nested).unwrap();
                git_ok(&nested, &["init"]);
            }
            "checkout failure" | "deletion failure" => {
                fs::create_dir(&nested).unwrap();
                let contents = if case == "checkout failure" {
                    "dirty"
                } else {
                    "saved"
                };
                fs::write(&leaf, contents).unwrap();
                fs::write(nested.join("fale"), "keep").unwrap();
                fs::set_permissions(&nested, fs::Permissions::from_mode(0o555)).unwrap();
            }
            _ => {
                fs::create_dir(&nested).unwrap();
                fs::write(&leaf, "dirty").unwrap();
            }
        }
        let restored = snapshot(Restore, Some("t1"), false, None);
        if matches!(case, "checkout failure" | "deletion failure") {
            fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
            let error = restored.as_ref().unwrap_err();
            assert!(!error.message.contains("Rollback"), "{error}");
            let expected = if case == "checkout failure" {
                "checkout-index"
            } else {
                "fale"
            };
            assert!(error.message.contains(expected), "{error}");
            assert_eq!(fs::read_to_string(nested.join("fale")).unwrap(), "keep");
        }
        if case == "regular restore" {
            let StateCommandOutput::Snapshot(out) = restored.unwrap() else {
                panic!("expected snapshot")
            };
            assert_eq!(out.refs, [format!("{prefix}/t1/r10")]);
            assert_eq!(
                fs::read_to_string(task.join("src/a.txt")).unwrap(),
                "manual"
            );
            assert_eq!(fs::read_to_string(&leaf).unwrap(), "saved");
            assert!(!task.join("src/extra.txt").exists());
        } else {
            assert_eq!(
                restored.unwrap_err().code,
                ErrorCode::ExternalGitDrift,
                "{case}"
            );
            assert_eq!(
                fs::read_to_string(task.join("src/a.txt")).unwrap(),
                "dirty",
                "{case}"
            );
            assert!(task.join("src/extra.txt").exists(), "{case}");
            if case == "nested repository" {
                assert!(!leaf.exists() && nested.join(".git").is_dir());
            }
        }
        assert_eq!(fs::read_to_string(outside.join("file")).unwrap(), "outside");
        assert_eq!(
            fs::read_to_string(repo.join("src/a.txt")).unwrap(),
            "primary"
        );
        assert_eq!(
            fs::read_to_string(task.join("README.md")).unwrap(),
            "task sentinel"
        );
        assert_eq!(
            fs::read_to_string(task.join(".acdd/sentinel")).unwrap(),
            "runtime"
        );
        match case {
            "symlink ancestor" | "file ancestor" => {
                fs::remove_file(&nested).unwrap();
                fs::create_dir(&nested).unwrap();
            }
            "directory leaf" => {
                assert_eq!(fs::read_to_string(leaf.join("unrelated")).unwrap(), "keep")
            }
            _ => {}
        }
    }
    snapshot(Restore, Some("t1"), false, Some(rev)).unwrap();
    assert_eq!(fs::read_to_string(task.join("src/a.txt")).unwrap(), "saved");
    assert_eq!(fs::read(&index).unwrap(), index_before);
    assert_eq!(git_ok(&task, &["rev-parse", "HEAD"]), head.as_str());
    fs::write(
        task.join("README.md"),
        fs::read(repo.join("README.md")).unwrap(),
    )
    .unwrap();

    fs::remove_file(repo.join("src/a.txt")).unwrap();
    fs::remove_dir_all(outside).unwrap();
    let second_rev = pass("t2.checkpoint", Some(repo.to_path_buf()));
    let second_ref = format!("{prefix}/t2/r{second_rev}");
    snapshot(Clear, Some("t1"), false, None).unwrap();
    assert_eq!(refs(), second_ref);
    let adhoc_ref = "refs/acdd/snapshots/_adhoc/unrelated/r1";
    git_ok(repo, &["update-ref", adhoc_ref, head.as_str()]);
    let StateCommandOutput::Snapshot(pruned) = engine
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: Clear,
            task_ref: None,
            all: false,
            revision: Some(second_rev),
            name: None,
            worktree: None,
            older_than: None,
        }))
        .unwrap()
    else {
        panic!("expected snapshot prune")
    };
    assert_eq!(pruned.refs, [second_ref]);
    assert!(refs().is_empty() && !git_ok(repo, &["for-each-ref", adhoc_ref]).is_empty());
    snapshot(Clear, None, true, None).unwrap();
    assert!(refs().is_empty());
    snapshot(Save, Some("t1"), false, Some(rev)).unwrap();
    pass("t1.deliver", Some(task.clone()));
    assert!(refs().is_empty());
    pass("t2.deliver", Some(repo.to_path_buf()));

    snapshot(Save, Some("t1"), false, Some(rev)).unwrap();
    pass("handoff", None);
    let tomb = engine.backend().load_tombstone(&rid).unwrap().unwrap();
    assert!(tomb.cleanup_manifest.is_empty() && !tomb.cleanup_pending && refs().is_empty());

    let commit_profile = write_profile(
        repo,
        "checkpoint-commit.yaml",
        &fs::read_to_string(repo.join(&profile))
            .unwrap()
            .replace("          save: none", "          save: commit"),
    );
    let rid = start_run(
        &engine,
        StartReq {
            spec_path: Some(spec.to_owned()),
            profile: Some(commit_profile),
            ..StartReq::default()
        },
    )
    .unwrap()
    .run
    .run_id;
    let foreign = repo.join(".worktrees/foreign");
    init_git_repo(&foreign, "main");
    fs::write(foreign.join("src/a.txt"), "candidate").unwrap();
    let claim = claim_node(
        &engine,
        rid.as_str(),
        "t1.checkpoint",
        "worker",
        None,
        false,
        false,
        Some(foreign.clone()),
    )
    .unwrap();
    for action in [Save, Restore] {
        let error = engine
            .execute(StateCommand::Snapshot(SnapshotReq {
                run: Some(rid.to_string()),
                action,
                task_ref: Some(TaskRef::new("t1").unwrap()),
                all: false,
                revision: None,
                name: None,
                worktree: None,
                older_than: None,
            }))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ExternalGitDrift);
        assert!(error.message.contains("outside the owning repositories"));
    }
    let rejected = submit_req(
        &engine,
        pass_req(
            rid.as_str(),
            "t1.checkpoint",
            "worker",
            Some(claim.claim_revision),
            false,
        ),
    )
    .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::ExternalGitDrift);
    assert!(git_ok(&foreign, &["for-each-ref", "refs/acdd/snapshots"]).is_empty());
    let mut linked_config = cfg.clone();
    let non_git = make_temp_dir("non-git-linked");
    let delivery_repo = repo.join(".worktrees/delivery");
    let delivery_head = init_git_repo(&delivery_repo, "main");
    git_ok(&delivery_repo, &["checkout", "-b", "delivery"]);
    fs::write(delivery_repo.join("src/a.txt"), "candidate").unwrap();
    linked_config.linked_workspaces.extend([
        ("checkpoint".to_owned(), foreign.clone()),
        ("alias".to_owned(), foreign.clone()),
        ("delivery".to_owned(), delivery_repo.clone()),
        ("non-git".to_owned(), non_git.path().to_path_buf()),
    ]);
    let linked = StateEngine::open(linked_config, provider.clone()).unwrap();
    submit_req(
        &linked,
        pass_req(
            rid.as_str(),
            "t1.checkpoint",
            "worker",
            Some(claim.claim_revision),
            false,
        ),
    )
    .unwrap();
    let linked_ref = format!("refs/acdd/snapshots/{rid}/t1/r{}", claim.claim_revision);
    assert_eq!(
        git_ok(
            &foreign,
            &["for-each-ref", "--format=%(refname)", &linked_ref]
        ),
        linked_ref
    );
    let checkpoint = git_ok(&foreign, &["rev-parse", &linked_ref]);
    let deliver = claim_node(
        &linked,
        rid.as_str(),
        "t1.deliver",
        "worker",
        None,
        false,
        false,
        Some(delivery_repo.clone()),
    )
    .unwrap();
    assert_eq!(
        deliver.worktree,
        acdd_core::fs::canonicalize_macos_var(&delivery_repo)
    );
    git_ok(
        &delivery_repo,
        &["update-ref", &linked_ref, delivery_head.as_str()],
    );
    linked
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: Clear,
            task_ref: Some(TaskRef::new("t1").unwrap()),
            all: false,
            revision: None,
            name: None,
            worktree: None,
            older_than: None,
        }))
        .unwrap();
    assert!(git_ok(&foreign, &["for-each-ref", &linked_ref]).is_empty());
    assert!(git_ok(&delivery_repo, &["for-each-ref", &linked_ref]).is_empty());
    git_ok(&foreign, &["update-ref", &linked_ref, &checkpoint]);
    let linked_lock = foreign.join(".git/acdd/repo.lock");
    let held = acdd_core::lock::acquire_exclusive(&linked_lock).unwrap();
    assert_eq!(linked.execute(StateCommand::Snapshot(SnapshotReq { run: Some(rid.to_string()), action: List, task_ref: Some(TaskRef::new("t1").unwrap()), all: false, revision: None, name: None, worktree: None, older_than: None })).unwrap_err().code, ErrorCode::BackendConflict);
    let all = linked
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: Clear,
            task_ref: None,
            all: true,
            revision: None,
            name: None,
            worktree: None,
            older_than: None,
        }))
        .unwrap_err();
    assert_eq!(all.code, ErrorCode::BackendConflict);
    assert_eq!(git_ok(&foreign, &["rev-parse", &linked_ref]), checkpoint);
    let blocked = submit_req(
        &linked,
        pass_req(
            rid.as_str(),
            "t1.deliver",
            "worker",
            Some(deliver.claim_revision),
            false,
        ),
    )
    .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::BackendConflict);
    assert_eq!(
        git_ok(&delivery_repo, &["rev-parse", "HEAD"]),
        delivery_head.as_str()
    );
    assert_eq!(git_ok(&foreign, &["rev-parse", &linked_ref]), checkpoint);
    drop(held);
    let ref_lock = foreign.join(format!(".git/{linked_ref}.lock"));
    fs::write(&ref_lock, "").unwrap();
    let blocked = submit_req(
        &linked,
        pass_req(
            rid.as_str(),
            "t1.deliver",
            "worker",
            Some(deliver.claim_revision),
            false,
        ),
    )
    .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::ExternalGitDrift);
    let committed_head = git_ok(&delivery_repo, &["rev-parse", "HEAD"]);
    assert_ne!(committed_head, delivery_head.as_str());
    assert_eq!(
        git_ok(&delivery_repo, &["show", "HEAD:src/a.txt"]),
        "candidate"
    );
    assert_eq!(git_ok(&foreign, &["rev-parse", &linked_ref]), checkpoint);
    fs::remove_file(ref_lock).unwrap();
    let delivered = submit_req(
        &linked,
        pass_req(
            rid.as_str(),
            "t1.deliver",
            "worker",
            Some(deliver.claim_revision),
            false,
        ),
    )
    .unwrap();
    assert!(delivered.receipt.is_some());
    assert_eq!(
        git_ok(&delivery_repo, &["rev-parse", "HEAD"]),
        committed_head
    );
    assert!(git_ok(&foreign, &["for-each-ref", &linked_ref]).is_empty());

    let linked_worktree = repo.join(".worktrees/transient");
    git_ok(
        &foreign,
        &[
            "worktree",
            "add",
            "--detach",
            linked_worktree.to_str().unwrap(),
        ],
    );
    let transient = claim_node(
        &linked,
        rid.as_str(),
        "t2.checkpoint",
        "worker",
        None,
        false,
        false,
        Some(linked_worktree.clone()),
    )
    .unwrap();
    submit_req(
        &linked,
        pass_req(
            rid.as_str(),
            "t2.checkpoint",
            "worker",
            Some(transient.claim_revision),
            false,
        ),
    )
    .unwrap();
    let transient_ref = format!("refs/acdd/snapshots/{rid}/t2/r{}", transient.claim_revision);
    assert!(!git_ok(&foreign, &["for-each-ref", &transient_ref]).is_empty());
    let StateCommandOutput::Snapshot(listed_linked) = linked
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: List,
            task_ref: Some(TaskRef::new("t2").unwrap()),
            all: false,
            revision: None,
            name: None,
            worktree: None,
            older_than: None,
        }))
        .unwrap()
    else {
        panic!("expected snapshot list")
    };
    assert_eq!(listed_linked.refs, vec![transient_ref.clone()]);
    git_ok(
        &foreign,
        &["worktree", "remove", linked_worktree.to_str().unwrap()],
    );
    linked
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.to_string()),
            action: Clear,
            task_ref: Some(TaskRef::new("t2").unwrap()),
            all: false,
            revision: None,
            name: None,
            worktree: None,
            older_than: None,
        }))
        .unwrap();
    assert!(git_ok(&foreign, &["for-each-ref", &transient_ref]).is_empty());

    git_ok(&foreign, &["update-ref", &linked_ref, &checkpoint]);
    let held = acdd_core::lock::acquire_exclusive(&linked_lock).unwrap();
    let reference = format!("refs/acdd/snapshots/{rid}/t1/r1");
    git_ok(repo, &["update-ref", &reference, head.as_str()]);
    let lock = repo.join(format!(".git/{reference}.lock"));
    fs::write(&lock, "").unwrap();
    let cleared = linked
        .execute(StateCommand::Clear(ClearReq {
            run: Some(rid.to_string()),
            reason: None,
            force: false,
        }))
        .unwrap();
    let StateCommandOutput::Clear(cleared) = cleared else {
        panic!("expected clear")
    };
    assert!(cleared.tombstone.cleanup_pending);
    assert_eq!(git_ok(repo, &["rev-parse", &reference]), head.as_str());
    fs::remove_file(lock).unwrap();
    assert!(
        start_run(
        &linked,
        StartReq {
            spec_path: Some("docs/milestones/missing.md".to_owned()),
            ..StartReq::default()
        }
    )
        .is_err()
    );
    assert!(
        linked
            .backend()
            .load_tombstone(&rid)
            .unwrap()
            .unwrap()
            .cleanup_pending
    );
    assert_eq!(git_ok(&foreign, &["rev-parse", &linked_ref]), checkpoint);
    assert_eq!(git_ok(repo, &["rev-parse", &reference]), head.as_str());
    drop(held);
    let next = start_run(
        &linked,
        StartReq {
            spec_path: Some(spec.to_owned()),
            profile: Some(profile.clone()),
            ..StartReq::default()
        },
    )
    .unwrap()
    .run
    .run_id;
    assert!(
        !engine
            .backend()
            .load_tombstone(&rid)
            .unwrap()
            .unwrap()
            .cleanup_pending
    );
    assert!(git_ok(&foreign, &["for-each-ref", &linked_ref]).is_empty());
    assert!(git_ok(repo, &["for-each-ref", "--format=%(refname)", &reference]).is_empty());
    drop(linked);
    let archive_dir = repo.join("docs/milestones/archive");
    fs::create_dir_all(&archive_dir).unwrap();
    let completed = fs::read_to_string(repo.join(spec))
        .unwrap()
        .replace("status: planned", "status: completed");
    fs::write(archive_dir.join("AC-SNAPSHOT.md"), completed).unwrap();
    let reconcile_ref = format!("refs/acdd/snapshots/{next}/t1/r1");
    git_ok(repo, &["update-ref", &reconcile_ref, head.as_str()]);
    git_ok(repo, &["add", "docs/milestones/archive"]);
    git_ok(repo, &["commit", "-m", "Archive before state recovery"]);
    let planned = fs::read(repo.join(spec)).unwrap();
    fs::remove_file(repo.join(spec)).unwrap();
    drop(engine);
    fs::remove_file(cfg.resolved_state_db_path().0).unwrap();
    let recovered = StateEngine::open(cfg.clone(), provider).unwrap();
    let tomb = recovered.backend().load_tombstone(&next).unwrap().unwrap();
    assert!(tomb.cleanup_manifest.is_empty() && tomb.cleanup_pending);
    fs::write(repo.join(spec), planned).unwrap();
    let restarted = start_run(
        &recovered,
        StartReq {
            spec_path: Some(spec.to_owned()),
            profile: Some(profile),
            ..StartReq::default()
        },
    )
    .unwrap();
    assert!(
        !recovered
            .backend()
            .load_tombstone(&next)
            .unwrap()
            .unwrap()
            .cleanup_pending
    );
    assert!(git_ok(repo, &["for-each-ref", &reconcile_ref]).is_empty());
    clear_run(&recovered, restarted.run.run_id.as_str());

    let bare = make_temp_dir("adhoc-snap");
    let bare_repo = bare.path();
    init_git_repo(bare_repo, "main");
    let symlinked = make_temp_dir("adhoc-snapshot-git-symlink");
    let symlinked_repo = symlinked.path();
    init_git_repo(symlinked_repo, "main");
    fs::rename(
        symlinked_repo.join(".git"),
        symlinked_repo.join(".git.real"),
    )
    .unwrap();
    symlink(".git.real", symlinked_repo.join(".git")).unwrap();
    let symlinked_error = acdd_state::usecases::execute_snapshot_readonly(
        &base_config(bare_repo),
        SnapshotReq {
            run: None,
            action: Save,
            task_ref: None,
            all: false,
            revision: None,
            name: Some("note".into()),
            worktree: Some(symlinked_repo.to_path_buf()),
            older_than: None,
        },
    )
    .unwrap_err();
    assert_eq!(symlinked_error.code, ErrorCode::ExternalGitDrift);
    fs::create_dir_all(bare_repo.join("src")).unwrap();
    fs::write(bare_repo.join("src/a.txt"), "one").unwrap();
    let mut bare_cfg = base_config(bare_repo);
    bare_cfg.state_db_override = Some(bare_repo.join("runs.sqlite"));
    {
        let mut conn = rusqlite::Connection::open(bare_cfg.resolved_state_db_path().0).unwrap();
        acdd_state::store::sqlite::migrations::ensure_schema(&mut conn).unwrap();
        for id in ["amb-a", "amb-b"] {
            conn.execute(
                "INSERT INTO runs (run_id, run_name, collection, workspace, profile_id, profile_json, spec_path, spec_id, spec_digest, is_ephemeral, isolation, target_base, target_base_commit, branch, worktree, trace_id, status, cancellation_reason, root_cycle_count, generation, created_at, completed_at)
                 VALUES (?1, ?1, 'milestones', 'w', 'p', '{}', 's', 's', 'd', 0, 'in_place', NULL, NULL, NULL, NULL, 't', 'active', NULL, 0, 1, 't', NULL)",
                [id],
            ).unwrap();
        }
    }
    {
        let amb = |action, revision| {
            acdd_state::usecases::execute_snapshot_readonly(
                &bare_cfg,
                SnapshotReq {
                    run: None,
                    action,
                    task_ref: None,
                    all: false,
                    revision,
                    name: Some("amb".into()),
                    worktree: Some(bare_repo.to_path_buf()),
                    older_than: None,
                },
            )
        };
        for action in [Save, List, Restore, Clear] {
            let out = amb(action, (action == Restore).then_some(1)).unwrap();
            let StateCommandOutput::Snapshot(out) = out else {
                panic!("amb")
            };
            assert!(
                out.refs.iter().any(|r| r.contains("_adhoc/amb/")),
                "{action:?} {out:?}"
            );
        }
    }
    let snap = |action, name: Option<&str>, revision, all, older_than: Option<&str>| {
        acdd_state::usecases::execute_snapshot_readonly(
            &bare_cfg,
            SnapshotReq {
                run: None,
                action,
                task_ref: None,
                all,
                revision,
                name: name.map(str::to_owned),
                worktree: Some(bare_repo.to_path_buf()),
                older_than: older_than.map(str::to_owned),
            },
        )
    };
    let saved = snap(Save, Some("note"), Some(1), false, None).unwrap();
    let StateCommandOutput::Snapshot(saved) = saved else {
        panic!("snapshot")
    };
    assert_eq!(
        saved.refs,
        ["refs/acdd/snapshots/_adhoc/note/r1".to_owned()]
    );
    assert!(!bare_repo.join(".acdd").exists());
    fs::write(bare_repo.join("src/a.txt"), "two").unwrap();
    let tree = git_ok(bare_repo, &["write-tree"]);
    let oid = std::process::Command::new("git")
        .current_dir(bare_repo)
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
    git_ok(
        bare_repo,
        &[
            "update-ref",
            "refs/acdd/snapshots/_adhoc/note/r2",
            String::from_utf8(oid.stdout).unwrap().trim(),
        ],
    );
    let listed = snap(List, Some("note"), None, false, None).unwrap();
    let StateCommandOutput::Snapshot(listed) = listed else {
        panic!("list")
    };
    assert!(
        listed.refs.iter().any(|r| r.ends_with("/r2"))
            && listed.refs.iter().any(|r| r.ends_with("/r1"))
    );
    let listed_rev1 = snap(List, Some("note"), Some(1), false, None).unwrap();
    let StateCommandOutput::Snapshot(listed_rev1) = listed_rev1 else {
        panic!("list revision")
    };
    assert_eq!(
        listed_rev1.refs,
        ["refs/acdd/snapshots/_adhoc/note/r1".to_owned()]
    );
    fs::write(bare_repo.join("src/a.txt"), "dirty").unwrap();
    snap(Restore, Some("note"), Some(1), false, None).unwrap();
    assert_eq!(
        fs::read_to_string(bare_repo.join("src/a.txt")).unwrap(),
        "one"
    );
    let checkpoint = git_ok(
        bare_repo,
        &["rev-parse", "refs/acdd/snapshots/_adhoc/note/r1"],
    );
    let extra = "refs/acdd/snapshots/_adhoc/note/r1_extra";
    git_ok(bare_repo, &["update-ref", extra, &checkpoint]);
    snap(Clear, Some("note"), Some(1), false, None).unwrap();
    assert!(
        git_ok(
        bare_repo,
        &["for-each-ref", "refs/acdd/snapshots/_adhoc/note/r1"]
    )
        .is_empty()
    );
    assert!(!git_ok(bare_repo, &["for-each-ref", extra]).is_empty());
    let child = "refs/acdd/snapshots/_adhoc/note/r1/child";
    git_ok(bare_repo, &["update-ref", child, &checkpoint]);
    snap(Clear, Some("note"), Some(1), false, None).unwrap();
    assert!(!git_ok(bare_repo, &["for-each-ref", child]).is_empty());
    snap(Save, Some("note"), Some(3), false, None).unwrap();
    snap(Clear, None, None, false, Some("1d")).unwrap();
    let left = git_ok(bare_repo, &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(
        left.contains("/note/r3") && !left.contains("/note/r2"),
        "{left}"
    );
    snap(Clear, None, None, true, None).unwrap();
    let refs = git_ok(bare_repo, &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(refs.contains(extra) && refs.contains(child), "{refs}");
    for malformed in ["refs/acdd/snapshots/_adhoc/note/pre", "refs/acdd/snapshots/_adhoc/note+other/r1"] {
        git_ok(bare_repo, &["update-ref", malformed, &checkpoint]);
    }
    snap(Clear, None, None, true, None).unwrap();
    let refs = git_ok(bare_repo, &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(refs.contains("/note/pre") && refs.contains("/note+other/r1"), "{refs}");
    assert!(snap(Save, Some("_adhoc"), Some(1), false, None).is_err());
    assert!(snap(Clear, Some(""), Some(1), false, None).is_err());
    assert_eq!(
        snap(Save, Some("note.lock"), Some(1), false, None)
            .unwrap_err()
            .code,
        ErrorCode::ProfileSchemaInvalid
    );
    let auto = snap(Save, Some("auto"), None, false, None).unwrap();
    let StateCommandOutput::Snapshot(auto) = auto else {
        panic!("auto")
    };
    assert_eq!(auto.refs, ["refs/acdd/snapshots/_adhoc/auto/r1".to_owned()]);
    let auto = snap(Save, Some("auto"), None, false, None).unwrap();
    let StateCommandOutput::Snapshot(auto) = auto else {
        panic!("auto")
    };
    assert_eq!(auto.refs, ["refs/acdd/snapshots/_adhoc/auto/r2".to_owned()]);
    let target_dir = make_temp_dir("adhoc-snap-target-lock");
    let target_repo = target_dir.path();
    init_git_repo(target_repo, "main");
    let held_primary = acdd_core::lock::acquire_exclusive(&bare_repo.join(".git/acdd/repo.lock")).unwrap();
    let target_snap = |action| acdd_state::usecases::execute_snapshot_readonly(&bare_cfg, SnapshotReq { run: None, action, task_ref: None, all: false, revision: None, name: Some("target".into()), worktree: Some(target_repo.to_path_buf()), older_than: None });
    target_snap(Save).unwrap();
    target_snap(List).unwrap();
    target_snap(Restore).unwrap();
    target_snap(Clear).unwrap();
    drop(held_primary);
    let sha256 = make_temp_dir("adhoc-snap-sha256");
    let sha256_repo = sha256.path();
    git_ok(
        sha256_repo,
        &["init", "--object-format=sha256", "-b", "main"],
    );
    git_ok(
        sha256_repo,
        &["config", "user.email", "test@contextunity.local"],
    );
    git_ok(sha256_repo, &["config", "user.name", "ACDD Test"]);
    fs::write(sha256_repo.join("file"), "sha256").unwrap();
    git_ok(sha256_repo, &["add", "file"]);
    git_ok(sha256_repo, &["commit", "-m", "initial"]);
    let sha256_cfg = base_config(sha256_repo);
    let sha256_saved = acdd_state::usecases::execute_snapshot_readonly(
        &sha256_cfg,
        SnapshotReq {
            run: None,
            action: Save,
            task_ref: None,
            all: false,
            revision: None,
            name: Some("sha256".into()),
            worktree: Some(sha256_repo.to_path_buf()),
            older_than: None,
        },
    )
    .unwrap();
    let StateCommandOutput::Snapshot(sha256_saved) = sha256_saved else {
        panic!("sha256 snapshot")
    };
    assert_eq!(
        sha256_saved.refs,
        ["refs/acdd/snapshots/_adhoc/sha256/r1".to_owned()]
    );
    assert_eq!(
        git_ok(sha256_repo, &["rev-parse", &sha256_saved.refs[0]]).len(),
        64
    );
    snap(Save, Some("other"), Some(1), false, None).unwrap();
    let pruned = snap(Clear, None, Some(1), false, None).unwrap();
    let StateCommandOutput::Snapshot(pruned) = pruned else {
        panic!("prune")
    };
    assert!(
        pruned.refs.iter().any(|r| r.ends_with("/auto/r1"))
            && pruned.refs.iter().any(|r| r.ends_with("/other/r1"))
    );
    assert!(
        git_ok(
        bare_repo,
        &["for-each-ref", "refs/acdd/snapshots/_adhoc/auto/r2"]
    )
        .contains("/r2")
    );
    let refs = git_ok(bare_repo, &["for-each-ref", "refs/acdd/snapshots/_adhoc"]);
    assert!(
        !refs.contains("/auto/r1") && !refs.contains("/other/r1"),
        "{refs}"
    );
    for raw in ["1д", "9999999999w"] {
        assert_eq!(
            snap(Clear, None, None, false, Some(raw)).unwrap_err().code,
            ErrorCode::ProfileSchemaInvalid,
            "{raw}"
        );
    }
}

#[test]
fn seam_readonly_queries_matrix_and_zero_db_creation() {
    let empty_tmp = make_temp_dir("empty-ro");
    let cfg = base_config(empty_tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let ro = StateQueryEngine::open_read_only(cfg, provider.clone()).unwrap();

    let empty_cases = [
        (StateQuery::Status(StatusReq::default()), false),
        (StateQuery::List(ListReq::default()), false),
        (
            StateQuery::Show(ShowReq {
                run: None,
                node: None,
            }),
            true,
        ),
        (StateQuery::Context(ContextReq::default()), true),
        (StateQuery::Next(NextReq::default()), false),
        (
            StateQuery::BlackboardRead(BlackboardReadReq::default()),
            true,
        ),
        (
            StateQuery::Observability(ObservabilityReq {
                include_prometheus: true,
            }),
            false,
        ),
    ];
    for (idx, (query, expect_ambiguous)) in empty_cases.into_iter().enumerate() {
        let res = ro.query(query);
        if expect_ambiguous {
            assert_eq!(
                res.unwrap_err().code,
                ErrorCode::RunSelectorAmbiguous,
                "query #{idx}"
            );
        } else {
            assert!(res.is_ok(), "query #{idx} failed: {res:?}");
        }
    }
    assert!(!empty_tmp.path().join(".acdd/state.sqlite").exists());
    assert!(!empty_tmp.path().join(".acdd/state.bak.sqlite").exists());

    let harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
    let ws = harness.workspace_root().unwrap().to_path_buf();
    let mut cfg_pop = base_config(&ws);
    cfg_pop.state_db_override = Some(harness.db_path().unwrap().to_path_buf());
    harness
        .seed_run(&build_rich_graph(&ws, "run-ro-001", IsolationMode::InPlace))
        .unwrap();

    let ro_pop = StateQueryEngine::open_read_only(cfg_pop, provider).unwrap();
    match ro_pop
        .query(StateQuery::Status(StatusReq::default()))
        .unwrap()
    {
        StateQueryOutput::Status(s) => {
            assert_eq!(s.run.unwrap().run_id.as_str(), "run-ro-001");
            assert!(s.ready_nodes.contains(&"t1.build".to_owned()));
        }
        other => panic!("unexpected Status output: {other:?}"),
    }
    match ro_pop.query(StateQuery::Next(NextReq::default())).unwrap() {
        StateQueryOutput::Next(n) => {
            assert_eq!(n.ready_cursors, vec!["t1.build".to_owned()]);
            assert_eq!(n.waves, vec![vec!["t1".to_owned()], vec!["t2".to_owned()]]);
        }
        other => panic!("unexpected Next output: {other:?}"),
    }
    match ro_pop
        .query(StateQuery::Show(ShowReq {
            run: Some("run-ro-001".to_owned()),
            node: Some("t1.build".to_owned()),
        }))
        .unwrap()
    {
        StateQueryOutput::Show(s) => {
            assert_eq!(s.run.run_id.as_str(), "run-ro-001");
            assert_eq!(s.node.unwrap().node_id, "task:t1:build");
            assert!(s.context_preview.is_some());
        }
        other => panic!("unexpected Show output: {other:?}"),
    }
    match ro_pop
        .query(StateQuery::Observability(ObservabilityReq {
            include_prometheus: true,
        }))
        .unwrap()
    {
        StateQueryOutput::Observability(o) => {
            assert_eq!(o.snapshot.active_runs, 1);
            assert!(
                o.prometheus_text
                .as_deref()
                    .is_some_and(|t| t.contains("acdd_runs_active 1"))
            );
        }
        other => panic!("unexpected Observability output: {other:?}"),
    }
    let graph_before = harness
        .load_run_graph(&RunId::new("run-ro-001").unwrap())
        .unwrap()
        .unwrap();
    let rev_before = graph_before
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1:build")
        .map(|n| n.claim_revision)
        .unwrap_or(0);
    match ro_pop
        .query(StateQuery::Context(ContextReq {
            run: Some("run-ro-001".to_owned()),
            node: Some("t1.build".to_owned()),
            worker_id: None,
        }))
        .unwrap()
    {
        StateQueryOutput::Context(c) => {
            assert_eq!(c.node_cursor.to_string(), "t1.build");
            assert!(!c.guidance.is_empty());
            assert!(c.contract.get("scope").is_some());
        }
        other => panic!("unexpected Context output: {other:?}"),
    }
    let graph_after = harness
        .load_run_graph(&RunId::new("run-ro-001").unwrap())
        .unwrap()
        .unwrap();
    let rev_after = graph_after
        .nodes
        .iter()
        .find(|n| n.node_id == "task:t1:build")
        .map(|n| n.claim_revision)
        .unwrap_or(0);
    assert_eq!(rev_before, rev_after);
}

#[test]
fn seam_modules_matrix_enforces_feature_disabled_and_domain_capabilities() {
    let tmp = make_temp_dir("modules-matrix");
    let mut cfg = base_config(tmp.path());
    cfg.modules.disabled.insert("state".to_owned());
    let (_, provider) = build_provider_with_mock(&cfg.modules);

    assert_eq!(
        StateEngine::open(cfg.clone(), provider.clone())
            .err()
            .unwrap()
            .code,
        ErrorCode::FeatureDisabled
    );
    assert!(!tmp.path().join(".acdd/state.sqlite").exists());

    let ro = StateQueryEngine::open_read_only(cfg.clone(), provider.clone()).unwrap();
    for q in [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Next(NextReq::default()),
        StateQuery::BlackboardRead(BlackboardReadReq::default()),
        StateQuery::Observability(ObservabilityReq::default()),
    ] {
        assert_eq!(ro.query(q).unwrap_err().code, ErrorCode::FeatureDisabled);
    }

    let harness = RunHarness::in_memory().unwrap();
    assert_eq!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Draft(DraftReq {
                collection: "milestones".to_owned(),
                slug: "blocked".to_owned(),
                prefix: None,
                title: None,
                profile: None,
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::FeatureDisabled
    );

    // #30: Stateless CallPluginTool remains usable with modules.state == false; run-scoped stays blocked.
    for (run, expect_ok) in [(None, true), (Some("run-x".to_owned()), false)] {
        let res = execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::CallPluginTool(CallPluginToolReq {
                tool_name: "http_probe".to_owned(),
                args: json!({ "url": "https://example.invalid/stateless" }),
                run,
                node: None,
                worktree: None,
            }),
        );
        if expect_ok {
            assert!(matches!(res, Ok(StateCommandOutput::CallPluginTool(_))));
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::FeatureDisabled);
        }
    }

    let mut bb_cfg = base_config(tmp.path());
    bb_cfg.modules.disabled.insert("blackboard".to_owned());
    let (_, bb_prov) = build_provider_with_mock(&bb_cfg.modules);
    assert_eq!(
        queries::execute_query(
            &bb_cfg,
            &harness,
            &bb_prov,
            StateQuery::BlackboardRead(BlackboardReadReq::default())
        )
        .unwrap_err()
        .code,
        ErrorCode::FeatureDisabled
    );
    assert!(
        queries::execute_query(
        &bb_cfg,
        &harness,
        &bb_prov,
        StateQuery::Status(StatusReq::default())
    )
        .is_ok()
    );
    assert_eq!(
        execute_on(
            &bb_cfg,
            &harness,
            &bb_prov,
            StateCommand::BlackboardPost(BlackboardPostReq {
                run: None,
                node: None,
                recipient_node: None,
                subtask_ref: None,
                gate: None,
                author: "builder".to_owned(),
                topic: BlackboardTopic::Notes,
                payload: "note".to_owned(),
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::FeatureDisabled
    );

    let mut no_cd_cfg = base_config(tmp.path());
    no_cd_cfg.modules.disabled.insert("code".to_owned());
    no_cd_cfg.modules.disabled.insert("docs".to_owned());
    let (_, no_cd_prov) = build_provider_with_mock(&no_cd_cfg.modules);
    assert!(
        !no_cd_prov.is_domain_enabled("code")
            && !no_cd_prov.is_domain_enabled("docs")
            && no_cd_prov.is_domain_enabled("http")
    );
    assert_eq!(
        no_cd_prov
            .validate_check_ref("code.graph_fresh", &json!({}))
            .unwrap_err()
            .code,
        ErrorCode::DomainCapabilityUnavailable
    );

    let mut no_http_cfg = base_config(tmp.path());
    no_http_cfg.modules.domains.insert("http".to_owned(), false);
    let (_, no_http_prov) = build_provider_with_mock(&no_http_cfg.modules);
    assert_eq!(
        execute_on(
            &no_http_cfg,
            &harness,
            &no_http_prov,
            StateCommand::CallPluginTool(CallPluginToolReq {
                tool_name: "http_probe".to_owned(),
                args: json!({ "url": "https://example.invalid" }),
                run: None,
                node: None,
                worktree: None,
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::DomainCapabilityUnavailable
    );
}

#[test]
fn seam_mock_domain_provider_plugin_tool_and_gate_tools_matrix() {
    let tmp = make_temp_dir("plugin-tool");
    let cfg = base_config(tmp.path());
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();

    for (name, tool_name, args, expect_err, delta) in [
        (
            "valid_probe",
            "http_probe",
            json!({ "url": "https://example.invalid/health" }),
            None,
            1usize,
        ),
        (
            "missing_url",
            "http_probe",
            json!({}),
            Some(ErrorCode::ProfileSchemaInvalid),
            0,
        ),
        (
            "extra_prop",
            "http_probe",
            json!({ "url": "https://x", "extra": 42 }),
            Some(ErrorCode::ProfileSchemaInvalid),
            0,
        ),
        (
            "unknown_tool",
            "http_nonexistent",
            json!({}),
            Some(ErrorCode::DomainCapabilityUnavailable),
            0,
        ),
    ] {
        let before = mock.tool_calls();
        let res = engine.execute(StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: tool_name.to_owned(),
            args,
            run: None,
            node: None,
            worktree: None,
        }));
        assert_eq!(mock.tool_calls() - before, delta, "delta for {name}");
        match expect_err {
            None => match res.unwrap() {
                StateCommandOutput::CallPluginTool(out) => assert_eq!(
                    out.output,
                    DomainToolOutput::Json(
                        json!({ "tool": "probe", "url": "https://example.invalid/health", "status": 200 })
                    )
                ),
                other => panic!("unexpected output for {name}: {other:?}"),
            },
            Some(code) => assert_eq!(res.unwrap_err().code, code, "case {name}"),
        }
    }

    let make_tool_prof = |tool: &str| {
        format!(
        "version: 2\ninherit: base\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {{test}}\"\n  check:\n    command: \"true\"\n  lint:\n    command: \"true\"\ngates:\n  - id: probe_gate\n    proof: none\n    tools: [{tool}]\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n"
    )
    };
    let bad_p = write_profile(
        tmp.path(),
        "plugin_bad.yaml",
        &make_tool_prof("http_missing_tool"),
    );
    let ok_p = write_profile(tmp.path(), "plugin_ok.yaml", &make_tool_prof("http_probe"));
    assert_eq!(
        start_run(
            &engine,
            StartReq {
                goal: Some("Bad".to_owned()),
                profile: Some(bad_p),
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::DomainCapabilityUnavailable
    );
    let ok_started = start_run(
        &engine,
        StartReq {
            goal: Some("Ok".to_owned()),
            profile: Some(ok_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    clear_run(&engine, ok_started.run.run_id.as_str());

    // #1: In-flight mutating call Running trace inspects prev.idempotency_key without abort/re-dispatch,
    // even under out-of-scope drift, and respects recovery_only if finished concurrently.
    let mut_tmp = make_temp_dir("mut-call");
    let _ = init_git_repo(mut_tmp.path(), "main");
    let mut_p = write_profile(
        mut_tmp.path(),
        "mut_call.yaml",
        r#"version: 2
inherit: base
protected_paths: [Cargo.toml]
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
  check: { command: "true" }
  lint: { command: "true" }
gates:
  - id: deploy_gate
    proof: call
    call: http.dispatch
    with: { target: prod }
  - tasks:
      gates:
        - id: build
          proof: none
"#,
    );
    let mut_cfg = base_config(mut_tmp.path());
    let (mut_mock, mut_prov) = build_provider_with_mock(&mut_cfg.modules);
    let mut_engine = StateEngine::open(mut_cfg, mut_prov).unwrap();
    let mut_start = start_run(
        &mut_engine,
        StartReq {
            goal: Some("Mutating call crash recovery".to_owned()),
            profile: Some(mut_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let mut_rid = mut_start.run.run_id.clone();
    let mut_claim = claim_node(
        &mut_engine,
        mut_rid.as_str(),
        "deploy_gate",
        "w-mut",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    let before_snap = mut_engine
        .backend()
        .load_run_graph(&mut_rid)
        .unwrap()
        .unwrap();
    let deploy_node = before_snap
        .nodes
        .iter()
        .find(|n| n.node_id == "root:deploy_gate")
        .unwrap();
    let precond = StoragePreconditions {
        expected_run_generation: before_snap.run.generation,
        expected_node_versions: vec![NodeVersionExpectation {
            node_id: deploy_node.node_id.clone(),
            expected_version: deploy_node.node_version,
        }],
        expected_claim_revision: Some(mut_claim.claim_revision),
        expected_contract_revision: Some(deploy_node.contract_revision),
        expected_worker_id: deploy_node.worker_id.clone(),
        required_predecessor_trees: Vec::new(),
    };
    mut_engine
        .backend()
        .exec_tx(|tx| {
            tx.start_trace(
                &precond,
                &TraceStartInput {
                    run_id: mut_rid.clone(),
                    node_id: deploy_node.node_id.clone(),
                    claim_revision: mut_claim.claim_revision,
                    contract_revision: deploy_node.contract_revision,
                    execution_digest: "deadbeef".to_owned(),
                    idempotency_key: Some("prior-crash-key".to_owned()),
                    command_id: None,
                    command_line: None,
                    action_id: Some("http.dispatch".to_owned()),
                    test_target: None,
                    candidate_tree_before: mut_claim.candidate_tree.clone(),
                    log_relpath: "logs/deploy.log".to_owned(),
                    started_at: "2026-11-18T00:00:00.000Z".to_owned(),
                },
            )
            .map(|_| ())
        })
        .unwrap();

    for (write_drift, expect_code, expect_lookup_delta) in [
        (false, ErrorCode::MutatingActionUncertainOutcome, 1usize),
        (true, ErrorCode::MutatingActionUncertainOutcome, 1),
    ] {
        if write_drift {
            fs::write(
                mut_tmp.path().join("Cargo.toml"),
                "[package]\nname = \"drift\"\n",
            )
            .unwrap();
        }
        let lb = mut_mock.lookup_calls();
        let ab = mut_mock.action_calls();
        let err = verify_node(
            &mut_engine,
            mut_rid.as_str(),
            "deploy_gate",
            "w-mut",
            None,
            Some(mut_claim.claim_revision),
        )
        .unwrap_err();
        assert_eq!(err.code, expect_code);
        assert_eq!(mut_mock.lookup_calls() - lb, expect_lookup_delta);
        assert_eq!(mut_mock.action_calls() - ab, 0);
        let tr = mut_engine
            .backend()
            .load_run_graph(&mut_rid)
            .unwrap()
            .unwrap()
            .traces
            .into_iter()
            .filter(|t| t.node_id == "root:deploy_gate")
            .max_by_key(|t| t.trace_row_id.unwrap_or(0))
            .unwrap();
        assert_eq!(tr.status, TraceStatus::Running);
    }

    let tr_id = mut_engine
        .backend()
        .load_run_graph(&mut_rid)
        .unwrap()
        .unwrap()
        .traces
        .into_iter()
        .filter(|t| t.node_id == "root:deploy_gate")
        .max_by_key(|t| t.trace_row_id.unwrap_or(0))
        .unwrap()
        .trace_row_id
        .unwrap();
    mut_engine
        .backend()
        .exec_tx(|tx| {
            tx.finish_trace(&TraceFinishInput {
                trace_row_id: tr_id,
                status: TraceStatus::Failed,
                exit_code: None,
                passed_tests: Vec::new(),
                failed_tests: Vec::new(),
                check_results: Vec::new(),
                candidate_tree_after: None,
                finished_at: "2026-11-18T00:01:00.000Z".to_owned(),
            })
        })
        .unwrap();
    let ab = mut_mock.action_calls();
    assert_eq!(
        verify_node(
            &mut_engine,
            mut_rid.as_str(),
            "deploy_gate",
            "w-mut",
            None,
            Some(mut_claim.claim_revision)
        )
        .unwrap_err()
        .code,
        ErrorCode::OutOfScopeChanges
    );
    assert_eq!(mut_mock.action_calls() - ab, 0);
}

#[test]
fn seam_draft_sequential_numbering_and_archive_scan() {
    let tmp = make_temp_dir("draft");
    let milestones_dir = tmp.path().join("docs/milestones");
    fs::create_dir_all(milestones_dir.join("archive")).unwrap();
    fs::write(milestones_dir.join("AC-003-alpha.md"), "# AC-003\n").unwrap();
    fs::write(milestones_dir.join("archive/AC-007-omega.md"), "# AC-007\n").unwrap();

    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();

    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "new-feature".to_owned(),
            prefix: None,
            title: Some("New Feature".to_owned()),
            profile: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Draft(d) => {
            assert_eq!(d.spec_id, "AC-008");
            assert_eq!(d.spec_path, "docs/milestones/AC-008-new-feature.md");
            assert!(tmp.path().join(&d.spec_path).is_file());
        }
        other => panic!("unexpected draft output: {other:?}"),
    }
    assert_eq!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Draft(DraftReq {
                collection: "milestones".to_owned(),
                slug: "---".to_owned(),
                prefix: None,
                title: None,
                profile: None,
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::ProfileSchemaInvalid
    );
}

#[test]
fn seam_start_ephemeral_goal_and_cross_workspace_depends_on_matrix() {
    let root_tmp = make_temp_dir("start-main");
    let other_tmp = make_temp_dir("start-other");
    let mut cfg = base_config(root_tmp.path());
    cfg.linked_workspaces
        .insert("other".to_owned(), other_tmp.path().to_path_buf());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider.clone()).unwrap();

    let bound_p = write_profile(
        root_tmp.path(),
        "bound.yaml",
        "version: 2\ninherit: base\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
    );
    assert_eq!(
        start_run(
            &engine,
            StartReq {
                spec_path: Some("docs/milestones/AC-001.md".to_owned()),
                goal: Some("Also".to_owned()),
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ProfileSchemaInvalid
    );
    let eph = start_run(
        &engine,
        StartReq {
            goal: Some("Ship ephemeral run".to_owned()),
            profile: Some(bound_p.clone()),
            scope: vec!["crates/a/**".to_owned()],
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    assert!(eph.run.is_ephemeral);
    assert!(
        root_tmp
        .path()
        .join(eph.materialized_spec_path.unwrap())
            .is_file()
    );
    assert!(
        root_tmp
        .path()
        .join(format!(
            ".acdd/run/{}/profile.resolved.yaml",
            eph.run.run_id
        ))
            .is_file()
    );
    clear_run(&engine, eph.run.run_id.as_str());

    fs::create_dir_all(root_tmp.path().join("docs/milestones")).unwrap();
    let dep_spec = "---\nid: AC-010\ntitle: Dependent Run\nstatus: planned\ndepends_on:\n  - \"other/AC-001:t1\"\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - Consumer works\n```\n";
    fs::write(
        root_tmp.path().join("docs/milestones/AC-010-dep.md"),
        dep_spec,
    )
    .unwrap();
    fs::write(
        root_tmp.path().join("docs/milestones/AC-010-bad-task.md"),
        dep_spec.replace("other/AC-001:t1", "other/AC-001:missing_task"),
    )
    .unwrap();

    let other_engine = StateEngine::open(base_config(other_tmp.path()), provider).unwrap();
    let og1 = build_rich_graph(other_tmp.path(), "run-other-001", IsolationMode::InPlace);
    let tomb1 = sample_tombstone(
        "run-other-001",
        "other-cleared",
        Some("AC-001"),
        TerminalStatus::Cleared,
        None,
    );
    other_engine
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&og1)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &tomb1)
        })
        .unwrap();
    assert_eq!(
        start_run(
            &engine,
            StartReq {
                spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()),
                profile: Some(bound_p.clone()),
                in_place: true,
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::GateVerificationFailed
    );

    let mut og2 = build_rich_graph(other_tmp.path(), "run-other-002", IsolationMode::InPlace);
    og2.run.run_name = RunName::new("other-completed").unwrap();
    let mut tomb2 = sample_tombstone(
        "run-other-002",
        "other-completed",
        Some("AC-001"),
        TerminalStatus::Completed,
        Some("docs/milestones/archive/AC-001.md"),
    );
    tomb2.closed_at = "2026-11-17T10:20:00.000Z".to_owned();
    fs::create_dir_all(other_tmp.path().join("docs/milestones/archive")).unwrap();
    fs::write(other_tmp.path().join("docs/milestones/archive/AC-001.md"), "---\nid: AC-001\ntitle: Producer Completed\nstatus: completed\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Producer task\nscope: [src/]\nstatus: completed\n```\n").unwrap();
    other_engine
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&og2)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &tomb2)
        })
        .unwrap();

    assert_eq!(
        start_run(
            &engine,
            StartReq {
                spec_path: Some("docs/milestones/AC-010-bad-task.md".to_owned()),
                profile: Some(bound_p.clone()),
                in_place: true,
                ..StartReq::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::GateVerificationFailed
    );
    assert!(
        start_run(
        &engine,
        StartReq {
            spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()),
            profile: Some(bound_p),
            in_place: true,
            ..StartReq::default()
        }
    )
        .is_ok()
    );
}

#[test]
fn seam_claim_context_hydrates_blueprint_slice_and_live_subtasks() {
    let tmp = make_temp_dir("claim-context");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let run = "run-context-001";
    let mut graph = build_rich_graph(tmp.path(), run, IsolationMode::Worktree);
    fs::write(
        tmp.path().join(graph.run.spec_path.as_ref().unwrap()),
        "---\nid: AC-001\ntitle: Sample\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nsubtasks:\n  - subtask_ref: s1\n    title: Stale checklist title\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [crates/b/**]\nsubtasks:\n  - subtask_ref: s2\n    title: YAML fallback\n```\n",
    )
    .unwrap();

    let root = graph
        .nodes
        .iter()
        .find(|n| n.node_id == "root:handoff")
        .unwrap()
        .clone();
    for node in &mut graph.nodes {
        node.seq_order += 3;
    }
    for (seq, gate, proof) in [
        (
            0,
            "synthesize",
            json!({"tasks": {"t1": {"max_net_loc": 160, "canonical_owners": ["crates/a"]}}}),
        ),
        (1, "review", json!({"decision": "pass"})),
        (2, "other_review", json!({"tasks": []})),
    ] {
        let mut node = root.clone();
        node.node_id = format!("root:blueprint_synthesis:{gate}");
        node.gate_id = gate.to_owned();
        node.status = NodeStatus::Completed;
        node.seq_order = seq;
        node.claim_revision = 1;
        graph.artifacts.push(GateArtifactRecord {
            run_id: graph.run.run_id.clone(),
            node_id: node.node_id.clone(),
            claim_revision: node.claim_revision,
            kind: ArtifactKind::Proof,
            payload_json: proof.to_string(),
            snapshot_commit: None,
            created_at: format!("2026-11-17T10:0{seq}:00.000Z"),
        });
        graph.nodes.push(node);
    }
    harness.seed_run(&graph).unwrap();

    let show = |cfg: &_, node: &str| {
        let query = StateQuery::Show(ShowReq {
            run: Some(run.to_owned()),
            node: Some(node.to_owned()),
        });
        let StateQueryOutput::Show(show) =
            queries::execute_query(cfg, &harness, &provider, query).unwrap()
        else {
            panic!("expected Show output");
        };
        show
    };

    let long_evidence = "🦀\\\"".repeat(440);
    assert_eq!(long_evidence.len(), 2640);
    let set_subtask = |subtask_ref: &str, status, evidence: Option<&str>| {
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Subtask(SubtaskReq {
                run: Some(run.to_owned()),
                task_ref: TaskRef::new("t1").unwrap(),
                subtask_ref: SubtaskRef::new(subtask_ref).unwrap(),
                status,
                evidence: evidence.map(str::to_owned),
            }),
        )
        .unwrap();
    };
    set_subtask("s1", SubtaskStatus::InProgress, None);
    let baseline_bytes = serde_json::to_vec(&show(&cfg, "t1.build").context_preview.unwrap())
        .unwrap()
        .len();
    let mut tight_cfg = cfg.clone();
    tight_cfg.response_policy.max_output_bytes = baseline_bytes + 300;
    for (config, status, evidence, include_evidence, count) in [
        (&cfg, SubtaskStatus::InProgress, None, true, 1),
        (
            &cfg,
            SubtaskStatus::Completed,
            Some("implementation verified"),
            true,
            1,
        ),
        (
            &cfg,
            SubtaskStatus::Cancelled,
            Some("superseded by another change"),
            true,
            1,
        ),
        (
            &tight_cfg,
            SubtaskStatus::Completed,
            Some(long_evidence.as_str()),
            false,
            1,
        ),
        (
            &tight_cfg,
            SubtaskStatus::Completed,
            Some("implementation verified"),
            true,
            30,
        ),
    ] {
        set_subtask("s1", status, evidence);
        for index in 1..count {
            let status = if index == count - 1 {
                SubtaskStatus::InProgress
            } else {
                SubtaskStatus::Pending
            };
            set_subtask(&format!("check-{index:02}"), status, None);
        }
        let shown = show(config, "t1.build");
        assert_eq!(shown.subtasks.len(), count);
        assert_eq!(shown.subtasks[0].evidence.as_deref(), evidence);
        let context = shown.context_preview.unwrap();
        assert!(
            serde_json::to_vec(&context).unwrap().len() <= config.response_policy.max_output_bytes
        );
        assert_eq!(context["pre_task_slice"]["max_net_loc"], 160);
        assert_eq!(
            context["pre_task_slice"]["canonical_owners"],
            json!(["crates/a"])
        );
        let subtasks = context["contract"]["subtasks"].as_array().unwrap();
        if count > 1 {
            assert_eq!(subtasks[0]["subtask_ref"], "check-29");
            assert_eq!(subtasks[0]["status"], "in_progress");
            assert!(subtasks.len() < count);
            assert_eq!(
                context["contract"]["subtasks_omitted"],
                count - subtasks.len()
            );
            continue;
        }
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0]["subtask_ref"], "s1");
        assert_eq!(subtasks[0]["title"], "Implement seam");
        assert_eq!(subtasks[0]["status"], json!(status));
        assert_eq!(
            subtasks[0]["evidence"],
            json!(evidence.filter(|_| include_evidence))
        );
    }

    let fallback = show(&cfg, "t2.build").context_preview.unwrap();
    assert_eq!(fallback["contract"]["subtasks"][0]["subtask_ref"], "s2");
    assert_eq!(fallback["contract"]["subtasks"][0]["status"], "pending");
}

struct RestoreCwd(Option<std::path::PathBuf>);
impl Drop for RestoreCwd {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::env::set_current_dir(dir);
        }
    }
}

#[test]
fn seam_start_and_sync_owned_worktree_checkout_matrix() {
    use std::path::PathBuf;

    let tmp = make_temp_dir("source-checkout");
    let primary = tmp.path().join("primary");
    init_git_repo(&primary, "primary");
    let primary = primary.canonicalize().unwrap();
    let linked = primary.join(".worktrees/linked");
    git_ok(
        &primary,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let spec = "work/AC-001.md";
    let dependent = "work/AC-003.md";
    for (path, body) in [
        (
            spec,
            "---\nid: AC-001\ntitle: Linked checkout\nstatus: active\n---\n",
        ),
        (".acdd/profile.yaml", "version: 2\ninherit: shared\n"),
        (
            dependent,
            "---\nid: AC-003\ntitle: Linked dependency\nstatus: active\ndepends_on: [AC-002]\n---\n",
        ),
        (
            "work/archive/AC-002.md",
            "---\nid: AC-002\ntitle: Linked archive\nstatus: completed\n---\n",
        ),
        (
            "profiles/shared.yaml",
            "version: 2\ninherit: none\nsave: none\ngates:\n  - tasks:\n      gates:\n        - id: build\n  - id: handoff\n",
        ),
        (
            "acdd.yaml",
            "state_db: .acdd/foreign.sqlite\nprofiles_dirs: [profiles]\nspecs:\n  custom: {path: work}\nlinked_workspaces:\n  - name: configured\n    path: .\n",
        ),
        (
            "work/AC-004.md",
            "---\nid: AC-004\ntitle: Alias dependency\nstatus: active\n---\n### task: consume\n```yaml\ntask_ref: consume\ntarget: consume\nscope: [src/]\ndepends_on: [\"alias/producer-case:build\"]\n```\n",
        ),
        (
            "work/AC-005.md",
            "---\nid: AC-005\ntitle: Relative alias\nstatus: active\nrepositories:\n  - name: alias\n    path: ../linked\n---\n### task: consume\n```yaml\ntask_ref: consume\ntarget: consume\nscope: [src/]\ndepends_on: [\"alias/producer-case:build\"]\n```\n",
        ),
        (
            "work/archive/AC-006.md",
            "---\nid: AC-006\ntitle: Archived task\nstatus: completed\n---\n### task: build\n```yaml\ntask_ref: build\ntarget: build\nscope: [src/]\nstatus: completed\n```\n",
        ),
        (
            "work/archive/AC-PRODUCER.md",
            "---\nid: AC-PRODUCER\ntitle: Archived producer\nstatus: completed\n---\n### task: build\n```yaml\ntask_ref: build\ntarget: build\nscope: [src/]\nstatus: completed\n```\n",
        ),
        (
            "work/AC-007.md",
            "---\nid: AC-007\ntitle: Archive dependency\nstatus: active\n---\n### task: consume\n```yaml\ntask_ref: consume\ntarget: consume\nscope: [src/]\ndepends_on: [\"AC-006:build\"]\n```\n",
        ),
        (
            "work/AC-008.md",
            "---\nid: AC-008\ntitle: Rebind task\nstatus: active\n---\n### task: build\n```yaml\ntask_ref: build\ntarget: build\nscope: [src/]\n```\n",
        ),
        (
            "work/AC-009.md",
            "---\nid: AC-009\ntitle: Configured alias\nstatus: active\n---\n### task: consume\n```yaml\ntask_ref: consume\ntarget: consume\nscope: [src/]\ndepends_on: [\"configured/AC-006:build\"]\n```\n",
        ),
    ] {
        let path = linked.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    git_ok(&linked, &["add", "work/archive", "profiles"]);
    git_ok(&linked, &["commit", "-m", "linked inputs"]);
    let head = git_ok(&linked, &["rev-parse", "HEAD"]);
    fs::create_dir_all(primary.join("work")).unwrap();
    fs::write(
        primary.join("work/AC-006.md"),
        "---\nid: AC-006\ntitle: Active substitute\nstatus: active\n---\n### task: build\n```yaml\ntask_ref: build\ntarget: build\nscope: [src/]\n```\n",
    )
    .unwrap();
    fs::create_dir_all(primary.join("profiles")).unwrap();
    fs::write(
        primary.join("profiles/new-parent.yaml"),
        "version: 2\ninherit: none\nprotected_paths: [src/]\n",
    )
    .unwrap();
    git_ok(
        &primary,
        &["add", "profiles/new-parent.yaml", "work/AC-006.md"],
    );
    git_ok(&primary, &["commit", "-m", "primary profile"]);
    assert!(!primary.join(spec).exists());
    assert!(!primary.join(".acdd/profile.yaml").exists());

    let foreign = tmp.path().join("foreign");
    init_git_repo(&foreign, "foreign");
    let alias = primary.join("alias");
    std::os::unix::fs::symlink(&foreign, &alias).unwrap();
    let owned_alias = primary.join("owned-alias");
    std::os::unix::fs::symlink(&linked, &owned_alias).unwrap();
    let mut cfg = base_config(&primary);
    cfg.linked_workspaces
        .insert("alias".to_owned(), linked.clone());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let shared = StateEngine::open(cfg.clone(), provider.clone()).unwrap();
    let mut producer = build_rich_graph(&primary, "producer-id", IsolationMode::InPlace);
    producer.run.worktree = Some(linked.display().to_string());
    producer.run.run_name = RunName::new("producer-case").unwrap();
    producer.run.spec_id = Some("AC-PRODUCER".to_owned());
    let mut completed = producer
        .nodes
        .iter()
        .find(|node| node.node_id == "task:t1")
        .unwrap()
        .clone();
    completed.node_id = "task:build".into();
    completed.task_ref = Some(TaskRef::new("build").unwrap());
    completed.status = NodeStatus::Completed;
    producer.nodes.push(completed);
    let producer_tomb = sample_tombstone(
        "producer-id",
        "producer-case",
        Some("AC-PRODUCER"),
        TerminalStatus::Completed,
        Some("work/archive/AC-PRODUCER.md"),
    );
    shared
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&producer)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &producer_tomb)
        })
        .unwrap();
    let mut started_run = None;
    let created = primary.join(".worktrees/custom-destination");
    let managed = primary.join(".worktrees/managed");
    let cases = [
        ("caller", linked.clone(), None, Some(&linked)),
        (
            "create",
            linked.clone(),
            Some(created.clone()),
            Some(&created),
        ),
        ("managed", linked.clone(), None, Some(&managed)),
        (
            "alias-case",
            linked.clone(),
            None,
            Some(&primary.join(".worktrees/alias-case")),
        ),
        ("relative", linked.clone(), None, Some(&linked)),
        ("relative-cwd", linked.clone(), None, Some(&linked)),
        ("archive-task", linked.clone(), None, Some(&linked)),
        (
            "configured",
            primary.clone(),
            Some(PathBuf::from(".worktrees/linked")),
            Some(&linked),
        ),
        ("archive-base", linked.clone(), None, None),
        (
            "rebind-worktree",
            linked.clone(),
            None,
            Some(&primary.join(".worktrees/rebind-worktree")),
        ),
        (
            "lineage",
            linked.clone(),
            None,
            Some(&primary.join(".worktrees/lineage")),
        ),
        ("base-missing", linked.clone(), None, None),
        ("dependencies", linked.clone(), None, Some(&linked)),
        (
            "explicit",
            primary.clone(),
            Some(PathBuf::from(".worktrees/linked")),
            Some(&linked),
        ),
        (
            "unowned",
            primary.clone(),
            Some(tmp.path().to_path_buf()),
            None,
        ),
        ("foreign", primary.clone(), Some(foreign), None),
        (
            "subdirectory",
            primary.clone(),
            Some(linked.join("src")),
            None,
        ),
        (
            "escape",
            primary.clone(),
            Some(PathBuf::from("alias")),
            None,
        ),
        (
            "owned-symlink",
            primary.clone(),
            Some(owned_alias.clone()),
            None,
        ),
        (
            "trailing-dot",
            primary.clone(),
            Some(owned_alias.join(".")),
            None,
        ),
    ];
    for case in cases {
        let (name, worktree, explicit, expected) = case;
        let mut caller = cfg.clone();
        caller.worktree = worktree;
        if name == "relative" || name == "relative-cwd" {
            caller.linked_workspaces.remove("alias");
            fs::write(
                linked.join("acdd.yaml"),
                "state_db: .acdd/foreign.sqlite\nprofiles_dirs: [profiles]\nspecs:\n  custom: {path: work}\n",
            )
            .unwrap();
        }
        if name == "rebind-worktree" {
            fs::write(
                linked.join("profiles/shared.yaml"),
                "version: 2\ninherit: none\nsave: none\ngit:\n  isolation: in_place\ngates:\n  - tasks:\n      gates:\n        - id: build\n  - id: handoff\n",
            )
            .unwrap();
        }
        if name == "lineage" {
            fs::write(
                linked.join(".acdd/profile.yaml"),
                "version: 2\ninherit: new-parent\n",
            )
            .unwrap();
            fs::write(
                linked.join("profiles/new-parent.yaml"),
                "version: 2\ninherit: none\nsave: none\ngates:\n  - id: handoff\n",
            )
            .unwrap();
        }
        let start = |engine: &StateEngine, worktree_path| {
            start_run(
                engine,
                StartReq {
                    spec_path: Some(
                        match name {
                            "dependencies" | "base-missing" => dependent,
                            "alias-case" => "work/AC-004.md",
                            "relative" | "relative-cwd" => "work/AC-005.md",
                            "archive-task" | "archive-base" => "work/AC-007.md",
                            "configured" => "work/AC-009.md",
                            "rebind-worktree" => "work/AC-008.md",
                            _ => spec,
                        }
                        .to_owned(),
                    ),
                    run_name: Some(RunName::new(name).unwrap()),
                    worktree_path,
                    worktree: matches!(
                        name,
                        "managed"
                            | "alias-case"
                            | "lineage"
                            | "base-missing"
                            | "archive-base"
                            | "rebind-worktree"
                    ),
                    target_base: matches!(name, "base-missing" | "lineage" | "archive-base")
                        .then(|| "primary".to_owned()),
                    ..StartReq::default()
                },
            )
        };
        let sync = |run: &RunId| {
            StateCommand::Sync(SyncReq {
                run: Some(run.to_string()),
                rebind_profile: true,
            })
        };

        if let Some(expected) = expected {
            let mut restore_cwd = RestoreCwd(None);
            if name == "relative-cwd" {
                restore_cwd.0 = Some(std::env::current_dir().unwrap());
                std::env::set_current_dir(&linked).unwrap();
            }
            let engine = StateEngine::open(caller, provider.clone()).unwrap();
            let started = start(&engine, explicit.clone());
            if name == "relative-cwd" {
                std::env::set_current_dir(&primary).unwrap();
            }
            let started = started.expect(name);
            let ready_nodes = started.ready_nodes.clone();
            let run = started.run;
            assert_eq!(run.collection.as_deref(), Some("custom"));
            assert_eq!(run.profile_id, ".acdd/profile.yaml");
            assert_eq!(
                run.target_base.as_deref(),
                Some(if name == "lineage" {
                    "primary"
                } else {
                    "linked"
                })
            );
            assert_eq!(run.target_base_commit.as_ref().unwrap().as_str(), head);
            assert_eq!(run.worktree.as_deref(), expected.to_str());
            assert!(matches!(shared.execute(sync(&run.run_id)).unwrap(),
                StateCommandOutput::Sync(output) if output.rebound_profile));
            if name == "explicit" {
                started_run = Some(run.run_id);
            } else if expected == &linked {
                if name == "relative" || name == "relative-cwd" {
                    let graph = shared
                        .backend()
                        .load_run_graph(&run.run_id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        graph.dependencies[0].depends_on_run_id.as_str(),
                        "producer-id"
                    );
                    assert!(graph.dependencies[0].satisfied, "{name}");
                    fs::write(
                        linked.join("acdd.yaml"),
                        "state_db: .acdd/foreign.sqlite\nprofiles_dirs: [profiles]\nspecs:\n  custom: {path: work}\nlinked_workspaces:\n  - name: configured\n    path: .\n",
                    )
                    .unwrap();
                }
                if name == "configured" {
                    let graph = shared
                        .backend()
                        .load_run_graph(&run.run_id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(graph.dependencies[0].depends_on_ref.as_str(), "build");
                    assert!(graph.dependencies[0].satisfied, "{name}");
                }
                clear_run(&shared, run.run_id.as_str());
            } else {
                if name == "rebind-worktree" {
                    assert_eq!(run.isolation, IsolationMode::Worktree);
                    let node = ready_nodes
                        .iter()
                        .find(|cursor| cursor.as_str().ends_with(".build"))
                        .expect("ready task gate")
                        .clone();
                    let claimed = shared
                        .execute(StateCommand::Claim(ClaimReq {
                            run: Some(run.run_id.to_string()),
                            node: Some(node),
                            worker_id: WorkerId::new("worker-rebind").unwrap(),
                            session_fingerprint: None,
                            force: false,
                            steal_remote: false,
                            worktree: None,
                        }))
                        .expect(name);
                    let StateCommandOutput::Claim(claim) = claimed else {
                        panic!("expected claim");
                    };
                    let task_wt = primary.join(".worktrees").join(format!("{name}-build"));
                    assert_eq!(claim.worktree, task_wt.to_str().unwrap());
                    assert!(task_wt.is_dir(), "{name}");
                    fs::write(
                        linked.join("profiles/shared.yaml"),
                        "version: 2\ninherit: none\nsave: none\ngates:\n  - tasks:\n      gates:\n        - id: build\n  - id: handoff\n",
                    )
                    .unwrap();
                } else if name == "alias-case" {
                    let graph = shared
                        .backend()
                        .load_run_graph(&run.run_id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        graph.dependencies[0].depends_on_run_id.as_str(),
                        "producer-id"
                    );
                    assert!(graph.dependencies[0].satisfied);
                } else {
                    assert_eq!(
                        fs::read(expected.join(spec)).unwrap(),
                        fs::read(linked.join(spec)).unwrap()
                    );
                }
                let profile = ".acdd/profile.yaml";
                assert_eq!(
                    fs::read(expected.join(profile)).unwrap(),
                    fs::read(linked.join(profile)).unwrap()
                );
            }
            if name == "lineage" {
                assert_eq!(
                    fs::read(expected.join("profiles/new-parent.yaml")).unwrap(),
                    fs::read(linked.join("profiles/new-parent.yaml")).unwrap()
                );
                fs::write(
                    linked.join(".acdd/profile.yaml"),
                    "version: 2\ninherit: shared\n",
                )
                .unwrap();
            }
        } else if name == "base-missing" || name == "archive-base" {
            let engine = StateEngine::open(caller, provider.clone()).unwrap();
            assert!(start(&engine, None).is_err(), "{name}");
            assert!(!primary.join(".worktrees").join(name).exists(), "{name}");
            assert!(
                git_ok(&primary, &["branch", "--list", &format!("run/{name}")]).is_empty(),
                "{name}"
            );
        } else {
            let path = explicit.unwrap();
            assert_eq!(
                start(&shared, Some(path.clone())).unwrap_err().code,
                ErrorCode::ExternalGitDrift,
                "{name}"
            );
            caller.worktree = primary.join(path);
            let engine = StateEngine::open(caller.clone(), provider.clone()).unwrap();
            assert_eq!(
                start(&engine, None).unwrap_err().code,
                ErrorCode::ExternalGitDrift,
                "{name}"
            );
            let run = started_run.as_ref().unwrap();
            assert_eq!(
                engine.execute(sync(run)).unwrap_err().code,
                ErrorCode::ExternalGitDrift,
                "{name}"
            );

            let mut graph = shared.backend().load_run_graph(run).unwrap().unwrap();
            graph.run.run_id = RunId::new(format!("invalid-{name}")).unwrap();
            graph.run.run_name = RunName::new(format!("invalid-{name}")).unwrap();
            graph.run.worktree = Some(caller.worktree.display().to_string());
            graph.nodes.clear();
            shared
                .backend()
                .exec_tx(|tx| tx.insert_run_graph(&graph))
                .unwrap();
            assert_eq!(
                shared.execute(sync(&graph.run.run_id)).unwrap_err().code,
                ErrorCode::ExternalGitDrift,
                "{name}"
            );
            assert_eq!(
                shared
                    .backend()
                    .load_run_graph(&graph.run.run_id)
                    .unwrap()
                    .unwrap()
                    .run
                    .generation,
                graph.run.generation
            );
        }
    }
    assert!(primary.join(".acdd/state.sqlite").is_file());
    assert!(!linked.join(".acdd/state.sqlite").exists());
}

#[test]
fn seam_in_memory_lifecycle_subtask_blackboard_extend_scope_reopen_sync_clear() {
    let tmp = make_temp_dir("lifecycle");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let mut graph = build_rich_graph(tmp.path(), "run-life-001", IsolationMode::Worktree);

    if let Some(n) = graph
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:t2:build")
    {
        n.status = NodeStatus::InProgress;
        n.claim_revision = 1;
        n.worker_id = Some(WorkerId::new("worker-b").unwrap());
    }
    if let Some(n) = graph.nodes.iter_mut().find(|n| n.node_id == "task:t2") {
        n.status = NodeStatus::InProgress;
    }
    graph.claims.push(GateClaimRecord {
        claim_id: None,
        run_id: RunId::new("run-life-001").unwrap(),
        node_id: "task:t2:build".to_owned(),
        claim_revision: 1,
        worker_id: WorkerId::new("worker-b").unwrap(),
        session_fingerprint: Some("host:1:1".to_owned()),
        worktree: tmp.path().display().to_string(),
        span_id: "0123456789abcdef".to_owned(),
        claim_tree: TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
        claimed_at: "2026-11-17T10:01:00.000Z".to_owned(),
        ended_at: None,
        outcome: ClaimOutcome::Active,
    });
    harness.seed_run(&graph).unwrap();

    assert_eq!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Subtask(SubtaskReq {
                run: Some("run-life-001".to_owned()),
                task_ref: TaskRef::new("t1").unwrap(),
                subtask_ref: SubtaskRef::new("s1").unwrap(),
                status: SubtaskStatus::Completed,
                evidence: Some("   ".to_owned()),
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::ProfileSchemaInvalid
    );
    assert!(matches!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Subtask(SubtaskReq {
                run: Some("run-life-001".to_owned()),
                task_ref: TaskRef::new("t1").unwrap(),
                subtask_ref: SubtaskRef::new("s1").unwrap(),
                status: SubtaskStatus::Completed,
                evidence: Some("implemented in crates/a/src/lib.rs".to_owned()),
            })
        )
        .unwrap(),
        StateCommandOutput::Subtask(_)
    ));

    let bb_post_out = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some("run-life-001".to_owned()),
            node: Some("t1.build".to_owned()),
            recipient_node: None,
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "worker-a".to_owned(),
            topic: BlackboardTopic::Notes,
            payload: "shared note context".to_owned(),
        }),
    )
    .unwrap();
    assert_eq!(
        bb_post_out.into_untagged_json().unwrap()["entry"]["payload"],
        "shared note context"
    );

    for (author, payload, expected_field) in
        [(" ", "non-empty", "author"), ("worker-a", " ", "payload")]
    {
        let err = execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some("run-life-001".to_owned()),
                node: None,
                recipient_node: None,
                subtask_ref: None,
                gate: None,
                author: author.to_owned(),
                topic: BlackboardTopic::Notes,
                payload: payload.to_owned(),
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        assert!(err.message.contains(expected_field));
    }
    let bb_query_out = queries::execute_query(
        &cfg,
        &harness,
        &provider,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some("run-life-001".to_owned()),
            topic: Some(BlackboardTopic::Notes),
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap();
    assert_eq!(
        bb_query_out.clone().into_untagged_json().unwrap()["total"],
        1
    );
    match bb_query_out {
        StateQueryOutput::BlackboardRead(bb) => {
            assert_eq!(bb.items[0].payload, "shared note context")
        }
        other => panic!("unexpected BlackboardRead output: {other:?}"),
    }

    assert_eq!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::ExtendScope(ExtendScopeReq {
                run: Some("run-life-001".to_owned()),
                node: "t1".to_owned(),
                paths: vec!["crates/b/src/main.rs".to_owned()],
                reason: "needs sibling crate".to_owned(),
                worker_id: None,
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskScopeConflict
    );
    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::ExtendScope(ExtendScopeReq {
            run: Some("run-life-001".to_owned()),
            node: "t1".to_owned(),
            paths: vec!["crates/c/**".to_owned()],
            reason: "shared test helper".to_owned(),
            worker_id: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::ExtendScope(e) => {
            assert_eq!(e.contract_revision, 2);
            assert!(e.admitted_scope.contains(&"crates/c/**".to_owned()));
        }
        other => panic!("unexpected ExtendScope output: {other:?}"),
    }

    let snap = harness
        .load_run_graph(&RunId::new("run-life-001").unwrap())
        .unwrap()
        .unwrap();
    harness
        .exec_tx(|tx| {
            let t1_n = snap.nodes.iter().find(|n| n.node_id == "task:t1").unwrap();
            let t1_b = snap
                .nodes
                .iter()
                .find(|n| n.node_id == "task:t1:build")
                .unwrap();
            let tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
            let mut p1 = NodeStatusPatch::status_only("task:t1", NodeStatus::Completed);
            p1.accepted_tree = Some(Some(tree.clone()));
            let mut p2 = NodeStatusPatch::status_only("task:t1:build", NodeStatus::Completed);
            p2.accepted_tree = Some(Some(tree));
            tx.apply_transition_plan(
                &snap.run.run_id,
                &TransitionPlan {
                    preconditions: StoragePreconditions {
                        expected_run_generation: snap.run.generation,
                        expected_node_versions: vec![
                            NodeVersionExpectation {
                                node_id: t1_n.node_id.clone(),
                                expected_version: t1_n.node_version,
                            },
                            NodeVersionExpectation {
                                node_id: t1_b.node_id.clone(),
                                expected_version: t1_b.node_version,
                            },
                        ],
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    bump_run_generation: true,
                    new_run_status: None,
                    new_root_cycle_count: None,
                    completed_at: None,
                    node_patches: vec![p1, p2],
                    dependency_patches: Vec::new(),
                    claim_patches: Vec::new(),
                    new_artifacts: Vec::new(),
                    new_blackboard: Vec::new(),
                    blackboard_patches: Vec::new(),
                    new_subtasks: Vec::new(),
                    significant_transition: true,
                },
            )
        })
        .unwrap();

    assert_eq!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Reopen(ReopenReq {
                run: Some("run-life-001".to_owned()),
                node: "t1.build".to_owned(),
                cascade: false,
                reason: Some("fix".to_owned()),
            })
        )
        .unwrap_err()
        .code,
        ErrorCode::DependentTaskCompleted
    );
    assert!(matches!(
        execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::Reopen(ReopenReq {
                run: Some("run-life-001".to_owned()),
                node: "t1.build".to_owned(),
                cascade: true,
                reason: Some("cascade".to_owned()),
            })
        )
        .unwrap(),
        StateCommandOutput::Reopen(_)
    ));

    fs::write(tmp.path().join("docs/milestones/AC-001-sample.md"), "---\nid: AC-001\ntitle: Sample Updated\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - One\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [crates/b/**]\ndepends_on: [t1]\nacceptance_criteria:\n  - Two\n```\n### task: t3\n```yaml\ntask_ref: t3\nscope: [crates/d/**]\nacceptance_criteria:\n  - Three\n```\n").unwrap();
    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Sync(SyncReq {
            run: Some("run-life-001".to_owned()),
            rebind_profile: false,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Sync(s) => assert!(s.added_nodes >= 2),
        other => panic!("unexpected Sync output: {other:?}"),
    }
    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Clear(ClearReq {
            run: Some("run-life-001".to_owned()),
            reason: Some("cancelled".to_owned()),
            force: true,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Clear(c) => {
            assert_eq!(c.tombstone.terminal_status, TerminalStatus::Cleared);
            assert!(
                harness
                .load_run_graph(&RunId::new("run-life-001").unwrap())
                .unwrap()
                    .is_none()
            );
        }
        other => panic!("unexpected Clear output: {other:?}"),
    }
}

#[test]
fn seam_multi_path_draft_prefixless_and_whole_spec_deps() {
    let tmp = make_temp_dir("multi-path-whole-spec");
    let root = tmp.path();
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("extensions/ext-a/docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/plans")).unwrap();
    fs::write(
        root.join("acdd.yaml"),
        "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    paths:\n      - docs/milestones\n      - extensions/*/docs/milestones\n    prefixes: [AC]\n    digits: 3\n  plans:\n    path: docs/plans\n    prefixes: []\n    digits: 3\n",
    )
    .unwrap();

    let mut cfg = base_config(root);
    cfg.spec_collections = acdd_core::config::load_adapter(root, None).unwrap().specs;
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg.clone(), provider.clone()).unwrap();

    // 1. Prefixless draft creates docs/plans/my-slug.md with id: my-slug and rejects collision.
    let draft_plan = match execute_on(
        &cfg,
        engine.backend(),
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "plans".to_owned(),
            slug: "my-slug".to_owned(),
            title: Some("My Plan".to_owned()),
            prefix: None,
            profile: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Draft(d) => d,
        other => panic!("unexpected Draft output: {other:?}"),
    };
    assert_eq!(draft_plan.spec_id, "my-slug");
    assert_eq!(draft_plan.spec_path, "docs/plans/my-slug.md");
    assert!(
        execute_on(
        &cfg,
        engine.backend(),
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "plans".to_owned(),
            slug: "my-slug".to_owned(),
            title: None,
            prefix: None,
            profile: None,
        }),
    )
        .is_err()
    );

    // 2. Multi-path draft scans extensions/*/docs/milestones for max_num.
    fs::write(
        root.join("extensions/ext-a/docs/milestones/AC-004-ext.md"),
        "---\nid: AC-004\ntitle: Ext\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let draft_ms = match execute_on(
        &cfg,
        engine.backend(),
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "next".to_owned(),
            title: None,
            prefix: None,
            profile: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Draft(d) => d,
        other => panic!("unexpected Draft output: {other:?}"),
    };
    assert_eq!(draft_ms.spec_id, "AC-005");
    assert_eq!(draft_ms.spec_path, "docs/milestones/AC-005-next.md");

    // 3. Task-to-whole-spec dependency validation, cycle detection, and runtime satisfaction.
    fs::write(
        root.join("docs/milestones/AC-005-next.md"),
        "---\nid: AC-005\ntitle: Next\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [my-slug]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let report =
        acdd_state::validate::validate_workspace(root, None, Some("base"), &provider).unwrap();
    assert!(
        report.valid,
        "expected valid workspace, got: {:?}",
        report.diagnostics
    );

    // Cross-spec cycle via task-to-whole-spec dep must be rejected.
    fs::write(
        root.join("docs/plans/my-slug.md"),
        "---\nid: my-slug\ntitle: My Plan\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [AC-005]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let cycle_report =
        acdd_state::validate::validate_workspace(root, None, Some("base"), &provider).unwrap();
    assert!(!cycle_report.valid, "expected cross-spec cycle failure");

    // Restore acyclic my-slug and start AC-005: T1 must be blocked until my-slug completes.
    fs::write(
        root.join("docs/plans/my-slug.md"),
        "---\nid: my-slug\ntitle: My Plan\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let p = write_profile(
        root,
        "p.yaml",
        "version: 2\ninherit: base\nroles:\n  builder:\n    mode: subagent\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n",
    );
    let ac5_run = match execute_on(
        &cfg,
        engine.backend(),
        &provider,
        StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-005-next.md".to_owned()),
            run_name: Some(RunName::new("ac5").unwrap()),
            profile: Some(p),
            in_place: true,
            ..Default::default()
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Start(s) => s.run.run_id,
        other => panic!("unexpected Start output: {other:?}"),
    };
    let next_before = match engine
        .query(StateQuery::Next(NextReq {
            run: Some(ac5_run.as_str().to_owned()),
            limit: None,
        }))
        .unwrap()
    {
        StateQueryOutput::Next(n) => n.ready_cursors,
        other => panic!("unexpected Next output: {other:?}"),
    };
    assert!(!next_before.contains(&"T1.build".to_owned()));

    // Complete my-slug via tombstone, then AC-005 T1 becomes ready.
    let mut plan_graph = build_rich_graph(root, "run-my-slug", IsolationMode::InPlace);
    plan_graph.run.worktree = None;
    let plan_tomb = sample_tombstone(
        "run-my-slug",
        "my-slug",
        Some("my-slug"),
        TerminalStatus::Completed,
        None,
    );
    engine
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&plan_graph)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &plan_tomb)
        })
        .unwrap();
    let next_after = match engine
        .query(StateQuery::Next(NextReq {
            run: Some(ac5_run.as_str().to_owned()),
            limit: None,
        }))
        .unwrap()
    {
        StateQueryOutput::Next(n) => n.ready_cursors,
        other => panic!("unexpected Next output: {other:?}"),
    };
    assert!(
        next_after.iter().any(|c| c == "T1" || c == "T1.build"),
        "got {next_after:?}"
    );

    // 4. Committed archive under glob collection (`extensions/*/docs/milestones`) in a git repo + linked-workspace cycle.
    let git_tmp = make_temp_dir("glob-git-archive");
    let git_root = git_tmp.path();
    let _ = init_git_repo(git_root, "main");
    let linked_tmp = make_temp_dir("linked-cycle");
    let linked_root = linked_tmp.path();
    fs::create_dir_all(git_root.join("extensions/pkg/docs/milestones/archive")).unwrap();
    fs::create_dir_all(git_root.join("docs/milestones/team")).unwrap();
    fs::write(
        git_root.join("acdd.yaml"),
        format!(
            "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    paths:\n      - docs/milestones\n      - extensions/*/docs/milestones\n    prefixes: [AC]\n    digits: 3\nlinked_workspaces:\n  - name: other\n    path: {}\n",
            linked_root.display()
        ),
    )
    .unwrap();
    fs::write(
        git_root.join("extensions/pkg/docs/milestones/archive/AC-010-done.md"),
        "---\nid: AC-010\ntitle: Done\nstatus: completed\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        git_root.join("docs/milestones/team/AC-011-nested.md"),
        "---\nid: AC-011\ntitle: Nested\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [AC-010]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    git_ok(git_root, &["add", "-A"]);
    git_ok(git_root, &["commit", "-m", "commit glob archive"]);
    let git_report =
        acdd_state::validate::validate_workspace(git_root, None, Some("base"), &provider).unwrap();
    assert!(
        git_report.valid,
        "expected valid glob archive dep, got: {:?}",
        git_report.diagnostics
    );

    // Linked-workspace cross-spec cycle (`AC-011 -> other/B-001 -> back/AC-011`) must be rejected.
    fs::create_dir_all(linked_root.join("docs/milestones")).unwrap();
    fs::write(
        linked_root.join("acdd.yaml"),
        format!(
            "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [B]\n    digits: 3\nlinked_workspaces:\n  - name: back\n    path: {}\n",
            git_root.display()
        ),
    )
    .unwrap();
    fs::write(
        linked_root.join("docs/milestones/B-001-mid.md"),
        "---\nid: B-001\ntitle: Mid\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [back/AC-011]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        git_root.join("docs/milestones/team/AC-011-nested.md"),
        "---\nid: AC-011\ntitle: Nested\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [other/B-001]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let lw_cycle_report =
        acdd_state::validate::validate_workspace(git_root, None, Some("base"), &provider).unwrap();
    assert!(
        !lw_cycle_report.valid,
        "expected linked-workspace cross-spec cycle failure"
    );

    // 5. `extensions/**/docs/milestones` archives `extensions/pkg/docs/milestones/team/target.md` -> `extensions/pkg/docs/milestones/archive/target.md`
    //    and satisfies `resolve_dep_target` across both Git and non-Git workspaces.
    let nongit_tmp = make_temp_dir("deep-glob-nongit");
    let nongit_root = nongit_tmp.path();
    fs::create_dir_all(nongit_root.join("extensions/pkg/docs/milestones/archive")).unwrap();
    fs::create_dir_all(nongit_root.join("docs/milestones")).unwrap();
    fs::write(
        nongit_root.join("acdd.yaml"),
        "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    paths:\n      - docs/milestones\n      - extensions/**/docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    )
    .unwrap();
    let deep_cols = acdd_core::config::load_adapter(nongit_root, None)
        .unwrap()
        .specs;
    assert_eq!(
        deep_cols["milestones"]
            .matched_concrete_root("extensions/pkg/docs/milestones/team/target.md")
            .as_deref(),
        Some("extensions/pkg/docs/milestones")
    );
    fs::write(
        nongit_root.join("extensions/pkg/docs/milestones/archive/target.md"),
        "---\nid: AC-020\ntitle: Deep Archived\nstatus: completed\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        nongit_root.join("docs/milestones/AC-021-consumer.md"),
        "---\nid: AC-021\ntitle: Consumer\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [AC-020]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let nongit_report =
        acdd_state::validate::validate_workspace(nongit_root, None, Some("base"), &provider)
            .unwrap();
    assert!(
        nongit_report.valid,
        "expected valid non-git deep glob archive dep, got: {:?}",
        nongit_report.diagnostics
    );

    // Reject `extensions/pkg/docs/milestones/team/archive/` in both non-Git and Git modes under `extensions/**/docs/milestones`.
    fs::create_dir_all(nongit_root.join("extensions/pkg/docs/milestones/team/archive")).unwrap();
    fs::write(
        nongit_root.join("extensions/pkg/docs/milestones/team/archive/AC-022-sub.md"),
        "---\nid: AC-022\ntitle: Sub\nstatus: completed\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        nongit_root.join("docs/milestones/AC-021-consumer.md"),
        "---\nid: AC-021\ntitle: Consumer\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [AC-022]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let nongit_sub_report =
        acdd_state::validate::validate_workspace(nongit_root, None, Some("base"), &provider)
            .unwrap();
    assert!(
        !nongit_sub_report.valid,
        "non-git mode must reject team/archive/ under extensions/**/docs/milestones"
    );

    let _ = init_git_repo(nongit_root, "main");
    git_ok(nongit_root, &["add", "-A"]);
    git_ok(nongit_root, &["commit", "-m", "deep glob archive parity"]);
    let git_sub_report =
        acdd_state::validate::validate_workspace(nongit_root, None, Some("base"), &provider)
            .unwrap();
    assert!(
        !git_sub_report.valid,
        "git mode must also reject team/archive/ under extensions/**/docs/milestones"
    );

    // 6. Parity matrix: `docs/milestones/**` rejects `team/archive/` and literal `docs/milestones` rejects `archive/team/` in both non-Git and Git modes.
    for (pat, bad_archive_rel, target_id) in [
        (
            "docs/milestones/**",
            "docs/milestones/team/archive/AC-030.md",
            "AC-030",
        ),
        (
            "docs/milestones",
            "docs/milestones/archive/team/AC-031.md",
            "AC-031",
        ),
    ] {
        let p_tmp = make_temp_dir("archive-parity");
        let p_root = p_tmp.path();
        fs::create_dir_all(p_root.join("docs/milestones")).unwrap();
        let bad_abs = p_root.join(bad_archive_rel);
        fs::create_dir_all(bad_abs.parent().unwrap()).unwrap();
        fs::write(
            &bad_abs,
            format!("---\nid: {target_id}\ntitle: Bad\nstatus: completed\n---\n### task: T1\n```yaml\ntask_ref: T1\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n"),
        )
        .unwrap();
        fs::write(
            p_root.join("acdd.yaml"),
            format!("roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    path: {pat}\n    prefixes: [AC]\n    digits: 3\n"),
        )
        .unwrap();
        fs::write(
            p_root.join("docs/milestones/AC-032-consumer.md"),
            format!("---\nid: AC-032\ntitle: Consumer\nstatus: planned\n---\n### task: T1\n```yaml\ntask_ref: T1\ndepends_on: [{target_id}]\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n"),
        )
        .unwrap();
        assert!(
            !acdd_state::validate::validate_workspace(p_root, None, Some("base"), &provider)
                .unwrap()
                .valid,
            "non-git mode must reject {bad_archive_rel} under {pat}"
        );
        let _ = init_git_repo(p_root, "main");
        git_ok(p_root, &["add", "-A"]);
        git_ok(p_root, &["commit", "-m", "parity"]);
        assert!(
            !acdd_state::validate::validate_workspace(p_root, None, Some("base"), &provider)
                .unwrap()
                .valid,
            "git mode must reject {bad_archive_rel} under {pat}"
        );
    }
}
