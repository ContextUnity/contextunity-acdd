use super::common::{base_config, build_provider_with_mock, git_ok, init_git_repo};
use acdd_core::config::{load_adapter, ACDD_CONFIG_FILE};
use acdd_core::errors::ErrorCode;
use acdd_core::format::sha256_hex;
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use acdd_core::path_policy::{normalize_binary_extensions, WorkspacePathPolicy};
use acdd_state::git_ops::commit::create_scoped_commit_with_expected;
use acdd_state::git_ops::*;
use acdd_state::store::sqlite::SqliteStateReader;
use acdd_state::types::IsolationMode;
use acdd_state::usecases::lifecycle::build_path_policy;
use acdd_state::usecases::{SnapshotAction, SnapshotReq, StateCommand, StateEngine};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn git_dir_path(worktree: &Path) -> PathBuf {
    let raw = git_ok(worktree, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    canonicalize_macos_var(Path::new(raw.trim()))
}

fn init_test_repo(dir: &Path) -> CommitOid {
    init_git_repo(dir, "main")
}

#[cfg(unix)]
fn install_hook(repo: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let hooks_dir = git_dir_path(repo).join("hooks");
    fs::create_dir_all(&hooks_dir).expect("hooks dir");
    let hook_path = hooks_dir.join(name);
    fs::write(&hook_path, script).expect("write hook");
    let mut perms = fs::metadata(&hook_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms).expect("chmod");
}

#[test]
fn worktree_layout_macos_var_symlink_and_snapshot_guards_matrix() {
    let tmp = TempDir::new().expect("tempdir");

    // 1. /var -> /private/var symlink alias + linked worktree git-common-dir resolution.
    let private_repo = tmp.path().join("private/var/repo");
    let _base_head = init_test_repo(&private_repo);
    #[cfg(unix)]
    let aliased_repo = {
        let alias_var = tmp.path().join("var");
        std::os::unix::fs::symlink(tmp.path().join("private/var"), &alias_var).expect("symlink var");
        alias_var.join("repo")
    };
    #[cfg(not(unix))]
    let aliased_repo = private_repo.clone();

    assert_eq!(
        canonicalize_macos_var(&aliased_repo),
        private_repo.canonicalize().expect("canonical private repo")
    );
    let primary_layout = resolve_repo_layout(&aliased_repo).expect("primary layout");
    assert_eq!(
        primary_layout.primary_worktree,
        private_repo.canonicalize().expect("canonical")
    );
    assert_eq!(
        primary_layout.git_common_dir,
        primary_layout.primary_worktree.join(".git")
    );
    assert_eq!(
        primary_layout.repo_lock_path,
        primary_layout.git_common_dir.join("acdd/repo.lock")
    );

    // Create managed run worktree with uncommitted spec_path copied over.
    let run_id = RunId::new("run-wt-01").expect("run_id");
    fs::create_dir_all(private_repo.join("docs/milestones")).expect("docs dir");
    fs::write(
        private_repo.join("docs/milestones/AC-001.md"),
        "# Uncommitted Spec\n",
    )
    .expect("write spec");

    let wt_dir = tmp.path().join("private/var/worktrees/run-wt-01");
    let created_wt = create_run_worktree(&CreateWorktreeReq {
        primary_worktree: aliased_repo.clone(),
        worktree_path: wt_dir.clone(),
        branch_name: "milestone/AC-001".to_owned(),
        base_ref: "main".to_owned(),
        copy_uncommitted_paths: vec!["docs/milestones/AC-001.md".to_owned()],
    })
    .expect("create run worktree");
    assert!(created_wt.worktree.join("docs/milestones/AC-001.md").is_file());

    let linked_layout = resolve_repo_layout(&created_wt.worktree).expect("linked layout");
    assert_eq!(linked_layout.primary_worktree, primary_layout.primary_worktree);
    assert_eq!(linked_layout.git_common_dir, primary_layout.git_common_dir);
    assert_eq!(linked_layout.repo_lock_path, primary_layout.repo_lock_path);
    assert_eq!(linked_layout.backup_lock_path, primary_layout.backup_lock_path);
    assert_eq!(linked_layout.traces_lock_path, primary_layout.traces_lock_path);
    assert_eq!(linked_layout.state_bak_path, primary_layout.state_bak_path);
    assert_eq!(linked_layout.target_worktree, created_wt.worktree);

    // 2. Scoped tree + InPlace vs Worktree diff.
    let policy = WorkspacePathPolicy::new(
        ["docs/milestones/AC-001.md".to_owned()],
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );
    fs::create_dir_all(created_wt.worktree.join(".acdd")).expect(".acdd");
    fs::write(created_wt.worktree.join(".acdd/state.sqlite"), "db-bytes").expect("db");

    let full_base_tree = current_scope_tree(&created_wt.worktree, None, &policy).expect("full base tree");
    let base_scope_tree = current_scope_tree(
        &created_wt.worktree,
        Some(&["src/**".to_owned()]),
        &policy,
    )
    .expect("base scope tree");
    assert_ne!(full_base_tree, base_scope_tree);
    assert!(
        blob_oid_at(
            &created_wt.worktree,
            created_wt.head_commit.as_str(),
            "src/lib.rs"
        )
        .expect("blob check")
        .is_some()
    );

    // Sequential in_place diff_scope_changed_paths ignores prior task's uncommitted changes in claim_tree.
    fs::write(created_wt.worktree.join("src/task_a.rs"), "pub fn a() {}\n").expect("task a");
    let wt_changed = diff_scope_changed_paths(
        &created_wt.worktree,
        IsolationMode::Worktree,
        Some(&created_wt.head_commit),
        None,
        &policy,
    )
    .expect("worktree diff");
    assert_eq!(wt_changed, vec!["src/task_a.rs".to_owned()]);

    let claim_tree_b = current_scope_tree(&created_wt.worktree, None, &policy).expect("claim tree b");
    fs::write(created_wt.worktree.join("src/task_b.rs"), "pub fn b() {}\n").expect("task b");
    let in_place_changed = diff_scope_changed_paths(
        &created_wt.worktree,
        IsolationMode::InPlace,
        None,
        Some(&claim_tree_b),
        &policy,
    )
    .expect("in_place diff");
    assert_eq!(in_place_changed, vec!["src/task_b.rs".to_owned()]);

    // 3. Table-driven binary / submodule / >16 MiB guards matrix under configured opt-in limits.
    let strict_policy = policy
        .clone()
        .with_snapshot_limits(Some(16 * 1024 * 1024), Some(BTreeSet::new()));
    enum GuardKind {
        Binary,
        Large,
        NestedGit,
    }
    let guard_cases = [
        ("binary", "src/evil.bin", GuardKind::Binary),
        ("gt_16mib", "src/huge.txt", GuardKind::Large),
        ("submodule", "src/nested_sub", GuardKind::NestedGit),
    ];
    for (label, rel, kind) in guard_cases {
        let p = created_wt.worktree.join(rel);
        match kind {
            GuardKind::Binary => {
                fs::write(&p, b"abc\x00def").expect("write binary");
            }
            GuardKind::Large => {
                let f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&p)
                    .expect("create sparse");
                f.set_len(16 * 1024 * 1024 + 1).expect("set_len >16 MiB");
            }
            GuardKind::NestedGit => {
                fs::create_dir_all(&p).expect("nested dir");
                git_ok(&p, &["init"]);
                fs::write(p.join("x.txt"), "hi").expect("nested file");
            }
        }
        let err = current_scope_tree(
            &created_wt.worktree,
            Some(&["src/**".to_owned()]),
            &strict_policy,
        )
        .expect_err(&format!("guard {label} should fail"));
        assert_eq!(err.code, ErrorCode::ExternalGitDrift, "{label}: {err}");
        if p.is_dir() {
            fs::remove_dir_all(&p).expect("cleanup dir");
        } else {
            fs::remove_file(&p).expect("cleanup file");
        }
    }

    // 4. Cleanup run refs across primary + separate linked repo (including legacy v0.6.0 snapshot/keep refs) & remove worktree/branch.
    let linked_repo = tmp.path().join("linked-repo");
    let linked_head = init_test_repo(&linked_repo);
    git_ok(
        &created_wt.worktree,
        &[
            "update-ref",
            "refs/acdd/runs/run-wt-01/state",
            created_wt.head_commit.as_str(),
        ],
    );
    // Cleanup deletes this observed OID through update-ref's compare-and-delete form.
    git_ok(
        &created_wt.worktree,
        &["update-ref", "refs/acdd/runs/run-wt-01/cas", created_wt.head_commit.as_str()],
    );
    git_ok(
        &created_wt.worktree,
        &[
            "update-ref",
            "refs/acdd/snapshots/run-wt-01/task-t1/pre",
            created_wt.head_commit.as_str(),
        ],
    );
    git_ok(
        &created_wt.worktree,
        &[
            "update-ref",
            "refs/acdd/keep/run-wt-01/k1",
            created_wt.head_commit.as_str(),
        ],
    );
    git_ok(
        &created_wt.worktree,
        &[
            "update-ref",
            "refs/acdd/snapshots/run-wt-02/state",
            created_wt.head_commit.as_str(),
        ],
    );
    git_ok(
        &linked_repo,
        &[
            "update-ref",
            "refs/acdd/runs/run-wt-01/state",
            linked_head.as_str(),
        ],
    );

    let cleaned = cleanup_run_refs(
        None,
        &[created_wt.worktree.clone(), linked_repo.clone()],
        &run_id,
    )
    .expect("cleanup refs");
    assert_eq!(cleaned.len(), 5);
    assert_eq!(
        git_ok(&private_repo, &["for-each-ref", "--format=%(refname)", "refs/acdd/"]).trim(),
        "refs/acdd/snapshots/run-wt-02/state"
    );
    assert!(git_ok(&linked_repo, &["for-each-ref", "refs/acdd/"]).is_empty());

    remove_worktree_and_branch(
        &private_repo,
        Some(&created_wt.worktree),
        Some("milestone/AC-001"),
        true,
    )
    .expect("remove worktree and branch");
    assert!(!created_wt.worktree.exists());
}

#[test]
fn scoped_commit_plumbing_and_archive_matrix() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    let base_head = init_test_repo(&repo);
    let run_id = RunId::new("run-commit-01").expect("run_id");
    let policy = WorkspacePathPolicy::new(
        [
            "docs/milestones/AC-002.md".to_owned(),
            "docs/milestones/archive/AC-002.md".to_owned(),
        ],
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );

    // 1. MainBranchProtected on main (unless allow_main: true).
    fs::write(repo.join("src/lib.rs"), "pub fn base() -> i32 { 2 }\n").expect("edit");
    let protected_err = create_scoped_commit(&ScopedCommitReq {
        worktree: repo.clone(),
        run_id: run_id.clone(),
        node_id: "task:t1:deliver".to_owned(),
        claim_revision: 1,
        base_head: base_head.clone(),
        admitted_scope: vec!["src/**".to_owned()],
        path_policy: policy.clone(),
        message: "deliver t1".to_owned(),
        allow_main: false,
    })
    .expect_err("main protected");
    assert_eq!(protected_err.code, ErrorCode::MainBranchProtected);

    // Switch to task branch for remaining commit checks.
    git_ok(&repo, &["checkout", "-b", "task/t1"]);
    // Dirty unrelated file out of scope: must NEVER be committed or staged!
    fs::write(repo.join("README.md"), "# Dirty Out Of Scope\n").expect("dirty readme");
    fs::write(repo.join("untracked_out.txt"), "keep out\n").expect("untracked");

    // 2. Plumbing commit bypasses mutating pre-commit hook and syncs .git/index only for committed paths.
    #[cfg(unix)]
    install_hook(
        &repo,
        "pre-commit",
        "#!/bin/sh\nblob=$(printf 'hooked' | git hash-object -w --stdin)\ngit update-index --add --cacheinfo 100644,\"$blob\",src/lib.rs\nexit 0\n",
    );

    let plumbing_resp = create_scoped_commit(&ScopedCommitReq {
        worktree: repo.clone(),
        run_id: run_id.clone(),
        node_id: "task:t1:deliver".to_owned(),
        claim_revision: 1,
        base_head: base_head.clone(),
        admitted_scope: vec!["src/**".to_owned()],
        path_policy: policy.clone(),
        message: "deliver t1 plumbing".to_owned(),
        allow_main: false,
    })
    .expect("plumbing commit");
    let plumbing_oid = plumbing_resp.committed_oid.clone().expect("oid");
    assert!(!plumbing_resp.recovered_idempotently);
    assert_eq!(
        git_ok(&repo, &["show", &format!("{plumbing_oid}:src/lib.rs")]),
        "pub fn base() -> i32 { 2 }"
    );
    assert_eq!(
        git_ok(&repo, &["show", &format!("{plumbing_oid}:README.md")]),
        "# Test Repo"
    );
    let msg = git_ok(&repo, &["show", "-s", "--format=%B", plumbing_oid.as_str()]);
    assert!(msg.contains("Acdd-Claim: run-commit-01/task:t1:deliver/1"));
    // Default index synced for src/lib.rs, README.md still modified unstaged.
    let status = git_ok(&repo, &["status", "--porcelain"]);
    assert!(status.contains(" M README.md"), "status: {status}");
    assert!(!status.contains("M  src/lib.rs"), "status: {status}");

    // 3. Broad `**` scope never stages engine-managed spec/archive paths, and concurrent drift fails closed with TaskCandidateMismatch.
    fs::create_dir_all(repo.join("docs/milestones")).expect("mkdir");
    fs::write(repo.join("docs/milestones/AC-002.md"), "---\nid: AC-002\ntitle: S\nstatus: active\n---\n").expect("spec");
    fs::write(repo.join("src/broad.rs"), "pub fn broad() {}\n").expect("broad");
    let broad_tree = current_scope_tree(&repo, None, &policy).expect("broad tree");
    let broad_commit = create_scoped_commit_with_expected(
        &ScopedCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "task:t1:broad".to_owned(),
            claim_revision: 4,
            base_head: plumbing_oid,
            admitted_scope: vec!["**".to_owned()],
            path_policy: policy.clone(),
            message: "broad commit".to_owned(),
            allow_main: false,
        },
        Some(&broad_tree),
    )
    .expect("broad commit");
    let broad_oid = broad_commit.committed_oid.expect("broad oid");
    let tree = snapshot::resolve_git_tree_oid(&repo, broad_oid.as_str()).unwrap();
    let expected = snapshot::list_git_tree_entries(&repo, "HEAD", "src").unwrap();
    for reference in [tree.as_str(), broad_oid.as_str(), "HEAD"] {
        assert_eq!(snapshot::list_git_tree_entries(&repo, reference, "src").unwrap(), expected);
    }
    for (reference, directory, code) in [
        ("-invalid", "src", ErrorCode::ProfileSchemaInvalid),
        (tree.as_str(), "../src", ErrorCode::ProfileSchemaInvalid),
        ("0000000000000000000000000000000000000000", "src", ErrorCode::ExternalGitDrift),
        ("missing-reference", "src", ErrorCode::ExternalGitDrift),
    ] {
        assert_eq!(snapshot::list_git_tree_entries(&repo, reference, directory).unwrap_err().code, code);
    }
    assert!(
        blob_oid_at(&repo, broad_oid.as_str(), "docs/milestones/AC-002.md")
            .expect("blob check")
            .is_none(),
        "broad `**` scope must never stage engine-managed spec path"
    );

    fs::write(repo.join("src/broad.rs"), "pub fn concurrently_edited() {}\n").expect("concurrent edit");
    let commit_mismatch = create_scoped_commit_with_expected(
        &ScopedCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "task:t1:drift".to_owned(),
            claim_revision: 5,
            base_head: broad_oid.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "stale commit".to_owned(),
            allow_main: false,
        },
        Some(&broad_tree),
    )
    .unwrap_err();
    assert_eq!(commit_mismatch.code, ErrorCode::TaskCandidateMismatch);
    assert_eq!(current_head_commit(&repo).expect("head unchanged"), broad_oid);

    // 4. Archive commit moves spec to archive/ cleanly even with pre-commit hook present.
    let archive_md = "# Archived Spec\n";
    let arch_resp = archive_spec_with_git(&ArchiveCommitReq {
        worktree: repo.clone(),
        run_id,
        node_id: "root:handoff".to_owned(),
        claim_revision: 1,
        run_base_head: Some(base_head),
        spec_relpath: "docs/milestones/AC-002.md".to_owned(),
        archive_relpath: "docs/milestones/archive/AC-002.md".to_owned(),
        rendered_archive_markdown: archive_md.to_owned(),
        rendered_archive_digest: sha256_hex(archive_md.as_bytes()),
        should_commit: true,
        allow_main: false,
        commit_message: "archive AC-002".to_owned(),
    })
    .expect("archive success");
    assert!(arch_resp.committed_oid.is_some());
    assert!(!repo.join("docs/milestones/AC-002.md").exists());
    assert!(repo.join("docs/milestones/archive/AC-002.md").is_file());
}

#[test]
fn integrate_rebase_ff_conflict_abort_and_completeness_matrix() {
    let run_id = RunId::new("run-int-01").expect("run_id");
    let policy = WorkspacePathPolicy::default();

    // 1. Happy-path rebase-ff integration with Acdd-Integrated trailer and completeness check.
    {
        let tmp = TempDir::new().expect("tempdir");
        let primary = tmp.path().join("primary");
        let _main_head = init_test_repo(&primary);

        let run_wt = tmp.path().join("run-wt");
        let created_run = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: run_wt,
            branch_name: "milestone/run-0".to_owned(),
            base_ref: "main".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("run wt");

        let task_wt = tmp.path().join("task-wt");
        let task_branch = "task/t-0".to_owned();
        let created_task = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: task_wt,
            branch_name: task_branch.clone(),
            base_ref: "milestone/run-0".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("task wt");

        let initial_claim_tree =
            current_scope_tree(&created_task.worktree, None, &policy).expect("claim tree");
        fs::write(
            created_task.worktree.join("src/feature_0.rs"),
            "pub fn f0() -> i32 { 0 }\n",
        )
        .expect("write task file");
        let accepted_tree =
            current_scope_tree(&created_task.worktree, None, &policy).expect("accepted tree");
        create_scoped_commit(&ScopedCommitReq {
            worktree: created_task.worktree.clone(),
            run_id: run_id.clone(),
            node_id: "task:t0:deliver".to_owned(),
            claim_revision: 1,
            base_head: created_task.head_commit.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "task 0".to_owned(),
            allow_main: false,
        })
        .expect("commit task");

        // Advance run_wt with a non-conflicting commit so rebase-ff must rebase + fast-forward.
        fs::write(
            created_run.worktree.join("src/unrelated_run.rs"),
            "pub fn u() {}\n",
        )
        .expect("unrelated");
        git_ok(&created_run.worktree, &["add", "src/unrelated_run.rs"]);
        git_ok(&created_run.worktree, &["commit", "-m", "run advance"]);

        let task_ref = TaskRef::new("t0").expect("task ref");
        let resp = integrate_task_branch(&IntegrateTaskReq {
            target_worktree: created_run.worktree.clone(),
            source_worktree: Some(created_task.worktree.clone()),
            source_branch: task_branch,
            run_id: run_id.clone(),
            task_ref: task_ref.clone(),
            accepted_tree: accepted_tree.clone(),
            allow_main: false,
        })
        .expect("integrate rebase-ff");

        let check_input = [TaskIntegrationCheckInput {
            task_ref: task_ref.clone(),
            initial_claim_tree,
            accepted_tree: accepted_tree.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            downstream_tasks: vec![],
        }];
        let missing = verify_tasks_integrated(&run_id, &created_run.worktree, &check_input)
            .expect("verify integrated");

        let landed = resp.integrated_commit.expect("landed commit");
        assert_eq!(missing, Vec::<TaskRef>::new());
        let msg = git_ok(
            &created_run.worktree,
            &["show", "-s", "--format=%B", landed.as_str()],
        );
        let expected_trailer =
            format!("Acdd-Integrated: {}/{}/{}", run_id, task_ref, accepted_tree);
        assert!(msg.contains(&expected_trailer), "{msg}");
    }

    // 2. Conflict abort cleanly restores worktrees on rebase-ff conflict.
    {
        let tmp = TempDir::new().expect("tempdir");
        let primary = tmp.path().join("primary");
        let _ = init_test_repo(&primary);
        let run_wt = tmp.path().join("run-wt");
        let created_run = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: run_wt,
            branch_name: "milestone/conflict".to_owned(),
            base_ref: "main".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("run wt");
        let task_wt = tmp.path().join("task-wt");
        let created_task = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: task_wt,
            branch_name: "task/conflict".to_owned(),
            base_ref: "milestone/conflict".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("task wt");

        fs::write(created_task.worktree.join("src/lib.rs"), "pub fn task_side() {}\n").expect("t");
        let accepted_tree =
            current_scope_tree(&created_task.worktree, None, &policy).expect("accepted");
        git_ok(&created_task.worktree, &["add", "src/lib.rs"]);
        git_ok(&created_task.worktree, &["commit", "-m", "task conflict"]);

        fs::write(created_run.worktree.join("src/lib.rs"), "pub fn run_side() {}\n").expect("r");
        git_ok(&created_run.worktree, &["add", "src/lib.rs"]);
        git_ok(&created_run.worktree, &["commit", "-m", "run conflict"]);

        let err = integrate_task_branch(&IntegrateTaskReq {
            target_worktree: created_run.worktree.clone(),
            source_worktree: Some(created_task.worktree.clone()),
            source_branch: "task/conflict".to_owned(),
            run_id: run_id.clone(),
            task_ref: TaskRef::new("t-conf").expect("t"),
            accepted_tree,
            allow_main: false,
        })
        .expect_err("conflict expected");
        assert_eq!(err.code, ErrorCode::GitIntegrationConflict);
        assert!(
            err.message.contains("src/lib.rs"),
            "expected conflicting path in error: {}",
            err.message
        );
        assert!(git_ok(&created_run.worktree, &["status", "--porcelain"]).is_empty());
        assert!(git_ok(&created_task.worktree, &["status", "--porcelain"]).is_empty());
        let run_gd = git_dir_path(&created_run.worktree);
        let task_gd = git_dir_path(&created_task.worktree);
        assert!(!run_gd.join("MERGE_HEAD").exists());
        assert!(!run_gd.join("SQUASH_MSG").exists());
        assert!(!task_gd.join("rebase-merge").exists());
        assert!(!task_gd.join("rebase-apply").exists());
    }

    // 3. TASK_NOT_INTEGRATED revert detection & transitive downstream takeover.
    let tmp = TempDir::new().expect("tempdir");
    let primary = tmp.path().join("primary");
    let _ = init_test_repo(&primary);
    let run_wt = tmp.path().join("run-wt");
    let created_run = create_run_worktree(&CreateWorktreeReq {
        primary_worktree: primary.clone(),
        worktree_path: run_wt,
        branch_name: "milestone/complete-check".to_owned(),
        base_ref: "main".to_owned(),
        copy_uncommitted_paths: vec![],
    })
    .expect("run wt");

    let task_a_wt = tmp.path().join("task-a");
    let created_a = create_task_worktree(&CreateWorktreeReq {
        primary_worktree: primary.clone(),
        worktree_path: task_a_wt,
        branch_name: "task/a".to_owned(),
        base_ref: "milestone/complete-check".to_owned(),
        copy_uncommitted_paths: vec![],
    })
    .expect("wt a");
    let init_a = current_scope_tree(&created_a.worktree, None, &policy).expect("init a");
    fs::write(created_a.worktree.join("src/lib.rs"), "pub fn a() -> i32 { 10 }\n").expect("a");
    let acc_a = current_scope_tree(&created_a.worktree, None, &policy).expect("acc a");
    git_ok(&created_a.worktree, &["add", "src/lib.rs"]);
    git_ok(&created_a.worktree, &["commit", "-m", "a"]);

    let int_a = integrate_task_branch(&IntegrateTaskReq {
        target_worktree: created_run.worktree.clone(),
        source_worktree: Some(created_a.worktree.clone()),
        source_branch: "task/a".to_owned(),
        run_id: run_id.clone(),
        task_ref: TaskRef::new("t-a").expect("t-a"),
        accepted_tree: acc_a.clone(),
        allow_main: false,
    })
    .expect("integrate a");
    let merge_a = int_a.integrated_commit.expect("merge a");

    // Revert t-a back to initial_claim_tree -> verify_tasks_integrated MUST catch revert!
    git_ok(
        &created_run.worktree,
        &["revert", "--no-edit", merge_a.as_str()],
    );
    let task_a_ref = TaskRef::new("t-a").expect("t-a");
    let task_b_ref = TaskRef::new("t-b").expect("t-b");
    let missing_after_revert = verify_tasks_integrated(
        &run_id,
        &created_run.worktree,
        &[TaskIntegrationCheckInput {
            task_ref: task_a_ref.clone(),
            initial_claim_tree: init_a.clone(),
            accepted_tree: acc_a.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            downstream_tasks: vec![],
        }],
    )
    .expect("check after revert");
    assert_eq!(missing_after_revert, vec![task_a_ref.clone()]);

    // Transitive downstream task B overwrites src/lib.rs -> satisfies A via rule (c)!
    let task_b_wt = tmp.path().join("task-b");
    let created_b = create_task_worktree(&CreateWorktreeReq {
        primary_worktree: primary,
        worktree_path: task_b_wt,
        branch_name: "task/b".to_owned(),
        base_ref: "milestone/complete-check".to_owned(),
        copy_uncommitted_paths: vec![],
    })
    .expect("wt b");
    let init_b = current_scope_tree(&created_b.worktree, None, &policy).expect("init b");
    fs::write(created_b.worktree.join("src/lib.rs"), "pub fn b() -> i32 { 20 }\n").expect("b");
    let acc_b = current_scope_tree(&created_b.worktree, None, &policy).expect("acc b");
    git_ok(&created_b.worktree, &["add", "src/lib.rs"]);
    git_ok(&created_b.worktree, &["commit", "-m", "b"]);
    integrate_task_branch(&IntegrateTaskReq {
        target_worktree: created_run.worktree.clone(),
        source_worktree: Some(created_b.worktree),
        source_branch: "task/b".to_owned(),
        run_id: run_id.clone(),
        task_ref: task_b_ref.clone(),
        accepted_tree: acc_b.clone(),
        allow_main: false,
    })
    .expect("integrate b");

    let missing_with_downstream = verify_tasks_integrated(
        &run_id,
        &created_run.worktree,
        &[
            TaskIntegrationCheckInput {
                task_ref: task_a_ref.clone(),
                initial_claim_tree: init_a.clone(),
                accepted_tree: acc_a.clone(),
                admitted_scope: vec!["src/**".to_owned()],
                downstream_tasks: vec![task_b_ref.clone()],
            },
            TaskIntegrationCheckInput {
                task_ref: task_b_ref.clone(),
                initial_claim_tree: init_b.clone(),
                accepted_tree: acc_b.clone(),
                admitted_scope: vec!["src/**".to_owned()],
                downstream_tasks: vec![],
            },
        ],
    )
    .expect("check downstream");
    assert_eq!(missing_with_downstream, Vec::<TaskRef>::new());

    // Hardened Rule (c): downstream task with empty admitted_scope OR unmodified tree must NOT satisfy an unintegrated upstream task.
    let unintegrated_ref = TaskRef::new("t-unintegrated").expect("t-unint");
    for (label, ds_init, ds_acc, ds_scope) in [
        ("empty_scope", init_b.clone(), acc_b.clone(), Vec::new()),
        (
            "unmodified_downstream",
            acc_b.clone(),
            acc_b.clone(),
            vec!["src/**".to_owned()],
        ),
    ] {
        let missing_rule_c = verify_tasks_integrated(
            &run_id,
            &created_run.worktree,
            &[
                TaskIntegrationCheckInput {
                    task_ref: unintegrated_ref.clone(),
                    initial_claim_tree: init_a.clone(),
                    accepted_tree: acc_a.clone(),
                    admitted_scope: vec!["src/**".to_owned()],
                    downstream_tasks: vec![task_b_ref.clone()],
                },
                TaskIntegrationCheckInput {
                    task_ref: task_b_ref.clone(),
                    initial_claim_tree: ds_init,
                    accepted_tree: ds_acc,
                    admitted_scope: ds_scope,
                    downstream_tasks: vec![],
                },
            ],
        )
        .expect("check hardened rule c");
        assert_eq!(missing_rule_c, vec![unintegrated_ref.clone()], "{label}");
    }

    // Rule (a): manual post-integration overwrite (not reverting to initial_claim_tree) passes via Acdd-Integrated trailer.
    fs::write(created_run.worktree.join("src/lib.rs"), "pub fn drifted() -> i32 { 99 }\n")
        .expect("drift");
    git_ok(&created_run.worktree, &["add", "src/lib.rs"]);
    git_ok(&created_run.worktree, &["commit", "-m", "manual drift after integrate"]);
    let missing_after_drift = verify_tasks_integrated(
        &run_id,
        &created_run.worktree,
        &[TaskIntegrationCheckInput {
            task_ref: task_a_ref.clone(),
            initial_claim_tree: init_a.clone(),
            accepted_tree: acc_a.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            downstream_tasks: vec![],
        }],
    )
    .expect("check after drift");
    assert_eq!(missing_after_drift, Vec::<TaskRef>::new());

    // Rule (a) revert guard: reverting `src/lib.rs` back to initial_claim_tree triggers TASK_NOT_INTEGRATED.
    git_ok(&created_run.worktree, &["checkout", "main", "--", "src/lib.rs"]);
    git_ok(&created_run.worktree, &["commit", "-m", "revert src/lib.rs to initial"]);
    let missing_after_second_revert = verify_tasks_integrated(
        &run_id,
        &created_run.worktree,
        &[TaskIntegrationCheckInput {
            task_ref: task_a_ref.clone(),
            initial_claim_tree: init_a,
            accepted_tree: acc_a,
            admitted_scope: vec!["src/**".to_owned()],
            downstream_tasks: vec![],
        }],
    )
    .expect("check after revert to initial");
    assert_eq!(missing_after_second_revert, vec![task_a_ref]);

    // 4. File-mode-only `(mode, oid)` checks for Rules (b) and (c): 100644 -> 100755 without blob change.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp_mode = TempDir::new().expect("tempdir mode");
        let primary_mode = tmp_mode.path().join("primary");
        let _ = init_test_repo(&primary_mode);
        let created_run_mode = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary_mode.clone(),
            worktree_path: tmp_mode.path().join("run-wt"),
            branch_name: "milestone/mode-check".to_owned(),
            base_ref: "main".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("run wt mode");

        let created_t1 = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary_mode.clone(),
            worktree_path: tmp_mode.path().join("task-m1"),
            branch_name: "task/m1".to_owned(),
            base_ref: "milestone/mode-check".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("wt m1");
        let init_m1 = current_scope_tree(&created_t1.worktree, None, &policy).expect("init m1");
        let lib_path = created_t1.worktree.join("src/lib.rs");
        let mut perms = fs::metadata(&lib_path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&lib_path, perms).expect("chmod 755");
        let acc_m1 = current_scope_tree(&created_t1.worktree, None, &policy).expect("acc m1");
        let m1_ref = TaskRef::new("t-m1").expect("t-m1");
        let m2_ref = TaskRef::new("t-m2").expect("t-m2");

        // Before integrating t-m1, mode-only change must NOT pass Rule (b) via identical blob OID.
        let unlanded_mode = verify_tasks_integrated(
            &run_id,
            &created_run_mode.worktree,
            &[TaskIntegrationCheckInput {
                task_ref: m1_ref.clone(),
                initial_claim_tree: init_m1.clone(),
                accepted_tree: acc_m1.clone(),
                admitted_scope: vec!["src/**".to_owned()],
                downstream_tasks: vec![],
            }],
        )
        .expect("verify unlanded mode");
        assert_eq!(unlanded_mode, vec![m1_ref.clone()]);

        // Downstream task t-m2 applies a mode-only change (`100644 -> 100755`) and integrates -> Rule (c) covers t-m1!
        let created_t2 = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary_mode,
            worktree_path: tmp_mode.path().join("task-m2"),
            branch_name: "task/m2".to_owned(),
            base_ref: "milestone/mode-check".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("wt m2");
        let init_m2 = current_scope_tree(&created_t2.worktree, None, &policy).expect("init m2");
        let lib_path2 = created_t2.worktree.join("src/lib.rs");
        let mut perms2 = fs::metadata(&lib_path2).expect("meta2").permissions();
        perms2.set_mode(0o755);
        fs::set_permissions(&lib_path2, perms2).expect("chmod 755 2");
        let acc_m2 = current_scope_tree(&created_t2.worktree, None, &policy).expect("acc m2");
        git_ok(&created_t2.worktree, &["add", "src/lib.rs"]);
        git_ok(&created_t2.worktree, &["commit", "-m", "chmod +x in downstream"]);
        integrate_task_branch(&IntegrateTaskReq {
            target_worktree: created_run_mode.worktree.clone(),
            source_worktree: Some(created_t2.worktree),
            source_branch: "task/m2".to_owned(),
            run_id: run_id.clone(),
            task_ref: m2_ref.clone(),
            accepted_tree: acc_m2.clone(),
            allow_main: false,
        })
        .expect("integrate m2");

        let covered_mode = verify_tasks_integrated(
            &run_id,
            &created_run_mode.worktree,
            &[
                TaskIntegrationCheckInput {
                    task_ref: m1_ref,
                    initial_claim_tree: init_m1,
                    accepted_tree: acc_m1,
                    admitted_scope: vec!["src/**".to_owned()],
                    downstream_tasks: vec![m2_ref.clone()],
                },
                TaskIntegrationCheckInput {
                    task_ref: m2_ref,
                    initial_claim_tree: init_m2,
                    accepted_tree: acc_m2,
                    admitted_scope: vec!["src/**".to_owned()],
                    downstream_tasks: vec![],
                },
            ],
        )
        .expect("verify covered mode");
        assert_eq!(covered_mode, Vec::<TaskRef>::new());
    }

    // 5. Rebase-ff integration failure on dirty tracked local edits must NEVER wipe them.
    let tmp_dirty = TempDir::new().expect("tempdir dirty");
    let primary_dirty = tmp_dirty.path().join("primary");
    let _ = init_test_repo(&primary_dirty);
    let created_run_dirty = create_run_worktree(&CreateWorktreeReq {
        primary_worktree: primary_dirty.clone(),
        worktree_path: tmp_dirty.path().join("run-dirty"),
        branch_name: "milestone/dirty-rebase".to_owned(),
        base_ref: "main".to_owned(),
        copy_uncommitted_paths: vec![],
    })
    .expect("run wt dirty");
    let created_task_dirty = create_task_worktree(&CreateWorktreeReq {
        primary_worktree: primary_dirty,
        worktree_path: tmp_dirty.path().join("task-dirty"),
        branch_name: "task/dirty".to_owned(),
        base_ref: "milestone/dirty-rebase".to_owned(),
        copy_uncommitted_paths: vec![],
    })
    .expect("task wt dirty");
    fs::write(created_task_dirty.worktree.join("src/lib.rs"), "pub fn task_edit() {}\n").expect("t");
    let dirty_acc = current_scope_tree(&created_task_dirty.worktree, None, &policy).expect("acc");
    git_ok(&created_task_dirty.worktree, &["add", "src/lib.rs"]);
    git_ok(&created_task_dirty.worktree, &["commit", "-m", "task dirty"]);

    fs::write(created_run_dirty.worktree.join("src/lib.rs"), "pub fn local_dirty_edit() {}\n")
        .expect("local dirty");
    let dirty_err = integrate_task_branch(&IntegrateTaskReq {
        target_worktree: created_run_dirty.worktree.clone(),
        source_worktree: Some(created_task_dirty.worktree),
        source_branch: "task/dirty".to_owned(),
        run_id,
        task_ref: TaskRef::new("t-dirty").expect("t-dirty"),
        accepted_tree: dirty_acc,
        allow_main: false,
    })
    .unwrap_err();
    assert_eq!(dirty_err.code, ErrorCode::GitIntegrationConflict);
    assert_eq!(
        fs::read_to_string(created_run_dirty.worktree.join("src/lib.rs")).expect("read preserved"),
        "pub fn local_dirty_edit() {}\n",
        "integration failure must preserve pre-existing tracked local edits"
    );

    // #6: A cleanly staged manual `git merge --squash` (`SQUASH_MSG` with zero unmerged paths)
    // must NOT be wiped by `abort_in_progress_git_ops`.
    let manual_squash_tmp = TempDir::new().expect("manual squash tempdir");
    let mrepo = manual_squash_tmp.path();
    let _ = init_test_repo(mrepo);
    fs::write(mrepo.join("src/lib.rs"), "pub fn staged_squash() {}\n").unwrap();
    git_ok(mrepo, &["add", "src/lib.rs"]);
    let m_git_dir = mrepo.join(git_ok(mrepo, &["rev-parse", "--git-dir"]).trim());
    fs::write(m_git_dir.join("SQUASH_MSG"), "manual user squash commit\n").unwrap();
    assert!(!abort_in_progress_git_ops(mrepo).expect("abort check"));
    assert!(m_git_dir.join("SQUASH_MSG").is_file());
    assert!(git_ok(mrepo, &["diff", "--cached", "--name-only"]).contains("src/lib.rs"));
}

#[test]
fn binary_files_and_snapshot_guards_matrix() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    let _init_head = init_test_repo(&repo);
    git_ok(&repo, &["checkout", "-b", "task/binaries"]);

    fs::create_dir_all(repo.join("assets/locale/uk/LC_MESSAGES")).expect("locale dir");
    fs::create_dir_all(repo.join("data")).expect("data dir");
    let initial_mo = b"\xde\x12\x04\x95\x00initial-mo";
    let initial_png = b"\x89PNG\r\n\x1a\n\x00initial-png";
    let initial_bin = b"\x00\x01\x02initial-committed-bin";
    fs::write(
        repo.join("assets/locale/uk/LC_MESSAGES/django.mo"),
        initial_mo,
    )
    .expect("write mo");
    fs::write(repo.join("assets/logo.png"), initial_png).expect("write png");
    fs::write(repo.join("data/committed.bin"), initial_bin).expect("write committed bin");
    git_ok(
        &repo,
        &[
            "add",
            "assets/locale/uk/LC_MESSAGES/django.mo",
            "assets/logo.png",
            "data/committed.bin",
        ],
    );
    git_ok(&repo, &["commit", "-m", "seed tracked binaries"]);

    let cfg = base_config(&repo);
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).expect("open engine");
    let policy = WorkspacePathPolicy::default();
    let run_id = RunId::new("run-bin-01").expect("run_id");
    let mut head = current_head_commit(&repo).expect("head");

    // 1. Positive contract matrix: tracked (clean + modified) and newly added untracked NUL binaries
    // succeed across current_scope_tree, create_scoped_commit, and snapshot save/restore.
    struct BinaryCase {
        label: &'static str,
        rel: &'static str,
        saved_bytes: &'static [u8],
        mutated_bytes: &'static [u8],
        scope_pat: &'static str,
    }
    let binary_cases = [
        BinaryCase {
            label: "tracked_mo_modified",
            rel: "assets/locale/uk/LC_MESSAGES/django.mo",
            saved_bytes: b"\xde\x12\x04\x95\x00updated-mo",
            mutated_bytes: b"\xde\x12\x04\x95\x00mutated-mo",
            scope_pat: "assets/**",
        },
        BinaryCase {
            label: "tracked_bin_modified",
            rel: "data/committed.bin",
            saved_bytes: b"\x00\xffupdated-bin",
            mutated_bytes: b"\x00\xfeother-bin",
            scope_pat: "data/**",
        },
        BinaryCase {
            label: "untracked_jpg_added",
            rel: "assets/photo.jpg",
            saved_bytes: b"\xff\xd8\xff\xe0\x00\x10JFIF",
            mutated_bytes: b"\xff\xd8\xff\xe0\x00\x11MUTATED",
            scope_pat: "assets/**",
        },
        BinaryCase {
            label: "untracked_bin_added",
            rel: "src/payload.bin",
            saved_bytes: b"abc\x00def\x00123",
            mutated_bytes: b"abc\x00mutated",
            scope_pat: "src/**",
        },
    ];

    for (rev, case) in (1_u32..).zip(binary_cases) {
        let abs = repo.join(case.rel);
        fs::write(&abs, case.saved_bytes).expect("write saved binary");

        let _full = current_scope_tree(&repo, None, &policy).expect(case.label);
        let scoped =
            current_scope_tree(&repo, Some(&[case.scope_pat.to_owned()]), &policy).expect(case.label);
        assert_ne!(
            scoped,
            current_scope_tree(&repo, Some(&["README.md".to_owned()]), &policy).expect(case.label)
        );

        let snap_name = format!("snap-{rev}");
        let snapshot_cmd = |action| {
            engine.execute(StateCommand::Snapshot(SnapshotReq {
                run: None,
                action,
                task_ref: None,
                all: false,
                revision: None,
                name: Some(snap_name.clone()),
                worktree: Some(repo.clone()),
                older_than: None,
            }))
        };
        snapshot_cmd(SnapshotAction::Save).expect("save binary snapshot");
        fs::write(&abs, case.mutated_bytes).expect("mutate binary");
        snapshot_cmd(SnapshotAction::Restore).expect("restore binary snapshot");
        assert_eq!(
            fs::read(&abs).expect("read restored"),
            case.saved_bytes,
            "{}",
            case.label
        );

        let resp = create_scoped_commit_with_expected(
            &ScopedCommitReq {
                worktree: repo.clone(),
                run_id: run_id.clone(),
                node_id: format!("task:bin:{rev}"),
                claim_revision: rev,
                base_head: head.clone(),
                admitted_scope: vec![case.scope_pat.to_owned()],
                path_policy: policy.clone(),
                message: case.label.to_owned(),
                allow_main: false,
            },
            Some(&scoped),
        )
        .expect(case.label);
        head = resp.committed_oid.expect("committed oid");
        assert_eq!(
            git_command_bytes(&repo, head.as_str(), case.rel),
            case.saved_bytes,
            "{}",
            case.label
        );
    }

    // 2. Default policy keeps size ceiling disabled (>16 MiB passes), while structural guards always fail.
    let huge_path = repo.join("src/large_default.bin");
    {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&huge_path)
            .expect("open large default");
        f.set_len(16 * 1024 * 1024 + 1).expect("set_len >16 MiB");
    }
    current_scope_tree(&repo, Some(&["src/large_default.bin".to_owned()]), &policy)
        .expect("default policy disables size ceiling");
    fs::remove_file(&huge_path).expect("cleanup large default");

    // 3. Real `acdd.yaml` opt-in limits (`max_snapshot_file_bytes` + `allowed_binary_extensions`)
    // plus `Some([])` strict-all-binary mode and unconditional structural guards.
    fs::write(
        repo.join(ACDD_CONFIG_FILE),
        "limits:\n  max_snapshot_file_bytes: 16777216\n  allowed_binary_extensions:\n    - \".PNG\"\n    - mo\n",
    )
    .expect("write acdd.yaml");
    let adapter = load_adapter(&repo, None).expect("load_adapter");
    let mut cfg = base_config(&repo);
    cfg.max_snapshot_file_bytes = adapter.limits.max_snapshot_file_bytes;
    cfg.allowed_binary_extensions = adapter
        .limits
        .allowed_binary_extensions
        .as_ref()
        .map(normalize_binary_extensions)
        .transpose()
        .expect("normalize extensions");
    let reader = SqliteStateReader::new(cfg.resolved_state_db_path().0);
    let configured_policy = build_path_policy(&cfg, &reader, &[], None, None).expect("build_path_policy");
    assert_eq!(configured_policy.max_snapshot_file_bytes, Some(16 * 1024 * 1024));
    assert_eq!(
        configured_policy.allowed_binary_extensions,
        Some(BTreeSet::from(["mo".to_owned(), "png".to_owned()]))
    );

    // Allowed `.png` & `.mo` pass under configured policy; `.jpg`/`.bin` & `>16 MiB` fail closed.
    current_scope_tree(
        &repo,
        Some(&[
            "assets/logo.png".to_owned(),
            "assets/locale/**".to_owned(),
        ]),
        &configured_policy,
    )
    .expect("allowed .png/.mo pass");
    let reject_all_policy = policy
        .clone()
        .with_snapshot_limits(Some(16 * 1024 * 1024), Some(BTreeSet::new()));

    enum FailKind {
        DisallowedBin,
        RejectAllBin,
        Gt16MibBinary,
        NestedGit,
        Gitlink160000,
        #[cfg(unix)]
        SymlinkEscape,
    }
    let fail_cases = [
        ("disallowed_bin_extension", "src/payload.bin", FailKind::DisallowedBin),
        ("empty_allowlist_rejects_png", "assets/logo.png", FailKind::RejectAllBin),
        ("gt_16mib_binary", "src/huge.bin", FailKind::Gt16MibBinary),
        ("nested_git", "src/nested_sub", FailKind::NestedGit),
        ("gitlink_160000", "src/submod", FailKind::Gitlink160000),
        #[cfg(unix)]
        ("symlink_escape", "assets/escape_dir/escape.png", FailKind::SymlinkEscape),
    ];
    for (label, rel, kind) in fail_cases {
        let abs = repo.join(rel);
        let active_policy = match kind {
            FailKind::DisallowedBin => &configured_policy,
            FailKind::RejectAllBin => &reject_all_policy,
            FailKind::Gt16MibBinary => {
                let f = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&abs)
                    .expect("open large binary");
                f.set_len(16 * 1024 * 1024 + 1).expect("set_len >16 MiB");
                &configured_policy
            }
            FailKind::NestedGit => {
                fs::create_dir_all(&abs).expect("nested dir");
                git_ok(&abs, &["init"]);
                fs::write(abs.join("x.txt"), "hi").expect("nested file");
                &policy
            }
            FailKind::Gitlink160000 => {
                git_ok(
                    &repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},{rel}", head.as_str()),
                    ],
                );
                &policy
            }
            #[cfg(unix)]
            FailKind::SymlinkEscape => {
                let outside = tmp.path().join("outside");
                fs::create_dir_all(&outside).expect("outside dir");
                fs::write(outside.join("escape.png"), b"\x89PNG\x00escape").expect("outside png");
                std::os::unix::fs::symlink(&outside, repo.join("assets/escape_dir"))
                    .expect("symlink dir");
                &policy
            }
        };
        let scope = vec![rel.to_owned()];
        let err = current_scope_tree(&repo, Some(&scope), active_policy).expect_err(label);
        assert_eq!(err.code, ErrorCode::ExternalGitDrift, "{label}: {err}");
        match kind {
            FailKind::DisallowedBin | FailKind::RejectAllBin => {}
            FailKind::Gt16MibBinary => fs::remove_file(&abs).expect("cleanup huge"),
            FailKind::NestedGit => fs::remove_dir_all(&abs).expect("cleanup nested"),
            FailKind::Gitlink160000 => {
                git_ok(&repo, &["rm", "--cached", "--quiet", rel]);
            }
            #[cfg(unix)]
            FailKind::SymlinkEscape => {
                fs::remove_file(repo.join("assets/escape_dir")).expect("cleanup symlink")
            }
        }
    }
}

fn git_command_bytes(repo: &Path, rev: &str, rel_path: &str) -> Vec<u8> {
    let out = acdd_core::git::git_command(repo, &["show", &format!("{rev}:{rel_path}")])
        .expect("spawn git show");
    assert!(out.status.success(), "git show failed for {rev}:{rel_path}");
    out.stdout
}
