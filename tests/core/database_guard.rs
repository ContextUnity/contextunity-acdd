use crate::common::Workspace;
use contextunity_acdd::db::tasks_store::TasksStore;
use contextunity_acdd::db::writer::{build, validate_code_map_target};

#[test]
fn database_publication_recovery_matrix() {
    let ws = Workspace::new();
    ws.write("src/unit_00.rs", "fn original() {}\n");
    let db = ws.path("index.sqlite");
    let old = build(ws.root(), &db, None).unwrap();
    let before = std::fs::read(&db).unwrap();
    let receipt_bytes = std::fs::read(ws.path("index.sqlite.verified.json")).unwrap();
    enum Input {
        PipelineFailures,
        SidecarHazards,
        StageSidecars,
        ActiveReader,
        InvalidCache,
    }
    for case in [
        Input::PipelineFailures,
        Input::SidecarHazards,
        Input::StageSidecars,
        Input::ActiveReader,
        Input::InvalidCache,
    ] {
        // Drop scenario-owned handles before this point; remove links without following them.
        for entry in std::fs::read_dir(ws.root()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                std::fs::remove_dir_all(entry.path()).unwrap();
            } else {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        ws.write("src/unit_00.rs", "fn original() {}\n");
        std::fs::write(&db, &before).unwrap();
        std::fs::write(ws.path("index.sqlite.verified.json"), &receipt_bytes).unwrap();
        match case {
            Input::PipelineFailures => {
                use contextunity_acdd::{
                    db::cache,
                    engine::pipeline::{build_with_test_hooks, PipelineTestHooks},
                };
                use std::sync::{Arc, Mutex};

                let receipt = ws.path("index.sqlite.verified.json");
                std::fs::write(ws.path("src/unit_00.rs"), "fn changed() {}\n").unwrap();
                let unrelated = ws.path("unrelated.txt");
                std::fs::write(&unrelated, b"preserve").unwrap();
                for (fail_consumer, obstruct_cleanup, published) in [
                    (false, true, true),
                    (true, false, false),
                    (true, true, false),
                ] {
                    std::fs::write(&db, &before).unwrap();
                    let conn = contextunity_acdd::db::reader::open(&db, ws.root()).unwrap();
                    contextunity_acdd::core::commitments::verify(&conn).unwrap();
                    drop(conn);
                    let seal = old["output_root"].as_str().unwrap();
                    assert!(cache::publish_verified(&db, ws.root(), seal).unwrap());
                    assert!(cache::matches(
                        &db,
                        ws.root(),
                        seal,
                        &cache::identity(&db).unwrap()
                    ));
                    let receipt_before = std::fs::read(&receipt).unwrap();
                    let recorded = Arc::new(Mutex::new(None));
                    let hook_recorded = Arc::clone(&recorded);
                    let obstruct = move |stage: &std::path::Path| {
                        let displaced = stage.with_extension("displaced");
                        std::fs::rename(stage, &displaced).unwrap();
                        std::fs::write(stage, b"obstruction").unwrap();
                        *hook_recorded.lock().unwrap() = Some((stage.to_owned(), displaced));
                    };
                    let hooks = match (fail_consumer, obstruct_cleanup) {
                        (false, true) => PipelineTestHooks::before_cleanup(obstruct),
                        (true, false) => PipelineTestHooks::fail_consumer(),
                        (true, true) => PipelineTestHooks::fail_consumer_before_cleanup(obstruct),
                        (false, false) => unreachable!(),
                    };
                    let result = build_with_test_hooks(ws.root(), &db, None, hooks);
                    if !obstruct_cleanup {
                        assert!(result.is_err());
                        assert_eq!(std::fs::read(&db).unwrap(), before);
                        assert_eq!(std::fs::read(&receipt).unwrap(), receipt_before);
                        assert!(cache::matches(
                            &db,
                            ws.root(),
                            seal,
                            &cache::identity(&db).unwrap()
                        ));
                    } else {
                        let (stage, displaced) = recorded.lock().unwrap().take().unwrap();
                        if published {
                            let report = result.unwrap();
                            let cleanup = report["cleanup_error"]
                                .as_str()
                                .expect("cleanup error must be reported");
                            assert!(cleanup.contains(&stage.display().to_string()), "{cleanup}");
                            assert_ne!(report["output_root"], old["output_root"]);
                            assert_eq!(report["verification_cache"], true);
                            let seal = report["output_root"].as_str().unwrap();
                            assert!(cache::matches(
                                &db,
                                ws.root(),
                                seal,
                                &cache::identity(&db).unwrap()
                            ));
                        } else {
                            let diagnostic = format!("{:#}", result.unwrap_err());
                            assert!(
                                diagnostic.contains("stage cleanup also failed"),
                                "{diagnostic}"
                            );
                            assert!(
                                diagnostic.contains(&stage.display().to_string()),
                                "{diagnostic}"
                            );
                            assert!(
                                diagnostic.contains("injected pipeline SQLite consumer failure"),
                                "{diagnostic}"
                            );
                            assert_eq!(std::fs::read(&stage).unwrap(), b"obstruction");
                            assert_eq!(std::fs::read(&db).unwrap(), before);
                            assert_eq!(std::fs::read(&receipt).unwrap(), receipt_before);
                            assert!(cache::matches(
                                &db,
                                ws.root(),
                                seal,
                                &cache::identity(&db).unwrap()
                            ));
                        }
                        assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
                        std::fs::remove_file(&stage).unwrap();
                        std::fs::rename(&displaced, &stage).unwrap();
                        std::fs::remove_dir_all(&stage).unwrap();
                    }
                    let recovered = build(ws.root(), &db, None).unwrap();
                    let conn = contextunity_acdd::db::reader::open(&db, ws.root()).unwrap();
                    contextunity_acdd::core::commitments::verify(&conn).unwrap();
                    drop(conn);
                    assert!(cache::matches(
                        &db,
                        ws.root(),
                        recovered["output_root"].as_str().unwrap(),
                        &cache::identity(&db).unwrap()
                    ));
                    assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
                    assert!(!std::fs::read_dir(ws.root()).unwrap().any(|e| e
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".acdd-stage-")));
                }
            }
            Input::SidecarHazards => {
                #[cfg(unix)]
                use std::os::unix::fs::symlink;

                for suffix in ["directory", "-wal", "-shm", "-journal"] {
                    let directory = suffix == "directory";
                    if !cfg!(unix) && !directory {
                        continue;
                    }
                    let suffix = if suffix == "directory" {
                        "-wal"
                    } else {
                        suffix
                    };
                    let sidecar = ws.path(format!("index.sqlite{suffix}"));

                    if directory {
                        std::fs::create_dir(&sidecar).unwrap();
                        std::fs::write(sidecar.join("unrelated"), "keep").unwrap();
                    } else {
                        #[cfg(unix)]
                        symlink(ws.path("missing"), &sidecar).unwrap();
                    }
                    let error = build(ws.root(), &db, None).unwrap_err();
                    assert!(format!("{error:#}").contains(&sidecar.display().to_string()));
                    assert_eq!(std::fs::read(&db).unwrap(), before);
                    if directory {
                        assert_eq!(std::fs::read(sidecar.join("unrelated")).unwrap(), b"keep");
                        std::fs::remove_dir_all(sidecar).unwrap();
                    } else {
                        std::fs::remove_file(sidecar).unwrap();
                    }
                }
                build(ws.root(), &db, None).unwrap();
                // Stale regular-file WAL/SHM sidecars left when a read-only handle outlives
                // the writer must checkpoint and clean up cleanly during publication.
                {
                    let rw = rusqlite::Connection::open(&db).unwrap();
                    let mode: String = rw
                        .query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0))
                        .unwrap();
                    assert_eq!(mode.to_ascii_lowercase(), "wal");
                    let ro = rusqlite::Connection::open_with_flags(
                        &db,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
                    )
                    .unwrap();
                    let _: i64 = ro
                        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
                        .unwrap();
                    drop(rw);
                    drop(ro);
                    assert!(
                        ws.path("index.sqlite-wal").exists()
                            || ws.path("index.sqlite-shm").exists()
                    );
                }
                build(ws.root(), &db, None).unwrap();
                assert!(!ws.path("index.sqlite-wal").exists());
                assert!(!ws.path("index.sqlite-shm").exists());
                assert!(!ws.path("index.sqlite-journal").exists());
                assert!(!std::fs::read_dir(ws.root()).unwrap().any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".acdd-stage-")));
            }
            Input::StageSidecars => {
                #[cfg(unix)]
                {
                    use contextunity_acdd::engine::pipeline::{
                        build_with_test_hooks, PipelineTestHooks,
                    };
                    use std::os::unix::fs::symlink;
                    use std::sync::{Arc, Mutex};

                    let stage_path = Arc::new(Mutex::new(None));
                    let recorded = Arc::clone(&stage_path);
                    let hooks = PipelineTestHooks::before_cleanup(move |stage| {
                        for suffix in ["-wal", "-shm"] {
                            std::fs::create_dir(stage.join(format!("generation.sqlite{suffix}")))
                                .unwrap();
                        }
                        symlink(
                            stage.join("missing"),
                            stage.join("generation.sqlite-journal"),
                        )
                        .unwrap();
                        *recorded.lock().unwrap() = Some(stage.to_owned());
                    });
                    let report = build_with_test_hooks(ws.root(), &db, None, hooks).unwrap();
                    let stage = stage_path.lock().unwrap().take().unwrap();
                    let error = report["cleanup_error"].as_str().unwrap();
                    for suffix in ["-wal", "-shm"] {
                        assert!(
                            error.contains(
                                &stage
                                    .join(format!("generation.sqlite{suffix}"))
                                    .display()
                                    .to_string()
                            ),
                            "{error}"
                        );
                    }
                    assert!(!error.contains("generation.sqlite-journal"), "{error}");
                    assert!(!stage.exists());
                }
            }
            Input::ActiveReader => {
                let path = ws.path("checkpoint.sqlite");
                let writer = rusqlite::Connection::open(&path).unwrap();
                writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);",
        )
        .unwrap();
                let reader = rusqlite::Connection::open(&path).unwrap();
                reader.execute_batch("BEGIN; SELECT * FROM t;").unwrap();
                writer.execute_batch("INSERT INTO t VALUES (2);").unwrap();
                for truncate in [false, true] {
                    let error =
                        contextunity_acdd::db::cache::checkpoint(&writer, truncate).unwrap_err();
                    assert!(error.to_string().contains("incomplete"), "{error:#}");
                }
                reader.execute_batch("ROLLBACK").unwrap();
                drop(reader);
                contextunity_acdd::db::cache::checkpoint(&writer, true).unwrap();
            }
            Input::InvalidCache => {
                std::fs::remove_dir_all(ws.path("src")).unwrap();
                let target = ws.root().join("cache.db");
                for contents in [
                    b"broken".as_slice(),
                    b"readable invalid SQLite header bytes".as_slice(),
                ] {
                    std::fs::write(&target, contents).unwrap();
                    build(ws.root(), &target, None).unwrap();
                    let connection = rusqlite::Connection::open(&target).unwrap();
                    let count: i64 = connection
                        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
                        .unwrap();
                    assert_eq!(count, 0);
                }
            }
        }
    }
}

#[test]
fn database_target_guard_matrix() {
    #[cfg(unix)]
    {
        if let Some(path) = std::env::var_os("ACDD_TEST_TASK_INITIALIZER") {
            std::fs::write(std::env::var_os("ACDD_TEST_TASK_READY").unwrap(), "ready").unwrap();
            let error = TasksStore::open(std::path::Path::new(&path))
                .err()
                .expect("published code index must not become a task store");
            assert!(error.to_string().contains("TASK_DATABASE_COLLISION"));
            return;
        }
    }
    let ws = Workspace::new();
    enum Input {
        PublicationLock,
        Unreadable,
        CollisionTargets,
    }
    for case in [
        Input::PublicationLock,
        Input::Unreadable,
        Input::CollisionTargets,
    ] {
        // Drop scenario-owned handles before this point; remove links without following them.
        for entry in std::fs::read_dir(ws.root()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                std::fs::remove_dir_all(entry.path()).unwrap();
            } else {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        match case {
            Input::PublicationLock => {
                #[cfg(unix)]
                {
                    let directory = ws.root();
                    let root = directory.join("source");
                    std::fs::create_dir_all(&root).unwrap();
                    std::fs::write(root.join("app.py"), "def published():\n    return 1\n")
                        .unwrap();
                    let index = directory.join("generation.sqlite");
                    build(&root, &index, None).unwrap();
                    let expected = std::fs::read(&index).unwrap();
                    for alias in [false, true] {
                        let target = directory.join(format!("published-{alias}.sqlite"));
                        let input = if alias {
                            std::fs::write(&target, []).unwrap();
                            let link = directory.join("task-alias.sqlite");
                            std::os::unix::fs::symlink(&target, &link).unwrap();
                            link
                        } else {
                            target.clone()
                        };
                        std::fs::write(&index, &expected).unwrap();
                        let mut lock_path = target.as_os_str().to_owned();
                        lock_path.push(".lock");
                        let lock = std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(lock_path)
                            .unwrap();
                        lock.lock().unwrap();
                        validate_code_map_target(&root, &target).unwrap();
                        let ready = directory.join(format!("ready-{alias}"));
                        let mut child =
                            std::process::Command::new(std::env::current_exe().unwrap())
                                .args([
                                    "--exact",
                                    "database_guard::database_target_guard_matrix",
                                    "--nocapture",
                                ])
                                .env("ACDD_TEST_TASK_INITIALIZER", &input)
                                .env("ACDD_TEST_TASK_READY", &ready)
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                                .unwrap();
                        let started = std::time::Instant::now();
                        while !ready.exists()
                            && started.elapsed() < std::time::Duration::from_secs(5)
                        {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        let blocked = child.try_wait().unwrap().is_none();
                        // Match atomic_build's final replacement while holding its real lock.
                        std::fs::rename(&index, &target).unwrap();
                        drop(lock);
                        let output = child.wait_with_output().unwrap();
                        assert!(ready.exists(), "initializer process did not start");
                        assert!(blocked, "task initialization bypassed the publication lock");
                        assert!(output.status.success(), "{output:?}");
                        assert_eq!(std::fs::read(&target).unwrap(), expected);
                    }
                }
            }
            Input::Unreadable => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    let target = ws.root().join("custom_store.db");
                    drop(TasksStore::open(&target).unwrap());
                    let before = std::fs::read(&target).unwrap();
                    let permissions = std::fs::metadata(&target).unwrap().permissions();
                    for command in ["delta", "build", "query", "serve"] {
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o0))
                            .unwrap();
                        if std::fs::File::open(&target).is_ok() {
                            std::fs::set_permissions(&target, permissions).unwrap();
                            eprintln!(
                                "permission-denial fixture requires a user without DAC override"
                            );
                            break;
                        }
                        let (rejected, diagnostic) = if command == "delta" {
                            let result = contextunity_acdd::db::writer::delta(
                                ws.root(),
                                &target,
                                &[ws.root().join("app.py")],
                            );
                            (result.is_err(), format!("{result:?}"))
                        } else {
                            let arguments = if command == "query" {
                                vec!["--db", target.to_str().unwrap(), "query", "overview"]
                            } else {
                                vec!["--db", target.to_str().unwrap(), command]
                            };
                            let output = crate::common::mcp_client::run_cli(&ws, &arguments);
                            (
                                !output.status.success(),
                                String::from_utf8_lossy(&output.stderr).into_owned(),
                            )
                        };
                        std::fs::set_permissions(&target, permissions.clone()).unwrap();
                        assert!(
                            rejected && diagnostic.contains("TASK_DATABASE_COLLISION"),
                            "{command}: {diagnostic}"
                        );
                        assert_eq!(std::fs::read(&target).unwrap(), before, "{command}");
                    }
                }
            }
            Input::CollisionTargets => {
                enum Target {
                    TaskName,
                    IndexName,
                    TaskSchema,
                    IndexSchema,
                    Configured,
                    CollidingConfig,
                    InvalidIdentity,
                    Alias,
                    Locked,
                    Fifo,
                }
                for (input, task_opener, expected) in [
                    (Target::TaskName, false, "TASK_DATABASE_COLLISION"),
                    (Target::IndexName, true, "TASK_DATABASE_COLLISION"),
                    (Target::TaskSchema, false, "TASK_DATABASE_COLLISION"),
                    (Target::IndexSchema, true, "TASK_DATABASE_COLLISION"),
                    (Target::Configured, false, "TASK_DATABASE_COLLISION"),
                    (Target::CollidingConfig, false, "TASK_DATABASE_COLLISION"),
                    (Target::InvalidIdentity, false, "TASK_DATABASE_COLLISION"),
                    (Target::Alias, false, "TASK_DATABASE_COLLISION"),
                    (Target::Locked, false, "TASK_DATABASE_COLLISION"),
                    (Target::Fifo, false, "regular file"),
                ] {
                    for entry in std::fs::read_dir(ws.root()).unwrap() {
                        let entry = entry.unwrap();
                        if entry.file_type().unwrap().is_dir() {
                            std::fs::remove_dir_all(entry.path()).unwrap();
                        } else {
                            std::fs::remove_file(entry.path()).unwrap();
                        }
                    }
                    let mut root = ws.root().to_path_buf();
                    let mut target = ws.path("custom.db");
                    let mut live_connection = None;
                    match input {
                        Target::TaskName => target = ws.path(".acdd/tasks.sqlite"),
                        Target::IndexName => target = ws.path(".acdd/code-map.sqlite"),
                        Target::TaskSchema | Target::IndexSchema => {
                            let connection = rusqlite::Connection::open(&target).unwrap();
                            connection.execute_batch(if task_opener {
                                "CREATE TABLE nodes (id TEXT PRIMARY KEY, name TEXT); CREATE TABLE edges (src TEXT, dst TEXT);"
                            } else {
                                "CREATE TABLE tasks (task_id TEXT PRIMARY KEY, milestone_ref TEXT, descriptor TEXT); CREATE TABLE task_gates (task_id TEXT, gate TEXT, revision INTEGER, state TEXT);"
                            }).unwrap();
                        }
                        Target::Configured | Target::CollidingConfig | Target::InvalidIdentity => {
                            let (config, path) = match input {
                                Target::Configured => ("task_repository: repo\ntasks_db: .acdd/state.sqlite\n", ".acdd/sub/../state.sqlite"),
                                Target::CollidingConfig => ("task_repository: repo\ntasks_db: .acdd/code-map.sqlite\n", ".acdd/code-map.sqlite"),
                                _ => ("task_repository: repo\ntask_project: invalid/name\ntasks_db: .acdd/state.sqlite\n", ".acdd/state.sqlite"),
                            };
                            ws.write("acdd.yaml", config);
                            target = ws.root().join(path);
                        }
                        Target::Alias => {
                            #[cfg(unix)]
                            {
                                let real = ws.path("private_var/repo");
                                std::fs::create_dir_all(&real).unwrap();
                                std::fs::write(
                                    real.join("acdd.yaml"),
                                    "task_repository: repo\ntasks_db: .acdd/state.sqlite\n",
                                )
                                .unwrap();
                                let alias = ws.path("var_repo");
                                std::os::unix::fs::symlink(&real, &alias).unwrap();
                                root = real.canonicalize().unwrap();
                                target = alias.join(".acdd/state.sqlite");
                            }
                            #[cfg(not(unix))]
                            {
                                continue;
                            }
                        }
                        Target::Locked => {
                            let connection = rusqlite::Connection::open(&target).unwrap();
                            connection
                                .execute_batch(
                                    "CREATE TABLE arbitrary (id INTEGER); BEGIN EXCLUSIVE;",
                                )
                                .unwrap();
                            live_connection = Some(connection);
                        }
                        Target::Fifo => {
                            #[cfg(unix)]
                            {
                                target = ws.path("test.fifo");
                                assert!(std::process::Command::new("mkfifo")
                                    .arg(&target)
                                    .status()
                                    .unwrap()
                                    .success());
                            }
                            #[cfg(not(unix))]
                            {
                                continue;
                            }
                        }
                    }
                    let before = if target.is_file() && live_connection.is_none() {
                        Some(std::fs::read(&target).unwrap())
                    } else {
                        None
                    };
                    if task_opener {
                        let error = TasksStore::open(&target)
                            .err()
                            .expect("code index must not become task store");
                        assert!(error.to_string().contains(expected), "{error:#}");
                    } else {
                        let error = validate_code_map_target(&root, &target).unwrap_err();
                        assert!(error.to_string().contains(expected), "{error:#}");
                        // Keep the external lock or FIFO live through validation.
                        if !matches!(input, Target::Locked | Target::Fifo) {
                            let error = build(&root, &target, None).unwrap_err();
                            assert!(error.to_string().contains(expected), "{error:#}");
                        }
                    }
                    if let Some(connection) = live_connection {
                        connection.execute_batch("ROLLBACK").unwrap();
                    }
                    if let Some(before) = before {
                        assert_eq!(std::fs::read(&target).unwrap(), before);
                    } else if !matches!(input, Target::Locked | Target::Fifo) {
                        assert!(!target.exists());
                    }
                }
            }
        }
    }
}
