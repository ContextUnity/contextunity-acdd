#![cfg(all(feature = "lang-python", feature = "lang-rust", feature = "lang-typescript"))]

use crate::common::Workspace;
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_index::ast::search::{search_page, PreparedAstMatcher};
use acdd_index::db::reader;
use contextunity_acdd::cli::ast as cli_ast;
use serde_json::Value;
use std::time::Instant;

#[test]
fn prefiltered_ast_search_matches_full_scan_with_100_percent_recall() {
    let workspace = Workspace::with_files(&[
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
    ]);
    workspace.build();
    let conn = workspace.open();

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

        // Brute-force unfiltered full-language oracle across all indexed files in path order.
        let all_files = reader::rows(
            &conn,
            "SELECT path FROM files WHERE status='indexed' AND language=?1 ORDER BY path",
            &[&language],
            1000,
        )
        .unwrap();
        let mut oracle_items: Vec<Value> = Vec::new();
        for row in &all_files {
            let file_path = row["path"].as_str().unwrap();
            let source = std::fs::read_to_string(workspace.path(file_path)).unwrap();
            let page = search_page(
                &source,
                file_path,
                language,
                pattern,
                0,
                1000,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
            oracle_items.extend(page.items);
        }

        // Paginated search_paged using prefilter + PreparedAstMatcher.
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
                cli_ast::search_paged(&conn, workspace.root(), pattern, language, None, &options)
                    .unwrap();
            let payload = &response["matches"];
            let batch = payload["items"].as_array().unwrap();
            paged_items.extend(batch.iter().cloned());
            if payload["has_more"] != true {
                assert_eq!(payload["total"], oracle_items.len(), "{language} `{pattern}`");
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
