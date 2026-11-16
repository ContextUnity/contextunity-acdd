use super::*;

#[test]
fn milestone_clear_matrix() {
    struct Case {
        selector: &'static str,
        preserve_foreign: bool,
        remaining_tasks: usize,
    }
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    let manifest = "docs/milestones/010-close.md";
    harness.write("src/lib.rs", "pub fn finish() {}\n");
    harness.write(
        manifest,
        &MilestoneFixtureBuilder::new("m-close")
            .title("Close work")
            .task("finish", &["src/lib.rs"])
            .task_target("Finish work")
            .task_scope_roots(&["src/"])
            .task("retired", &["cancelled/"])
            .task_target("Transferred work")
            .task_status("cancelled")
            .build(),
    );
    harness.stage_forced_paths(&["acdd.yaml", "src/lib.rs", manifest]);
    harness.commit_staged("initialize clear matrix");
    harness.sync(manifest).unwrap();
    let baseline = crate::acdd::snapshot::FixtureSnapshot::capture(&harness);
    let head = baseline.baseline_commit();
    let state_ref = "refs/acdd/state/acdd/acdd/m-close/finish";
    let neighbors = [
        "refs/acdd/state/acdd/acdd/m-close-other/first",
        "refs/acdd/state/foreign/project/m-close/first",
    ];
    for case in [
        Case {
            selector: "010",
            preserve_foreign: false,
            remaining_tasks: 0,
        },
        Case {
            selector: "m-close",
            preserve_foreign: true,
            remaining_tasks: 1,
        },
    ] {
        baseline.restore(&harness);
        assert!(!harness.store().list(None, "all", None).unwrap().is_empty());
        assert!(
            crate::acdd::support::git_command(root, &["update-ref", state_ref, head])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            crate::acdd::support::git_command(root, &["rev-parse", "--verify", state_ref])
                .status()
                .unwrap()
                .success()
        );
        if case.preserve_foreign {
            for reference in neighbors {
                assert!(
                    crate::acdd::support::git_command(root, &["update-ref", reference, head])
                        .status()
                        .unwrap()
                        .success()
                );
            }
            // Seed execution provenance and foreign tasks into SQLite
            let db_path = root.join(".acdd/tasks.sqlite");
            {
                let conn = rusqlite::Connection::open(&db_path).unwrap();
                // Foreign task sharing milestone_ref
                conn.execute(
                    "INSERT INTO tasks(task_id,milestone_ref,descriptor) VALUES(?1,?2,?3)",
                    rusqlite::params![
                        "foreign/m-other:neighbor",
                        "docs/milestones/010-close.md",
                        "{\"task_id\":\"foreign/m-other:neighbor\",\"project\":\"foreign\",\"milestone_id\":\"m-other\"}"
                    ],
                ).unwrap();

                // Foreign trace with underscore / wildcard name
                conn.execute(
                    "INSERT INTO gate_execution_traces(milestone_id,subject,gate_path,command_id,command_str,status,duration_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params!["foreign/project/mXa", "acdd/mXa:first", "build", "test", "true", "passed", 1],
                ).unwrap();

                // Provenance records for m-close
                conn.execute(
                    "INSERT INTO gate_execution_runs(milestone_id,subject,gate_path,claim_revision,contract_revision,worker_id,worktree,candidate_fingerprint,status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    rusqlite::params!["acdd/acdd/m-close", "acdd/m-close:first", "build", 1, 1, "builder", root.to_str().unwrap(), "candidate", "completed"],
                ).unwrap();
                let run_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO gate_execution_attempts(run_id,milestone_id,subject,gate_path,command_id,attempt_id,state) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![run_id, "acdd/acdd/m-close", "acdd/m-close:first", "build", "check", 1, "completed"],
                ).unwrap();
                conn.execute(
                    "INSERT INTO command_execution_proofs(run_id,command_id,attempt_id,payload) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![run_id, "check", 1, "{}"],
                ).unwrap();
            }
        }
        let cleared = harness
            .run_cli(&["milestone", "clear", case.selector])
            .unwrap();
        assert_eq!(cleared["id"], "m-close");
        assert_eq!(cleared["cleared"], true);
        assert!(harness.store().list(None, "all", None).unwrap().is_empty());
        {
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            let remaining_tasks: usize = conn
                .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
                .unwrap();
            assert_eq!(remaining_tasks, case.remaining_tasks);
        }
        assert!(
            !crate::acdd::support::git_command(root, &["rev-parse", "--verify", state_ref])
                .status()
                .unwrap()
                .success()
        );
        if case.preserve_foreign {
            for reference in neighbors {
                assert!(crate::acdd::support::git_command(
                    root,
                    &["rev-parse", "--verify", reference]
                )
                .status()
                .unwrap()
                .success());
            }
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            for (query, expected) in [
                ("SELECT count(*) FROM tasks WHERE task_id='foreign/m-other:neighbor'", 1),
                ("SELECT count(*) FROM gate_execution_traces WHERE subject='acdd/mXa:first'", 1),
                ("SELECT count(*) FROM gate_execution_runs WHERE milestone_id='acdd/acdd/m-close'", 0),
                ("SELECT count(*) FROM gate_execution_attempts WHERE milestone_id='acdd/acdd/m-close'", 0),
                ("SELECT count(*) FROM command_execution_proofs", 0),
            ] {
                let count: i64 = conn.query_row(query, [], |row| row.get(0)).unwrap();
                assert_eq!(count, expected, "{query}");
            }
        }
    }
}

#[test]
fn plan_cleanup_matrix() {
    struct Case {
        selector: &'static str,
        neighbor: Option<&'static str>,
        commands: &'static [[&'static str; 3]],
    }
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    drop(harness.store());
    let baseline = crate::acdd::snapshot::FixtureSnapshot::capture(&harness);
    let head = baseline.baseline_commit();
    let namespace = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(
            root.canonicalize().unwrap().to_string_lossy().as_bytes()
        )
    );
    for case in [
        Case {
            selector: "cleanup-test",
            neighbor: None,
            commands: &[
                ["plan", "clear", "cleanup-test"],
                ["plan", "archive", "cleanup-test"],
            ],
        },
        Case {
            selector: "uni-plan",
            neighbor: None,
            commands: &[["clear", "plan", "uni-plan"]],
        },
        Case {
            selector: "foo",
            neighbor: Some("foo-bar"),
            commands: &[["plan", "clear", "foo"]],
        },
        Case {
            selector: "release plan",
            neighbor: None,
            commands: &[["plan", "clear", "release plan"]],
        },
    ] {
        baseline.restore(&harness);
        let manifest = format!("docs/plans/{}.md", case.selector);
        let mut references = Vec::new();
        for selector in std::iter::once(case.selector).chain(case.neighbor) {
            let path = format!("docs/plans/{selector}.md");
            harness.write(
                &path,
                "---\ntitle: Cleanup Plan\ndoc_type: plan\npurpose: testing cleanup\ndepends_on: []\n---\n# Cleanup Plan\n\n## 1. Step One\n- **Target**: T\n- **Scope**: [src]\n- **Status**: ready\n",
            );
            let tag = format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(path.as_bytes())
            );
            references.push(format!(
                "refs/acdd/snapshots/{namespace}/plan-{}-step-1",
                &tag[..16]
            ));
        }
        for command in case.commands {
            // Recreate the exact snapshot before archive as well as clear.
            for reference in &references {
                assert!(
                    crate::acdd::support::git_command(root, &["update-ref", reference, head])
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(crate::acdd::support::git_command(
                    root,
                    &["rev-parse", "--verify", reference]
                )
                .status()
                .unwrap()
                .success());
            }
            let archive = command[1] == "archive";
            let result = harness.run_cli(command).unwrap();
            assert_eq!(result[if archive { "archived" } else { "cleared" }], true);
            assert_eq!(root.join(&manifest).exists(), !archive);
            if archive {
                assert!(root
                    .join(format!("docs/plans/archive/{}.md", case.selector))
                    .exists());
            }
            for (index, reference) in references.iter().enumerate() {
                let exists =
                    crate::acdd::support::git_command(root, &["rev-parse", "--verify", reference])
                        .status()
                        .unwrap()
                        .success();
                assert_eq!(exists, index != 0, "{command:?}: {reference}");
            }
        }
    }
}

#[test]
fn milestone_clear_rejects_pending_publication_fence() {
    let harness = MilestoneHarness::linear("m-close");
    let db_path = harness.root().join(".acdd/tasks.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO task_store_metadata(key,value) VALUES('active_checkpoint_pending',?1)",
            ["{\"active\":[],\"retired\":[]}"],
        )
        .unwrap();
    }
    let res = harness.run_cli(&["milestone", "clear", "m-close"]);
    assert!(
        res.is_err(),
        "expected rejection when active_checkpoint_pending is present"
    );
}

#[test]
fn milestone_ambiguous_selector_fails_closed() {
    let harness = MilestoneHarness::configured_workspace();
    harness.write(
        "docs/milestones/010-a.md",
        "---\nid: m-a\ntitle: Milestone A\nstatus: active\n---\n# M A\n## Tasks in this milestone\n",
    );
    harness.write(
        "docs/milestones/010-b.md",
        "---\nid: m-b\ntitle: Milestone B\nstatus: active\n---\n# M B\n## Tasks in this milestone\n",
    );
    let res = harness.run_cli(&["milestone", "clear", "010"]);
    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert!(err.contains("MILESTONE_AMBIGUOUS"));
}

#[test]
fn plan_archive_rejects_overwrite_and_symlinks() {
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    harness.write(
        "docs/plans/cleanup-test.md",
        "---\ntitle: Cleanup Test Plan\ndoc_type: plan\npurpose: testing cleanup\ndepends_on: []\n---\n# Cleanup Test Plan\n\n## 1. Step One\n- **Target**: Target one\n- **Scope**: [src]\n- **Status**: ready\n",
    );
    harness.write(
        "docs/plans/archive/cleanup-test.md",
        "previous archived authority\n",
    );

    // Attempting to archive when destination already exists must fail closed
    let res = harness.run_cli(&["plan", "archive", "cleanup-test"]);
    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert!(err.contains("PLAN_ARCHIVE_EXISTS"));
    assert_eq!(
        std::fs::read_to_string(root.join("docs/plans/archive/cleanup-test.md")).unwrap(),
        "previous archived authority\n"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside_dir = root.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();

        harness.write(
            "acdd.yaml",
            "roots: [src]\ndocs: [docs]\nplans: [plans]\ntasks_db: .acdd/tasks.sqlite\n",
        );
        harness.write(
            "plans/symlink-plan.md",
            "---\ntitle: S\ndoc_type: plan\npurpose: s\ndepends_on: []\n---\n# S\n## 1. S\n- **Target**: T\n- **Scope**: [src]\n- **Status**: ready\n",
        );
        std::fs::remove_dir_all(root.join("docs/plans/archive")).unwrap();
        symlink(&outside_dir, root.join("docs/plans/archive")).unwrap();

        let res = harness.run_cli(&["plan", "archive", "symlink-plan"]);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("PLAN_PATH_INVALID"));

        std::fs::create_dir_all(root.join(".acdd")).unwrap();
        symlink(&outside_dir, root.join(".acdd/runs")).unwrap();
        let res_wf = harness.run_cli(&["workflow", "clear", "check"]);
        assert!(res_wf.is_err());
        assert!(res_wf
            .unwrap_err()
            .to_string()
            .contains("WORKFLOW_PATH_INVALID"));
    }
}

#[test]
fn cleanup_ownership_matrix() {
    struct Case {
        workflows: &'static [&'static str],
        plan: Option<&'static str>,
        logs: &'static [(&'static str, bool)],
        foreign_trace: bool,
        commands: &'static [[&'static str; 3]],
        files_deleted: Option<u64>,
    }
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    drop(harness.store());
    let baseline = crate::acdd::snapshot::FixtureSnapshot::capture(&harness);
    for case in [
        Case {
            workflows: &[],
            plan: None,
            logs: &[("check-2026-11-10.log", false), ("check-other.log", true)],
            foreign_trace: false,
            commands: &[["workflow", "clear", "check"]],
            files_deleted: Some(1),
        },
        Case {
            workflows: &["check", "check-2", "check-2026", "check-2026-11-10"],
            plan: None,
            logs: &[
                ("check-2026-11-12.log", false),
                ("check-2026-11-10.log", true),
                ("check-2-2026-11-10.log", true),
                ("check-2026-2026-11-10.log", true),
                ("check-2026-11-10-2026-11-10.log", true),
                ("check-202é.log", true),
            ],
            foreign_trace: false,
            commands: &[["workflow", "clear", "check"]],
            files_deleted: Some(1),
        },
        Case {
            workflows: &[],
            plan: Some("milestone"),
            logs: &[],
            foreign_trace: true,
            commands: &[
                ["workflow", "clear", "milestone"],
                ["plan", "clear", "milestone"],
            ],
            files_deleted: None,
        },
    ] {
        baseline.restore(&harness);
        for name in case.workflows {
            harness.write(
                format!("docs/workflows/{name}.yaml"),
                "version: 2\ninherit: none\ngates: []\n",
            );
        }
        if let Some(plan) = case.plan {
            harness.write(
                format!("docs/plans/{plan}.md"),
                "---\ntitle: Milestone Plan\ndoc_type: plan\npurpose: testing\ndepends_on: []\n---\n# Milestone Plan\n## 1. S\n- **Target**: T\n- **Scope**: [src]\n- **Status**: ready\n",
            );
        }
        let runs = root.join(".acdd/runs");
        if !case.logs.is_empty() {
            std::fs::create_dir_all(&runs).unwrap();
            for (name, _) in case.logs {
                std::fs::write(runs.join(name), "workflow log\n").unwrap();
            }
        }
        if case.foreign_trace {
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            conn.execute(
                "INSERT INTO gate_execution_traces(milestone_id, subject, gate_path, command_id, command_str, status, duration_ms)
                 VALUES('foreign/project/m-important', 'milestone', 'verify', 'check', 'true', 'passed', 1)", [],
            ).unwrap();
        }
        for command in case.commands {
            let cleared = harness.run_cli(command).unwrap();
            assert_eq!(cleared["cleared"], true);
            if let Some(count) = case.files_deleted {
                assert_eq!(cleared["files_deleted"], count);
            }
            assert_eq!(cleared["traces_deleted"], 0);
            for (name, preserved) in case.logs {
                assert_eq!(runs.join(name).exists(), *preserved, "{command:?}: {name}");
            }
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            let count: i64 = conn
                .query_row("SELECT count(*) FROM gate_execution_traces", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, i64::from(case.foreign_trace), "{command:?}");
        }
    }
}

#[test]
fn workflow_clear_ambiguity_matrix() {
    struct Case {
        shared_stem: bool,
        runs_directory: bool,
        logs: &'static [&'static str],
        trace: bool,
        commands: &'static [[&'static str; 3]],
        expected_error: Option<&'static str>,
        traces_remaining: i64,
    }
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    drop(harness.store());
    let baseline = crate::acdd::snapshot::FixtureSnapshot::capture(&harness);
    for case in [
        Case {
            shared_stem: true,
            runs_directory: true,
            logs: &["check-2026-11-10.log"],
            trace: false,
            commands: &[["workflow", "clear", "check"]],
            expected_error: Some("WORKFLOW_AMBIGUOUS"),
            traces_remaining: 0,
        },
        Case {
            shared_stem: true,
            runs_directory: true,
            logs: &["check.log", "check-2026-11-10.log"],
            trace: false,
            commands: &[
                ["workflow", "clear", "docs/workflows/check.yaml"],
                ["clear", "workflow", "docs/workflows/check.yaml"],
            ],
            expected_error: Some("WORKFLOW_AMBIGUOUS"),
            traces_remaining: 0,
        },
        Case {
            shared_stem: true,
            runs_directory: false,
            logs: &[],
            trace: true,
            commands: &[
                ["workflow", "clear", "docs/workflows/check.yaml"],
                ["clear", "workflow", "docs/workflows/check.yaml"],
            ],
            expected_error: Some("WORKFLOW_AMBIGUOUS"),
            traces_remaining: 1,
        },
        Case {
            shared_stem: true,
            runs_directory: true,
            logs: &[],
            trace: true,
            commands: &[["workflow", "clear", "docs/workflows/check.yaml"]],
            expected_error: Some("WORKFLOW_AMBIGUOUS"),
            traces_remaining: 1,
        },
        Case {
            shared_stem: true,
            runs_directory: true,
            logs: &["other.log"],
            trace: true,
            commands: &[["workflow", "clear", "docs/workflows/check.yaml"]],
            expected_error: Some("WORKFLOW_AMBIGUOUS"),
            traces_remaining: 1,
        },
        Case {
            shared_stem: false,
            runs_directory: false,
            logs: &[],
            trace: true,
            commands: &[["workflow", "clear", "docs/workflows/check.yaml"]],
            expected_error: None,
            traces_remaining: 0,
        },
    ] {
        baseline.restore(&harness);
        harness.write(
            "docs/workflows/check.yaml",
            "version: 2\ninherit: none\ngates: []\n",
        );
        if case.shared_stem {
            harness.write(
                "docs/workflows/check.yml",
                "version: 2\ninherit: none\ngates: []\n",
            );
        }
        let runs = root.join(".acdd/runs");
        if case.runs_directory {
            std::fs::create_dir_all(&runs).unwrap();
        }
        assert_eq!(runs.exists(), case.runs_directory);
        for name in case.logs {
            std::fs::write(runs.join(name), "preserved log\n").unwrap();
        }
        if case.trace {
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            conn.execute(
                "INSERT INTO gate_execution_traces(milestone_id,subject,gate_path,command_id,command_str,status,duration_ms) VALUES(?,?,?,?,?,?,?)",
                rusqlite::params!["workflow/check", "check#gate", "gate", "own", "echo check", "passed", 1],
            ).unwrap();
        }
        for command in case.commands {
            let result = harness.run_cli(command);
            if let Some(expected) = case.expected_error {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(expected), "{command:?}: {error}");
            } else {
                let cleared = result.unwrap();
                assert_eq!(cleared["cleared"], true);
                assert_eq!(cleared["traces_deleted"], 1);
            }
            for name in case.logs {
                assert!(runs.join(name).exists(), "{command:?} must preserve {name}");
            }
            let conn = rusqlite::Connection::open(root.join(".acdd/tasks.sqlite")).unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM gate_execution_traces WHERE subject='check#gate'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, case.traces_remaining, "{command:?}");
        }
    }
}

#[test]
fn milestone_enumeration_failure_fails_closed() {
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    let head = String::from_utf8(
        crate::acdd::support::git_command(root, &["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let head = head.trim();
    let state_ref = "refs/acdd/state/acdd/acdd/m-close/first";
    crate::acdd::support::git_command(root, &["update-ref", state_ref, head])
        .status()
        .unwrap();
    std::fs::write(root.join(".git/packed-refs"), "invalid packed refs\n").unwrap();
    let res = harness.run_cli(&["milestone", "clear", "m-close"]);
    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert!(err.contains("MILESTONE_CLEAR_FAILED") || err.contains("git for-each-ref failed"));
}

#[test]
fn cleanup_confinement_matrix() {
    #[cfg(unix)]
    struct ExternalDir(std::path::PathBuf);
    #[cfg(unix)]
    impl Drop for ExternalDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    harness.write(
        "docs/plans/shared.md",
        "---\ntitle: Shared Plan\ndoc_type: plan\npurpose: testing\ndepends_on: []\n---\n# Shared Plan\n## 1. S\n- **Target**: T\n- **Scope**: [src]\n- **Status**: ready\n",
    );
    #[cfg(unix)]
    let outside_dir = ExternalDir(root.with_extension("external-test-db"));
    #[cfg(unix)]
    std::fs::create_dir(&outside_dir.0).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_dir.0, root.join("link")).unwrap();

    let mut cases = vec![
        (
            "../external-tasks.sqlite".to_string(),
            vec!["workflow", "clear", "check"],
            false,
        ),
        (
            "../external-tasks.sqlite".to_string(),
            vec!["plan", "clear", "shared"],
            false,
        ),
    ];
    #[cfg(unix)]
    {
        cases.extend([
            (
                root.join("link/tasks.sqlite").display().to_string(),
                vec!["milestone", "clear", "m-init"],
                false,
            ),
            (
                root.join("..")
                    .join(outside_dir.0.file_name().unwrap())
                    .join("tasks.sqlite")
                    .display()
                    .to_string(),
                vec!["milestone", "clear", "m-init"],
                false,
            ),
            (
                ".acdd/tasks.sqlite".to_string(),
                vec!["workflow", "clear", "check"],
                true,
            ),
        ]);
    }
    for (database, command, dangling_runs) in cases {
        harness.write(
            "acdd.yaml",
            &format!("roots: [src]\ndocs: [docs]\ntasks_db: {database}\n"),
        );
        #[cfg(unix)]
        if dangling_runs {
            std::fs::create_dir_all(root.join(".acdd")).unwrap();
            std::os::unix::fs::symlink(root.join("missing-runs"), root.join(".acdd/runs")).unwrap();
        }
        let error = harness.run_cli(&command).unwrap_err().to_string();
        if dangling_runs {
            assert!(error.contains("WORKFLOW_PATH_INVALID"));
        }
        #[cfg(unix)]
        assert!(!outside_dir.0.join("tasks.sqlite").exists());
        #[cfg(unix)]
        if dangling_runs {
            std::fs::remove_file(root.join(".acdd/runs")).unwrap();
        }
    }
}

#[test]
fn named_task_snapshot_ownership_is_preserved() {
    let harness = MilestoneHarness::configured_workspace();
    let root = harness.root();
    harness.write(
        "docs/plans/shared.md",
        "---\ntitle: Shared Plan\ndoc_type: plan\npurpose: testing\ndepends_on: []\n---\n# Shared Plan\n## 1. S\n- **Target**: T\n- **Scope**: [src]\n- **Status**: ready\n",
    );
    harness.write(
        "profile.yaml",
        &std::fs::read_to_string("tests/acdd/fixtures/plans/snapshot.yaml").unwrap(),
    );

    // Create real named snapshot using actual engine
    acdd_tasks::manage(
        root,
        acdd_tasks::Manage {
            action: acdd_tasks::ManageAction::Snapshot,
            name: Some("shared".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let worktree = root.canonicalize().unwrap();
    let namespace = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(worktree.to_string_lossy().as_bytes())
    );
    let task_snapshot_ref = format!("refs/acdd/snapshots/{namespace}/shared");
    assert!(crate::acdd::support::git_command(
        root,
        &["rev-parse", "--verify", &task_snapshot_ref]
    )
    .status()
    .unwrap()
    .success());

    // Run real plan to generate plan snapshot
    let plan_run = harness
        .run_cli(&[
            "--profile",
            "profile.yaml",
            "plan",
            "run",
            "shared",
            "--in-place",
        ])
        .unwrap();
    assert_eq!(plan_run["status"], "completed");

    let cleared = harness.run_cli(&["plan", "clear", "shared"]).unwrap();
    assert_eq!(cleared["cleared"], true);
    assert_eq!(cleared["refs_pruned"], 2);

    // Task snapshot is strictly preserved!
    assert!(crate::acdd::support::git_command(
        root,
        &["rev-parse", "--verify", &task_snapshot_ref]
    )
    .status()
    .unwrap()
    .success());
}
