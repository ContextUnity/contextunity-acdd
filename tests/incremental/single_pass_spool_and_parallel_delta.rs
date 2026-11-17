#![cfg(all(feature = "lang-python", feature = "lang-rust"))]

use crate::common::Workspace;
use acdd_core::config::Adapter;
use acdd_index::{
    db::{
        commitments, reader,
        writer::{self, BuildInputs, StagedFilePayload, StagedPayloadBlob},
    },
    models::{compact_graph::Graph as CompactGraph, FileEntry},
    typed_facts::{encode_facts, TypedFacts},
};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

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
        (
            "pkg/extra.py",
            "def helper(value):\n    return value + 1\n",
        ),
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
    workspace.write(
        "pkg/extra.py",
        "def helper(value):\n    return value + 2\n",
    );
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
    let cold_conn = reader::open(&workspace.path(".acdd/cold-after.sqlite"), workspace.root()).unwrap();
    commitments::verify(&delta_conn).unwrap();
    commitments::verify(&cold_conn).unwrap();

    // Temp-table RAII guard must restore PRAGMA query_only = ON on the reader connection.
    let query_only: bool = delta_conn
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert!(query_only, "reader connection must keep PRAGMA query_only = ON");

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
    workspace.write(
        "pkg/extra.py",
        "def helper(value):\n    return value + 3\n",
    );
    let warm_delta = workspace.delta(&["pkg/extra.py"]);
    let warm_cold = workspace.build_to(workspace.path(".acdd/warm-cold-2.sqlite"));
    assert_eq!(warm_delta["output_root"], warm_cold["output_root"]);

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
            encoded_facts: StagedPayloadBlob {
                offset: 999_999,
                length: valid_blob.len() as u64,
            },
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
