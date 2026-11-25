use super::*;

#[test]
#[cfg(unix)]
fn excluded_entry_does_not_skip_unresolved_symlink_validation() {
    use std::os::unix::fs::symlink;
    let outside = Workspace::with_files(&[("file.rs", "pub fn outside() {}\n")]);
    let ws = Workspace::with_files(&[
        ("excluded.rs", "pub fn before() {}\n"),
        ("dir/file.rs", "pub fn inside() {}\n"),
    ]);
    let server = in_process_server(&ws);
    snapshot(&server).unwrap();
    fs::remove_file(ws.path("excluded.rs")).unwrap();
    fs::create_dir(ws.path("excluded.rs")).unwrap();
    fs::remove_dir_all(ws.path("dir")).unwrap();
    symlink(outside.root(), ws.path("dir")).unwrap();
    invalidate_mcp_connection_cache(&server.db).unwrap();
    let error = snapshot(&server).unwrap_err();
    assert!(format!("{error:#}").contains("dir/file.rs"), "{error:#}");
    fs::remove_file(ws.path("dir")).unwrap();
    drop(server);
}

#[test]
fn same_server_refreshes_exact_spelling_and_alias_renames() {
    let linked = Workspace::with_files(&[
        ("Foo.rs", "pub fn linked() {}\n"),
        ("Parent/Foo.rs", "pub fn parent() {}\n"),
        ("lib.rs", "pub fn before() {}\n"),
    ]);
    let ws = Workspace::with_files(&[
        ("Main.rs", "pub fn primary() {}\n"),
        ("case/Foo.rs", "pub fn renamed() {}\n"),
        ("unicode/caf\u{e9}.rs", "pub fn unicode() {}\n"),
        ("old.rs", "pub fn coordinate() {}\n"),
    ]);
    fs::create_dir_all(ws.path(".acdd")).unwrap();
    let primary_alias = ws.path(".acdd/primary-alias");
    let linked_alias = ws.path(".acdd/linked-alias");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(ws.root(), &primary_alias).unwrap();
        std::os::unix::fs::symlink(linked.root(), &linked_alias).unwrap();
    }
    #[cfg(not(unix))]
    let (primary_alias, linked_alias) = (ws.root().to_path_buf(), linked.root().to_path_buf());
    ws.write(
        "acdd.yaml",
        &format!(
            "linked_workspaces:\n  - name: library\n    path: {}\n",
            linked_alias.display()
        ),
    );
    let server = Server::new(primary_alias.clone(), ws.db())
        .with_cache_dir(ws.path(".acdd/extraction-cache"));
    snapshot(&server).unwrap();
    let mut client = McpTestHarness::new(&ws).unwrap();

    fn spelling(root: &std::path::Path, path: &str) -> String {
        let path = std::path::Path::new(path);
        let parent = path.parent().unwrap();
        let expected = fs::canonicalize(root.join(path)).unwrap();
        fs::read_dir(root.join(parent))
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| fs::canonicalize(entry.path()).unwrap() == expected)
            .unwrap()
            .file_name()
            .to_str()
            .map(|name| parent.join(name).to_string_lossy().into_owned())
            .unwrap()
    }
    struct Rename {
        linked: bool,
        old: &'static str,
        new: &'static str,
        paired_linked: bool,
        delete_after: bool,
        uri: bool,
    }
    for case in [
        Rename {
            linked: false,
            old: "Main.rs",
            new: "main.rs",
            paired_linked: true,
            delete_after: false,
            uri: false,
        },
        Rename {
            linked: false,
            old: "case/Foo.rs",
            new: "case/foo.rs",
            paired_linked: false,
            delete_after: true,
            uri: false,
        },
        Rename {
            linked: false,
            old: "unicode/caf\u{e9}.rs",
            new: "unicode/cafe\u{301}.rs",
            paired_linked: false,
            delete_after: true,
            uri: false,
        },
        Rename {
            linked: true,
            old: "Parent",
            new: "parent",
            paired_linked: false,
            delete_after: false,
            uri: false,
        },
        Rename {
            linked: false,
            old: "old.rs",
            new: "a #%.rs",
            paired_linked: false,
            delete_after: false,
            uri: true,
        },
    ] {
        let owner = if case.linked { &linked } else { &ws };
        let before = snapshot(&server).unwrap();
        let old_key = spelling(owner.root(), case.old);
        let prefix = if case.linked { "[library]/" } else { "" };
        let suffix = if case.linked { "/Foo.rs" } else { "" };
        let old_path = format!("{prefix}{old_key}{suffix}");
        assert!(before["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["path"] == old_path));
        fs::rename(owner.path(case.old), owner.path(case.new)).unwrap();
        if case.paired_linked {
            fs::rename(linked.path("Foo.rs"), linked.path("foo.rs")).unwrap();
        }
        let new_key = spelling(owner.root(), case.new);
        let new_path = format!("{prefix}{new_key}{suffix}");
        if case.uri {
            let (_, response) = client
                .call(
                    "code_map_search",
                    json!({"pattern":"coordinate", "format":"markdown"}),
                )
                .unwrap();
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            let uri = ws.file_uri(case.new, Some(1));
            assert!(uri.ends_with("/a%20%23%25.rs#L1"), "{uri}");
            assert!(text.contains(&uri), "{text}");
        }
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let after = snapshot(&server).unwrap();
        let paths: Vec<_> = after["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&new_path.as_str()), "{paths:?}");
        if case.paired_linked {
            assert!(paths.contains(&"[library]/foo.rs"), "{paths:?}");
        }
        if old_key != new_key {
            // The URI row already refreshed through the MCP search caller.
            if !case.uri {
                assert_eq!(after["freshness"]["refresh"], "delta");
            }
            assert!(!paths.contains(&old_path.as_str()), "{paths:?}");
        }
        if case.delete_after {
            fs::remove_file(owner.path(&new_key)).unwrap();
            invalidate_mcp_connection_cache(&server.db).unwrap();
            assert!(!snapshot(&server).unwrap()["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["path"] == new_path));
        }
    }

    for (first, second, case_sensitive) in [
        ("Foo.rs", "foo.rs", true),
        ("caf\u{e9}.rs", "cafe\u{301}.rs", false),
    ] {
        if case_sensitive && acdd_core::fs::is_case_insensitive_volume(ws.root()) {
            continue;
        }
        ws.write(first, "pub fn upper() {}\n");
        invalidate_mcp_connection_cache(&server.db).unwrap();
        snapshot(&server).unwrap();
        ws.write(second, "pub fn lower() {}\n");
        if !case_sensitive {
            let entries = fs::read_dir(ws.root())
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("caf"))
                .count();
            if entries == 1 {
                continue;
            }
            assert_eq!(entries, 2);
        }
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let after = snapshot(&server).unwrap();
        for path in [first, second] {
            assert!(
                after["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["path"] == path),
                "{after}"
            );
        }
    }

    enum Mutation {
        Write(&'static str),
        Rename(&'static str),
        Delete,
    }
    for (mutation, expected_path, expected_name) in [
        (
            Mutation::Write("pub fn changed() {}\n"),
            Some("[library]/lib.rs"),
            Some("changed"),
        ),
        (
            Mutation::Rename("next.rs"),
            Some("[library]/next.rs"),
            Some("changed"),
        ),
        (Mutation::Delete, None, None),
    ] {
        let before = snapshot(&server).unwrap();
        let old = if matches!(mutation, Mutation::Delete) {
            "next.rs"
        } else {
            "lib.rs"
        };
        assert!(before["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["path"] == format!("[library]/{old}")));
        match mutation {
            Mutation::Write(source) => linked.write(old, source),
            Mutation::Rename(new) => fs::rename(linked.path(old), linked.path(new)).unwrap(),
            Mutation::Delete => fs::remove_file(linked.path(old)).unwrap(),
        }
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let after = snapshot(&server).unwrap();
        let node =
            after["nodes"].as_array().unwrap().iter().find(|node| {
                node["path"] == "[library]/lib.rs" || node["path"] == "[library]/next.rs"
            });
        assert_eq!(
            node.map(|node| node["path"].as_str().unwrap()),
            expected_path
        );
        assert_eq!(
            node.map(|node| node["name"].as_str().unwrap()),
            expected_name
        );
        assert_eq!(after["freshness"]["refresh"], "delta");
    }
    let after = snapshot(&server).unwrap();
    let cold = ws.path(".acdd/cold-spellings.sqlite");
    ws.build_to(&cold);
    let conn = db::reader::open(&cold, ws.root()).unwrap();
    let mut statement = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap();
    let cold_paths: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let paths: Vec<_> = after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, cold_paths);
    assert!(cold_paths
        .iter()
        .any(|path| path == "[library]/parent/Foo.rs"));
    let cold_root: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after["output_root"], cold_root);
    acdd_index::db::commitments::verify(&conn).unwrap();
    drop(statement);
    drop(conn);
    fs::remove_file(cold).unwrap();
    client.finish().unwrap();
    drop(server);
    #[cfg(unix)]
    {
        fs::remove_file(primary_alias).unwrap();
        fs::remove_file(linked_alias).unwrap();
    }
}

#[test]
fn same_server_refreshes_add_edit_rename_and_delete() {
    enum Mutation {
        Write(&'static str),
        SameMtime(&'static str),
        Rename(&'static str),
        Delete,
    }
    struct Case {
        path: &'static str,
        mutation: Mutation,
        expected: &'static [(&'static str, &'static str, u64)],
    }
    let ws = Workspace::with_files(&[("first.rs", "pub fn first() {}\n")]);
    let server = in_process_server(&ws);
    let mut before = snapshot(&server).unwrap();
    for case in [
        Case {
            path: "added.rs",
            mutation: Mutation::Write("pub fn added() {}\n"),
            expected: &[("added.rs", "added", 1), ("first.rs", "first", 1)],
        },
        Case {
            path: "first.rs",
            mutation: Mutation::SameMtime("pub fn after_() {}\n"),
            expected: &[("added.rs", "added", 1), ("first.rs", "after_", 1)],
        },
        Case {
            path: "first.rs",
            mutation: Mutation::Write("pub fn current() {\n    let value = 1;\n}\n"),
            expected: &[("added.rs", "added", 1), ("first.rs", "current", 3)],
        },
        Case {
            path: "first.rs",
            mutation: Mutation::Write(
                "pub fn current() {\n    let value = 1;\n    let next = value + 1;\n}\n",
            ),
            expected: &[("added.rs", "added", 1), ("first.rs", "current", 4)],
        },
        Case {
            path: "added.rs",
            mutation: Mutation::Rename("renamed.rs"),
            expected: &[("first.rs", "current", 4), ("renamed.rs", "added", 1)],
        },
        Case {
            path: "first.rs",
            mutation: Mutation::Delete,
            expected: &[("renamed.rs", "added", 1)],
        },
    ] {
        match case.mutation {
            Mutation::Write(source) => ws.write(case.path, source),
            Mutation::SameMtime(source) => {
                let path = ws.path(case.path);
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                ws.write(case.path, source);
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
            Mutation::Rename(new) => fs::rename(ws.path(case.path), ws.path(new)).unwrap(),
            Mutation::Delete => fs::remove_file(ws.path(case.path)).unwrap(),
        }
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let after = snapshot(&server).unwrap();
        let nodes = after["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), case.expected.len());
        for (node, &(path, name, end_line)) in nodes.iter().zip(case.expected) {
            assert_eq!(node["path"], path);
            assert_eq!(node["name"], name);
            assert_eq!(node["end_line"], end_line);
        }
        assert_ne!(before["corpus_hash"], after["corpus_hash"]);
        assert_ne!(before["output_root"], after["output_root"]);
        assert_eq!(after["freshness"]["status"], "source_inventory_matched");
        assert_eq!(after["freshness"]["output_root"], after["output_root"]);
        assert_eq!(after["freshness"]["corpus_hash"], after["corpus_hash"]);
        assert_eq!(after["freshness"]["refresh"], "delta");
        before = after;
    }
}

#[test]
fn external_delta_invalidates_cached_connection_without_another_write() {
    let ws = Workspace::with_files(&[("main.rs", "pub fn before() {}\n")]);
    let server = in_process_server(&ws);
    snapshot(&server).unwrap();
    ws.write("main.rs", "pub fn after() {}\n");
    let delta = contextunity_acdd::engine::pipeline::delta_with_selection(
        ws.root(),
        &server.db,
        &[PathBuf::from("main.rs")],
        None,
        Some(&ws.path(".acdd/extraction-cache")),
    )
    .unwrap();
    let identity = db::cache::identity(&server.db).unwrap();
    let after = snapshot(&server).unwrap();
    assert_eq!(after["nodes"][0]["name"], "after");
    assert_eq!(after["output_root"], delta["output_root"]);
    assert_eq!(after["freshness"]["refresh"], "none");
    assert_eq!(identity, db::cache::identity(&server.db).unwrap());
}

#[test]
#[cfg(unix)]
fn cold_rebuild_waits_for_active_mcp_reader_before_rename() {
    use std::os::unix::fs::MetadataExt;
    let ws = Workspace::with_files(&[("main.rs", "pub fn before() {}\n")]);
    let server = in_process_server(&ws);
    let second = in_process_server(&ws);
    snapshot(&server).unwrap();
    for (active, expected_blocked) in [(false, false), (true, true)] {
        ws.write("main.rs", "pub fn before() {}\n");
        invalidate_mcp_connection_cache(&server.db).unwrap();
        assert_eq!(snapshot(&server).unwrap()["nodes"][0]["name"], "before");
        assert!(snapshot(&second).unwrap()["nodes"][0]["name"] == "before");
        let inode = fs::metadata(&server.db).unwrap().ino();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let reader = active.then(|| {
            let server = server.clone();
            std::thread::spawn(move || {
                server.read(|conn| {
                    let name = crate::sqlite_fixture::first_function_name(conn)?;
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(json!({"name": name}))
                })
            })
        });
        if active {
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        ws.write("main.rs", "pub fn after() {}\n");
        let root = ws.root().to_path_buf();
        let db = server.db.clone();
        let (boundary_tx, boundary_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let builder = std::thread::spawn(move || {
            use contextunity_acdd::engine::pipeline::{
                build_with_selection_and_guard, hooks::PipelineTestHooks,
            };
            let hooks = PipelineTestHooks::before_generation_lock(move |_| {
                let _ = boundary_tx.send(());
            });
            let result = build_with_selection_and_guard(&root, &db, None, None, Some(hooks), None);
            let _ = done_tx.send(result);
        });
        let reached = boundary_rx.recv_timeout(Duration::from_secs(10));
        let early = done_rx.recv_timeout(if active {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(30)
        });
        let blocked = matches!(&early, Err(mpsc::RecvTimeoutError::Timeout));
        let inode_while_reading = fs::metadata(&server.db).unwrap().ino();
        if let Some(reader) = reader {
            release_tx.send(()).unwrap();
            assert_eq!(reader.join().unwrap().unwrap()["name"], "before");
        }
        let finished = match early {
            Ok(result) => Some(result),
            Err(_) => done_rx.recv_timeout(Duration::from_secs(30)).ok(),
        };
        assert!(
            reached.is_ok(),
            "builder did not reach generation lock: {reached:?}"
        );
        assert!(
            finished.is_some(),
            "builder did not complete after reader release"
        );
        builder.join().unwrap();
        let report = finished.unwrap().unwrap();
        assert_eq!(report["verification_cache"], true);
        assert_eq!(blocked, expected_blocked, "active={active}");
        if active {
            assert_eq!(inode_while_reading, inode);
        }
        assert_ne!(fs::metadata(&server.db).unwrap().ino(), inode);
        assert_eq!(snapshot(&server).unwrap()["nodes"][0]["name"], "after");
        assert_eq!(snapshot(&second).unwrap()["nodes"][0]["name"], "after");
        let conn = rusqlite::Connection::open(&server.db).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}

#[test]
fn unchanged_warm_reads_and_metadata_only_touch_do_not_rewrite_database() {
    let mut sources = Vec::new();
    for i in 0..64 {
        sources.push((
            format!("src/file_{i}.rs"),
            format!("pub fn value_{i}() {{}}\n"),
        ));
    }
    let files: Vec<_> = sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let ws = Workspace::with_files(&files);
    let server = in_process_server(&ws);
    let before = snapshot(&server).unwrap();
    server
        .read(|conn| {
            conn.busy_timeout(Duration::from_millis(1234))?;
            Ok(json!({}))
        })
        .unwrap();
    let response = server
        .read(|conn| {
            let timeout: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
            Ok(json!({"timeout": timeout}))
        })
        .unwrap();
    assert_eq!(response["timeout"], 1234);
    let identity = db::cache::identity(&server.db).unwrap();
    let bytes = fs::read(&server.db).unwrap();
    let mut elapsed = Vec::new();
    for _ in 0..5 {
        let current = snapshot(&server).unwrap();
        assert_eq!(current["corpus_hash"], before["corpus_hash"]);
        assert_eq!(current["output_root"], before["output_root"]);
        assert_eq!(current["freshness"]["refresh"], "none");
        assert_eq!(current["freshness"]["files_checked"], 64);
        assert_eq!(current["freshness"]["inventory_scan_ms"], 0.0);
        elapsed.push(current["freshness"]["inventory_scan_ms"].as_f64().unwrap());
    }
    fs::File::options()
        .write(true)
        .open(ws.path("src/file_0.rs"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
        .unwrap();
    let touched = snapshot(&server).unwrap();
    assert_eq!(touched["output_root"], before["output_root"]);
    assert_eq!(touched["corpus_hash"], before["corpus_hash"]);
    assert_eq!(touched["freshness"]["refresh"], "none");
    assert_eq!(identity, db::cache::identity(&server.db).unwrap());
    assert_eq!(bytes, fs::read(&server.db).unwrap());
    eprintln!("64-file warm source inventory scans (ms): {elapsed:?}");
}
