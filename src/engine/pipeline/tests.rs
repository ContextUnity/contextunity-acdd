use super::*;

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
    delta(root, &db, &[PathBuf::from("src/beta.rs")]).unwrap();
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
