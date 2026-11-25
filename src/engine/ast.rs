use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_index::{
    ast::{self, PatternSearchDiagnostic},
    scanner,
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
/// Executes an unpaged structural AST pattern search across `languages` under `root`.
pub(crate) fn search_languages_with_adapter(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    pattern: &str,
    languages: &[&str],
    path: Option<&str>,
    limit: usize,
) -> Result<Value> {
    if limit == 0 || limit > 10000 {
        bail!("limit must be 1..=10000");
    }
    let mut matches = Vec::new();
    let mut offset = 0;
    let mut generation = None;
    let mut truncated = false;
    while matches.len() < limit {
        let page_limit = (limit - matches.len()).min(100);
        let options = QueryOptions::resolve(
            &ResponsePolicy::default(),
            Some(page_limit),
            offset,
            Some(Detail::Compact),
            generation.clone(),
        )?;
        let page = search_paged_languages_with_adapter(
            conn, root, adapter, pattern, languages, path, &options,
        )?;
        if page.get("diagnostic").is_some() {
            return Ok(page);
        }
        let payload = &page["matches"];
        if let Some(items) = payload["items"].as_array() {
            matches.extend(items.iter().cloned());
        }
        if payload["computation_truncated"] == true {
            truncated = true;
            break;
        }
        if payload["has_more"] != true {
            break;
        }
        if matches.len() >= limit {
            truncated = true;
            break;
        }
        generation = payload["generation"].as_str().map(str::to_owned);
        offset = payload["next_offset"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .context("AST continuation offset is missing")?;
    }
    Ok(json!({"matches":matches,"limit":limit,"truncated":truncated}))
}

#[cfg(all(test, feature = "lang-python"))]
#[test]
fn test_prefilter_empty_candidate_fallback() {
    let root = acdd_core::fs::ScopedTempDir::create("acdd-ast-prefilter").unwrap();
    // File A defines top-level `def x()`, while File B assigns local `x = ...`.
    // `x = $EXPR` must NOT SQL-prefilter via `nodes.name = 'x'` (which would skip File B).
    std::fs::write(root.path().join("a_decl.py"), "def x():\n    return 0\n").unwrap();
    std::fs::write(
        root.path().join("b_local.py"),
        "def compute():\n    x = 1 + 2\n    return x\n",
    )
    .unwrap();
    let database = root.path().join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(root.path(), &database, None).unwrap();
    let conn = acdd_index::db::reader::open(&database, root.path()).unwrap();
    let pattern = "x = $EXPR";
    assert_eq!(
        ast::pattern_symbol_name(pattern, "python").unwrap(),
        None,
        "assignment patterns must never use nodes.name prefiltering"
    );
    assert_eq!(
        ast::pattern_symbol_name("def x():", "python")
            .unwrap()
            .as_deref(),
        Some("x")
    );
    let indexed: i64 = conn
        .query_row("SELECT count(*) FROM nodes WHERE name='x'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexed, 1, "file A defines top-level x");
    let options = QueryOptions::resolve(&ResponsePolicy::default(), None, 0, None, None).unwrap();
    let result = search_paged(&conn, root.path(), pattern, "python", None, &options).unwrap();
    assert_eq!(result["matches"]["total"], 1, "{result}");
    assert_eq!(result["matches"]["items"][0]["path"], "b_local.py");
    assert_eq!(result["matches"]["items"][0]["line"], 2);
}

#[cfg(all(test, feature = "lang-rust"))]
#[test]
fn public_rust_candidate_selection() {
    let root = acdd_core::fs::ScopedTempDir::create("acdd-ast-rust-select").unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src/lib.rs"),
        "fn target() {}\nfn target(value: i32) {}\nfn target() -> i32 { 1 }\nfn target<T>(value: T) -> T { value }\nfn other() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("src/a_candidate.rs"),
        "fn coverage_owner_language() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("src/z_body.rs"),
        "fn other() { let value = \"coverage_owner_language\"; }\n",
    )
    .unwrap();
    let database = root.path().join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(root.path(), &database, None).unwrap();
    let conn = acdd_index::db::reader::open(&database, root.path()).unwrap();
    let options = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(10),
        0,
        Some(Detail::Compact),
        None,
    )
    .unwrap();
    for (case_id, pattern, expected, item) in [
        (
            "bare_rust_headers_match_signatures_without_weakening_explicit_parameters",
            "fn target",
            4,
            None,
        ),
        (
            "bare_rust_headers_match_signatures_without_weakening_explicit_parameters",
            "fn target()",
            1,
            None,
        ),
        (
            "bare_rust_headers_match_signatures_without_weakening_explicit_parameters",
            "fn target(value: i32)",
            1,
            None,
        ),
        (
            "bare_rust_headers_match_signatures_without_weakening_explicit_parameters",
            "fn target<T>",
            1,
            None,
        ),
        (
            "public_ast_search_keeps_rust_body_only_literal_candidates",
            "\"coverage_owner_language\"",
            1,
            Some(("src/z_body.rs", "\"coverage_owner_language\"")),
        ),
    ] {
        let result = search_paged(&conn, root.path(), pattern, "rust", None, &options).unwrap();
        assert_eq!(
            result["matches"]["total"], expected,
            "{case_id}: {pattern}: {result}"
        );
        if let Some((path, text)) = item {
            let items = result["matches"]["items"].as_array().unwrap();
            assert_eq!(items.len(), 1, "{result}");
            assert_eq!(items[0]["path"], path);
            assert_eq!(items[0]["text"], text);
        }
    }
}

#[cfg(all(test, feature = "lang-rust"))]
#[test]
fn public_pattern_diagnostics() {
    fn smallest_error(root: tree_sitter::Node<'_>) -> Option<std::ops::Range<usize>> {
        let mut stack = vec![root];
        let mut smallest = None;
        while let Some(node) = stack.pop() {
            if node.is_error() && node.start_byte() < node.end_byte() {
                let range = node.start_byte()..node.end_byte();
                if smallest
                    .as_ref()
                    .is_none_or(|current: &std::ops::Range<usize>| range.len() < current.len())
                {
                    smallest = Some(range);
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        smallest
    }

    let root = acdd_core::fs::ScopedTempDir::create("acdd-ast-pattern-diag").unwrap();
    let database = root.path().join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(root.path(), &database, None).unwrap();
    let options = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(10),
        0,
        Some(Detail::Compact),
        None,
    )
    .unwrap();
    for rust_only in [false, true] {
        if rust_only {
            std::fs::create_dir_all(root.path().join("src")).unwrap();
            std::fs::write(root.path().join("src/lib.rs"), "fn valid() {}\n").unwrap();
            crate::engine::pipeline::delta_with_selection(
                root.path(),
                &database,
                &[std::path::PathBuf::from("src/lib.rs")],
                None,
                None,
            )
            .unwrap();
        }
        let conn = acdd_index::db::reader::open(&database, root.path()).unwrap();
        let mut cases = vec![(
            "public_ast_search_reports_structured_tree_sitter_pattern_diagnostics",
            "rust",
            "src/lib.rs",
            "let = ;",
            Some("fn target()"),
        )];
        #[cfg(feature = "lang-python")]
        cases.push((
            "invalid_ast_patterns_report_diagnostics_without_candidate_files",
            "python",
            "service.py",
            "def (",
            Some("def target()"),
        ));
        for (case_id, language, path, pattern, valid) in cases {
            let result =
                search_paged(&conn, root.path(), pattern, language, None, &options).unwrap();
            let diagnostic = &result["diagnostic"];
            assert_eq!(
                diagnostic["code"], "unsupported_ast_pattern",
                "{case_id}: {result}"
            );
            assert_eq!(diagnostic["language"], language);
            assert!(diagnostic["span"].is_object());
            if language == "rust" {
                let mut parser = acdd_index::languages::require(language)
                    .unwrap()
                    .create_parser(path)
                    .unwrap();
                let tree = parser.parse(pattern, None).unwrap();
                let expected = smallest_error(tree.root_node()).expect("nonempty ERROR node");
                assert_eq!(diagnostic["span"]["start_byte"], expected.start);
                assert_eq!(diagnostic["span"]["end_byte"], expected.end);
                assert!(
                    diagnostic["span"]["end_byte"].as_u64().unwrap()
                        > diagnostic["span"]["start_byte"].as_u64().unwrap()
                );
            }
            assert!(!diagnostic["hint"].as_str().unwrap().is_empty());
            assert!(!diagnostic["examples"].as_array().unwrap().is_empty());
            if let Some(valid) = valid {
                let result =
                    search_paged(&conn, root.path(), valid, language, None, &options).unwrap();
                assert_eq!(result["matches"]["total"], 0, "{case_id}: {result}");
            }
        }
    }
}

#[cfg(all(test, feature = "lang-typescript"))]
#[test]
fn upfront_ast_validation_accepts_tsx_grammar_patterns() {
    let root = acdd_core::fs::ScopedTempDir::create("acdd-ast-tsx-upfront").unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/app.ts"), "const count: number = 1;\n").unwrap();
    std::fs::write(
        root.path().join("src/view.tsx"),
        "const view = <Widget value={value} />;\n",
    )
    .unwrap();
    let database = root.path().join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(root.path(), &database, None).unwrap();
    let conn = acdd_index::db::reader::open(&database, root.path()).unwrap();
    let options =
        QueryOptions::resolve(&ResponsePolicy::default(), Some(10), 0, None, None).unwrap();
    for path in [None, Some("src/view.tsx")] {
        let result = search_paged(
            &conn,
            root.path(),
            "<Widget value={$VALUE} />",
            "typescript",
            path,
            &options,
        )
        .unwrap();
        assert_eq!(result["matches"]["total"], 1, "{result}");
        assert_eq!(result["matches"]["items"][0]["captures"]["VALUE"], "value");
    }
    let ts_only = search_paged(
        &conn,
        root.path(),
        "<Widget value={$VALUE} />",
        "typescript",
        Some("src/app.ts"),
        &options,
    )
    .unwrap();
    assert_eq!(ts_only["diagnostic"]["code"], "unsupported_ast_pattern");
    let invalid = search_paged(
        &conn,
        root.path(),
        "function (",
        "typescript",
        None,
        &options,
    )
    .unwrap();
    assert_eq!(invalid["diagnostic"]["code"], "unsupported_ast_pattern");
}

#[cfg(all(
    test,
    feature = "lang-python",
    feature = "lang-rust",
    feature = "lang-typescript"
))]
#[test]
fn prefiltered_ast_search_matches_full_scan_with_100_percent_recall() {
    use acdd_index::ast::search::PreparedAstMatcher;
    use acdd_index::db::reader;
    use std::time::Instant;

    let root = acdd_core::fs::ScopedTempDir::create("acdd-ast-prefilter-recall").unwrap();
    for dir in ["src", "pkg", "web"] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    for (rel, content) in [
        (
            "src/lib.rs",
            "pub fn helper_fn(x: i32) -> i32 {\n    let local_value = foo(x);\n    local_value\n}\n",
        ),
        (
            "src/client.rs",
            "pub struct Obj;\nimpl Obj { pub fn method(&self, a: i32, b: i32) -> i32 { a + b } }\n\
             pub fn call_site(obj: &Obj) -> i32 {\n    let a = obj.method(1, 2);\n    let b = obj.method(3, 4);\n    a + b\n}\n",
        ),
        ("src/empty.rs", "pub fn unrelated() -> i32 { 42 }\n"),
        (
            "pkg/app.py",
            "def helper_fn(x):\n    return foo(x)\n\ndef run(obj):\n    return obj.method(10, 20)\n",
        ),
        (
            "pkg/local.py",
            "def compute():\n    local_value = 1 + 2\n    obj.method(5, 6)\n    return foo(local_value)\n",
        ),
        ("pkg/empty.py", "def idle():\n    return 0\n"),
        (
            "web/client.ts",
            "export function helperFn(x: number) {\n  const local_value = foo(x);\n  return obj.method(local_value, 2);\n}\n",
        ),
        (
            "web/unicode.ts",
            "export function escaped() {\n  const \\u0066oo = 1;\n  return foo(1);\n}\n",
        ),
        (
            "web/empty.tsx",
            "export const View = () => <div>ok</div>;\n",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    let database = root.path().join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(root.path(), &database, None).unwrap();
    let conn = reader::open(&database, root.path()).unwrap();

    let cases: &[(&str, &str, usize, bool)] = &[
        ("rust", "foo($X)", 1, true),
        ("rust", "obj.method($A, $B)", 1, true),
        ("rust", "let $A = $B;", 2, false),
        ("rust", "fn helper_fn($$$)", 5, true),
        ("rust", "nonexistent_anchor_xyz($X)", 5, true),
        ("python", "foo($X)", 1, true),
        ("python", "obj.method($A, $B)", 1, true),
        ("python", "local_value = $EXPR", 5, true),
        ("python", "$A = $B", 2, false),
        ("python", "nonexistent_anchor_xyz($X)", 5, true),
        ("typescript", "foo($X)", 1, true),
        ("typescript", "obj.method($A, $B)", 5, true),
        ("typescript", "$A = $B", 2, false),
        ("typescript", "nonexistent_anchor_xyz($X)", 5, true),
    ];

    for &(language, pattern, page_limit, expects_anchor) in cases {
        let mut matcher = PreparedAstMatcher::new(language, pattern).unwrap();
        let probe_path = match language {
            "rust" => "src/lib.rs",
            "python" => "pkg/app.py",
            "typescript" => "web/client.ts",
            _ => unreachable!(),
        };
        let anchors = matcher.required_anchors(probe_path).unwrap();
        assert_eq!(
            !anchors.is_empty(),
            expects_anchor,
            "unexpected anchor extraction for {language} pattern `{pattern}`: {anchors:?}"
        );
        for anchor in &anchors {
            assert!(
                !anchor.starts_with("__ACDD_") && !anchor.starts_with("__acdd_"),
                "placeholder leaked into anchors: {anchor}"
            );
        }

        let all_files = reader::rows(
            &conn,
            "SELECT path FROM files WHERE status='indexed' AND language=?1 ORDER BY path",
            &[&language],
            1000,
        )
        .unwrap();
        let mut oracle_items: Vec<Value> = Vec::new();
        let mut oracle_matcher = PreparedAstMatcher::new(language, pattern).unwrap();
        for row in &all_files {
            let file_path = row["path"].as_str().unwrap();
            let source = std::fs::read_to_string(root.path().join(file_path)).unwrap();
            let page = oracle_matcher
                .search_page(
                    &source,
                    file_path,
                    0,
                    1000,
                    Instant::now() + std::time::Duration::from_secs(5),
                )
                .unwrap();
            oracle_items.extend(page.items);
        }

        let mut paged_items: Vec<Value> = Vec::new();
        let mut offset = 0usize;
        let mut generation: Option<String> = None;
        loop {
            let options = QueryOptions::resolve(
                &ResponsePolicy::default(),
                Some(page_limit),
                offset,
                Some(Detail::Compact),
                generation.clone(),
            )
            .unwrap();
            let response =
                search_paged(&conn, root.path(), pattern, language, None, &options).unwrap();
            let payload = &response["matches"];
            let batch = payload["items"].as_array().unwrap();
            paged_items.extend(batch.iter().cloned());
            if payload["has_more"] != true {
                assert_eq!(
                    payload["total"],
                    oracle_items.len(),
                    "{language} `{pattern}`"
                );
                assert!(payload["next_offset"].is_null());
                break;
            }
            let next_offset = payload["next_offset"].as_u64().unwrap() as usize;
            assert_eq!(next_offset, offset + batch.len());
            offset = next_offset;
            generation = payload["generation"].as_str().map(str::to_owned);
        }

        assert_eq!(
            paged_items, oracle_items,
            "100% recall parity failed for {language} `{pattern}`"
        );
    }
}

fn normalize_ast_scope(path: Option<&str>) -> Option<&str> {
    let mut trimmed = path?.trim();
    if trimmed.is_empty() {
        return Some("");
    }
    while let Some(rest) = trimmed.strip_prefix("./") {
        trimmed = rest;
    }
    (!matches!(trimmed, "" | ".")).then_some(trimmed)
}

fn try_pattern_diagnostic(error: anyhow::Error) -> Result<Value> {
    if let Some(diagnostic) = error.downcast_ref::<PatternSearchDiagnostic>() {
        return Ok(json!({"diagnostic": diagnostic}));
    }
    Err(error)
}

fn is_config_data_language(language: &str) -> bool {
    matches!(language, "yaml" | "json" | "toml")
}

/// Resolves an explicit AST language or infers compatible language(s) from `path` or `pattern`.
pub(crate) fn resolve_ast_languages(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    pattern: &str,
    language: Option<&str>,
    path: Option<&str>,
) -> Result<Vec<String>> {
    if let Some(lang) = language.map(str::trim).filter(|l| !l.is_empty()) {
        return Ok(vec![lang.to_owned()]);
    }
    let trimmed_path = normalize_ast_scope(path);
    let abs_path = match trimmed_path {
        None => None,
        Some(p) => Some(scanner::resolve_file_path(root, adapter, p)?),
    };
    if let Some(p) = trimmed_path {
        if let Some(profile) = acdd_index::languages::for_path(Path::new(p)) {
            return Ok(vec![profile.id().to_owned()]);
        }
    }

    let is_dir_scope = abs_path.as_ref().is_none_or(|abs| abs.is_dir());

    if is_dir_scope {
        let scope = trimmed_path.map(|p| p.trim_end_matches('/')).unwrap_or("");
        let (path_start, path_end) = acdd_index::db::reader::path_bounds(scope);
        let rows = acdd_index::db::reader::rows(
            conn,
            "SELECT DISTINCT language FROM files WHERE status='indexed' AND (?1='' OR path=?1 OR (path>=?2 AND path<?3)) ORDER BY language",
            &[&scope, &path_start, &path_end],
            1000,
        )?;
        let detected: std::collections::BTreeSet<String> = rows
            .into_iter()
            .filter_map(|row| row["language"].as_str().map(str::to_owned))
            .filter(|lang| acdd_index::languages::require(lang).is_ok())
            .collect();
        if detected.len() == 1 {
            return Ok(detected.into_iter().collect());
        }
        if detected.len() > 1 {
            let mut compatible = Vec::new();
            for lang in &detected {
                match ast::pattern_symbol_name(pattern, lang) {
                    Ok(_) => compatible.push(lang.clone()),
                    Err(error) if error.downcast_ref::<PatternSearchDiagnostic>().is_some() => {}
                    Err(error) => return Err(error),
                }
            }
            if !compatible.is_empty() {
                let programming: Vec<String> = compatible
                    .iter()
                    .filter(|lang| !is_config_data_language(lang))
                    .cloned()
                    .collect();
                if !programming.is_empty() {
                    return Ok(programming);
                }
                return Ok(compatible);
            }
            let compiled = compiled_languages_summary();
            let listed = detected.into_iter().collect::<Vec<_>>().join(", ");
            let display_scope = if scope.is_empty() { "." } else { scope };
            bail!(
                "AST pattern does not parse in any detected language for '{display_scope}' ({listed}); specify 'language' explicitly or rewrite 'pattern'.\nCompiled languages: {compiled}.\nExamples: language='python' with pattern='class Widget', or path='src/example.py' with pattern='class Widget'.\nCall acdd_guide(topic='ast') for profile capabilities and examples."
            );
        }
    }

    let compiled = compiled_languages_summary();
    bail!(
        "Cannot infer AST language. Specify 'language' or provide a file 'path' with a recognized extension.\nCompiled languages: {compiled}.\nExamples: language='python' with pattern='class Widget', or path='src/example.py' with pattern='class Widget'.\nCall acdd_guide(topic='ast') for profile capabilities and examples."
    );
}

fn compiled_languages_summary() -> String {
    let compiled = acdd_index::languages::profiles()
        .map(|p| p.id())
        .collect::<Vec<_>>()
        .join(", ");
    if compiled.is_empty() {
        "none (document-only build)".to_owned()
    } else {
        compiled
    }
}

/// Loads workspace config and executes a paged structural AST search.
#[cfg(test)]
fn search_paged(
    conn: &rusqlite::Connection,
    root: &Path,
    pattern: &str,
    language: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let canon_root = scanner::canonical_root(root)?;
    let explicit_config = std::env::var_os("ACDD_CONFIG").map(std::path::PathBuf::from);
    let adapter = scanner::load_adapter(&canon_root, explicit_config.as_deref())?;
    search_paged_with_adapter(
        conn,
        &canon_root,
        &adapter,
        pattern,
        language,
        path,
        options,
    )
}

/// Executes a paged structural AST pattern search using an already-loaded [`scanner::Adapter`].
#[cfg(test)]
pub(crate) fn search_paged_with_adapter(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    pattern: &str,
    language: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    search_paged_languages_with_adapter(conn, root, adapter, pattern, &[language], path, options)
}

/// Performs paged AST search across one or more compiled languages using an already-loaded [`scanner::Adapter`].
pub(crate) fn search_paged_languages_with_adapter(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    pattern: &str,
    languages: &[&str],
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    anyhow::ensure!(
        !languages.is_empty(),
        "at least one AST language is required"
    );
    for language in languages {
        acdd_index::languages::require(language)?;
    }
    anyhow::ensure!((1..=100).contains(&options.limit), "limit must be 1..=100");
    anyhow::ensure!(
        options.offset < ast::SEARCH_MATCH_HORIZON,
        "AST search offset must be below 10000; narrow path or pattern"
    );
    let limit = options
        .limit
        .min(ast::SEARCH_MATCH_HORIZON - options.offset);
    anyhow::ensure!(
        options.offset == 0 || options.generation.is_some(),
        "AST continuation requires generation"
    );
    let root = scanner::canonical_root(root)?;
    let generation = acdd_index::db::paging::generation(conn, options)?;
    let path = normalize_ast_scope(path);
    if let Some(path) = path {
        scanner::resolve_file_path(&root, adapter, path)?;
    }
    let path = path.map(|p| p.trim_end_matches('/')).unwrap_or("");
    let (path_start, path_end) = acdd_index::db::reader::path_bounds(path);
    let single_language = languages.len() == 1;
    let mut files: Vec<(String, String, &str)> = Vec::new();
    let mut matchers = std::collections::HashMap::with_capacity(languages.len());
    for &language in languages {
        let declaration = match ast::pattern_symbol_name(pattern, language) {
            Ok(declaration) => declaration,
            Err(error) => {
                let diag = try_pattern_diagnostic(error)?;
                if single_language {
                    return Ok(diag);
                }
                continue;
            }
        };
        let lang_files = if let Some(name) = declaration.as_deref() {
            let candidate_files = acdd_index::db::reader::rows(
                conn,
                "SELECT DISTINCT f.path,f.digest FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id JOIN files f ON f.path=p.path WHERE n.name=?1 COLLATE NOCASE AND f.status='indexed' AND f.language=?2 AND (?3='' OR f.path=?3 OR (f.path>=?4 AND f.path<?5)) ORDER BY f.path",
                &[&name, &language, &path, &path_start, &path_end],
                500000,
            )?;
            if candidate_files.is_empty() {
                // Local syntax can bind a name without creating an indexed symbol.
                acdd_index::db::reader::rows(
                    conn,
                    "SELECT path,digest FROM files WHERE status='indexed' AND language=?1 AND (?2='' OR path=?2 OR (path>=?3 AND path<?4)) ORDER BY path",
                    &[&language, &path, &path_start, &path_end],
                    500000,
                )?
            } else {
                candidate_files
            }
        } else {
            acdd_index::db::reader::rows(
                conn,
                "SELECT path,digest FROM files WHERE status='indexed' AND language=?1 AND (?2='' OR path=?2 OR (path>=?3 AND path<?4)) ORDER BY path",
                &[&language, &path, &path_start, &path_end],
                500000,
            )?
        };
        let matcher = match ast::search::PreparedAstMatcher::new(language, pattern) {
            Ok(matcher) => matcher,
            Err(error) => {
                let diag = try_pattern_diagnostic(error)?;
                if single_language {
                    return Ok(diag);
                }
                continue;
            }
        };
        matchers.insert(language, matcher);
        for row in lang_files {
            if let (Some(file_path), Some(file_digest)) =
                (row["path"].as_str(), row["digest"].as_str())
            {
                files.push((file_path.to_owned(), file_digest.to_owned(), language));
            }
        }
    }
    if !single_language {
        files.sort_by(|left, right| left.0.cmp(&right.0));
    }
    let mut items = Vec::new();
    let mut matched = 0;
    let mut complete = true;
    let mut work_limited = false;
    let mut seen_compatible_file = false;
    let mut deferred_diagnostic = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for (file_path, file_digest, file_lang) in files {
        if std::time::Instant::now() >= deadline {
            complete = false;
            work_limited = true;
            break;
        }
        let source =
            std::fs::read_to_string(scanner::resolve_file_path(&root, adapter, &file_path)?)?;
        anyhow::ensure!(
            scanner::digest(source.as_bytes()) == file_digest,
            "source changed during AST query; retry"
        );
        let remaining = limit - items.len();
        if remaining == 0 {
            complete = false;
            break;
        }
        let matcher = matchers.get_mut(file_lang).expect("matcher initialized");
        let page = match matcher.search_page(
            &source,
            &file_path,
            options.offset.saturating_sub(matched),
            remaining,
            deadline,
        ) {
            Ok(page) => {
                seen_compatible_file = true;
                page
            }
            Err(error) => {
                let diag = try_pattern_diagnostic(error)?;
                if single_language && deferred_diagnostic.is_none() {
                    deferred_diagnostic = Some(diag);
                }
                continue;
            }
        };
        matched += page.matched;
        items.extend(page.items);
        if !page.complete {
            complete = false;
            work_limited = page.work_limited;
            break;
        }
    }
    if !seen_compatible_file {
        if let Some(diagnostic) = deferred_diagnostic {
            return Ok(diagnostic);
        }
    }
    let next = options.offset + items.len();
    let has_more = !complete;
    work_limited |= has_more && next >= ast::SEARCH_MATCH_HORIZON;
    let can_continue = has_more && !work_limited;
    let hint = if work_limited {
        Some(
            "AST computation limit reached; narrow path or pattern. Totals are unavailable and continuation cannot advance beyond this work horizon.",
        )
    } else {
        None
    };
    Ok(
        json!({"matches":{"total":if complete {Some(matched)} else {None},"total_status":if complete {"exact"} else {"unavailable"},"offset":options.offset,"limit":limit,"items":items,"has_more":has_more,"next_offset":if can_continue {Some(next)} else {None},"generation":generation,"computation_truncated":work_limited,"continuation_hint":hint},"generation":generation}),
    )
}
