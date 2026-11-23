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

fn skip_literal(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes[i] == b'\'' {
        if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
            return Some(i + 4);
        }
        if i + 2 < bytes.len() && bytes[i + 1] != b'\\' && bytes[i + 2] == b'\'' {
            return Some(i + 3);
        }
    }
    if bytes[i] == b'"' {
        let mut j = i + 1;
        while j < bytes.len() {
            if bytes[j] == b'\\' && j + 1 < bytes.len() { j += 2; }
            else if bytes[j] == b'"' { return Some(j + 1); }
            else { j += 1; }
        }
        return Some(bytes.len());
    }
    if bytes[i] == b'r' {
        let hashes = bytes[i + 1..].iter().take_while(|b| **b == b'#').count();
        let q = i + 1 + hashes;
        if q < bytes.len() && bytes[q] == b'"' {
            let mut j = q + 1;
            while j < bytes.len() {
                if bytes[j] == b'"' && j + hashes < bytes.len() && bytes[j + 1..=j + hashes].iter().all(|b| *b == b'#') {
                    return Some(j + 1 + hashes);
                }
                j += 1;
            }
            return Some(bytes.len());
        }
    }
    None
}

fn strip_cfg_test_items(code: &str) -> String {
    let (bytes, mut out, mut i) = (code.as_bytes(), String::with_capacity(code.len()), 0usize);
    while i < bytes.len() {
        if let Some(next) = skip_literal(bytes, i) {
            out.push_str(&code[i..next]);
            i = next;
            continue;
        }
        if bytes[i..].starts_with(b"#[") {
            let (mut attr_depth, mut j) = (0usize, i);
            while j < bytes.len() {
                if let Some(next) = skip_literal(bytes, j) { j = next; }
                else if bytes[j] == b'[' { attr_depth += 1; j += 1; }
                else if bytes[j] == b']' {
                    attr_depth -= 1;
                    j += 1;
                    if attr_depth == 0 { break; }
                } else { j += 1; }
            }
            let compact: String = code[i..j].chars().filter(|c| !c.is_whitespace()).collect();
            let is_test = compact
                .strip_prefix("#[cfg(")
                .and_then(|s| s.strip_suffix(")]"))
                .is_some_and(|inner| inner.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-').any(|t| t == "test"));
            if is_test {
                i = j;
                let (mut depth, mut seen_block) = (0usize, false);
                while i < bytes.len() {
                    if let Some(next) = skip_literal(bytes, i) { i = next; }
                    else if bytes[i] == b'{' { seen_block = true; depth += 1; i += 1; }
                    else if bytes[i] == b'}' && depth > 0 { depth -= 1; i += 1; if depth == 0 { break; } }
                    else if bytes[i] == b';' && depth == 0 && !seen_block { i += 1; break; }
                    else { i += 1; }
                }
                continue;
            }
            out.push_str(&code[i..j]);
            i = j;
            continue;
        }
        let ch = code[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
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
    for moved in ["commitments.rs", "interner.rs", "models.rs", "semantic.rs", "typed_facts.rs"] {
        assert!(!core_src_dir.join(moved).exists(), "crates/acdd-core/src/{moved} must remain moved into crates/acdd-index");
    }

    let core_cargo_path = manifest_dir.join("crates/acdd-core/Cargo.toml");
    let core_cargo = std::fs::read_to_string(&core_cargo_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", core_cargo_path.display()));
    let parsed_core_cargo: toml::Value = toml::from_str(&core_cargo)
        .unwrap_or_else(|e| panic!("parse {}: {e}", core_cargo_path.display()));
    let core_forbidden_crates = [
        "acdd-index", "acdd-state", "rusqlite", "tree-sitter", "zstd",
        "rayon", "hashbrown", "clap", "rmcp", "tokio",
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
        } else {
            let prod_code = strip_cfg_test_items(&code);
            for forbidden in ["acdd_core::git", "crate::git_ops", "crate::spec", "crate::usecases"] {
                assert!(
                    !prod_code.contains(forbidden),
                    "SQLite store file {} must not reference '{forbidden}'",
                    rel.display()
                );
            }
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

    assert!(acdd_index::languages::profiles().any(|p| p.id() == "rust"));
    assert!(acdd_index::languages::by_id("rust").is_some());

    let mem_conn = rusqlite::Connection::open_in_memory().unwrap();
    mem_conn.execute_batch(acdd_index::db::schema::SCHEMA_DDL).unwrap();
    let table_count: i64 = mem_conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('nodes', 'edges', 'metadata', 'resolution_coverage')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(table_count, 4);

    // acdd-state + host state_bridge::build_domain_providers + acdd_index::ast::extract_typed
    let ws = crate::common::Workspace::new();
    let backend = acdd_state::store::sqlite::SqliteStateBackend::open(ws.path("test-state-store.sqlite")).unwrap();
    assert_eq!(backend.read_meta().unwrap().schema_version, 2);
    drop(backend);

    let providers = contextunity_acdd::state_bridge::build_domain_providers(
        ws.root(),
        &acdd_core::config::Adapter::default(),
        None,
        Vec::new(),
    )
        .expect("state_bridge domain providers must initialize cleanly");
    assert!(providers.plugin_tools().is_empty());

    #[cfg(feature = "lang-rust")]
    {
        let facts = acdd_index::ast::extract_typed("tests/seam.rs", "rust", "#[test] fn sample_seam() { assert!(true); }")
            .expect("production extractor must be wired");
        assert!(facts.facts.nodes.iter().any(|n| n.is_test && n.name == "sample_seam"));
    }
}

#[test]
fn inv_single_owner_architectural_fences() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // 1. CLI validation is a thin (< 150-line) adapter delegating to `acdd_state::validate`.
    let cli_validate_path = manifest_dir.join("src/cli/validate.rs");
    let cli_validate_raw = std::fs::read_to_string(&cli_validate_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", cli_validate_path.display()));
    let cli_validate_lines = cli_validate_raw.lines().count();
    assert!(
        cli_validate_lines < 150,
        "src/cli/validate.rs must stay < 150 lines (got {cli_validate_lines})"
    );
    let cli_validate_code = strip_rust_comments(&cli_validate_raw);
    assert!(
        cli_validate_code.contains("acdd_state::validate"),
        "src/cli/validate.rs must delegate to acdd_state::validate"
    );
    for forbidden in ["parse_spec", "VecDeque", "in_degree"] {
        assert!(
            !cli_validate_code.contains(forbidden),
            "src/cli/validate.rs must not duplicate spec parsing or DAG cycle detection ('{forbidden}')"
        );
    }

    // 2. `crates/acdd-state/src/machine/graph.rs` reuses `crate::profile::when::WhenExpr`.
    let graph_path = manifest_dir.join("crates/acdd-state/src/machine/graph.rs");
    let graph_raw = std::fs::read_to_string(&graph_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", graph_path.display()));
    let graph_code = strip_rust_comments(&graph_raw);
    assert!(
        graph_code.contains("crate::profile::when"),
        "crates/acdd-state/src/machine/graph.rs must use crate::profile::when"
    );
    for forbidden in ["enum WhenExpr", "fn tokenize_when", "fn eval_when"] {
        assert!(
            !graph_code.contains(forbidden),
            "crates/acdd-state/src/machine/graph.rs must not duplicate WhenExpr parser/evaluator ('{forbidden}')"
        );
    }

    // 3. Single-owner fences across `crates/{acdd-core,acdd-state,acdd-index}/src` and `src`.
    let mut all_src_rs = Vec::new();
    for root in ["crates/acdd-core/src", "crates/acdd-state/src", "crates/acdd-index/src", "src"] {
        collect_rs_files(&manifest_dir.join(root), &mut all_src_rs);
    }
    let forbidden_glob_engines = [
        "fn glob_matches", "fn glob_match_bytes", "fn match_pattern_intersection",
        "fn segment_globs_intersect", "fn is_valid_scope_glob", "fn has_valid_scope_braces",
        "fn has_balanced_nonempty_braces", "fn expand_glob_braces",
    ];
    let banned_duplicate_fns = [
        "fn verify_no_symlink_traversal", "fn glob_pattern_matches", "fn matches_path_pattern",
        "fn now_utc_iso(", "fn collapse_placeholder_spaces", "fn crate_name_from_cargo_toml",
    ];
    let mut violations = Vec::new();
    for file in &all_src_rs {
        let raw = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let code = strip_rust_comments(&raw);
        let rel = file.strip_prefix(manifest_dir).unwrap_or(file).to_string_lossy().replace('\\', "/");
        if rel != "crates/acdd-core/src/path_policy.rs" {
            for pat in forbidden_glob_engines {
                if code.contains(pat) {
                    violations.push(format!("{rel}: duplicate glob/overlap helper '{pat}' (delegate to acdd_core::path_policy)"));
                }
            }
        }
        if rel != "crates/acdd-core/src/git.rs" {
            if code.contains("Command::new(\"git\")")
                || code.split_whitespace().collect::<String>().contains("Command::new(\"git\")")
            {
                violations.push(format!("{rel}: direct `Command::new(\"git\")` forbidden outside crates/acdd-core/src/git.rs"));
            }
            if code.contains("fn read_git_blob_utf8") {
                violations.push(format!("{rel}: duplicate `fn read_git_blob_utf8` forbidden outside crates/acdd-core/src/git.rs"));
            }
        }
        for pat in banned_duplicate_fns {
            if code.contains(pat) {
                violations.push(format!("{rel}: banned duplicate helper '{pat}'"));
            }
        }
        let check_prod_temp = rel.starts_with("src/")
            || rel.starts_with("crates/acdd-index/src/")
            || rel.starts_with("crates/acdd-state/src/");
        if check_prod_temp && strip_cfg_test_items(&code).contains("env::temp_dir") {
            violations.push(format!("{rel}: production `env::temp_dir` forbidden (use `acdd_core::fs::ScopedTempDir`)"));
        }
    }
    assert!(violations.is_empty(), "INV-SINGLE-OWNER architectural fence violations:\n{}", violations.join("\n"));
    for delegator in [
        "crates/acdd-state/src/profile/validate.rs",
        "crates/acdd-state/src/machine/schedule.rs",
        "crates/acdd-state/src/validate.rs",
    ] {
        let code = strip_rust_comments(&std::fs::read_to_string(manifest_dir.join(delegator)).unwrap());
        assert!(code.contains("acdd_core::path_policy"), "{delegator} must delegate to acdd_core::path_policy");
    }

    // 4. Embedded `base.yaml` is domain-pure and `code.yaml` inherits `base`.
    let base_profile_raw = std::fs::read_to_string(
        manifest_dir.join("crates/acdd-state/src/profile/embedded/base.yaml"),
    )
    .unwrap();
    for forbidden in ["101", "134", ".acdd/frameworks", "code.", "docs."] {
        assert!(
            !base_profile_raw.contains(forbidden),
            "embedded base.yaml must stay domain-pure (found '{forbidden}')"
        );
    }
    let code_profile_raw = std::fs::read_to_string(
        manifest_dir.join("crates/acdd-state/src/profile/embedded/code.yaml"),
    )
    .unwrap();
    let code_profile_yaml: serde_yaml::Value = serde_yaml::from_str(&code_profile_raw).unwrap();
    assert_eq!(
        code_profile_yaml["inherit"].as_str(),
        Some("base"),
        "embedded code.yaml must use `inherit: base`"
    );

    // 5. Embedded `in_place.yaml` is removed; embedded `audit.yaml` inherits `base` with explicit `save.report`.
    assert!(
        !manifest_dir
            .join("crates/acdd-state/src/profile/embedded/in_place.yaml")
            .exists(),
        "embedded in_place.yaml must be removed (`in_place` is an execution mode)"
    );
    let audit_raw = std::fs::read_to_string(
        manifest_dir.join("crates/acdd-state/src/profile/embedded/audit.yaml"),
    )
    .unwrap();
    let audit_yaml: serde_yaml::Value = serde_yaml::from_str(&audit_raw).unwrap();
    assert_eq!(audit_yaml["inherit"].as_str(), Some("base"), "embedded audit.yaml must use `inherit: base`");
    assert_eq!(audit_yaml["git"]["isolation"].as_str(), Some("in_place"), "audit.yaml must isolate in_place");
    assert_eq!(audit_yaml["save"].as_str(), Some("none"), "audit.yaml must default save: none");
    let triage_gate = audit_yaml["gates"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|g| g["id"].as_str() == Some("triage"))
        .expect("audit.yaml must declare triage gate");
    assert_eq!(
        triage_gate["save"]["report"].as_str(),
        Some(".acdd/reports/{run_name}.md"),
        "audit.yaml triage gate must declare explicit save.report"
    );

    // 6. Embedded `spec_planning.yaml` inherits `base`, isolates `in_place`, defaults `save: none`, and has no hardcoded runners/models.
    let sp_raw = std::fs::read_to_string(
        manifest_dir.join("crates/acdd-state/src/profile/embedded/spec_planning.yaml"),
    )
    .unwrap();
    let sp_yaml: serde_yaml::Value = serde_yaml::from_str(&sp_raw).unwrap();
    assert_eq!(sp_yaml["inherit"].as_str(), Some("base"));
    assert_eq!(sp_yaml["git"]["isolation"].as_str(), Some("in_place"));
    assert_eq!(sp_yaml["save"].as_str(), Some("none"));
    assert!(sp_yaml["roles"]["architect"]["runner"].is_null());
    assert!(sp_yaml["roles"]["architect"]["models"].is_null());
    let sp_review = sp_yaml["gates"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|g| g["id"].as_str() == Some("review"))
        .expect("spec_planning.yaml must declare review gate");
    assert_eq!(
        sp_review["save"]["report"].as_str(),
        Some(".acdd/reports/{run_name}.md")
    );
}
