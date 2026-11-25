use crate::common::Workspace;
use acdd_index::db::reader;
use contextunity_acdd::engine::pipeline as writer;
use rusqlite::Connection;
use std::path::PathBuf;

#[cfg(feature = "lang-python")]
#[test]
fn navigation_storage_preserves_compressed_analysis_and_delta_calls() {
    let ws = Workspace::new();
    ws.write("service.py", "class Service:\n    def execute(self):\n        \"\"\"Execute the request.\"\"\"\n        return 42\n");
    ws.write("caller.py", "from service import Service\n\ndef run():\n    service = Service()\n    return service.execute()\n");
    let db_path = ws.root().join(".acdd/code-map.sqlite");
    writer::build(ws.root(), &db_path, None).unwrap();
    let conn = reader::open(&db_path, ws.root()).unwrap();
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT facts_blob FROM local_facts WHERE path='caller.py'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let facts = crate::common::decode_local_facts(&blob).unwrap();
    let caller = facts.nodes.iter().find(|node| node.name == "run").unwrap();
    let caller_details = crate::common::assertions::projected_node_details(&facts, caller);
    assert!(caller_details.get("value_flow").is_some());
    let stored: String = conn
        .query_row(
            "SELECT details FROM nodes WHERE id=?1",
            [&caller.id],
            |row| row.get(0),
        )
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["signature"], caller_details["signature"]);
    let method: String = conn
        .query_row(
            "SELECT details FROM nodes WHERE name='execute'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let method: serde_json::Value = serde_json::from_str(&method).unwrap();
    assert!(method["signature"].as_str().unwrap().contains("execute"));
    assert!(method["doc"]
        .as_str()
        .unwrap()
        .contains("Execute the request"));
    assert_eq!(method["receiver_name"], "self");
    drop(conn);
    ws.write("service.py", "class Service:\n    def execute(self):\n        \"\"\"Execute the updated request.\"\"\"\n        return 43\n");
    writer::delta_with_selection(
        ws.root(),
        &db_path,
        &[PathBuf::from("service.py")],
        None,
        None,
    )
    .unwrap();
    let conn = reader::open(&db_path, ws.root()).unwrap();
    let calls: i64 = conn.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='run' AND d.name='execute' AND e.kind='calls'", [], |row| row.get(0)).unwrap();
    assert_eq!(
        calls, 1,
        "cached complete local analysis supports receiver resolution during delta"
    );
    acdd_index::db::commitments::verify(&conn).unwrap();
    let root: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    writer::build(ws.root(), &db_path, None).unwrap();
    let rebuilt = reader::open(&db_path, ws.root()).unwrap();
    let cold_root: String = rebuilt
        .query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cold_root, root,
        "cold and delta catalog storage must seal identically"
    );
    drop(rebuilt);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE local_facts SET facts_blob=?1 WHERE path='caller.py'",
        [&blob[..blob.len() / 2]],
    )
    .unwrap();
    drop(conn);
    ws.write(
        "service.py",
        "class Service:\n    def execute(self):\n        return 44\n",
    );
    assert!(
        writer::delta_with_selection(
            ws.root(),
            &db_path,
            &[PathBuf::from("service.py")],
            None,
            None
        )
        .is_err(),
        "truncated durable facts fail closed before graph publication"
    );
    let conn = Connection::open(&db_path).unwrap();
    let retained_root: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='output_root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained_root, root);
    let retained_calls: i64 = conn.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name='run' AND d.name='execute' AND e.kind='calls'", [], |row| row.get(0)).unwrap();
    assert_eq!(retained_calls, calls);
}
