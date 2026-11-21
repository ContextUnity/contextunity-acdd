use super::harness::*;
use acdd_core::format::stable_hash64;
use rusqlite::params;
use std::collections::BTreeSet;

#[test]
fn parallel_owner_sealing_matches_serial_encoding_and_thread_counts() {
    let (owner_count, nodes_per_owner) = (128, 64);
    let mut conn = connection();
    let mut owners = BTreeSet::new();
    let transaction = conn.transaction().expect("transaction begins");
    {
        let mut insert_owner = transaction
            .prepare_cached("INSERT OR IGNORE INTO path_dictionary(path) VALUES(?1)")
            .unwrap();
        let mut select_owner = transaction
            .prepare_cached("SELECT path_id FROM path_dictionary WHERE path=?1")
            .unwrap();
        let mut insert_node = transaction
            .prepare_cached(
                "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) VALUES(?1,'function',?1,?1,?2,1,1,0,'rust',0,'{}',?3,?2)",
            )
            .unwrap();
        let mut insert_edge = transaction
            .prepare_cached("INSERT INTO edges VALUES(?1,'calls',?2,?3,?4,1,2,2)")
            .unwrap();
        let mut insert_occurrence = transaction
            .prepare_cached("INSERT INTO edge_occurrences VALUES(?1,?2,?3,?4,'calls',?5,1,2)")
            .unwrap();
        let mut insert_dependency = transaction
            .prepare_cached("INSERT INTO dependencies VALUES(?1,?2,NULL,'imports',?3,'external')")
            .unwrap();

        for owner_index in (0..owner_count).rev() {
            let path = format!("src/owner_{owner_index:03}.rs");
            owners.insert(path.clone());
            insert_owner.execute([&path]).unwrap();
            let owner: i64 = select_owner.query_row([&path], |row| row.get(0)).unwrap();
            for ordinal in (0..nodes_per_owner).rev() {
                let id = format!("fn:{owner_index}:{ordinal}");
                insert_node
                    .execute(params![id, owner, stable_hash64(&id)])
                    .unwrap();
            }
            for ordinal in (0..nodes_per_owner).rev() {
                let src = stable_hash64(&format!("fn:{owner_index}:{ordinal}"));
                let dst = stable_hash64(&format!("fn:{owner_index}:0"));
                insert_edge
                    .execute(params![src, dst, owner, ordinal + 1])
                    .unwrap();
                insert_occurrence
                    .execute(params![owner, ordinal, src, dst, ordinal + 1])
                    .unwrap();
                insert_dependency
                    .execute(params![owner, ordinal, format!("symbol_{ordinal}")])
                    .unwrap();
            }
        }
    }
    transaction
        .execute_batch(
            "INSERT INTO coverage_expressions(expression) VALUES('item_00');
         INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id)
         SELECT p.path_id, 0, x.expression_id, 'resolved', 1
         FROM path_dictionary p
         CROSS JOIN coverage_expressions x
         WHERE p.path LIKE 'src/owner_%';",
        )
        .unwrap();
    transaction.commit().unwrap();

    assert_eq!(owners.len(), owner_count);

    let single = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let connection = &mut conn;
    let serial_root = single
        .install(move || commitments::seal(connection))
        .expect("serial sealing produces the reference root");

    use acdd_index::commitments::CommitmentDomain;
    let owner_domains = [
        (CommitmentDomain::Nodes, "nodes"),
        (CommitmentDomain::Edges, "edges"),
        (CommitmentDomain::EdgeOccurrences, "edge_occurrences"),
        (CommitmentDomain::Dependencies, "dependencies"),
        (CommitmentDomain::ResolutionCoverage, "resolution_coverage"),
    ];
    let mut digest_query = conn
        .prepare_cached(
            "SELECT p.path,d.digest FROM domain_commitments d
         JOIN path_dictionary p ON p.path_id=d.owner_id
         WHERE d.domain=?1 AND p.path LIKE 'src/owner_%' ORDER BY p.path",
        )
        .unwrap();
    for (domain, table) in owner_domains {
        assert_eq!(domain.as_str(), table);
        let rows = digest_query
            .query_map([table], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .unwrap();
        let mut observed_owners = BTreeSet::new();
        let mut count = 0;
        for row in rows {
            let (owner, digest) = row.unwrap();
            assert_eq!(digest.len(), 32, "{table}: {owner}");
            observed_owners.insert(owner);
            count += 1;
        }
        assert_eq!(count, owner_count, "{table}");
        let expected_owners: BTreeSet<&str> = owners.iter().map(String::as_str).collect();
        let observed_owners_ref: BTreeSet<&str> =
            observed_owners.iter().map(String::as_str).collect();
        assert_eq!(observed_owners_ref, expected_owners, "{table}");
    }
    drop(digest_query);

    commitments::verify(&conn).expect("serial verification succeeds");

    for thread_count in [1, 2, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .unwrap();
        let connection = &mut conn;
        let full_seal = pool
            .install(move || commitments::seal(connection))
            .expect("full sealing succeeds for every pool");
        assert_eq!(
            full_seal, serial_root,
            "full sealing must match the serial root with {thread_count} threads"
        );

        let connection = &mut conn;
        let owner_paths = &owners;
        let owner_seal = pool
            .install(move || commitments::seal_owners(connection, Some(owner_paths)))
            .expect("selected-owner sealing succeeds for every pool");
        assert_eq!(
            owner_seal, serial_root,
            "owner sealing must match the serial root with {thread_count} threads"
        );

        let connection = &mut conn;
        pool.install(move || commitments::verify(connection))
            .expect("every pool's sealed database verifies");
    }
    {
        let conn = super::harness::connection();
        search_nodes(&conn, 2);
        conn.execute_batch(
            "INSERT INTO node_search(rowid,search_text) VALUES(1,'alpha'),(2,'beta');",
        )
        .unwrap();
        let before =
            commitments::fts_digest_vocab(&conn, "node_search", "term,doc,col,offset").unwrap();
        assert_eq!(
            commitments::fts_digest_fastpath(&conn, "node_search").unwrap(),
            Some(before.clone())
        );
        let sizes: Vec<(i64, Vec<u8>)> = conn
            .prepare("SELECT id,sz FROM node_search_docsize ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        commitments::seal(&conn).unwrap();
        conn.execute_batch(
            "DELETE FROM node_search WHERE rowid IN (1,2);
             INSERT INTO node_search(rowid,search_text) VALUES(1,'beta'),(2,'alpha');",
        )
        .unwrap();
        let after_sizes: Vec<(i64, Vec<u8>)> = conn
            .prepare("SELECT id,sz FROM node_search_docsize ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(sizes, after_sizes);
        let after =
            commitments::fts_digest_vocab(&conn, "node_search", "term,doc,col,offset").unwrap();
        assert_eq!(
            commitments::fts_digest_fastpath(&conn, "node_search").unwrap(),
            None
        );
        assert_ne!(before, after);
        assert!(commitments::verify(&conn).is_err());
        conn.execute_batch("INSERT INTO node_search(node_search) VALUES('optimize');")
            .unwrap();
        assert_eq!(
            commitments::fts_digest_fastpath(&conn, "node_search").unwrap(),
            Some(after.clone())
        );
    }
    {
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
        let (negative, positive): (i64, i64) = conn.query_row(
            "SELECT count(*) FILTER (WHERE node_hash < 0), count(*) FILTER (WHERE node_hash > 0) FROM nodes",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert!(negative > 0 && positive > 0);
        let fast = commitments::fts_digest_fastpath(&conn, "node_search_postings").unwrap();
        assert_eq!(
            fast,
            Some(
                commitments::fts_digest_vocab(&conn, "node_search_postings", "term,doc,col,offset")
                    .unwrap()
            )
        );
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
        contextunity_acdd::engine::pipeline::delta_with_cache_dir(
            &source,
            &path,
            &["lib.rs".into()],
            &workspace.path("cache"),
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
    {
        let workspace = crate::common::Workspace::new();
        let path = workspace.path("fts.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(acdd_index::db::SCHEMA_DDL).unwrap();
        search_nodes(&conn, 512);
        conn.execute_batch("INSERT INTO node_search(node_search,rank) VALUES('pgsz',256);")
            .unwrap();
        for (start, end) in [(1, 256), (257, 512)] {
            conn.execute_batch("BEGIN").unwrap();
            for id in start..=end {
                let text = if id == 1 {
                    format!("{} zzz", "aaa ".repeat(350))
                } else {
                    "shared λ_fn café_symbol 日本語_node foo.bar_baz-qux/path".to_owned()
                };
                conn.execute(
                    "INSERT INTO node_search(rowid,search_text) VALUES(?1,?2)",
                    params![id, text],
                )
                .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
        }
        conn.execute_batch(
            "INSERT INTO doc_sections(doc_id,path,section_title,doc_type,content,invariants,referenced_symbols,mtime,size)
             VALUES('doc-1','docs/1.md','λ_fn foo.bar','guide','café_symbol 日本語_node','foo.bar_baz-qux/path','',0,0);
             INSERT INTO doc_search(rowid,section_title,content,invariants)
             SELECT rowid,section_title,content,invariants FROM doc_sections;",
        )
        .unwrap();
        for (domain, expected) in [
            (commitments::FtsDomain::NodeSearch, "node_search"),
            (commitments::FtsDomain::DocSearch, "doc_search"),
        ] {
            let table = domain.as_str();
            assert_eq!(table, expected);
            assert_eq!(
                commitments::fts_digest_fastpath(&conn, table).unwrap(),
                Some(commitments::fts_digest_vocab(&conn, table, "term,doc,col,offset").unwrap()),
                "{table}"
            );
        }
        for (key, value) in [("version", "3"), ("detail", "column"), ("detail", "none")] {
            conn.execute_batch("SAVEPOINT config_case").unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO node_search_config(k,v) VALUES(?1,?2)",
                params![key, value],
            )
            .unwrap();
            assert_eq!(
                commitments::fts_digest_fastpath(&conn, "node_search").unwrap(),
                None
            );
            conn.execute_batch("ROLLBACK TO config_case; RELEASE config_case")
                .unwrap();
        }
        commitments::seal(&conn).unwrap();
        for (sql, restore, original, fastpath_errors) in [
            (
                "UPDATE node_search_data SET block = X'0102' WHERE id = 10",
                "UPDATE node_search_data SET block = ?1 WHERE id = 10",
                conn.query_row(
                    "SELECT block FROM node_search_data WHERE id = 10",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap(),
                false,
            ),
            (
                "UPDATE node_search_docsize SET sz = X'ffffffffff' WHERE id = 1",
                "UPDATE node_search_docsize SET sz = ?1 WHERE id = 1",
                conn.query_row(
                    "SELECT sz FROM node_search_docsize WHERE id = 1",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap(),
                true,
            ),
        ] {
            conn.execute_batch(sql).unwrap();
            let fresh = rusqlite::Connection::open(&path).unwrap();
            let result = commitments::fts_digest_fastpath(&fresh, "node_search");
            if fastpath_errors {
                assert!(result.is_err(), "{sql}");
            } else {
                assert_eq!(result.unwrap(), None, "{sql}");
            }
            assert!(commitments::verify(&fresh).is_err(), "{sql}");
            drop(fresh);
            conn.execute(restore, [original]).unwrap();
        }
    }
}
