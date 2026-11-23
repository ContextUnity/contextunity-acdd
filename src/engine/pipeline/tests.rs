use super::cold::build;
use super::invalidation::{contract_mentions_import, module_exposes_provider_contract};
use acdd_index::{db::reader as db_reader, typed_facts::TypedFacts};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[test]
fn unreadable_existing_target_is_not_a_rebuildable_cache() {
    use std::os::unix::fs::PermissionsExt;
    let cleanup = acdd_core::fs::ScopedTempDir::create("acdd-unreadable").unwrap();
    let root = cleanup.path().to_path_buf();
    let target = root.join("custom_store.db");
    let connection = Connection::open(&target).unwrap();
    connection
        .execute_batch("CREATE TABLE tasks(id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(connection);
    let before = fs::read(&target).unwrap();
    let permissions = fs::metadata(&target).unwrap().permissions();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::File::open(&target).is_ok() {
        fs::set_permissions(&target, permissions).unwrap();
        eprintln!("permission-denial fixture requires a user without DAC override");
        return;
    }
    let result = build(&root, &target, None);
    fs::set_permissions(&target, permissions).unwrap();
    let after = fs::read(&target).unwrap();
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("TASK_DATABASE_COLLISION"));
    assert_eq!(after, before);
}

#[test]
fn added_trait_implementor_refreshes_unchanged_caller_candidates() {
    let cleanup = acdd_core::fs::ScopedTempDir::create("acdd-delta-dispatch").unwrap();
    let root = cleanup.path();
    fs::create_dir(root.join("src")).unwrap();
    for (path, source) in [
        ("defs.rs", "pub trait Work { fn run(&self); }\n"),
        (
            "alpha.rs",
            "use crate::defs::Work; pub struct Alpha; impl Work for Alpha { fn run(&self) {} }\n",
        ),
        (
            "caller.rs",
            "use crate::defs::Work; pub fn caller(value: &dyn Work) { value.run(); }\n",
        ),
    ] {
        fs::write(root.join("src").join(path), source).unwrap();
    }
    let db = root.join(".acdd/code-map.sqlite");
    let candidates = |path: &Path| {
        let conn = db_reader::open(path, root).unwrap();
        let mut statement = conn.prepare("SELECT d.id FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='caller' AND e.kind='possible_calls' ORDER BY d.id").unwrap();
        let result = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        result
    };
    build(root, &db, None).unwrap();
    assert_eq!(
        candidates(&db).len(),
        1,
        "cold build links caller to existing Alpha::run candidate"
    );
    let cold = root.join(".acdd/cold.sqlite");
    fs::write(
        root.join("src/beta.rs"),
        "use crate::defs::Work; pub struct Beta; impl Work for Beta { fn run(&self) {} }\n",
    )
    .unwrap();
    super::delta_with_selection(root, &db, &[PathBuf::from("src/beta.rs")], None, None).unwrap();
    build(root, &cold, None).unwrap();
    assert_eq!(
        candidates(&db),
        candidates(&cold),
        "delta and cold builds discover the same dynamic candidates after a new implementor is added"
    );
    assert_eq!(candidates(&db).len(), 2);
}

#[test]
fn import_contract_matching_observes_identifier_boundaries_and_unknowns() {
    let imported = BTreeSet::from(["Api".to_owned()]);
    let aliases = BTreeSet::from(["pkg.service".to_owned()]);

    assert!(contract_mentions_import(
        &serde_json::json!("Api"),
        &imported,
        &aliases
    ));
    assert!(contract_mentions_import(
        &serde_json::json!("pkg.service"),
        &imported,
        &aliases
    ));
    assert!(contract_mentions_import(
        &serde_json::json!("pkg.service.Client"),
        &imported,
        &aliases
    ));
    assert!(!contract_mentions_import(
        &serde_json::json!("ApiKey"),
        &imported,
        &aliases
    ));
    assert!(!contract_mentions_import(
        &serde_json::json!("pkg.service_extra"),
        &imported,
        &aliases
    ));
    assert!(contract_mentions_import(
        &serde_json::json!({ "kind": "unknown" }),
        &imported,
        &aliases
    ));
    assert!(contract_mentions_import(
        &serde_json::json!({
            "kind": "field",
            "receiver": "pkg.service.Client",
            "field": "call"
        }),
        &imported,
        &aliases
    ));
    assert!(!contract_mentions_import(
        &serde_json::json!({
            "kind": "field",
            "receiver": "Api",
            "field": "call"
        }),
        &imported,
        &aliases
    ));
}

#[test]
fn unavailable_or_ambiguous_import_facts_expand_conservatively() {
    use acdd_index::models::{Facts, Node, Reference};

    let missing_facts_db = Connection::open_in_memory().unwrap();
    missing_facts_db
        .execute_batch("CREATE TABLE local_facts(path TEXT PRIMARY KEY, facts_blob BLOB);")
        .unwrap();
    let mut facts = BTreeMap::new();
    let mut loaded_fact_files = 0;
    assert!(module_exposes_provider_contract(
        &missing_facts_db,
        "middle.py",
        "provider.py",
        &mut facts,
        &mut loaded_fact_files
    )
    .unwrap());

    let ambiguous_facts_db = Connection::open_in_memory().unwrap();
    ambiguous_facts_db
        .execute_batch(
            "CREATE TABLE local_facts(path TEXT PRIMARY KEY, facts_blob BLOB);
             CREATE TABLE path_dictionary(path_id INTEGER PRIMARY KEY, path TEXT UNIQUE);
             CREATE TABLE nodes(node_hash INTEGER PRIMARY KEY, path_id INTEGER, kind TEXT);
             CREATE TABLE edge_occurrences(line INTEGER, dst_hash INTEGER, owner_id INTEGER, evidence_id INTEGER, kind TEXT);
             CREATE TABLE coverage_evidence(evidence_id INTEGER PRIMARY KEY, evidence TEXT);
             INSERT INTO path_dictionary(path_id,path) VALUES(1,'middle.py'),(2,'provider.py');
             INSERT INTO nodes(node_hash,path_id,kind) VALUES(1,1,'module'),(2,2,'module'),(3,2,'function');
             INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'provider');
             INSERT INTO edge_occurrences(line,dst_hash,owner_id,evidence_id,kind) VALUES
                (1,2,1,1,'imports'),(1,3,1,1,'imports');",
        )
        .unwrap();
    let module = Node {
        native_semantic: None,
        id: "module:middle.py".to_owned(),
        kind: "module".to_owned(),
        name: "middle".to_owned(),
        qualname: "middle".to_owned(),
        path: "middle.py".to_owned(),
        line: 1,
        end_line: 1,
        is_test: false,
        language: "python".to_owned(),
        generated: false,
        details: serde_json::json!({"exports": [], "bindings": []}),
    };
    let typed = TypedFacts::from_public(Facts {
        nodes: vec![module.clone()],
        references: vec![Reference {
            source: module.id,
            expression: "provider".to_owned(),
            kind: "imports".to_owned(),
            line: 1,
            column: 0,
            alias: Some("provider".to_owned()),
            module: Some("provider".to_owned()),
            dynamic: false,
            receiver_hint: None,
        }],
        ..Facts::default()
    });
    facts.insert("middle.py".to_owned(), typed);
    assert!(module_exposes_provider_contract(
        &ambiguous_facts_db,
        "middle.py",
        "provider.py",
        &mut facts,
        &mut loaded_fact_files
    )
    .unwrap());
}

#[test]
fn return_contract_and_known_private_import_bound_the_python_frontier() {
    use acdd_index::{
        models::{Facts, Node, Reference},
        semantic::{TypeExpr, ValueFlowFacts},
    };

    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE local_facts(path TEXT PRIMARY KEY, facts_blob BLOB);
         CREATE TABLE path_dictionary(path_id INTEGER PRIMARY KEY, path TEXT UNIQUE);
         CREATE TABLE nodes(node_hash INTEGER PRIMARY KEY, path_id INTEGER, kind TEXT);
         CREATE TABLE edge_occurrences(line INTEGER, dst_hash INTEGER, owner_id INTEGER, evidence_id INTEGER, kind TEXT);
         CREATE TABLE coverage_evidence(evidence_id INTEGER PRIMARY KEY, evidence TEXT);
         INSERT INTO path_dictionary(path_id,path) VALUES(1,'middle.py'),(2,'provider.py');
         INSERT INTO nodes(node_hash,path_id,kind) VALUES(1,2,'module');
         INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'provider');
         INSERT INTO edge_occurrences(line,dst_hash,owner_id,evidence_id,kind) VALUES(1,1,1,1,'imports');",
    )
    .unwrap();

    let module = Node {
        native_semantic: None,
        id: "module:middle.py".to_owned(),
        kind: "module".to_owned(),
        name: "middle".to_owned(),
        qualname: "middle".to_owned(),
        path: "middle.py".to_owned(),
        line: 1,
        end_line: 2,
        is_test: false,
        language: "python".to_owned(),
        generated: false,
        details: serde_json::json!({
            "exports": [{"local": "visible", "name": "visible"}],
            "bindings": ["_provider", "visible"]
        }),
    };
    let function = Node {
        native_semantic: None,
        id: "function:middle.py:visible".to_owned(),
        kind: "function".to_owned(),
        name: "visible".to_owned(),
        qualname: "visible".to_owned(),
        path: "middle.py".to_owned(),
        line: 2,
        end_line: 2,
        is_test: false,
        language: "python".to_owned(),
        generated: false,
        details: serde_json::Value::Null,
    };
    let import = Reference {
        source: module.id.clone(),
        expression: "provider".to_owned(),
        kind: "imports".to_owned(),
        line: 1,
        column: 0,
        alias: Some("_provider".to_owned()),
        module: Some("provider".to_owned()),
        dynamic: false,
        receiver_hint: None,
    };
    let public_facts = || Facts {
        nodes: vec![module.clone(), function.clone()],
        references: vec![import.clone()],
        ..Facts::default()
    };

    let mut exposed_facts = TypedFacts::from_public(public_facts());
    exposed_facts.flows.insert(
        function.id.clone(),
        ValueFlowFacts {
            return_type: Some(TypeExpr::Named {
                name: "_provider.Api".to_owned(),
            }),
            ..ValueFlowFacts::default()
        },
    );
    let mut facts = BTreeMap::from([("middle.py".to_owned(), exposed_facts)]);
    let mut loaded_fact_files = 0;
    assert!(module_exposes_provider_contract(
        &db,
        "middle.py",
        "provider.py",
        &mut facts,
        &mut loaded_fact_files
    )
    .unwrap());

    let mut known_facts = TypedFacts::from_public(public_facts());
    known_facts.flows.insert(
        function.id.clone(),
        ValueFlowFacts {
            return_type: Some(TypeExpr::Named {
                name: "int".to_owned(),
            }),
            ..ValueFlowFacts::default()
        },
    );
    facts.insert("middle.py".to_owned(), known_facts);
    assert!(!module_exposes_provider_contract(
        &db,
        "middle.py",
        "provider.py",
        &mut facts,
        &mut loaded_fact_files
    )
    .unwrap());

    facts.insert(
        "middle.py".to_owned(),
        TypedFacts::from_public(public_facts()),
    );
    assert!(module_exposes_provider_contract(
        &db,
        "middle.py",
        "provider.py",
        &mut facts,
        &mut loaded_fact_files
    )
    .unwrap());
}

#[test]
fn extraction_rejects_a_file_entry_digest_mismatch() {
    let cleanup = acdd_core::fs::ScopedTempDir::create("acdd-ut-digest-mismatch").unwrap();
    let root = cleanup.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let source = "pub fn current() -> u8 { 1 }\n";
    fs::write(root.join("src/lib.rs"), source).unwrap();
    let adapter = acdd_core::config::load_adapter(root, None).unwrap();
    let mut entry = acdd_index::scanner::scan_with_adapter(root, &adapter)
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.path == "src/lib.rs")
        .unwrap();
    let actual_digest = super::shared::hash_source_bytes(source);
    entry.digest = "deliberately-stale-digest".to_owned();
    let dependencies = acdd_index::languages::dependency_registry::DependencyRegistry::collect(None);
    let error = super::shared::extract_typed_source_with_digest(
        &entry,
        source,
        &actual_digest,
        &dependencies,
        &adapter.spec_paths(),
        None,
    )
    .expect_err("stale extraction must fail closed");
    assert!(
        error.to_string().contains("source changed during extraction"),
        "unexpected error: {error:#}"
    );
}

fn database_sidecars(database: &Path) -> Vec<(PathBuf, std::io::Result<fs::Metadata>)> {
    ["-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let path = PathBuf::from(format!("{}{suffix}", database.display()));
            let metadata = fs::symlink_metadata(&path);
            (path, metadata)
        })
        .collect()
}

#[test]
fn delta_rollback_receipt_verification_matrix() {
    use super::delta::delta_with_test_hooks;
    use super::hooks::PipelineTestHooks;
    use acdd_index::db;
    use serde_json::{json, Value};
    use std::time::SystemTime;

    #[derive(Clone, Copy)]
    enum Phase {
        Failure,
        Verification,
        Publication,
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Receipt {
        Trusted,
        Untrusted,
        Absent,
        Directory,
    }
    struct Case {
        name: &'static str,
        before: fn(&Path),
        after: fn(&Path),
        late: fn(&Path),
        phase: Phase,
        receipt: Receipt,
        drift: bool,
        wal: bool,
        tampered: bool,
        retry: bool,
    }
    fn noop(_: &Path) {}
    fn touch(db: &Path) {
        fs::File::options()
            .write(true)
            .open(db)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
            .unwrap();
    }
    fn epoch(db: &Path) {
        fs::File::options()
            .write(true)
            .open(db)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
    }
    let base = acdd_core::fs::ScopedTempDir::create("acdd-delta-rollback-base").unwrap();
    fs::write(base.path().join("main.rs"), "pub fn before() {}\n").unwrap();
    let base_database = base.path().join(".acdd/code-map.sqlite");
    let first = build(base.path(), &base_database, None).unwrap();
    let conn = Connection::open(&base_database).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_file_delete BEFORE DELETE ON files BEGIN SELECT RAISE(ABORT, 'forced delta rollback'); END;").unwrap();
    drop(conn);
    let root = first["output_root"].as_str().unwrap();
    assert!(db::cache::publish_verified(&base_database, base.path(), root).unwrap());
    fs::write(base.path().join("main.rs"), "pub fn after_() {}\n").unwrap();
    let seed = fs::read(&base_database).unwrap();
    let copy = acdd_core::fs::ScopedTempDir::create("acdd-delta-rollback-copy").unwrap();
    fs::create_dir_all(copy.path().join(".acdd")).unwrap();
    let database = copy.path().join(".acdd/code-map.sqlite");
    let receipt = PathBuf::from(format!("{}.verified.json", database.display()));
    let mut cases = Vec::new();
    for (name, before) in [
        (
            "missing",
            (|db: &Path| fs::remove_file(format!("{}.verified.json", db.display())).unwrap())
                as fn(&Path),
        ),
        ("corrupt", |db: &Path| {
            fs::write(format!("{}.verified.json", db.display()), b"invalid json").unwrap()
        }),
        ("root", |db: &Path| {
            let path = format!("{}.verified.json", db.display());
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["root"] = json!("/wrong-root");
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }),
        ("database", |db: &Path| {
            let path = format!("{}.verified.json", db.display());
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["database"] = json!("/wrong-database");
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }),
        ("identity", |db: &Path| {
            let path = format!("{}.verified.json", db.display());
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["identity"]["main"]["size"] = json!(0);
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }),
        #[cfg(unix)]
        ("permissions", |db: &Path| {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                format!("{}.verified.json", db.display()),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }),
    ] {
        cases.push(Case {
            name,
            before,
            after: touch,
            late: noop,
            phase: Phase::Failure,
            receipt: Receipt::Untrusted,
            drift: false,
            wal: false,
            tampered: false,
            retry: false,
        });
    }
    cases.extend([
        Case {
            name: "tampered database",
            before: noop,
            after: |db| {
                Connection::open(db)
                    .unwrap()
                    .execute("UPDATE nodes SET name='tampered' WHERE kind='function'", [])
                    .unwrap();
            },
            late: noop,
            phase: Phase::Failure,
            receipt: Receipt::Untrusted,
            drift: false,
            wal: false,
            tampered: true,
            retry: false,
        },
        Case {
            name: "benign identity drift",
            before: noop,
            after: epoch,
            late: noop,
            phase: Phase::Failure,
            receipt: Receipt::Trusted,
            drift: true,
            wal: false,
            tampered: false,
            retry: false,
        },
        Case {
            name: "publication directory",
            before: noop,
            after: epoch,
            late: |db| {
                let path = format!("{}.verified.json", db.display());
                fs::remove_file(&path).unwrap();
                fs::create_dir(path).unwrap();
            },
            phase: Phase::Publication,
            receipt: Receipt::Directory,
            drift: false,
            wal: false,
            tampered: false,
            retry: false,
        },
        Case {
            name: "nonempty wal",
            before: noop,
            after: epoch,
            late: |db| {
                fs::write(format!("{}-wal", db.display()), b"nonempty wal").unwrap();
            },
            phase: Phase::Publication,
            receipt: Receipt::Absent,
            drift: false,
            wal: true,
            tampered: false,
            retry: false,
        },
        Case {
            name: "verification drift",
            before: noop,
            after: epoch,
            late: touch,
            phase: Phase::Verification,
            receipt: Receipt::Absent,
            drift: false,
            wal: false,
            tampered: false,
            retry: false,
        },
        Case {
            name: "publication drift",
            before: noop,
            after: epoch,
            late: touch,
            phase: Phase::Publication,
            receipt: Receipt::Absent,
            drift: false,
            wal: false,
            tampered: false,
            retry: false,
        },
        Case {
            name: "valid receipt and retry",
            before: noop,
            after: noop,
            late: noop,
            phase: Phase::Failure,
            receipt: Receipt::Trusted,
            drift: false,
            wal: false,
            tampered: false,
            retry: true,
        },
    ]);
    for case in cases {
        fs::write(&database, &seed).unwrap();
        assert!(db::cache::publish_verified(&database, base.path(), root).unwrap());
        let before = db::cache::identity(&database).unwrap();
        let hooks = match case.phase {
            Phase::Failure => PipelineTestHooks::delta_failure(case.before, case.after),
            Phase::Verification => {
                PipelineTestHooks::delta_failure_during_verification(case.after, case.late)
            }
            Phase::Publication => {
                PipelineTestHooks::delta_failure_before_publication(case.after, case.late)
            }
        };
        let error = delta_with_test_hooks(
            base.path(),
            &database,
            &[PathBuf::from("main.rs")],
            &copy.path().join(".acdd/extraction-cache"),
            &hooks,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("forced delta rollback"),
            "{}: {error:#}",
            case.name
        );
        #[cfg(unix)]
        if case.name == "permissions" {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let identity = db::cache::identity(&database).unwrap();
        assert_eq!(
            db::cache::matches_with_reason(&database, base.path(), root, &identity).is_ok(),
            case.receipt == Receipt::Trusted,
            "{}",
            case.name
        );
        if case.drift {
            assert_ne!(before, identity, "identity drift was not exercised");
        }
        if case.tampered {
            assert!(db::reader::open(&database, base.path()).is_err());
        }
        if case.receipt == Receipt::Directory {
            assert!(receipt.is_dir());
            assert_eq!(fs::read_dir(&receipt).unwrap().count(), 0);
            fs::remove_dir(&receipt).unwrap();
        }
        if case.receipt == Receipt::Absent {
            assert!(!receipt.exists());
        }
        if case.wal {
            let wal = format!("{}-wal", database.display());
            assert_eq!(fs::read(&wal).unwrap(), b"nonempty wal");
            fs::remove_file(wal).unwrap();
        }
        let conn = Connection::open(&database).unwrap();
        if !case.tampered {
            assert_eq!(
                conn.query_row("SELECT name FROM nodes WHERE name='before'", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
                "before"
            );
        }
        conn.execute_batch("DROP TRIGGER reject_file_delete;").unwrap();
        drop(conn);
        if case.retry {
            assert!(
                receipt.exists(),
                "rollback must preserve the verified receipt"
            );
            let retried = super::delta::delta_with_selection(
                base.path(),
                &database,
                &[PathBuf::from("main.rs")],
                None,
                Some(&copy.path().join(".acdd/extraction-cache")),
            )
            .unwrap();
            assert_ne!(retried["output_root"].as_str().unwrap(), root);
            let conn = Connection::open(&database).unwrap();
            let name: String = conn
                .query_row("SELECT name FROM nodes WHERE name='after_'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(name, "after_");
        }
        for (sidecar, metadata) in database_sidecars(&database) {
            assert!(
                matches!(metadata, Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                "{}: sidecar remains or cannot be inspected: {}",
                case.name,
                sidecar.display()
            );
        }
        if receipt.exists() {
            fs::remove_file(&receipt).unwrap();
        }
    }
}
