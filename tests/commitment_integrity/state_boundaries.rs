use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Strip Rust `//` line comments and `/* ... */` block comments while preserving code tokens.
fn strip_rust_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    let mut block_depth = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        if block_depth > 0 {
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                block_depth += 1;
                i += 2;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                block_depth -= 1;
                i += 2;
                continue;
            }
            if bytes[i] == b'\n' {
                out.push('\n');
            }
            i += 1;
            continue;
        }

        if in_string {
            out.push(bytes[i] as char);
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            block_depth = 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = true;
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    out
}

fn contains_brace_std_import(code: &str, mod_name: &str) -> bool {
    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.contains("std::{") {
            let after = trimmed.split("std::{").nth(1).unwrap_or("");
            let inside = after.split('}').next().unwrap_or("");
            for part in inside.split(',') {
                let item = part.trim().split("::").next().unwrap_or("").trim();
                if item == mod_name {
                    return true;
                }
            }
        }
    }
    false
}

#[test]
fn acdd_state_cargo_dependencies_and_pure_core_import_fence() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !manifest_dir.join("crates/acdd-spi").exists(),
        "crates/acdd-spi must remain folded into crates/acdd-core"
    );

    let core_src_dir = manifest_dir.join("crates/acdd-core/src");
    for moved in [
        "commitments.rs",
        "interner.rs",
        "models.rs",
        "semantic.rs",
        "typed_facts.rs",
    ] {
        assert!(
            !core_src_dir.join(moved).exists(),
            "crates/acdd-core/src/{moved} must remain moved into crates/acdd-index"
        );
    }

    let core_cargo_path = manifest_dir.join("crates/acdd-core/Cargo.toml");
    let core_cargo = std::fs::read_to_string(&core_cargo_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", core_cargo_path.display()));
    let parsed_core_cargo: toml::Value = toml::from_str(&core_cargo)
        .unwrap_or_else(|e| panic!("parse {}: {e}", core_cargo_path.display()));
    let core_forbidden_crates = [
        "acdd-index",
        "acdd-state",
        "rusqlite",
        "tree-sitter",
        "zstd",
        "rayon",
        "hashbrown",
        "clap",
        "rmcp",
        "tokio",
    ];
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = parsed_core_cargo
            .get(section)
            .and_then(toml::Value::as_table)
        {
            for forbidden in core_forbidden_crates {
                assert!(
                    !table.contains_key(forbidden),
                    "crates/acdd-core/Cargo.toml [{section}] must not depend on '{forbidden}'"
                );
            }
        }
    }

    let state_dir = manifest_dir.join("crates/acdd-state");
    let state_cargo_path = state_dir.join("Cargo.toml");
    let state_cargo = std::fs::read_to_string(&state_cargo_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", state_cargo_path.display()));
    let parsed_cargo: toml::Value = toml::from_str(&state_cargo)
        .unwrap_or_else(|e| panic!("parse {}: {e}", state_cargo_path.display()));

    let forbidden_crates = ["acdd-index", "tree-sitter", "clap", "rmcp", "tokio"];
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = parsed_cargo.get(section).and_then(toml::Value::as_table) {
            for forbidden in forbidden_crates {
                assert!(
                    !table.contains_key(forbidden),
                    "crates/acdd-state/Cargo.toml [{section}] must not depend on '{forbidden}'"
                );
            }
        }
    }

    // Check Pure Core (`crates/acdd-state/src/machine/**`) and rusqlite isolation across `crates/acdd-state/src/**`.
    let state_src_dir = state_dir.join("src");
    let mut files = Vec::new();
    collect_rs_files(&state_src_dir, &mut files);
    assert!(
        !files.is_empty(),
        "expected Rust source files under {}",
        state_src_dir.display()
    );

    let machine_dir = state_src_dir.join("machine");
    let sqlite_dir = state_src_dir.join("store/sqlite");

    // Table-driven fence rules checked against every file under `crates/acdd-state/src/**`.
    let pure_core_forbidden: &[(&str, &str)] = &[
        ("std::fs", "fs"),
        ("std::process", "process"),
        ("std::net", "net"),
        ("rusqlite", "rusqlite"),
    ];

    for file in &files {
        let raw = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let code = strip_rust_comments(&raw);
        let rel = file.strip_prefix(manifest_dir).unwrap_or(file);

        if file.starts_with(&machine_dir) {
            for &(path_pat, mod_name) in pure_core_forbidden {
                assert!(
                    !code.contains(path_pat) && !contains_brace_std_import(&code, mod_name),
                    "Pure Core file {} must not import or reference '{}'",
                    rel.display(),
                    path_pat
                );
            }
        }

        if !file.starts_with(&sqlite_dir) {
            assert!(
                !code.contains("rusqlite"),
                "file {} outside crates/acdd-state/src/store/sqlite/ must not import or reference 'rusqlite'",
                rel.display()
            );
        }
    }
}

#[test]
fn three_crate_wiring_and_state_bridge_providers_seam() {
    // acdd-core: hashing, scanner limits, and path policy predicates
    assert_eq!(
        acdd_core::format::stable_hash64("workspace_boundary"),
        7897085170651836845
    );
    assert_eq!(acdd_core::config::ScannerLimits::default().max_files, 100_000);
    assert!(acdd_core::path_policy::WorkspacePathPolicy::default().is_candidate_included("Cargo.lock"));

    // acdd-index: commitments, language registry, route index, schema DDL, and AST extractor
    assert!(acdd_index::commitments::DOMAINS.contains(&"nodes"));
    let mut leaves = std::collections::BTreeMap::new();
    leaves.insert("src/lib.rs".to_string(), "a1b2c3d4".to_string());
    let agg = acdd_index::commitments::aggregate_domain_digest("nodes", &leaves);
    assert!(!agg.is_empty());
    let merkle_root = acdd_index::commitments::compute_merkle_root(
        &[("nodes".to_string(), agg)],
        &[("schema_version".to_string(), "15".to_string())],
    );
    assert_eq!(merkle_root.len(), 64);

    let mut reg = acdd_index::languages::LanguageRegistry::new();
    assert!(reg.is_empty());
    reg.enable("rust");
    assert!(reg.is_enabled("rust"));
    assert_eq!(reg.len(), 1);

    let route_node = acdd_index::models::Node {
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
    let routes = acdd_index::linker::RouteIndex::build(&[&route_node]);
    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes
            .match_route("GET", "/api/users/123")
            .unwrap()
            .unwrap()
            .id,
        "route:get_user"
    );
    assert!(routes.match_route("POST", "/api/users/123").unwrap().is_none());

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

    // acdd-state + host state_bridge::build_domain_providers + acdd_index::ast::extract_typed
    let ws = crate::common::Workspace::new();
    let backend = acdd_state::store::sqlite::SqliteStateBackend::open(ws.path("test-state-store.sqlite"))
        .unwrap();
    assert_eq!(backend.read_meta().unwrap().schema_version, 1);
    drop(backend);

    let providers = contextunity_acdd::state_bridge::build_domain_providers(
        ws.root(),
        &acdd_core::config::Adapter::default(),
    )
    .expect("state_bridge domain providers must initialize cleanly");
    assert!(providers.plugin_tools().is_empty());

    #[cfg(feature = "lang-rust")]
    {
        let facts = acdd_index::ast::extract_typed(
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
}
