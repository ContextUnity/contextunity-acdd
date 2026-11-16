use super::*;

#[test]
fn worktree_churn_keeps_one_registration_for_the_shared_slot() {
    let owner = Workspace::with_files(&[("main.rs", "pub fn first() {}\n")]);
    let server = in_process_server(&owner);
    snapshot(&server).unwrap();
    let roots: [PathBuf; 5] = std::array::from_fn(|i| {
        let root = owner.path(format!(".acdd/churn-{i}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "pub fn other() {}\n").unwrap();
        root
    });
    for root in roots {
        let mut rebound = server.clone();
        rebound.db = root.join(".acdd/index.sqlite");
        rebound.root = root;
        snapshot(&rebound).unwrap();
        assert_eq!(server.reader_registration_count().unwrap(), 1);
    }
}

#[test]
fn receipt_failures_report_reason_and_read_recovers() {
    #[derive(Debug)]
    enum Fault {
        Corrupt,
        Directory,
        Missing,
    }
    let ws = Workspace::with_files(&[("main.rs", "pub fn before() {}\n")]);
    let server = in_process_server(&ws);
    let first = snapshot(&server).unwrap();
    let database = server.db.clone();
    drop(server);
    let receipt = PathBuf::from(format!("{}.verified.json", database.display()));
    assert!(receipt.exists());
    for (fault, expected_diagnostic) in [
        (
            Fault::Corrupt,
            Some(("verified receipt unavailable", "expected value")),
        ),
        (
            Fault::Directory,
            Some(("verified receipt publication failed", "os error")),
        ),
        (Fault::Missing, None),
    ] {
        assert!(db::cache::publish_verified(
            &database,
            ws.root(),
            first["output_root"].as_str().unwrap()
        )
        .unwrap());
        assert!(receipt.exists());
        match fault {
            Fault::Corrupt => fs::write(&receipt, b"invalid json").unwrap(),
            Fault::Directory => {
                fs::remove_file(&receipt).unwrap();
                fs::create_dir(&receipt).unwrap();
            }
            Fault::Missing => fs::remove_file(&receipt).unwrap(),
        }
        // Each observation reopens the reader after the receipt changed.
        let response = snapshot(&in_process_server(&ws)).unwrap();
        if let Some((message, reason)) = expected_diagnostic {
            let diagnostics = response["diagnostics"].to_string();
            assert!(diagnostics.contains(message), "{diagnostics}");
            assert!(
                diagnostics.contains(&database.display().to_string()),
                "{diagnostics}"
            );
            assert!(diagnostics.contains(reason), "{diagnostics}");
        }
        assert_eq!(response["nodes"][0]["name"], "before");
        assert_eq!(response["corpus_hash"], first["corpus_hash"]);
        assert_eq!(response["freshness"]["refresh"], "none");
        if receipt.is_dir() {
            assert_eq!(fs::read_dir(&receipt).unwrap().count(), 0);
            fs::remove_dir(&receipt).unwrap();
        } else {
            assert!(
                receipt.exists(),
                "verified database should regain its receipt"
            );
        }
    }
}

#[test]
fn query_failure_preserves_error_when_rollback_also_fails() {
    let ws = Workspace::with_files(&[("main.rs", "pub fn before() {}\n")]);
    let server = in_process_server(&ws);
    let error = server
        .read(|conn| {
            conn.execute_batch("ROLLBACK")?;
            anyhow::bail!("intentional query failure")
        })
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("intentional query failure"), "{message}");
    assert!(message.contains("query rollback failed"), "{message}");
    assert_eq!(snapshot(&server).unwrap()["nodes"][0]["name"], "before");
    snapshot(&server).unwrap();
    assert!(server
        .read(|_| anyhow::bail!("intentional query failure"))
        .is_err());
    snapshot(&server).unwrap();
    let err = server
        .read(|_| {
            IndexFixture::open(&server.db).tamper_function_names("tampered")?;
            Ok(json!({"should_not_escape": true}))
        })
        .unwrap_err();
    assert!(err.to_string().contains("generation changed"));
    assert!(snapshot(&server).is_err());
}

#[test]
fn mcp_rebuild_admission_preserves_corruption_and_rebuilds_valid_legacy_index() {
    #[derive(Debug)]
    enum Fault {
        Foreign {
            cached: bool,
        },
        Corruption {
            mismatch: &'static str,
            corruption: IndexCorruption,
        },
        Bytes(&'static [u8]),
        TamperedRoots,
        #[cfg(unix)]
        Symlink,
        Legacy,
    }
    struct Case {
        fault: Fault,
        accepted: bool,
        diagnostic: Option<&'static str>,
        mcp: bool,
    }
    let ws = Workspace::with_files(&[
        ("main.rs", "pub fn current() {}\n"),
        ("src/main.rs", "pub fn original() {}\n"),
        ("README.md", "# Current\nIndexed documentation.\n"),
    ]);
    let other = Workspace::with_files(&[("src/main.rs", "pub fn original() {}\n")]);
    let database = ws.db();
    ws.build_to(&database);
    let seed = fs::read(&database).unwrap();
    let server = Server::new(ws.root().to_path_buf(), database.clone())
        .with_cache_dir(ws.path(".acdd/extraction-cache"));
    let mut client = McpTestHarness::new(&ws).expect("MCP initialize");
    let cases = [
        Case {
            fault: Fault::Foreign { cached: false },
            accepted: false,
            diagnostic: Some("different workspace"),
            mcp: false,
        },
        Case {
            fault: Fault::Foreign { cached: true },
            accepted: false,
            diagnostic: Some("different workspace"),
            mcp: false,
        },
        Case {
            fault: Fault::Corruption {
                mismatch: "schema",
                corruption: IndexCorruption::Freelist,
            },
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::Corruption {
                mismatch: "schema",
                corruption: IndexCorruption::DocumentSearchData,
            },
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::Corruption {
                mismatch: "semantics",
                corruption: IndexCorruption::DocumentSearchData,
            },
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::Corruption {
                mismatch: "adapter",
                corruption: IndexCorruption::Freelist,
            },
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::Bytes(b"corrupt database"),
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::TamperedRoots,
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        Case {
            fault: Fault::Bytes(b""),
            accepted: false,
            diagnostic: None,
            mcp: true,
        },
        #[cfg(unix)]
        Case {
            fault: Fault::Symlink,
            accepted: false,
            diagnostic: None,
            mcp: false,
        },
        Case {
            fault: Fault::Legacy,
            accepted: true,
            diagnostic: None,
            mcp: true,
        },
    ];
    for case in cases {
        let fault_label = format!("{:?}", case.fault);
        {
            let _readers = db::cache::release_idle_readers(&database).unwrap();
            let _lock = db::cache::exclusive_lock(&database).unwrap();
            if ws.path("acdd.yaml").exists() {
                fs::remove_file(ws.path("acdd.yaml")).unwrap();
            }
            for suffix in ["-wal", "-shm", "-journal", ".verified.json"] {
                let path = PathBuf::from(format!("{}{suffix}", database.display()));
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
            fs::write(&database, &seed).unwrap();
        }
        let mut direct = server.clone();
        match case.fault {
            Fault::Foreign { cached } => {
                snapshot(&server).unwrap();
                direct = if cached {
                    let mut foreign = server.clone();
                    foreign.root = other.root().to_path_buf();
                    foreign
                } else {
                    Server::new(other.root().to_path_buf(), database.clone())
                        .with_cache_dir(other.path(".acdd/extraction-cache"))
                };
            }
            Fault::Corruption {
                mismatch,
                corruption,
            } => {
                if mismatch == "adapter" {
                    ws.write("acdd.yaml", "adapter_version: 2\n");
                } else {
                    let version = if mismatch == "schema" {
                        MetadataVersion::Schema
                    } else {
                        MetadataVersion::Semantics
                    };
                    let index = IndexFixture::open(&database);
                    index.outdated(version);
                    index.checkpoint();
                }
                IndexFixture::corrupt_file(&database, corruption);
            }
            Fault::Bytes(bytes) => fs::write(&database, bytes).unwrap(),
            Fault::TamperedRoots => {
                let index = IndexFixture::open(&database);
                index.tamper_function_names("tampered").unwrap();
                index.checkpoint();
                drop(index);
                ws.write("acdd.yaml", "roots:\n  - src\n");
            }
            #[cfg(unix)]
            Fault::Symlink => {
                let alias = ws.path("alias.sqlite");
                std::os::unix::fs::symlink(&database, &alias).unwrap();
                direct = Server::new(ws.root().to_path_buf(), alias)
                    .with_cache_dir(ws.path(".acdd/extraction-cache"));
            }
            Fault::Legacy => {
                rusqlite::Connection::open(&database)
                    .unwrap()
                    .execute_batch("DROP TABLE metadata")
                    .unwrap();
            }
        }
        let before = fs::read(&database).unwrap();
        let identity = db::cache::identity(&database).unwrap();
        if case.mcp {
            let response = client.request_value("tools/call",
                json!({"name":"code_map_overview","arguments":{"detail":"compact","format":"json"}})).expect("MCP transport");
            assert_eq!(
                response["result"]["isError"] == true,
                !case.accepted,
                "{response}"
            );
            if case.accepted {
                let overview: Value = serde_json::from_str(
                    response["result"]["content"][0]["text"].as_str().unwrap(),
                )
                .unwrap();
                assert_eq!(overview["freshness"]["refresh"], "rebuild");
                assert!(db::reader::open(&database, ws.root()).is_ok());
            }
        }
        let result = snapshot(&direct);
        assert_eq!(result.is_ok(), case.accepted, "{fault_label}: {result:?}");
        if let Some(diagnostic) = case.diagnostic {
            assert!(result.unwrap_err().to_string().contains(diagnostic));
        }
        if !case.accepted {
            assert_eq!(fs::read(&database).unwrap(), before);
            assert_eq!(db::cache::identity(&database).unwrap(), identity);
        }
        drop(direct);
        if ws.path("alias.sqlite").exists() {
            fs::remove_file(ws.path("alias.sqlite")).unwrap();
        }
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn newly_created_configured_root_is_admitted() {
    let ws = Workspace::with_gitdir(&[("acdd.yaml", "roots:\n  - src\n")])
        .expect("workspace with .git directory");
    let server = in_process_server(&ws);
    assert!(snapshot(&server).unwrap()["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
    for (initial, mutation_path, mutation_source, expected_before, expected_after) in [
        (
            vec![],
            "src/created.rs",
            "pub fn created() {}\n",
            vec![],
            vec!["created"],
        ),
        (
            vec![
                ("src/visible.rs", "pub fn visible() {}\n"),
                ("src/ignored.rs", "pub fn ignored() {}\n"),
            ],
            ".gitignore",
            "src/ignored.rs\n",
            vec!["ignored", "visible"],
            vec!["visible"],
        ),
    ] {
        if ws.path("src").exists() {
            fs::remove_dir_all(ws.path("src")).unwrap();
        }
        for (path, source) in initial {
            ws.write(path, source);
        }
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let before = snapshot(&server).unwrap();
        let names: Vec<_> = before["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, expected_before);
        ws.write(mutation_path, mutation_source);
        invalidate_mcp_connection_cache(&server.db).unwrap();
        let after = snapshot(&server).unwrap();
        let names: Vec<_> = after["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, expected_after);
        assert_eq!(after["freshness"]["refresh"], "rebuild");
    }
}
