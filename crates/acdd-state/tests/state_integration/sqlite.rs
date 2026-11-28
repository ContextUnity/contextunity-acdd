use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::git::git_command;
use acdd_core::ids::{RunId, TaskRef};
use acdd_state::harness::RunHarness;
use acdd_state::machine::SyncPlan;
use acdd_state::store::sqlite::{
    load_run_graph_with_version, open_read_only_with_version, schema, SqliteBackendOptions,
    SqliteStateBackend, SqliteStateReader,
};
use acdd_state::store::traits::{exec_tx_with, StateBackend, StateReader};
use acdd_state::types::{
    ListRunsReq, StoragePreconditions, TaskDependencyRecord, TerminalStatus, TransitionPlan,
};
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn sqlite_v1_to_v2_migration_and_readonly_wal_without_shm() {
    let ws = TempDir::new().expect("tempdir");
    let db_dir = ws.path().join(".acdd");
    fs::create_dir_all(&db_dir).expect("create .acdd");
    let db_path = db_dir.join("state.sqlite");

    // 1. Seed a genuine schema v1 database without `runs.spec_id`, plus an active run row,
    //    and verify SqliteStateReader reads v1 cleanly without `no such column: spec_id`.
    {
        let mut conn = Connection::open(&db_path).expect("open v1 seed db");
        conn.execute_batch(schema::SCHEMA_V1_SQL)
            .expect("apply v1 sql");
        conn.execute(
            "INSERT INTO _sqlite_meta (id, schema_version, write_seq, last_backup_seq)
             VALUES (1, 1, 5, 0)",
            [],
        )
        .expect("seed meta v1");
        conn.execute(
            "INSERT INTO runs (
                run_id, run_name, workspace, profile_id, profile_json,
                spec_digest, is_ephemeral, isolation, worktree, trace_id, status, created_at
            ) VALUES (
                'run-v1-ro', 'run-v1-ro', '/tmp/ws', 'code', '{}',
                'digest-v1', 0, 'in_place', '/tmp/ws-v1', 'trace-v1', 'active', '2026-01-01T00:00:00Z'
            )",
            [],
        )
        .expect("seed v1 run row");
        let has_spec_id: bool = conn
            .prepare("PRAGMA table_info(runs)")
            .expect("pragma")
            .query_map([], |r| r.get::<_, String>(1))
            .expect("query")
            .filter_map(Result::ok)
            .any(|c| c == "spec_id");
        assert!(!has_spec_id, "v1 schema should not have runs.spec_id yet");
        let _ = &mut conn;
    }
    {
        let v1_reader = SqliteStateReader::new(&db_path);
        let v1_page = v1_reader
            .list_runs(&ListRunsReq {
                limit: 10,
                ..ListRunsReq::default()
            })
            .expect("v1 list_runs must succeed read-only");
        assert_eq!(v1_page.runs.len(), 1);
        assert_eq!(v1_page.runs[0].run_id.as_str(), "run-v1-ro");
        assert_eq!(v1_page.runs[0].spec_id, None);
        let v1_graph = v1_reader
            .load_run_graph(&RunId::new("run-v1-ro").expect("run_id"))
            .expect("v1 load_run_graph must succeed")
            .expect("v1 run present");
        assert_eq!(v1_graph.run.spec_id, None);
        let v1_by_wt = v1_reader
            .resolve_active_run_by_worktree("/tmp/ws-v1")
            .expect("v1 resolve_active_run_by_worktree must succeed")
            .expect("v1 run matched by worktree");
        assert_eq!(v1_by_wt.spec_id, None);

        // Clean up seeded v1 run row before v2 migration test below, and verify newer schema fails closed.
        let conn = Connection::open(&db_path).expect("reopen v1 seed");
        conn.execute("DELETE FROM runs WHERE run_id = 'run-v1-ro'", [])
            .expect("delete v1 seed row");
        conn.execute(
            "UPDATE _sqlite_meta SET schema_version = ?1 WHERE id = 1",
            [i64::from(schema::CURRENT_SCHEMA_VERSION + 1)],
        )
        .expect("bump schema_version to future");
        drop(conn);

        let future_err = v1_reader
            .list_runs(&ListRunsReq::default())
            .expect_err("future schema_version must fail closed on read-only open");
        assert_eq!(future_err.code, ErrorCode::WorkflowSchemaInvalid);

        let conn = Connection::open(&db_path).expect("restore v1 version");
        conn.execute(
            "UPDATE _sqlite_meta SET schema_version = 1 WHERE id = 1",
            [],
        )
        .expect("reset schema_version to 1");
    }

    // 2. Open with SqliteStateBackend -> migrates v1 -> v3, persists and reads spec_id + attached_specs.
    {
        let backend = SqliteStateBackend::open(&db_path).expect("open backend migrates v1 -> v3");
        assert_eq!(
            backend.read_meta().expect("meta").schema_version,
            acdd_state::store::sqlite::schema::CURRENT_SCHEMA_VERSION
        );
        // Reopen is idempotent when schema is already at CURRENT_SCHEMA_VERSION.
        drop(backend);
        let backend = SqliteStateBackend::open(&db_path).expect("reopen idempotent v3");
        assert_eq!(
            backend.read_meta().expect("meta2").schema_version,
            acdd_state::store::sqlite::schema::CURRENT_SCHEMA_VERSION
        );

        backend
            .inspect_writer(|conn| {
                conn.execute(
                    "INSERT INTO runs (
                        run_id, run_name, workspace, profile_id, profile_json,
                        spec_digest, is_ephemeral, isolation, trace_id, status, created_at
                    ) VALUES (
                        'run-1', 'run-1', '/tmp/ws', 'code', '{}',
                        'digest-1', 0, 'in_place', 'trace-1', 'active', '2026-01-01T00:00:00Z'
                    )",
                    [],
                )
                .map_err(|e| AcddError::new(ErrorCode::BackendConflict, e.to_string()))?;
                Ok(())
            })
            .expect("seed run row");

        exec_tx_with(&backend, |tx| {
            tx.update_run_spec_id(&RunId::new("run-1")?, Some("AC-V2-SPEC"))
        })
        .expect("update_run_spec_id");

        let loaded = backend
            .load_run_graph(&RunId::new("run-1").expect("run_id"))
            .expect("load_run_graph")
            .expect("run present");
        assert_eq!(loaded.run.spec_id.as_deref(), Some("AC-V2-SPEC"));
    }

    // 3. Ensure SQLite is in WAL mode, remove any `-shm` sidecar, make path read-only on Unix,
    // and verify SqliteStateReader reads cleanly without mutating files or creating `-shm`.
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    if shm_path.exists() {
        fs::remove_file(&shm_path).expect("remove shm");
    }
    assert!(!shm_path.exists());

    let reader = SqliteStateReader::new(&db_path);
    {
        #[cfg(unix)]
        let _perm_guard = {
            use std::os::unix::fs::PermissionsExt;
            struct PermGuard {
                dir: PathBuf,
                file: PathBuf,
            }
            impl Drop for PermGuard {
                fn drop(&mut self) {
                    let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
                    let _ = fs::set_permissions(&self.file, fs::Permissions::from_mode(0o644));
                }
            }
            fs::set_permissions(&db_path, fs::Permissions::from_mode(0o444)).expect("ro file");
            fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o555)).expect("ro dir");
            PermGuard {
                dir: db_dir.clone(),
                file: db_path.clone(),
            }
        };

        let before_bytes = fs::read(&db_path).expect("read db bytes before");
        let page = reader
            .list_runs(&ListRunsReq {
                limit: 10,
                ..ListRunsReq::default()
            })
            .expect("reader opens read-only WAL db without -shm");
        assert_eq!(page.runs.len(), 1);
        assert_eq!(page.runs[0].spec_id.as_deref(), Some("AC-V2-SPEC"));
        assert!(!shm_path.exists(), "read-only path must not create -shm");
        assert_eq!(
            fs::read(&db_path).expect("read db bytes after"),
            before_bytes,
            "read-only open must not mutate db bytes"
        );
    }

    // 4. Non-empty `-wal` on a read-only path without `-shm` (where only `?immutable=1`
    //    would be possible) MUST fail closed instead of silently returning stale data.
    #[cfg(unix)]
    {
        let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
        fs::write(&wal_path, b"uncheckpointed-wal-frame-bytes").expect("write non-empty wal");

        use std::os::unix::fs::PermissionsExt;
        struct PermGuard {
            dir: PathBuf,
            file: PathBuf,
            wal: PathBuf,
        }
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
                let _ = fs::set_permissions(&self.file, fs::Permissions::from_mode(0o644));
                let _ = fs::remove_file(&self.wal);
            }
        }
        fs::set_permissions(&db_path, fs::Permissions::from_mode(0o444)).expect("ro file");
        fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o555)).expect("ro dir");
        let _perm_guard_wal = PermGuard {
            dir: db_dir.clone(),
            file: db_path.clone(),
            wal: wal_path.clone(),
        };

        let wal_err = reader
            .list_runs(&ListRunsReq {
                limit: 10,
                ..ListRunsReq::default()
            })
            .expect_err("non-empty WAL on immutable read-only path must fail closed");
        assert_eq!(wal_err.code, ErrorCode::BackendConflict);
    }

    // 5. Writer open MUST reject a newer-schema DB (`PRAGMA user_version > CURRENT_SCHEMA_VERSION`)
    //    BEFORE issuing `PRAGMA journal_mode = WAL` or mutating the database file.
    {
        let future_db = db_dir.join("future-user-version.sqlite");
        {
            let seed = Connection::open(&future_db).expect("seed future db");
            seed.execute_batch(&format!(
                "PRAGMA journal_mode = DELETE; PRAGMA user_version = {};",
                schema::CURRENT_SCHEMA_VERSION + 1
            ))
            .expect("set future user_version");
            seed.execute("CREATE TABLE sentinel(x INTEGER)", [])
                .expect("create sentinel");
        }
        let bytes_before = fs::read(&future_db).expect("read future db before");
        let err = match SqliteStateBackend::open(&future_db) {
            Ok(_) => panic!("writer open on newer user_version must fail closed"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::BackendConflict);
        assert_eq!(
            fs::read(&future_db).expect("read future db after"),
            bytes_before,
            "writer open must not mutate newer-schema DB bytes before rejecting"
        );
        assert!(!PathBuf::from(format!("{}-wal", future_db.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", future_db.display())).exists());
    }

    // 6. #8 Backup restore reconciliation boundaries + #17/#18 apply_sync_plan + #21 atomic read tx.
    {
        let git_ws = ws.path().join("backup-scope-repo");
        fs::create_dir_all(git_ws.join("docs/milestones/archive")).unwrap();
        fs::create_dir_all(git_ws.join("docs/plans/archive")).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(git_command(&git_ws, &args).unwrap().status.success());
        }
        fs::write(
            git_ws.join("docs/plans/archive/AC-101.md"),
            "---\nid: AC-101\ntitle: Other Collection\nstatus: completed\n---\n",
        )
        .unwrap();
        fs::write(
            git_ws.join("docs/milestones/archive/AC-103.md"),
            "---\nid: AC-999\ntitle: Mismatched Spec ID\nstatus: completed\n---\n",
        )
        .unwrap();
        for args in [vec!["add", "."], vec!["commit", "-m", "seed head"]] {
            assert!(git_command(&git_ws, &args).unwrap().status.success());
        }
        // Uncommitted working-tree archive must NOT be trusted in a Git repo.
        fs::write(
            git_ws.join("docs/milestones/archive/AC-102.md"),
            "---\nid: AC-102\ntitle: Uncommitted Disk\nstatus: completed\n---\n",
        )
        .unwrap();
        // Branch-only committed archive on `milestone/AC-104` MUST reconcile when `row.branch` matches.
        assert!(
            git_command(&git_ws, &["checkout", "-b", "milestone/AC-104"])
                .unwrap()
                .status
                .success()
        );
        fs::write(
            git_ws.join("docs/milestones/archive/AC-104.md"),
            "---\nid: AC-104\ntitle: Branch Archive\nstatus: completed\n---\n",
        )
        .unwrap();
        assert!(
            git_command(&git_ws, &["add", "docs/milestones/archive/AC-104.md"])
                .unwrap()
                .status
                .success()
        );
        assert!(git_command(&git_ws, &["commit", "-m", "branch archive"])
            .unwrap()
            .status
            .success());
        assert!(git_command(&git_ws, &["checkout", "-"])
            .unwrap()
            .status
            .success());

        let b_db = git_ws.join(".acdd/state.sqlite");
        let b_bak = git_ws.join(".git/acdd");
        let b_opts = SqliteBackendOptions {
            db_path: b_db.clone(),
            backup_dir: Some(b_bak),
            workspace_root: Some(git_ws.clone()),
            spec_collection_paths: vec!["docs/milestones".to_owned(), "docs/plans".to_owned()],
            spec_collections: std::collections::BTreeMap::new(),
            is_custom_db: false,
        };
        let b_backend = SqliteStateBackend::open_with_options(b_opts.clone()).unwrap();
        let mut r101 = RunHarness::build_sample_graph(
            "run-101",
            "AC-101",
            git_ws.to_str().unwrap(),
            None,
            &["T1"],
        )
        .unwrap();
        r101.run.spec_path = Some("docs/milestones/AC-101.md".to_owned());
        r101.run.spec_id = Some("AC-101".to_owned());
        let mut r102 = RunHarness::build_sample_graph(
            "run-102",
            "AC-102",
            git_ws.to_str().unwrap(),
            None,
            &["T1"],
        )
        .unwrap();
        r102.run.spec_path = Some("docs/milestones/AC-102.md".to_owned());
        r102.run.spec_id = Some("AC-102".to_owned());
        let mut r103 = RunHarness::build_sample_graph(
            "run-103",
            "AC-103",
            git_ws.to_str().unwrap(),
            None,
            &["T1"],
        )
        .unwrap();
        r103.run.spec_path = Some("docs/milestones/AC-103.md".to_owned());
        r103.run.spec_id = Some("AC-103".to_owned());
        let mut r104 = RunHarness::build_sample_graph(
            "run-104",
            "AC-104",
            git_ws.to_str().unwrap(),
            None,
            &["T1"],
        )
        .unwrap();
        r104.run.spec_path = Some("docs/milestones/AC-104.md".to_owned());
        r104.run.spec_id = Some("AC-104".to_owned());
        r104.run.branch = Some("milestone/AC-104".to_owned());

        // Test #17/#18 apply_sync_plan removes dependencies and updates seq_order in SQLite.
        r101.dependencies.push(TaskDependencyRecord {
            run_id: r101.run.run_id.clone(),
            task_ref: TaskRef::new("T1").unwrap(),
            depends_on_workspace: "/ext".to_owned(),
            depends_on_run_id: RunId::new("ext-run").unwrap(),
            depends_on_ref: TaskRef::new("E1").unwrap(),
            satisfied: false,
        });
        b_backend
            .exec_tx(|tx| {
                tx.insert_run_graph(&r101)?;
                tx.insert_run_graph(&r102)?;
                tx.insert_run_graph(&r103)?;
                tx.insert_run_graph(&r104)
            })
            .unwrap();

        let sync_plan = SyncPlan {
            transition: TransitionPlan {
                preconditions: StoragePreconditions {
                    expected_run_generation: 1,
                    expected_node_versions: Vec::new(),
                    expected_claim_revision: None,
                    expected_contract_revision: None,
                    expected_worker_id: None,
                    required_predecessor_trees: Vec::new(),
                },
                bump_run_generation: true,
                new_run_status: None,
                new_root_cycle_count: None,
                completed_at: None,
                node_patches: Vec::new(),
                dependency_patches: Vec::new(),
                claim_patches: Vec::new(),
                new_artifacts: Vec::new(),
                new_blackboard: Vec::new(),
                blackboard_patches: Vec::new(),
                new_subtasks: Vec::new(),
                significant_transition: true,
            },
            added_nodes: Vec::new(),
            added_dependencies: Vec::new(),
            removed_dependencies: r101.dependencies.clone(),
            seq_order_updates: vec![("task:T1".to_owned(), 9)],
            spec_digest: "synced-digest".to_owned(),
        };
        b_backend
            .exec_tx(|tx| tx.apply_sync_plan(&r101.run.run_id, &sync_plan, None))
            .unwrap();
        let r101_after = b_backend.load_run_graph(&r101.run.run_id).unwrap().unwrap();
        assert!(r101_after.dependencies.is_empty());
        assert_eq!(
            r101_after
                .nodes
                .iter()
                .find(|n| n.node_id == "task:T1")
                .unwrap()
                .seq_order,
            9
        );

        // Verify #21 atomic BEGIN DEFERRED / ROLLBACK read snapshot under interleaved writer DELETE.
        {
            let (ro_conn, sv) = open_read_only_with_version(&b_db).unwrap().unwrap();
            let deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let deleted_hook = std::sync::Arc::clone(&deleted);
            let db_hook = b_db.clone();
            ro_conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
                if let rusqlite::hooks::AuthAction::Read { table_name, .. } = ctx.action {
                    if table_name == "gate_nodes"
                        && !deleted_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        if let Ok(wconn) = Connection::open(&db_hook) {
                            let _ = wconn.execute_batch(
                                "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
                            );
                            let _ = wconn.execute("DELETE FROM runs WHERE run_id = 'run-101'", []);
                        }
                    }
                }
                rusqlite::hooks::Authorization::Allow
            }));
            let snap_during_delete =
                load_run_graph_with_version(&ro_conn, &RunId::new("run-101").unwrap(), sv)
                    .unwrap()
                    .expect("snapshot should remain atomic across interleaved DELETE");
            assert!(deleted.load(std::sync::atomic::Ordering::SeqCst));
            assert!(!snap_during_delete.nodes.is_empty());
            assert!(ro_conn.is_autocommit(), "read tx must roll back on exit");
            assert!(b_backend
                .load_run_graph(&r101.run.run_id)
                .unwrap()
                .is_none());
            // Re-insert r101 cleanly for restore test below.
            b_backend.exec_tx(|tx| tx.insert_run_graph(&r101)).unwrap();
        }

        b_backend.after_commit_maintenance().unwrap();
        drop(b_backend);

        let _ = fs::remove_file(&b_db);
        let _ = fs::remove_file(git_ws.join(".acdd/state.sqlite-wal"));
        let _ = fs::remove_file(git_ws.join(".acdd/state.sqlite-shm"));
        let b_restored = RunHarness::open_sqlite_with_archive_reconciler(b_opts).unwrap();
        assert!(b_restored
            .load_run_graph(&RunId::new("run-101").unwrap())
            .unwrap()
            .is_some());
        assert!(b_restored
            .load_run_graph(&RunId::new("run-102").unwrap())
            .unwrap()
            .is_some());
        assert!(b_restored
            .load_run_graph(&RunId::new("run-103").unwrap())
            .unwrap()
            .is_some());
        assert!(b_restored
            .load_run_graph(&RunId::new("run-104").unwrap())
            .unwrap()
            .is_none());
        let tomb_104 = b_restored
            .load_tombstone(&RunId::new("run-104").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(tomb_104.terminal_status, TerminalStatus::Completed);
        assert!(tomb_104.closed_commit.is_some());
    }
}
