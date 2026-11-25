use acdd_index::db::commitments;

#[test]
fn clean_delta_preserves_node_fts_data_and_commitment() {
    let workspace = crate::common::Workspace::new();
    let source = workspace.path("source");
    let code: String = (0..64)
        .map(|i| format!("pub fn signed_symbol_{i}() {{}}\n"))
        .collect();
    workspace.write("source/lib.rs", &code);
    let path = workspace.path("pipeline.sqlite");
    contextunity_acdd::engine::pipeline::build_with_cache_dir(
        &source,
        &path,
        None,
        &workspace.path("cache"),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let fts_data = |conn: &rusqlite::Connection| -> Vec<(i64, Vec<u8>)> {
        conn.prepare("SELECT id,block FROM node_search_data ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let before = fts_data(&conn);
    drop(conn);
    workspace.write("source/lib.rs", &format!("{code}\n"));
    contextunity_acdd::engine::pipeline::delta_with_selection(
        &source,
        &path,
        &["lib.rs".into()],
        None,
        Some(&workspace.path("cache")),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        fts_data(&conn),
        before,
        "clean delta must not mutate node FTS"
    );
    commitments::verify(&conn).unwrap();
}
