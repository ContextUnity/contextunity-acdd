use super::commit::*;
use super::integrate::*;
use super::snapshot::*;
use super::worktree::*;
use acdd_core::errors::ErrorCode;
use acdd_core::format::sha256_hex;
use acdd_core::git::git_command;
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use acdd_core::path_policy::WorkspacePathPolicy;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git_command(dir, args).expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn init_test_repo(dir: &Path) -> CommitOid {
    fs::create_dir_all(dir.join("src")).expect("create repo dir");
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.email", "test@contextunity.local"],
        &["config", "user.name", "ACDD Test"],
    ] {
        git_ok(dir, args);
    }
    fs::write(dir.join("README.md"), "# Test Repo\n").unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\n").unwrap();
    git_ok(dir, &["add", "README.md", "src/lib.rs"]);
    git_ok(dir, &["commit", "-m", "initial commit"]);
    current_head_commit(dir).expect("initial head")
}

fn run_failpoint_child(case: &str, repo: &Path) {
    let run_id = RunId::new("run-fp-01").expect("run_id");
    let policy = WorkspacePathPolicy::new(
        [
            "docs/milestones/AC-FP.md".to_owned(),
            "docs/milestones/archive/AC-FP.md".to_owned(),
        ],
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );
    match case {
        "commit_plumbing_after_ref" => {
            let base_head = current_head_commit(repo).expect("base");
            let _ = create_scoped_commit_with_expected(
                &ScopedCommitReq {
                    worktree: repo.to_path_buf(),
                    run_id,
                    node_id: "task:t1:deliver".to_owned(),
                    claim_revision: 1,
                    base_head,
                    admitted_scope: vec!["src/**".to_owned()],
                    path_policy: policy,
                    message: "failpoint commit".to_owned(),
                    allow_main: false,
                },
                None,
            );
        }
        "integrate_rebase_conflict" => {
            let target_wt = repo.join("run-wt");
            let source_wt = repo.join("task-wt");
            let accepted_tree = current_scope_tree(&source_wt, None, &policy).expect("accepted");
            let _ = integrate_task_branch(&IntegrateTaskReq {
                target_worktree: target_wt,
                source_worktree: Some(source_wt),
                source_branch: "task/fp".to_owned(),
                run_id,
                task_ref: TaskRef::new("t1").expect("t1"),
                accepted_tree,
                allow_main: false,
            });
        }
        "archive_after_commit" | "archive_after_write_no_commit" => {
            let should_commit = case == "archive_after_commit";
            let archive_md = "# Archived Failpoint Spec\n";
            let base_raw = std::env::var("ACDD_FAILPOINT_BASE").ok();
            let run_base_head = base_raw.map(|s| CommitOid::new(s).expect("oid"));
            let _ = archive_spec_with_git(&ArchiveCommitReq {
                worktree: repo.to_path_buf(),
                run_id,
                node_id: "root:handoff".to_owned(),
                claim_revision: 1,
                run_base_head,
                spec_relpath: "docs/milestones/AC-FP.md".to_owned(),
                archive_relpath: "docs/milestones/archive/AC-FP.md".to_owned(),
                rendered_archive_markdown: archive_md.to_owned(),
                rendered_archive_digest: sha256_hex(archive_md.as_bytes()),
                should_commit,
                allow_main: false,
                commit_message: "archive failpoint".to_owned(),
            });
        }
        other => panic!("unknown failpoint child case '{other}'"),
    }
}

#[test]
fn failpoint_crash_recovery_matrix() {
    if let Ok(child_case) = std::env::var("ACDD_GIT_OPS_FAILPOINT_CHILD") {
        let repo =
            PathBuf::from(std::env::var("ACDD_FAILPOINT_REPO").expect("ACDD_FAILPOINT_REPO"));
        run_failpoint_child(&child_case, &repo);
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let run_id = RunId::new("run-fp-01").expect("run_id");
    let policy = WorkspacePathPolicy::new(
        [
            "docs/milestones/AC-FP.md".to_owned(),
            "docs/milestones/archive/AC-FP.md".to_owned(),
        ],
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );

    // 1. Commit crash recovery (plumbing after update-ref, external drift).
    {
        let (case, fp_spec) = (
            "commit_plumbing_after_ref",
            "git_commit_after_update_ref:exit",
        );
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        let base_head = init_test_repo(&repo);
        git_ok(&repo, &["checkout", "-b", "task/fp"]);
        fs::write(repo.join("src/lib.rs"), "pub fn fp() -> i32 { 42 }\n").expect("edit");

        let child = Command::new(&exe)
            .args([
                "--exact",
                "git_ops::tests::failpoint_crash_recovery_matrix",
                "--quiet",
            ])
            .env("TMPDIR", tmp.path())
            .env("ACDD_GIT_OPS_FAILPOINT_CHILD", case)
            .env("ACDD_FAILPOINT", fp_spec)
            .env("ACDD_FAILPOINT_REPO", &repo)
            .output()
            .expect("spawn failpoint child");
        assert!(!child.status.success(), "{case} child should crash");

        let crashed_head = current_head_commit(&repo).expect("crashed head");
        assert_ne!(crashed_head, base_head);

        // Retry create_scoped_commit_with_expected under repo.lock semantics -> idempotently recovers crashed commit.
        let retry = create_scoped_commit_with_expected(
            &ScopedCommitReq {
                worktree: repo.clone(),
                run_id: run_id.clone(),
                node_id: "task:t1:deliver".to_owned(),
                claim_revision: 1,
                base_head: base_head.clone(),
                admitted_scope: vec!["src/**".to_owned()],
                path_policy: policy.clone(),
                message: "failpoint commit".to_owned(),
                allow_main: false,
            },
            None,
        )
        .expect("idempotent commit recovery");
        assert!(retry.recovered_idempotently, "{case}");
        assert_eq!(retry.committed_oid, Some(crashed_head));
        assert!(git_ok(&repo, &["status", "--porcelain"]).is_empty());

        // Unrelated external commit on top of base_head -> fail-closed EXTERNAL_GIT_DRIFT.
        fs::write(repo.join("README.md"), "# External Drift\n").expect("drift");
        git_ok(&repo, &["add", "README.md"]);
        git_ok(&repo, &["commit", "-m", "external drift"]);
        let drift_err = create_scoped_commit_with_expected(
            &ScopedCommitReq {
                worktree: repo,
                run_id: run_id.clone(),
                node_id: "task:t1:deliver".to_owned(),
                claim_revision: 1,
                base_head,
                admitted_scope: vec!["src/**".to_owned()],
                path_policy: policy.clone(),
                message: "failpoint commit".to_owned(),
                allow_main: false,
            },
            None,
        )
        .expect_err("external drift expected");
        assert_eq!(drift_err.code, ErrorCode::ExternalGitDrift);
    }

    // 2. Dangling rebase-merge crash cleanup.
    {
        let case = "integrate_rebase_conflict";
        let tmp = TempDir::new().expect("tempdir");
        let primary = tmp.path().join("primary");
        let _ = init_test_repo(&primary);
        let run_wt = tmp.path().join("run-wt");
        let created_run = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: run_wt,
            branch_name: "milestone/fp".to_owned(),
            base_ref: "main".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("run wt");
        let task_wt = tmp.path().join("task-wt");
        let created_task = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: task_wt,
            branch_name: "task/fp".to_owned(),
            base_ref: "milestone/fp".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("task wt");

        fs::write(created_task.worktree.join("src/lib.rs"), "pub fn t() {}\n").expect("t");
        git_ok(&created_task.worktree, &["add", "src/lib.rs"]);
        git_ok(&created_task.worktree, &["commit", "-m", "task"]);

        fs::write(created_run.worktree.join("src/lib.rs"), "pub fn r() {}\n").expect("r");
        git_ok(&created_run.worktree, &["add", "src/lib.rs"]);
        git_ok(&created_run.worktree, &["commit", "-m", "run"]);

        let child = Command::new(&exe)
            .args([
                "--exact",
                "git_ops::tests::failpoint_crash_recovery_matrix",
                "--quiet",
            ])
            .env("TMPDIR", tmp.path())
            .env("ACDD_GIT_OPS_FAILPOINT_CHILD", case)
            .env("ACDD_FAILPOINT", "git_integrate_before_abort:exit")
            .env("ACDD_FAILPOINT_REPO", tmp.path())
            .output()
            .expect("spawn integrate child");
        assert!(!child.status.success(), "{case} child should crash");

        let gd = super::exec::git_dir_path(&created_task.worktree).expect("gd");
        assert!(gd.join("rebase-merge").exists() || gd.join("rebase-apply").exists());
        assert!(abort_in_progress_git_ops(&created_task.worktree).expect("abort rebase"));
        assert!(git_ok(&created_run.worktree, &["status", "--porcelain"]).is_empty());
        assert!(git_ok(&created_task.worktree, &["status", "--porcelain"]).is_empty());
    }

    // 3. Archive crash retry matrix (after commit even with later commits in run_base_head..HEAD, and after write without commit).
    for (case, fp_spec, should_commit) in [
        ("archive_after_commit", "archive_after_commit:exit", true),
        (
            "archive_after_write_no_commit",
            "archive_after_write:exit",
            false,
        ),
    ] {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        let run_base_head = init_test_repo(&repo);
        git_ok(&repo, &["checkout", "-b", "milestone/fp-arch"]);
        fs::create_dir_all(repo.join("docs/milestones")).expect("dir");
        fs::write(repo.join("docs/milestones/AC-FP.md"), "# Active FP\n").expect("spec");
        git_ok(&repo, &["add", "docs/milestones/AC-FP.md"]);
        git_ok(&repo, &["commit", "-m", "add spec"]);

        let child = Command::new(&exe)
            .args([
                "--exact",
                "git_ops::tests::failpoint_crash_recovery_matrix",
                "--quiet",
            ])
            .env("TMPDIR", tmp.path())
            .env("ACDD_GIT_OPS_FAILPOINT_CHILD", case)
            .env("ACDD_FAILPOINT", fp_spec)
            .env("ACDD_FAILPOINT_REPO", &repo)
            .env("ACDD_FAILPOINT_BASE", run_base_head.as_str())
            .output()
            .expect("spawn archive child");
        assert!(!child.status.success(), "{case} child should crash");

        // Spec still exists on disk because crash occurred before spec deletion.
        assert!(repo.join("docs/milestones/AC-FP.md").is_file());
        assert!(repo.join("docs/milestones/archive/AC-FP.md").is_file());

        let expected_oid = should_commit.then(|| current_head_commit(&repo).expect("arch oid"));
        if should_commit {
            // Even if a subsequent commit lands on top, run_base_head..HEAD finds the archive commit!
            fs::write(repo.join("README.md"), "# Later Commit\n").expect("later");
            git_ok(&repo, &["add", "README.md"]);
            git_ok(&repo, &["commit", "-m", "later commit on top"]);
        }

        let archive_md = "# Archived Failpoint Spec\n";
        let retry = archive_spec_with_git(&ArchiveCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "root:handoff".to_owned(),
            claim_revision: 1,
            run_base_head: Some(run_base_head),
            spec_relpath: "docs/milestones/AC-FP.md".to_owned(),
            archive_relpath: "docs/milestones/archive/AC-FP.md".to_owned(),
            rendered_archive_markdown: archive_md.to_owned(),
            rendered_archive_digest: sha256_hex(archive_md.as_bytes()),
            should_commit,
            allow_main: false,
            commit_message: "archive failpoint".to_owned(),
        })
        .expect("archive idempotent retry");
        assert!(retry.recovered_idempotently, "{case}");
        assert_eq!(retry.committed_oid, expected_oid);
        assert_eq!(
            repo.join("docs/milestones/AC-FP.md").exists(),
            !should_commit,
            "{case}"
        );
        assert!(repo.join("docs/milestones/archive/AC-FP.md").is_file());
    }
}
