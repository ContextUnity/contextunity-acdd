use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_index::{ast, scanner};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
/// Performs search.
pub fn search(
    conn: &rusqlite::Connection,
    root: &Path,
    pattern: &str,
    language: &str,
    path: Option<&str>,
    limit: usize,
) -> Result<Value> {
    if limit == 0 || limit > 10000 {
        bail!("limit must be1..10000");
    }
    let explicit_config = std::env::var_os("ACDD_CONFIG").map(std::path::PathBuf::from);
    let adapter = scanner::load_adapter(&scanner::canonical_root(root)?, explicit_config.as_deref())?;
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
        let page = search_paged_with_adapter(conn, root, &adapter, pattern, language, path, &options)?;
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
    struct TempRoot(std::path::PathBuf);
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let root = TempRoot(std::env::temp_dir().join(format!(
        "acdd-ast-prefilter-{}-{}",
        std::process::id(),
        acdd_core::telemetry::unix_epoch_ms()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    // File A defines top-level `def x()`, while File B assigns local `x = ...`.
    // `x = $EXPR` must NOT SQL-prefilter via `nodes.name = 'x'` (which would skip File B).
    std::fs::write(root.0.join("a_decl.py"), "def x():\n    return 0\n").unwrap();
    std::fs::write(
        root.0.join("b_local.py"),
        "def compute():\n    x = 1 + 2\n    return x\n",
    )
    .unwrap();
    let database = root.0.join(".acdd/code-map.sqlite");
    crate::engine::pipeline::build(&root.0, &database, None).unwrap();
    let conn = acdd_index::db::reader::open(&database, &root.0).unwrap();
    let pattern = "x = $EXPR";
    assert_eq!(
        ast::pattern_symbol_name(pattern, "python").unwrap(),
        None,
        "assignment patterns must never use nodes.name prefiltering"
    );
    assert_eq!(
        ast::pattern_symbol_name("def x():", "python").unwrap().as_deref(),
        Some("x")
    );
    let indexed: i64 = conn
        .query_row("SELECT count(*) FROM nodes WHERE name='x'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexed, 1, "file A defines top-level x");
    let options = QueryOptions::resolve(&ResponsePolicy::default(), None, 0, None, None).unwrap();
    let result = search_paged(&conn, &root.0, pattern, "python", None, &options).unwrap();
    assert_eq!(result["matches"]["total"], 1, "{result}");
    assert_eq!(result["matches"]["items"][0]["path"], "b_local.py");
    assert_eq!(result["matches"]["items"][0]["line"], 2);
}

/// Resolves an explicit AST language or infers it from a file extension or single-language directory.
pub(crate) fn resolve_ast_language(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    language: Option<&str>,
    path: Option<&str>,
) -> Result<String> {
    if let Some(lang) = language.map(str::trim).filter(|l| !l.is_empty()) {
        return Ok(lang.to_owned());
    }
    let trimmed_path = path.map(str::trim).filter(|p| !p.is_empty());
    if let Some(p) = trimmed_path {
        if let Some(profile) = acdd_index::languages::for_path(Path::new(p)) {
            return Ok(profile.id().to_owned());
        }
    }

    let is_dir_scope = match trimmed_path {
        None | Some(".") => true,
        Some(p) => scanner::resolve_file_path(root, adapter, p).is_ok_and(|abs| abs.is_dir()),
    };

    if is_dir_scope {
        let scope = match trimmed_path {
            None | Some(".") => "",
            Some(p) => p.trim_start_matches("./").trim_end_matches('/'),
        };
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
            return Ok(detected.into_iter().next().expect("checked len == 1"));
        }
        if detected.len() > 1 {
            let compiled = compiled_languages_summary();
            let listed = detected.into_iter().collect::<Vec<_>>().join(", ");
            let display_scope = if scope.is_empty() { "." } else { scope };
            bail!(
                "Cannot infer AST language for '{display_scope}' (found multiple languages: {listed}); specify 'language' explicitly.\nCompiled languages: {compiled}.\nExamples: language='python' with pattern='class Widget', or path='src/example.py' with pattern='class Widget'.\nCall acdd_guide(topic='ast') for profile capabilities and examples."
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

/// Performs search paged.
pub fn search_paged(
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
    search_paged_with_adapter(conn, &canon_root, &adapter, pattern, language, path, options)
}

/// Performs search paged using an already-loaded [`scanner::Adapter`].
pub(crate) fn search_paged_with_adapter(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &scanner::Adapter,
    pattern: &str,
    language: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    acdd_index::languages::require(language)?;
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
    if let Some(path) = path {
        scanner::resolve_file_path(&root, adapter, path)?;
    }
    let path = path.unwrap_or("");
    let (path_start, path_end) = acdd_index::db::reader::path_bounds(path);
    let declaration = match ast::pattern_symbol_name(pattern, language) {
        Ok(declaration) => declaration,
        Err(error) => {
            if let Some(diagnostic) = error.downcast_ref::<ast::PatternSearchDiagnostic>() {
                return Ok(json!({"diagnostic":diagnostic}));
            }
            return Err(error);
        }
    };
    let files = if let Some(name) = declaration.as_deref() {
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
    let mut matcher = match ast::search::PreparedAstMatcher::new(language, pattern) {
        Ok(matcher) => matcher,
        Err(error) => {
            if let Some(diagnostic) = error.downcast_ref::<ast::PatternSearchDiagnostic>() {
                return Ok(json!({"diagnostic":diagnostic}));
            }
            return Err(error);
        }
    };
    let mut items = Vec::new();
    let mut matched = 0;
    let mut complete = true;
    let mut work_limited = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for file in files {
        let file_path = file["path"].as_str().unwrap_or_default();
        let file_digest = file["digest"].as_str().unwrap_or_default();
        if std::time::Instant::now() >= deadline {
            complete = false;
            work_limited = true;
            break;
        }
        let source =
            std::fs::read_to_string(scanner::resolve_file_path(&root, adapter, file_path)?)?;
        anyhow::ensure!(
            scanner::digest(source.as_bytes()) == file_digest,
            "source changed during AST query; retry"
        );
        let remaining = limit - items.len();
        if remaining == 0 {
            complete = false;
            break;
        }
        let page = match matcher.search_page(
            &source,
            file_path,
            options.offset.saturating_sub(matched),
            remaining,
            deadline,
        ) {
            Ok(page) => page,
            Err(error) => {
                if let Some(diagnostic) = error.downcast_ref::<ast::PatternSearchDiagnostic>() {
                    return Ok(json!({"diagnostic":diagnostic}));
                }
                return Err(error);
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
