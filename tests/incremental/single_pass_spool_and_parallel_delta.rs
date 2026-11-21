#![cfg(all(feature = "lang-python", feature = "lang-rust"))]

use crate::common::Workspace;
use acdd_core::config::Adapter;
use acdd_index::{
    db::{
        commitments, reader,
        writer::{self, BuildInputs, SpoolSlice, StagedFilePayload, StagedPayload},
    },
    models::{compact_graph::Graph as CompactGraph, FileEntry},
    typed_facts::{encode_facts, TypedFacts},
};
use rusqlite::{ffi, Connection};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

thread_local! {
    static FTS_READS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

unsafe extern "C" fn observe_fts_read(
    _: *mut std::ffi::c_void,
    action: i32,
    table: *const std::ffi::c_char,
    column: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> i32 {
    if action == ffi::SQLITE_READ && !table.is_null() && !column.is_null() {
        // SAFETY: SQLite supplies a NUL-terminated table name during the callback.
        let table = unsafe { std::ffi::CStr::from_ptr(table) }.to_bytes();
        // SAFETY: SQLite supplies a NUL-terminated column name for a read.
        if unsafe { std::ffi::CStr::from_ptr(column) }.to_bytes() != b"term" {
            return ffi::SQLITE_OK;
        }
        FTS_READS.with(|reads| {
            let (mut nodes, mut docs) = reads.get();
            nodes += usize::from(table == b"node_search_commit_vocab");
            docs += usize::from(table == b"doc_search_commit_vocab");
            reads.set((nodes, docs));
        });
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn observe_fts_connection(
    db: *mut ffi::sqlite3,
    _: *mut *const std::ffi::c_char,
    _: *const ffi::sqlite3_api_routines,
) -> i32 {
    // SAFETY: SQLite supplies a live connection for authorizer registration.
    unsafe { ffi::sqlite3_set_authorizer(db, Some(observe_fts_read), std::ptr::null_mut()) }
}

struct ObserveFts;
impl Drop for ObserveFts {
    fn drop(&mut self) {
        // SAFETY: unregister only this test's callback.
        unsafe { ffi::sqlite3_cancel_auto_extension(Some(observe_fts_connection)) };
    }
}

#[test]
fn cold_and_delta_match_commitments_after_spool_and_parallel_delta() {
    let workspace = Workspace::with_files(&[
        (
            "pkg/provider.py",
            "def greet(name):\n    return f'hello {name}'\n",
        ),
        (
            "pkg/middle.py",
            "from pkg.provider import greet\n\ndef forward(name):\n    return greet(name)\n",
        ),
        (
            "pkg/consumer.py",
            "from pkg.middle import forward\n\ndef run():\n    return forward('world')\n",
        ),
        ("pkg/extra.py", "def helper(value):\n    return value + 1\n"),
        ("src/defs.rs", "pub trait Work { fn run(&self); }\n"),
        (
            "src/alpha.rs",
            "use crate::defs::Work;\npub struct Alpha;\nimpl Work for Alpha { fn run(&self) {} }\n",
        ),
        (
            "src/caller.rs",
            "use crate::defs::Work;\npub fn invoke(worker: &dyn Work) { worker.run(); }\n",
        ),
        ("docs/guide.md", "# Guide\nCalls `pkg.provider.greet`.\n"),
    ]);

    let cold_first = workspace.build();
    let cold_warm_cache = workspace.build_to(workspace.path(".acdd/warm-cold.sqlite"));
    assert_eq!(cold_first["output_root"], cold_warm_cache["output_root"]);
    assert_eq!(cold_first["files"], cold_warm_cache["files"]);
    assert_eq!(cold_first["nodes"], cold_warm_cache["nodes"]);
    assert_eq!(cold_first["edges"], cold_warm_cache["edges"]);

    // Verify local_facts uses ACDD-LOCAL-FACTS\0\x03 with Zstd framing.
    {
        let conn = workspace.open();
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT facts_blob FROM local_facts WHERE path='pkg/provider.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(blob.starts_with(b"ACDD-LOCAL-FACTS\0\x03"));
        assert!(blob["ACDD-LOCAL-FACTS\0\x03".len()..].starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
    }

    // Mutate multiple files across Python and Rust and run parallel delta + TEMP TABLE frontier.
    workspace.write(
        "pkg/provider.py",
        "def greet(name, title='mx'):\n    return f'hello {title} {name}'\n",
    );
    workspace.write("pkg/extra.py", "def helper(value):\n    return value + 2\n");
    workspace.write(
        "src/beta.rs",
        "use crate::defs::Work;\npub struct Beta;\nimpl Work for Beta { fn run(&self) {} }\n",
    );
    let delta_report = workspace.delta(&["pkg/provider.py", "pkg/extra.py", "src/beta.rs"]);
    let cold_after = workspace.build_to(workspace.path(".acdd/cold-after.sqlite"));

    assert_eq!(
        delta_report["output_root"], cold_after["output_root"],
        "parallel delta output_root must match cold rebuild"
    );
    assert_eq!(delta_report["files"], cold_after["files"]);

    let delta_conn = workspace.open();
    let cold_conn =
        reader::open(&workspace.path(".acdd/cold-after.sqlite"), workspace.root()).unwrap();
    commitments::verify(&delta_conn).unwrap();
    commitments::verify(&cold_conn).unwrap();

    // Temp-table RAII guard must restore PRAGMA query_only = ON on the reader connection.
    let query_only: bool = delta_conn
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert!(
        query_only,
        "reader connection must keep PRAGMA query_only = ON"
    );

    // Verify Rust dyn-trait caller and Python transitive consumers match cold rebuild.
    let query_edges = |conn: &Connection| -> Vec<String> {
        conn.prepare(
            "SELECT s.id||'|'||e.kind||'|'||d.id FROM edges e \
             JOIN nodes s ON s.node_hash=e.src_hash \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             ORDER BY s.id,e.kind,d.id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    };
    assert_eq!(query_edges(&delta_conn), query_edges(&cold_conn));
    let possible_calls = query_edges(&delta_conn)
        .into_iter()
        .filter(|edge| edge.contains("|possible_calls|"))
        .collect::<Vec<_>>();
    assert_eq!(
        possible_calls.len(),
        2,
        "added Beta impl must refresh unchanged caller.rs possible_calls: {possible_calls:?}"
    );
    drop(delta_conn);
    drop(cold_conn);

    // Warm extraction cache parity on subsequent delta edit.
    workspace.write("pkg/extra.py", "def helper(value):\n    return value + 3\n");
    let warm_delta = workspace.delta(&["pkg/extra.py"]);
    let warm_cold = workspace.build_to(workspace.path(".acdd/warm-cold-2.sqlite"));
    assert_eq!(warm_delta["output_root"], warm_cold["output_root"]);

    // SAFETY: this callback has the SQLite auto-extension signature.
    assert_eq!(
        unsafe { ffi::sqlite3_auto_extension(Some(observe_fts_connection)) },
        ffi::SQLITE_OK
    );
    let _observer = ObserveFts;
    for (path, content, expected_reads, corruption) in [
        (
            "docs/guide.md",
            Some("# Guide\nUpdated body content.\n"),
            (0, 1), None,
        ),
        (
            "docs/guide.md",
            Some("# Revised heading\nUpdated body content.\n"),
            (1, 1), None,
        ),
        ("docs/guide.md", Some("# Revised heading\n- INV-MATRIX: Preserve contract.\n"), (1, 1), None),
        (
            "pkg/extra.py",
            Some("def helper(value):\n    return value + 4\n"),
            (0, 0), None,
        ),
        (
            "pkg/extra.py",
            Some("def renamed_helper(value):\n    return value + 4\n"),
            (1, 0), None,
        ),
        ("pkg/extra.py", Some("def renamed_helper(value):\n    return value + 5\n"), (1, 0), Some("DELETE FROM domain_commitments WHERE domain='node_search_postings' AND owner_id=0")),
        ("pkg/extra.py", Some("def renamed_helper(value):\n    return value + 6\n"), (0, 1), Some("UPDATE domain_commitments SET digest='bad' WHERE domain='doc_search_postings' AND owner_id=0")),
        ("src/beta.rs", None, (1, 0), None),
    ] {
        if let Some(sql) = corruption {
            let conn = Connection::open(workspace.db()).unwrap();
            conn.execute_batch(sql).unwrap();
            conn.execute("UPDATE domain_commitments SET digest=CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END WHERE domain='nodes' AND owner_id=(SELECT path_id FROM path_dictionary WHERE path='src/defs.rs')", []).unwrap();
            let root: String = conn.query_row("SELECT value FROM metadata WHERE key='output_root'", [], |row| row.get(0)).unwrap();
            drop(conn);
            // Force the admitted delta seam to exercise its fail-closed repair of injected damage.
            assert!(acdd_index::db::cache::publish_verified(&workspace.db(), workspace.root(), &root).unwrap());
        }
        if let Some(content) = content { workspace.write(path, content); } else { std::fs::remove_file(workspace.path(path)).unwrap(); }
        FTS_READS.with(|reads| reads.set((0, 0)));
        let delta = workspace.delta(&[path]);
        assert_eq!(
            FTS_READS.with(|reads| reads.get()),
            expected_reads,
            "{path}"
        );
        let cold_path = workspace.path(".acdd/matrix-cold.sqlite");
        let cold = workspace.build_to(&cold_path);
        assert_eq!(delta["output_root"], cold["output_root"], "{path}");
        let delta_conn = workspace.open();
        let cold_conn = reader::open(&cold_path, workspace.root()).unwrap();
        commitments::verify(&delta_conn).unwrap();
        if corruption.is_some() {
            let representation: String = delta_conn.query_row("SELECT typeof(digest) FROM domain_commitments WHERE domain='nodes' AND owner_id=(SELECT path_id FROM path_dictionary WHERE path='src/defs.rs')", [], |row| row.get(0)).unwrap();
            assert_eq!(representation, "text");
            let valid: usize = delta_conn.query_row("SELECT count(*) FROM domain_commitments WHERE owner_id=0 AND domain IN ('node_search_postings','doc_search_postings') AND typeof(digest)='blob' AND length(digest)=32", [], |row| row.get(0)).unwrap();
            assert_eq!(valid, 2);
        }
        let evidence = |conn: &Connection| -> Vec<String> {
            conn.prepare("SELECT evidence FROM coverage_evidence ORDER BY evidence")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(evidence(&delta_conn), evidence(&cold_conn), "{path}");
    }
    let before = workspace.open();
    let candidates: Vec<String> = before.prepare(
        "SELECT DISTINCT e.evidence FROM coverage_evidence e WHERE e.evidence_id IN (\
         SELECT evidence_id FROM resolution_coverage WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='pkg/consumer.py') \
         UNION SELECT evidence_id FROM edge_occurrences WHERE owner_id=(SELECT path_id FROM path_dictionary WHERE path='pkg/consumer.py') \
         UNION SELECT confidence_id FROM edge_occurrences WHERE owner_id=(SELECT path_id FROM path_dictionary WHERE path='pkg/consumer.py'))"
    ).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    drop(before);
    workspace.write("pkg/consumer.py", "def run():\n    return 1\n");
    let delta = workspace.delta(&["pkg/consumer.py"]);
    let cold_path = workspace.path(".acdd/gc-cold.sqlite");
    let cold = workspace.build_to(&cold_path);
    assert_eq!(delta["output_root"], cold["output_root"]);
    let after = workspace.open();
    let retained: usize = candidates
        .iter()
        .filter(|value| {
            after
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM coverage_evidence WHERE evidence=?1)",
                    [value],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
        })
        .count();
    assert!(
        retained > 0 && retained < candidates.len(),
        "removed and shared evidence must both be represented: {candidates:?}"
    );
    commitments::verify(&after).unwrap();

    // Sensitivity check: corrupt staged spool offset must fail closed in populate_owned.
    let spool_file = workspace.path(".acdd/corrupt-spool.bin");
    let valid_blob = encode_facts(&TypedFacts::default()).unwrap();
    let mut handle = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&spool_file)
        .unwrap();
    handle.write_all(&valid_blob).unwrap();
    drop(handle);

    let mut stage_conn = Connection::open_in_memory().unwrap();
    let entry = FileEntry {
        path: "src/unit.rs".to_owned(),
        digest: "00".repeat(32),
        bytes: 10,
        mtime_ns: 1,
        identity: [0, 0, 0],
        language: "rust".to_owned(),
        is_doc: false,
    };
    let mut staged_payloads = BTreeMap::from([(
        entry.path.clone(),
        StagedFilePayload {
            encoded_facts: StagedPayload::Spilled(SpoolSlice {
                offset: 999_999,
                length: valid_blob.len() as u64,
            }),
            storage_rows: None,
        },
    )]);
    let err = writer::populate_owned(
        &mut stage_conn,
        workspace.root(),
        &Adapter::default(),
        std::slice::from_ref(&entry),
        BTreeMap::from([(entry.path.clone(), TypedFacts::default())]),
        &CompactGraph::default(),
        BuildInputs {
            manifest_digest: "test",
            memory_budget: 8 * 1024 * 1024,
            schema_ready: false,
        },
        &mut staged_payloads,
        Some(&spool_file),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("EOF") || err.to_string().contains("staged"),
        "corrupt spool offset must fail closed: {err:#}"
    );
    let _ = PathBuf::from(&spool_file);
}
