use crate::common::Workspace;
use acdd_core::response::{QueryOptions, ResponsePolicy};
use acdd_index::db::reader;
use contextunity_acdd::engine::pipeline as writer;

#[test]
fn cold_build_roundtrip_query_suite() {
    let ws = Workspace::new();
    ws.write(
        "src/main.py",
        r#"
class Service:
    def execute(self):
        return helper()

def helper():
    return 42
"#,
    );
    ws.write(
        "README.md",
        r#"# Workspace Documentation
## Standards
All handlers must be asynchronous and tested.
See `ServiceClient`.
"#,
    );

    let db_path = ws.root().join("code_map.sqlite");
    let report = writer::build(ws.root(), &db_path, None).expect("cold build failed");
    assert!(report["nodes"].as_i64().unwrap_or(0) > 0);
    assert!(report["files"].as_i64().unwrap_or(0) >= 2);
    assert_eq!(
        report["node_row_count"].as_u64(),
        report["nodes"].as_u64(),
        "cold build reports the number of node rows inserted"
    );
    assert!(
        report["node_row_insert_ms"].as_f64().is_some(),
        "cold build reports node-row batch insertion time"
    );
    assert!(report["rows_ms"].as_f64().is_some());
    assert!(report["persist_ms"].as_f64().is_some());

    let conn = reader::open(&db_path, ws.root()).expect("reader open failed");
    let page = QueryOptions::resolve(&ResponsePolicy::default(), Some(10), 0, None, None).unwrap();
    let overview = reader::overview_with_options(
        &conn,
        &reader::OverviewOptions {
            aspects: None,
            page: &page,
        },
    )
    .expect("overview failed");
    assert!(overview["counts"]["files"].as_i64().unwrap_or(0) >= 2);

    let docs = reader::search_docs_with_options(
        &conn,
        "Standards",
        &reader::DocSearchOptions {
            doc_type: None,
            component: None,
            include_excerpt: false,
            page: &page,
        },
    )
    .expect("search docs failed");
    assert!(!docs["sections"]["items"].as_array().unwrap().is_empty());

    let sections: Vec<(String, String, String, i64)> = conn
        .prepare(
            "SELECT section_title, content, referenced_symbols, is_invariant FROM doc_sections WHERE path='README.md' ORDER BY rowid",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(sections.len(), 2, "{sections:?}");
    assert_eq!(sections[0].0, "Workspace Documentation");
    assert_eq!(sections[1].0, "Standards");
    assert!(sections[1].1.contains("asynchronous"), "{}", sections[1].1);
    assert_eq!(sections[1].2, "[\"ServiceClient\"]");
    assert_eq!(sections[0].3, 0);
    assert_eq!(sections[1].3, 0);

    ws.write(
        "src/many.py",
        &(0..1500)
            .map(|i| format!("def function_{i}():\n    return {i}\n"))
            .collect::<String>(),
    );
    let mut roots = Vec::new();
    let mut graph_baseline = None;
    for (label, budget) in [
        ("default", None),
        ("spill", Some(1024 * 1024)),
        ("ram", Some(256 * 1024 * 1024)),
    ] {
        let output = ws.path(format!(".acdd/{label}.sqlite"));
        let mut hooks = writer::PipelineTestHooks::before_cleanup(|_| {});
        std::sync::Arc::get_mut(&mut hooks)
            .unwrap()
            .memory_budget_override = budget;
        std::sync::Arc::get_mut(&mut hooks)
            .unwrap()
            .cache_dir_override = Some(ws.path(".acdd/cache"));
        let report = writer::build_with_test_hooks(ws.root(), &output, None, hooks).unwrap();
        if label == "spill" {
            assert!(report["staged_disk_bytes"].as_u64().unwrap() > 0);
        } else if label == "ram" {
            assert_eq!(report["staged_disk_bytes"].as_u64(), Some(0));
        }
        roots.push(report["output_root"].clone());
        let connection = reader::open(&output, ws.root()).unwrap();
        acdd_index::db::commitments::verify(&connection).unwrap();
        let rows = |sql: &str| -> Vec<String> {
            connection
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let graph = (
            rows("SELECT id FROM nodes ORDER BY id"),
            rows("SELECT s.id || '|' || e.kind || '|' || d.id FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash ORDER BY 1"),
        );
        if let Some(baseline) = &graph_baseline {
            assert_eq!(
                &graph, baseline,
                "node and resolved edge parity for {label}"
            );
        } else {
            graph_baseline = Some(graph);
        }
    }
    assert_eq!(roots[0], roots[1]);
    assert_eq!(roots[1], roots[2]);
    let modified = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    std::fs::File::options()
        .write(true)
        .open(ws.path("README.md"))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let refreshed_path = ws.path(".acdd/refreshed.sqlite");
    let refreshed =
        writer::build_with_cache_dir(ws.root(), &refreshed_path, None, &ws.path(".acdd/cache"))
            .unwrap();
    assert_eq!(
        refreshed["pipeline"]["extraction_cache_hits"], refreshed["files"],
        "every file, including README.md, must hit the seeded V5 cache"
    );
    let connection = reader::open(&refreshed_path, ws.root()).unwrap();
    let mtime: f64 = connection
        .query_row(
            "SELECT mtime FROM doc_sections WHERE path='README.md' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let expected = std::fs::metadata(ws.path("README.md"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!((mtime - expected).abs() < 0.001);
    let blob: Vec<u8> = connection
        .query_row(
            "SELECT facts_blob FROM local_facts WHERE path='README.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let facts = acdd_index::typed_facts::decode_facts(&blob).unwrap();
    assert!(!facts.docs.is_empty());
    assert!(facts
        .docs
        .iter()
        .all(|section| (section.mtime - expected).abs() < 0.001));
    drop(connection);
    let original = std::fs::read_to_string(ws.path("src/main.py")).unwrap();
    ws.write("src/main.py", "def changed():\n    return 9\n");
    writer::delta_with_cache_dir(
        ws.root(),
        &refreshed_path,
        &["src/main.py".into()],
        &ws.path(".acdd/cache"),
    )
    .unwrap();
    ws.write("src/main.py", &original);
    let delta = writer::delta_with_cache_dir(
        ws.root(),
        &refreshed_path,
        &["src/main.py".into()],
        &ws.path(".acdd/cache"),
    )
    .unwrap();
    let cold_path = ws.path(".acdd/delta-comparison.sqlite");
    let cold =
        writer::build_with_cache_dir(ws.root(), &cold_path, None, &ws.path(".acdd/cache")).unwrap();
    assert_eq!(delta["output_root"], cold["output_root"]);
    let corrupt_path = ws.path(".acdd/corrupt.sqlite");
    std::fs::copy(&cold_path, &corrupt_path).unwrap();
    use std::io::Write;
    let mut corrupt = std::fs::OpenOptions::new()
        .write(true)
        .open(&corrupt_path)
        .unwrap();
    corrupt.write_all(b"broken SQLite header").unwrap();
    drop(corrupt);
    assert!(reader::open(&corrupt_path, ws.root()).is_err());
    let rejected = ws.path(".acdd/rejected.sqlite");
    let mut hook = writer::PipelineTestHooks::before_cleanup(|_| {});
    std::sync::Arc::get_mut(&mut hook)
        .unwrap()
        .before_candidate_verify = Some(Box::new(|path| {
        use std::io::{Seek, SeekFrom};
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let pages: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        assert!(pages > 0);
        drop(conn);
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        file.write_all(&[0xff]).unwrap();
    }));
    let error = writer::build_with_test_hooks(ws.root(), &rejected, None, hook).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("candidate database failed integrity check"),
        "{error:#}"
    );
    assert!(!rejected.exists());
    assert!(!ws.path(".acdd/rejected.sqlite.verified.json").exists());
}
