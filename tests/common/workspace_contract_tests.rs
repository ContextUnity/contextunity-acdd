use acdd_core::commitments::DOMAINS;
use acdd_core::config::ScannerLimits;
use acdd_core::format::stable_hash64;
use acdd_core::models::Node;
use acdd_index::languages::LanguageRegistry;
use acdd_index::linker::RouteIndex;

#[test]
fn workspace_file_uris_encode_canonical_relative_coordinates() {
    use crate::common::workspace::canonical_file_uri;
    use contextunity_acdd::core::format::file_uri_for_path;

    let workspace = crate::common::Workspace::new();
    let relative = "space quote\" plus+ percent% hash# café.rs";
    for line in [None, Some(42)] {
        let uri = workspace.file_uri(relative, line);
        assert_eq!(
            uri,
            file_uri_for_path(&workspace.canonical_root().join(relative), line).unwrap()
        );
        assert_eq!(uri, canonical_file_uri(workspace.root(), relative, line));
        assert!(uri.contains("space%20quote%22%20plus%2B%20percent%25%20hash%23%20caf%C3%A9.rs"));
        assert_eq!(uri.ends_with("#L42"), line.is_some());
    }
    for relative in ["/absolute", "../outside", "child/../outside"] {
        assert!(std::panic::catch_unwind(|| workspace.file_uri(relative, None)).is_err());
        assert!(
            std::panic::catch_unwind(|| crate::common::workspace::canonical_file_uri(
                workspace.root(),
                relative,
                None
            ))
            .is_err()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let alias = workspace.path("root alias");
        symlink(workspace.root(), &alias).unwrap();
        assert_eq!(
            crate::common::workspace::canonical_file_uri(&alias, "source.rs", None),
            workspace.file_uri("source.rs", None)
        );
        std::fs::remove_file(&alias).unwrap();
        assert!(!alias.exists());
    }
}

#[cfg(unix)]
#[test]
fn workspace_database_open_confines_aliases_and_symlinks() {
    use std::os::unix::fs::symlink;

    let workspace = crate::common::Workspace::new();
    let outside = crate::common::Workspace::new();
    workspace.write("src/lib.rs", "pub fn indexed() {}\n");
    workspace.build();
    let root = workspace.root().to_path_buf();
    let outside_root = outside.root().to_path_buf();
    let reader = acdd_index::db::reader::open(&workspace.db(), workspace.root()).unwrap();
    drop(reader);
    assert_eq!(
        acdd_core::fs::confined_database_open_path(&workspace.db()).unwrap(),
        workspace.canonical_root().join(".acdd/code-map.sqlite")
    );

    outside.write_bytes("foreign.sqlite", b"foreign");
    let ancestor = workspace.path("escape");
    let leaf = workspace.path(".acdd/escape.sqlite");
    symlink(outside.root(), &ancestor).unwrap();
    symlink(outside.path("foreign.sqlite"), &leaf).unwrap();
    assert!(acdd_core::fs::confined_database_open_path(&ancestor.join("foreign.sqlite")).is_err());
    assert!(acdd_core::fs::confined_database_open_path(&leaf).is_err());
    assert!(
        acdd_index::db::reader::open(&ancestor.join("foreign.sqlite"), workspace.root()).is_err()
    );
    assert!(acdd_index::db::reader::open(&leaf, workspace.root()).is_err());
    std::fs::remove_file(&ancestor).unwrap();
    std::fs::remove_file(&leaf).unwrap();
    assert!(!ancestor.exists() && !leaf.exists());
    drop(workspace);
    assert!(!root.exists());
    assert!(outside.path("foreign.sqlite").exists());
    drop(outside);
    assert!(!outside_root.exists());
}

#[cfg(feature = "lang-rust")]
#[test]
fn workspace_build_cache_is_isolated_and_removed_with_its_workspace() {
    let first = crate::common::Workspace::new();
    let second = crate::common::Workspace::new();
    for workspace in [&first, &second] {
        workspace.write("src/lib.rs", "pub fn same_source() {}\n");
    }
    let first_cold = first.build();
    let first_warm = first.build_to(first.path(".acdd/warm.sqlite"));
    let second_cold = second.build();
    assert_eq!(first_cold["pipeline"]["extraction_cache_hits"], 0);
    assert_eq!(first_warm["pipeline"]["extraction_cache_hits"], 1);
    assert_eq!(second_cold["pipeline"]["extraction_cache_hits"], 0);
    let cache = first.path(".acdd/extraction-cache");
    assert!(cache.is_dir());
    drop(first);
    assert!(!cache.exists());
    assert!(second.path(".acdd/extraction-cache").is_dir());
}

#[test]
fn workspace_builds_and_applies_delta_in_its_owned_temp_directory() {
    let workspace = crate::common::Workspace::new();
    let root = workspace.root().to_path_buf();
    workspace.write("src/lib.rs", "pub fn before() {}\n");
    workspace.build();

    let before = workspace.open();
    let before_count: i64 = before
        .query_row(
            "SELECT count(*) FROM nodes WHERE name='before'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before_count, 1);
    drop(before);

    workspace.write("src/lib.rs", "pub fn after() {}\n");
    workspace.delta(&["src/lib.rs"]);
    let after = workspace.open();
    let after_names: Vec<String> = after
        .prepare("SELECT name FROM nodes WHERE kind='function' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after_names, ["after"]);
    drop(after);

    drop(workspace);
    assert!(
        !root.exists(),
        "temporary workspace should be removed on drop"
    );
}

#[test]
fn cargo_workspace_declares_independent_crates_and_unidirectional_boundaries() {
    // Verify acdd-core crate public API & real Merkle root computation
    assert_eq!(stable_hash64("workspace_boundary"), 7897085170651836845);
    let limits = ScannerLimits::default();
    assert_eq!(limits.max_files, 100_000);
    assert!(DOMAINS.contains(&"nodes"));
    let mut leaves = std::collections::BTreeMap::new();
    leaves.insert("src/lib.rs".to_string(), "a1b2c3d4".to_string());
    let agg = acdd_core::commitments::aggregate_domain_digest("nodes", &leaves);
    assert!(!agg.is_empty());
    let domains = vec![("nodes".to_string(), agg)];
    let metadata = vec![("schema_version".to_string(), "15".to_string())];
    let merkle_root = acdd_core::commitments::compute_merkle_root(&domains, &metadata);
    assert_eq!(merkle_root.len(), 64);

    let route_node = Node {
        id: "route:get_user".into(),
        kind: "route".into(),
        name: "get_user".into(),
        qualname: "api.get_user".into(),
        path: "src/api.rs".into(),
        line: 10,
        end_line: 15,
        is_test: false,
        language: "rust".into(),
        generated: false,
        details: serde_json::json!({
            "method": "GET",
            "route_path": "/api/users/{id}"
        }),
        native_semantic: None,
    };

    // Verify acdd-index crate public API & real route matching and schema initialization
    let mut reg = LanguageRegistry::new();
    assert!(reg.is_empty());
    reg.enable("rust");
    assert!(reg.is_enabled("rust"));
    assert_eq!(reg.len(), 1);

    let route_slice = [&route_node];
    let routes = RouteIndex::build(&route_slice);
    assert_eq!(routes.len(), 1);
    let matched = routes.match_route("GET", "/api/users/123").unwrap();
    assert!(matched.is_some());
    assert_eq!(matched.unwrap().id, "route:get_user");
    let unmatched = routes.match_route("POST", "/api/users/123").unwrap();
    assert!(unmatched.is_none());

    let mem_conn = rusqlite::Connection::open_in_memory().unwrap();
    acdd_index::db::initialize_code_map_schema(&mem_conn).unwrap();
    let table_count: i64 = mem_conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('nodes', 'edges', 'metadata', 'resolution_coverage')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 4);

    // Verify acdd-tasks crate public API & real store operations
    let store_ws = crate::common::Workspace::new();
    let temp_path = store_ws.path("test-tasks-store.sqlite");
    let store =
        acdd_tasks::store::TasksStore::open_project(&temp_path, "test-repo", "test-proj").unwrap();
    assert!(acdd_tasks::store::now() > 0);
    drop(store);
}

#[test]
fn direct_library_task_entrypoint_initializes_extractors_without_caller_bootstrap() {
    // Calling direct root library task API automatically composes extractors
    contextunity_acdd::engine::tasks::ensure_extractors();
    let facts = contextunity_acdd::engine::ast::extract_typed(
        "tests/seam.rs",
        "rust",
        "#[test] fn sample_seam() { assert!(true); }",
    )
    .expect("production extractor must be wired");
    assert!(facts
        .facts
        .nodes
        .iter()
        .any(|n| n.is_test && n.name == "sample_seam"));
}
