use contextunity_acdd::{
    core::response::{QueryOptions, ResponsePolicy, SourceOptions},
    db::{reader, symbols},
    engine::{ast, scanner},
};
use rusqlite::Connection;
use std::collections::BTreeSet;

fn rows(conn: &Connection, sql: &str) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn persisted_config_key_identity() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    let cases = [
        ("yaml_indexes_nested_quoted_flow_and_sequence_keys_with_unique_ids", "settings.yml", "service:\n  port: 8080\n  \"literal.dot\": yes\nflow: {left: {port: 1}, right: {port: 2}}\nitems:\n  - name: first\n  - name: second\n", &[
            "settings.service", "settings.service.port", "settings.service.\"literal.dot\"",
            "settings.flow.left.port", "settings.flow.right.port", "settings.items.name",
        ][..], "settings.yml:2", "port: 8080", "timeout: 15", 0, Some("variable")),
        ("toml_indexes_tables_array_tables_dotted_and_inline_keys", "settings.toml", "title = \"app\"\n[server]\nport = 8080\n\"literal.dot\" = true\nlimits.cpu = 2\ninline = { left = { value = 1 }, right = { value = 2 } }\n[[workers]]\nname = \"first\"\n[[workers]]\nname = \"second\"\n", &[
            "settings.title", "settings.server", "settings.server.port", "settings.server.\"literal.dot\"",
            "settings.server.limits.cpu", "settings.server.inline.left.value", "settings.workers.name",
        ][..], "settings.toml:3", "port = 8080", "timeout = 15", 2, None),
    ];
    for (_, path, source, ..) in cases {
        w.write(path, source);
    }
    w.build();
    let conn = w.open();
    for (case_id, path, _, expected, selector, snippet, _, workers, kind) in cases {
        let mut stmt = conn.prepare("SELECT n.qualname FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND (?2 IS NULL OR n.kind=?2) ORDER BY n.qualname").unwrap();
        let names: Vec<String> = stmt
            .query_map(rusqlite::params![path, kind], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for name in expected {
            assert!(
                names.iter().any(|actual| actual == name),
                "{case_id}: missing {name}: {names:?}"
            );
        }
        let actual_workers: usize = conn.query_row("SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.kind='namespace' AND n.name='workers'", [path], |row| row.get(0)).unwrap();
        assert_eq!(actual_workers, workers, "{case_id}");
        let node = reader::select(&conn, selector).unwrap();
        assert_eq!(node["name"], "port", "{case_id}");
        let policy = ResponsePolicy::default();
        let options = QueryOptions::resolve(&policy, None, 0, None, None).unwrap();
        let source = SourceOptions::resolve(&policy, Some(true), Some(0), None, 0).unwrap();
        let result = symbols::snippet_paged(&conn, w.root(), selector, &source, &options).unwrap();
        assert!(
            result["source"].as_str().unwrap().contains(snippet),
            "{case_id}: {result}"
        );
    }
    let ids = rows(&conn, "SELECT id FROM nodes");
    assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
    assert!(rows(&conn, "SELECT message FROM errors").is_empty());
    drop(conn);
    for (case_id, path, source, _, _, old, new, _, _) in cases {
        w.write(path, &source.replace(old, new));
        let parity = crate::languages::support::delta_cold(&w, path);
        assert_eq!(parity.incremental_verification, Ok(()), "{case_id}");
        assert_eq!(parity.cold_verification, Ok(()), "{case_id}");
        assert_eq!(parity.incremental, parity.cold, "{case_id}");
    }
}

#[test]
fn config_ast_search_finds_inner_pairs_and_key_captures() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    for (path, language, source, patterns) in [
        (
            "settings.yml",
            "yaml",
            "service:\n  port: 8080\n  host: local\n",
            ["port: 8080", "$KEY: 8080"],
        ),
        (
            "settings.toml",
            "toml",
            "[server]\nport = 8080\nhost = 'local'\n",
            ["port = 8080", "$KEY = 8080"],
        ),
    ] {
        w.write(path, source);
        for pattern in patterns {
            assert!(
                !ast::search(source, path, language, pattern, 20)
                    .unwrap()
                    .is_empty(),
                "{language}: {pattern}"
            );
        }
    }
    assert!(ast::search("key: 1\n", "settings.yaml", "yaml", "key: [", 20).is_err());
    assert!(ast::search("key = 1\n", "settings.toml", "toml", "key = [", 20).is_err());
}

#[test]
fn config_syntax_errors_are_recorded_and_repaired_by_delta() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write("broken.yaml", "key: [one, two\n");
    w.write("broken.toml", "[server\nport =\n");
    w.build();
    let conn = w.open();
    assert!(!rows(&conn, "SELECT message FROM errors WHERE path='broken.yaml'").is_empty());
    assert!(!rows(&conn, "SELECT message FROM errors WHERE path='broken.toml'").is_empty());
    drop(conn);
    w.write("broken.yaml", "key: [one, two]\n");
    let parity = crate::languages::support::delta_cold(&w, "broken.yaml");
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    w.write("broken.toml", "[server]\nport = 80\n");
    let parity = crate::languages::support::delta_cold(&w, "broken.toml");
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    let conn = w.open();
    assert!(rows(&conn, "SELECT message FROM errors").is_empty());
    let adapter = scanner::load_adapter(w.root(), None).unwrap();
    let scan = scanner::scan_with_adapter(w.root(), &adapter).unwrap();
    assert!(scan.entries.iter().any(|entry| entry.language == "yaml"));
    assert!(scan.entries.iter().any(|entry| entry.language == "toml"));
}
