use super::*;
use crate::profile::{GitCommitNoteFormat, GitCommitNotePolicy, GitCommitNoteSection};
use crate::types::{CommitMode, IntegrateStrategy, IsolationMode};
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use acdd_core::path_policy::WorkspacePathPolicy;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} in '{}' failed:\nstdout: {}\nstderr: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn init_test_repo(dir: &Path) -> CommitOid {
    fs::create_dir_all(dir).expect("create repo dir");
    git_ok(dir, &["init", "-b", "main"]);
    git_ok(dir, &["config", "user.email", "test@contextunity.local"]);
    git_ok(dir, &["config", "user.name", "ACDD Test"]);
    fs::write(dir.join("README.md"), "# Test Repo\n").expect("write readme");
    fs::create_dir_all(dir.join("src")).expect("create src");
    fs::write(dir.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\n").expect("write lib.rs");
    git_ok(dir, &["add", "README.md", "src/lib.rs"]);
    git_ok(dir, &["commit", "-m", "initial commit"]);
    current_head_commit(dir).expect("initial head")
}

fn sha256_hex(s: &str) -> String {
    acdd_core::format::sha256_hex(s.as_bytes())
}

#[cfg(unix)]
fn install_hook(repo: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let hooks_dir = git_dir_path(repo).expect("git dir").join("hooks");
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

    // 2. Snapshot + GC protection + Rollback + InPlace vs Worktree diff.
    let policy = WorkspacePathPolicy::new(
        ["docs/milestones/AC-001.md".to_owned()],
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );
    fs::create_dir_all(created_wt.worktree.join(".acdd")).expect(".acdd");
    fs::write(created_wt.worktree.join(".acdd/state.sqlite"), "db-bytes").expect("db");

    let snap_t1 = capture_snapshot(&CaptureSnapshotReq {
        worktree: created_wt.worktree.clone(),
        run_id: run_id.clone(),
        namespace: "task-t1".to_owned(),
        label: "rev-1".to_owned(),
        scope: Some(vec!["src/**".to_owned()]),
        path_policy: policy.clone(),
        message: "snapshot rev 1".to_owned(),
    })
    .expect("capture snapshot");
    assert_eq!(
        snap_t1.snapshot_ref,
        "refs/acdd/snapshots/run-wt-01/task-t1/rev-1"
    );
    // Survives aggressive GC.
    git_ok(&created_wt.worktree, &["gc", "--prune=now"]);
    assert!(blob_oid_at(&created_wt.worktree, &snap_t1.snapshot_ref, "src/lib.rs")
        .expect("blob check")
        .is_some());

    // Mutate src/lib.rs, create untracked src/extra.rs, then rollback confined to src/**.
    fs::write(created_wt.worktree.join("src/lib.rs"), "pub fn mutated() {}\n").expect("mutate");
    fs::write(created_wt.worktree.join("src/extra.rs"), "pub fn extra() {}\n").expect("extra");
    let head_before_rb = current_head_commit(&created_wt.worktree).expect("head");

    let rb = restore_rollback(&RollbackReq {
        worktree: created_wt.worktree.clone(),
        target_ref_or_oid: snap_t1.snapshot_ref.clone(),
        admitted_scope: Some(vec!["src/**".to_owned()]),
        path_policy: policy.clone(),
    })
    .expect("rollback");
    assert_eq!(rb.target_tree, snap_t1.tree_oid);
    assert_eq!(
        rb.restored_paths,
        vec!["src/extra.rs".to_owned(), "src/lib.rs".to_owned()]
    );
    assert!(!created_wt.worktree.join("src/extra.rs").exists());
    assert_eq!(
        fs::read_to_string(created_wt.worktree.join("src/lib.rs")).expect("read"),
        "pub fn base() -> i32 { 1 }\n"
    );
    assert_eq!(
        current_head_commit(&created_wt.worktree).expect("head after rb"),
        head_before_rb
    );
    // Engine-managed & state.sqlite untouched by rollback.
    assert!(created_wt.worktree.join(".acdd/state.sqlite").is_file());
    assert!(created_wt.worktree.join("docs/milestones/AC-001.md").is_file());

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

    // 3. Table-driven binary / submodule / >16 MiB guards matrix.
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
        let err = current_scope_tree(&created_wt.worktree, Some(&["src/**".to_owned()]), &policy)
            .expect_err(&format!("guard {label} should fail"));
        assert_eq!(err.code, ErrorCode::ExternalGitDrift, "{label}: {err}");
        if p.is_dir() {
            fs::remove_dir_all(&p).expect("cleanup dir");
        } else {
            fs::remove_file(&p).expect("cleanup file");
        }
    }

    // 4. Cleanup snapshot refs across primary + separate linked repo & remove worktree/branch.
    let linked_repo = tmp.path().join("linked-repo");
    let _linked_head = init_test_repo(&linked_repo);
    capture_snapshot(&CaptureSnapshotReq {
        worktree: linked_repo.clone(),
        run_id: run_id.clone(),
        namespace: "manual".to_owned(),
        label: "checkpoint-1".to_owned(),
        scope: None,
        path_policy: WorkspacePathPolicy::default(),
        message: "linked manual snap".to_owned(),
    })
    .expect("linked snap");

    let cleaned = cleanup_run_refs(
        &[created_wt.worktree.clone(), linked_repo.clone()],
        &run_id,
    )
    .expect("cleanup refs");
    assert_eq!(cleaned.len(), 2);
    assert!(git_ok(&private_repo, &["for-each-ref", "refs/acdd/"]).is_empty());
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
fn scoped_commit_plumbing_cli_manual_notes_and_archive_matrix() {
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
        commit_mode: CommitMode::Plumbing,
        admitted_scope: vec!["src/**".to_owned()],
        path_policy: policy.clone(),
        message: "deliver t1".to_owned(),
        allow_main: false,
        manual_snapshot_namespace: None,
    })
    .expect_err("main protected");
    assert_eq!(protected_err.code, ErrorCode::MainBranchProtected);

    // Switch to task branch for remaining commit checks.
    git_ok(&repo, &["checkout", "-b", "task/t1"]);
    // Dirty unrelated file out of scope: must NEVER be committed or staged!
    fs::write(repo.join("README.md"), "# Dirty Out Of Scope\n").expect("dirty readme");
    fs::write(repo.join("untracked_out.txt"), "keep out\n").expect("untracked");

    // 2. Plumbing mode bypasses mutating pre-commit hook and syncs .git/index only for committed paths.
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
        commit_mode: CommitMode::Plumbing,
        admitted_scope: vec!["src/**".to_owned()],
        path_policy: policy.clone(),
        message: "deliver t1 plumbing".to_owned(),
        allow_main: false,
        manual_snapshot_namespace: None,
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

    // 3. Git commit notes (markdown + json + truncation + non-fatal warning on bad ref).
    let note_md = GitNoteReq {
        worktree: repo.clone(),
        commit_oid: plumbing_oid.clone(),
        policy: GitCommitNotePolicy {
            ref_name: "refs/notes/acdd".to_owned(),
            max_bytes: 32,
            format: GitCommitNoteFormat::Markdown,
            sections: vec![GitCommitNoteSection::Duration],
        },
        body: "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".to_owned(),
    };
    write_git_note(&note_md).expect("write md note");
    let shown_md = git_ok(
        &repo,
        &["notes", "--ref=refs/notes/acdd", "show", plumbing_oid.as_str()],
    );
    assert!(shown_md.len() <= 32, "note exceeded max_bytes: {shown_md}");
    assert!(shown_md.ends_with("..."));

    let note_json = GitNoteReq {
        worktree: repo.clone(),
        commit_oid: plumbing_oid.clone(),
        policy: GitCommitNotePolicy {
            ref_name: "refs/notes/acdd-json".to_owned(),
            max_bytes: 48,
            format: GitCommitNoteFormat::Json,
            sections: vec![GitCommitNoteSection::TasksSummary],
        },
        body: r#"{"very_long_key":"abcdefghijklmnopqrstuvwxyz0123456789"}"#.to_owned(),
    };
    write_git_note(&note_json).expect("write json note");
    let shown_json = git_ok(
        &repo,
        &["notes", "--ref=refs/notes/acdd-json", "show", plumbing_oid.as_str()],
    );
    assert!(shown_json.len() <= 48);
    assert!(serde_json::from_str::<serde_json::Value>(&shown_json).is_ok());

    // Invalid ref logs warning without failing or rolling back commit.
    write_git_note(&GitNoteReq {
        worktree: repo.clone(),
        commit_oid: plumbing_oid.clone(),
        policy: GitCommitNotePolicy {
            ref_name: "refs/notes/invalid ref name".to_owned(),
            max_bytes: 256,
            format: GitCommitNoteFormat::Markdown,
            sections: vec![],
        },
        body: "ignored".to_owned(),
    })
    .expect("bad note ref must not fail");
    assert_eq!(current_head_commit(&repo).expect("head"), plumbing_oid);

    // 4. Cli mode with hook mutating tree -> GitHookMutatedTree, HEAD reset, working files untouched.
    #[cfg(unix)]
    {
        fs::write(repo.join("src/lib.rs"), "pub fn base() -> i32 { 3 }\n").expect("edit 3");
        let mutated_err = create_scoped_commit(&ScopedCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "task:t1:deliver".to_owned(),
            claim_revision: 2,
            base_head: plumbing_oid.clone(),
            commit_mode: CommitMode::Cli,
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "cli mutated".to_owned(),
            allow_main: false,
            manual_snapshot_namespace: None,
        })
        .expect_err("hook mutated tree");
        assert_eq!(mutated_err.code, ErrorCode::GitHookMutatedTree);
        assert_eq!(current_head_commit(&repo).expect("head reset"), plumbing_oid);
        assert_eq!(
            fs::read_to_string(repo.join("src/lib.rs")).expect("working file untouched"),
            "pub fn base() -> i32 { 3 }\n"
        );

        // Hook rejecting with non-zero exit -> GitHookRejected + archive rollback removes untracked archive_path.
        install_hook(&repo, "pre-commit", "#!/bin/sh\nexit 1\n");
        fs::create_dir_all(repo.join("docs/milestones")).expect("milestones");
        fs::write(repo.join("docs/milestones/AC-002.md"), "# Active Spec\n").expect("spec");
        let archive_md = "# Archived Spec\n";
        let archive_err = archive_spec_with_git(&ArchiveCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "root:handoff".to_owned(),
            claim_revision: 1,
            run_base_head: Some(base_head.clone()),
            spec_relpath: "docs/milestones/AC-002.md".to_owned(),
            archive_relpath: "docs/milestones/archive/AC-002.md".to_owned(),
            rendered_archive_markdown: archive_md.to_owned(),
            rendered_archive_digest: sha256_hex(archive_md),
            should_commit: true,
            commit_mode: CommitMode::Cli,
            allow_main: false,
            commit_message: "archive AC-002".to_owned(),
        })
        .expect_err("hook rejected archive");
        assert_eq!(archive_err.code, ErrorCode::GitHookRejected);
        assert!(!repo.join("docs/milestones/archive/AC-002.md").exists());
        assert!(repo.join("docs/milestones/AC-002.md").is_file());
        assert_eq!(current_head_commit(&repo).expect("head"), plumbing_oid);

        // Clean passing hook -> Cli commit & archive succeed.
        install_hook(&repo, "pre-commit", "#!/bin/sh\nexit 0\n");
        let cli_resp = create_scoped_commit(&ScopedCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "task:t1:deliver".to_owned(),
            claim_revision: 2,
            base_head: plumbing_oid.clone(),
            commit_mode: CommitMode::Cli,
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "cli clean".to_owned(),
            allow_main: false,
            manual_snapshot_namespace: None,
        })
        .expect("cli clean commit");
        let cli_oid = cli_resp.committed_oid.expect("cli oid");
        assert_ne!(cli_oid, plumbing_oid);

        let arch_resp = archive_spec_with_git(&ArchiveCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "root:handoff".to_owned(),
            claim_revision: 1,
            run_base_head: Some(base_head.clone()),
            spec_relpath: "docs/milestones/AC-002.md".to_owned(),
            archive_relpath: "docs/milestones/archive/AC-002.md".to_owned(),
            rendered_archive_markdown: archive_md.to_owned(),
            rendered_archive_digest: sha256_hex(archive_md),
            should_commit: true,
            commit_mode: CommitMode::Cli,
            allow_main: false,
            commit_message: "archive AC-002".to_owned(),
        })
        .expect("archive cli success");
        assert!(arch_resp.committed_oid.is_some());
        assert!(!repo.join("docs/milestones/AC-002.md").exists());
        assert!(repo.join("docs/milestones/archive/AC-002.md").is_file());
    }

    // 5. CommitMode::Manual creates snapshot without moving HEAD.
    let head_before_manual = current_head_commit(&repo).expect("head");
    fs::write(repo.join("src/manual.rs"), "pub fn manual() {}\n").expect("manual");
    let manual_resp = create_scoped_commit(&ScopedCommitReq {
        worktree: repo.clone(),
        run_id,
        node_id: "task:t1:deliver".to_owned(),
        claim_revision: 3,
        base_head: head_before_manual.clone(),
        commit_mode: CommitMode::Manual,
        admitted_scope: vec!["src/**".to_owned()],
        path_policy: policy,
        message: "manual snap".to_owned(),
        allow_main: false,
        manual_snapshot_namespace: None,
    })
    .expect("manual commit");
    assert!(manual_resp.committed_oid.is_none());
    assert!(manual_resp.snapshot_ref.is_some());
    assert_eq!(
        current_head_commit(&repo).expect("head after manual"),
        head_before_manual
    );
}

#[test]
fn integrate_strategies_conflict_abort_and_completeness_matrix() {
    let run_id = RunId::new("run-int-01").expect("run_id");
    let policy = WorkspacePathPolicy::default();

    // 1. All 5 strategies happy-path matrix.
    let strategies = [
        IntegrateStrategy::Manual,
        IntegrateStrategy::RebaseFf,
        IntegrateStrategy::FfOnly,
        IntegrateStrategy::NoFf,
        IntegrateStrategy::Squash,
    ];
    for (idx, strategy) in strategies.into_iter().enumerate() {
        let tmp = TempDir::new().expect("tempdir");
        let primary = tmp.path().join("primary");
        let _main_head = init_test_repo(&primary);

        let run_wt = tmp.path().join("run-wt");
        let created_run = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: run_wt.clone(),
            branch_name: format!("milestone/run-{idx}"),
            base_ref: "main".to_owned(),
            copy_uncommitted_paths: vec![],
        })
        .expect("run wt");

        let task_wt = tmp.path().join("task-wt");
        let task_branch = format!("task/t-{idx}");
        let created_task = create_task_worktree(&CreateWorktreeReq {
            primary_worktree: primary.clone(),
            worktree_path: task_wt.clone(),
            branch_name: task_branch.clone(),
            base_ref: format!("milestone/run-{idx}"),
            copy_uncommitted_paths: vec![],
        })
        .expect("task wt");

        let initial_claim_tree =
            current_scope_tree(&created_task.worktree, None, &policy).expect("claim tree");
        let task_file = format!("src/feature_{idx}.rs");
        fs::write(
            created_task.worktree.join(&task_file),
            format!("pub fn f{idx}() -> i32 {{ {idx} }}\n"),
        )
        .expect("write task file");
        let accepted_tree =
            current_scope_tree(&created_task.worktree, None, &policy).expect("accepted tree");
        create_scoped_commit(&ScopedCommitReq {
            worktree: created_task.worktree.clone(),
            run_id: run_id.clone(),
            node_id: format!("task:t{idx}:deliver"),
            claim_revision: 1,
            base_head: created_task.head_commit.clone(),
            commit_mode: CommitMode::Plumbing,
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: format!("task {idx}"),
            allow_main: false,
            manual_snapshot_namespace: None,
        })
        .expect("commit task");

        // Advance run_wt with a non-conflicting commit for non-ff-only strategies.
        if strategy != IntegrateStrategy::FfOnly && strategy != IntegrateStrategy::Manual {
            fs::write(
                created_run.worktree.join("src/unrelated_run.rs"),
                "pub fn u() {}\n",
            )
            .expect("unrelated");
            git_ok(&created_run.worktree, &["add", "src/unrelated_run.rs"]);
            git_ok(&created_run.worktree, &["commit", "-m", "run advance"]);
        }

        let task_ref = TaskRef::new(format!("t{idx}")).expect("task ref");
        let resp = integrate_task_branch(&IntegrateTaskReq {
            target_worktree: created_run.worktree.clone(),
            source_worktree: Some(created_task.worktree.clone()),
            source_branch: task_branch.clone(),
            run_id: run_id.clone(),
            task_ref: task_ref.clone(),
            accepted_tree: accepted_tree.clone(),
            strategy,
            allow_main: false,
        })
        .expect("integrate strategy");

        let check_input = [TaskIntegrationCheckInput {
            task_ref: task_ref.clone(),
            initial_claim_tree,
            accepted_tree: accepted_tree.clone(),
            admitted_scope: vec!["src/**".to_owned()],
            downstream_tasks: vec![],
        }];
        let missing = verify_tasks_integrated(&run_id, &created_run.worktree, &check_input)
            .expect("verify integrated");

        if strategy == IntegrateStrategy::Manual {
            assert!(resp.integrated_commit.is_none());
            assert_eq!(missing, vec![task_ref]);
        } else {
            let landed = resp.integrated_commit.expect("landed commit");
            assert_eq!(missing, Vec::<TaskRef>::new(), "strategy {strategy}");
            if matches!(
                strategy,
                IntegrateStrategy::RebaseFf
                    | IntegrateStrategy::NoFf
                    | IntegrateStrategy::Squash
            ) {
                let msg = git_ok(
                    &created_run.worktree,
                    &["show", "-s", "--format=%B", landed.as_str()],
                );
                let expected_trailer =
                    format!("Acdd-Integrated: {}/{}/{}", run_id, task_ref, accepted_tree);
                assert!(msg.contains(&expected_trailer), "{strategy}: {msg}");
            }
            if strategy == IntegrateStrategy::NoFf {
                let parents = git_ok(
                    &created_run.worktree,
                    &["rev-list", "--parents", "-n", "1", landed.as_str()],
                );
                assert_eq!(parents.split_whitespace().count(), 3);
            }
        }
    }

    // 2. Conflict abort matrix across RebaseFf, FfOnly, NoFf, Squash -> clean worktrees.
    for strategy in [
        IntegrateStrategy::RebaseFf,
        IntegrateStrategy::FfOnly,
        IntegrateStrategy::NoFf,
        IntegrateStrategy::Squash,
    ] {
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
            strategy,
            allow_main: false,
        })
        .expect_err("conflict expected");
        assert_eq!(err.code, ErrorCode::GitIntegrationConflict);
        if strategy != IntegrateStrategy::FfOnly {
            assert!(
                err.message.contains("src/lib.rs"),
                "expected conflicting path in error: {}",
                err.message
            );
        }
        assert!(git_ok(&created_run.worktree, &["status", "--porcelain"]).is_empty());
        assert!(git_ok(&created_task.worktree, &["status", "--porcelain"]).is_empty());
        let run_gd = git_dir_path(&created_run.worktree).expect("run gd");
        let task_gd = git_dir_path(&created_task.worktree).expect("task gd");
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
        strategy: IntegrateStrategy::NoFf,
        allow_main: false,
    })
    .expect("integrate a");
    let merge_a = int_a.integrated_commit.expect("merge a");

    // Revert t-a back to initial_claim_tree -> verify_tasks_integrated MUST catch revert!
    git_ok(
        &created_run.worktree,
        &["revert", "--no-edit", "-m", "1", merge_a.as_str()],
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
        strategy: IntegrateStrategy::RebaseFf,
        allow_main: false,
    })
    .expect("integrate b");

    let missing_with_downstream = verify_tasks_integrated(
        &run_id,
        &created_run.worktree,
        &[
            TaskIntegrationCheckInput {
                task_ref: task_a_ref,
                initial_claim_tree: init_a,
                accepted_tree: acc_a,
                admitted_scope: vec!["src/**".to_owned()],
                downstream_tasks: vec![task_b_ref.clone()],
            },
            TaskIntegrationCheckInput {
                task_ref: task_b_ref,
                initial_claim_tree: init_b,
                accepted_tree: acc_b,
                admitted_scope: vec!["src/**".to_owned()],
                downstream_tasks: vec![],
            },
        ],
    )
    .expect("check downstream");
    assert_eq!(missing_with_downstream, Vec::<TaskRef>::new());
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
        "commit_plumbing_after_ref" | "commit_cli_after_sync" => {
            let mode = if case == "commit_plumbing_after_ref" {
                CommitMode::Plumbing
            } else {
                CommitMode::Cli
            };
            let base_head = current_head_commit(repo).expect("base");
            let _ = create_scoped_commit(&ScopedCommitReq {
                worktree: repo.to_path_buf(),
                run_id,
                node_id: "task:t1:deliver".to_owned(),
                claim_revision: 1,
                base_head,
                commit_mode: mode,
                admitted_scope: vec!["src/**".to_owned()],
                path_policy: policy,
                message: "failpoint commit".to_owned(),
                allow_main: false,
                manual_snapshot_namespace: None,
            });
        }
        "integrate_no_ff_conflict" | "integrate_rebase_conflict" => {
            let strategy = if case == "integrate_no_ff_conflict" {
                IntegrateStrategy::NoFf
            } else {
                IntegrateStrategy::RebaseFf
            };
            let target_wt = repo.join("run-wt");
            let source_wt = repo.join("task-wt");
            let accepted_tree =
                current_scope_tree(&source_wt, None, &policy).expect("accepted");
            let _ = integrate_task_branch(&IntegrateTaskReq {
                target_worktree: target_wt,
                source_worktree: Some(source_wt),
                source_branch: "task/fp".to_owned(),
                run_id,
                task_ref: TaskRef::new("t1").expect("t1"),
                accepted_tree,
                strategy,
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
                rendered_archive_digest: sha256_hex(archive_md),
                should_commit,
                commit_mode: CommitMode::Plumbing,
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
        let repo = PathBuf::from(
            std::env::var("ACDD_FAILPOINT_REPO").expect("ACDD_FAILPOINT_REPO"),
        );
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

    // 1. Commit crash recovery matrix (plumbing after update-ref, cli after index sync, external drift).
    for (case, fp_spec, mode) in [
        (
            "commit_plumbing_after_ref",
            "git_commit_after_update_ref:exit",
            CommitMode::Plumbing,
        ),
        (
            "commit_cli_after_sync",
            "git_commit_after_index_sync:abort",
            CommitMode::Cli,
        ),
    ] {
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

        // Retry create_scoped_commit under repo.lock semantics -> idempotently recovers crashed commit.
        let retry = create_scoped_commit(&ScopedCommitReq {
            worktree: repo.clone(),
            run_id: run_id.clone(),
            node_id: "task:t1:deliver".to_owned(),
            claim_revision: 1,
            base_head: base_head.clone(),
            commit_mode: mode,
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "failpoint commit".to_owned(),
            allow_main: false,
            manual_snapshot_namespace: None,
        })
        .expect("idempotent commit recovery");
        assert!(retry.recovered_idempotently, "{case}");
        assert_eq!(retry.committed_oid, Some(crashed_head));
        assert!(git_ok(&repo, &["status", "--porcelain"]).is_empty());

        // Unrelated external commit on top of base_head -> fail-closed EXTERNAL_GIT_DRIFT.
        fs::write(repo.join("README.md"), "# External Drift\n").expect("drift");
        git_ok(&repo, &["add", "README.md"]);
        git_ok(&repo, &["commit", "-m", "external drift"]);
        let drift_err = create_scoped_commit(&ScopedCommitReq {
            worktree: repo,
            run_id: run_id.clone(),
            node_id: "task:t1:deliver".to_owned(),
            claim_revision: 1,
            base_head,
            commit_mode: mode,
            admitted_scope: vec!["src/**".to_owned()],
            path_policy: policy.clone(),
            message: "failpoint commit".to_owned(),
            allow_main: false,
            manual_snapshot_namespace: None,
        })
        .expect_err("external drift expected");
        assert_eq!(drift_err.code, ErrorCode::ExternalGitDrift);
    }

    // 2. Dangling MERGE_HEAD / rebase-merge crash cleanup matrix.
    for case in ["integrate_no_ff_conflict", "integrate_rebase_conflict"] {
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

        if case == "integrate_no_ff_conflict" {
            assert!(git_dir_path(&created_run.worktree)
                .expect("gd")
                .join("MERGE_HEAD")
                .exists());
            assert!(abort_in_progress_git_ops(&created_run.worktree).expect("abort merge"));
        } else {
            let gd = git_dir_path(&created_task.worktree).expect("gd");
            assert!(gd.join("rebase-merge").exists() || gd.join("rebase-apply").exists());
            assert!(abort_in_progress_git_ops(&created_task.worktree).expect("abort rebase"));
        }
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
            rendered_archive_digest: sha256_hex(archive_md),
            should_commit,
            commit_mode: CommitMode::Plumbing,
            allow_main: false,
            commit_message: "archive failpoint".to_owned(),
        })
        .expect("archive idempotent retry");
        assert!(retry.recovered_idempotently, "{case}");
        assert_eq!(retry.committed_oid, expected_oid);
        assert!(!repo.join("docs/milestones/AC-FP.md").exists());
        assert!(repo.join("docs/milestones/archive/AC-FP.md").is_file());
    }
}
