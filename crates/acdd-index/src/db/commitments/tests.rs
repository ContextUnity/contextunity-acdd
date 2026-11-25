use super::*;
use crate::commitments::CommitmentDomain;
use crate::db::schema::SCHEMA_DDL;
use acdd_core::format::stable_hash64;
use rusqlite::{config::DbConfig, params, Connection};
use std::collections::BTreeSet;

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA_DDL).unwrap();
    conn.execute_batch(
        "INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'call'),(2,'exact')",
    )
    .unwrap();
    conn
}

fn owner_id(conn: &Connection, path: &str) -> i64 {
    conn.execute(
        "INSERT OR IGNORE INTO path_dictionary(path) VALUES(?1)",
        [path],
    )
    .unwrap();
    conn.query_row(
        "SELECT path_id FROM path_dictionary WHERE path=?1",
        [path],
        |row| row.get(0),
    )
    .unwrap()
}

fn search_nodes(conn: &Connection, count: i64) {
    let path_id = owner_id(conn, "src/fts.rs");
    for node_hash in 1..=count {
        let id = format!("fts:{node_hash}");
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) \
             VALUES(?1,'function',?1,?1,?2,1,1,0,'rust',0,'{}',?3,?2)",
            params![id, path_id, node_hash],
        )
        .unwrap();
    }
}

fn fts_postings(conn: &Connection, vocab: &str) -> Vec<(String, i64, String, i64)> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT term,doc,col,offset FROM {vocab} ORDER BY term,doc,col,offset"
        ))
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn coverage_leaf(conn: &Connection, owner: &str) -> Vec<u8> {
    conn.query_row(
        "SELECT digest FROM domain_commitments WHERE domain='resolution_coverage'
         AND owner_id=(SELECT path_id FROM path_dictionary WHERE path=?1)",
        [owner],
        |row| row.get(0),
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum ExpressionOrder {
    Ascending,
    Descending,
}

#[test]
fn logical_fts_commitments_ignore_physical_insertion_order() {
    let cases = [("forward", false), ("reverse", true)];
    let mut roots = Vec::with_capacity(2);

    for (label, reverse) in cases {
        let conn = connection();
        search_nodes(&conn, 64);
        let path_id = owner_id(&conn, "src/fts.rs");
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) VALUES('fts:negative','function','negative','negative',?1,1,1,0,'rust',0,'{}',-1,?1)",
            [path_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_search(rowid,search_text) VALUES(-1,'α unicode unicode')",
            [],
        )
        .unwrap();
        for rowid in 1..=16 {
            conn.execute(
                "INSERT INTO doc_sections(doc_id,path,section_title,doc_type,content,invariants,referenced_symbols,mtime,size) \
                 VALUES(?1,?2,?3,'guide',?4,?5,'',0,0)",
                params![
                    format!("doc-{rowid}"),
                    format!("docs/{rowid}.md"),
                    format!("Section {rowid}"),
                    format!("content token {rowid}"),
                    format!("invariant token {rowid}")
                ],
            )
            .unwrap();
        }

        let mut rowids: Vec<_> = (1..=64).collect();
        if reverse {
            rowids.reverse();
        }
        for rowid in rowids {
            conn.execute(
                "INSERT INTO node_search(rowid,search_text) VALUES(?1,?2)",
                params![rowid, format!("node token {rowid}")],
            )
            .unwrap();
        }

        let mut doc_rowids: Vec<_> = (1..=16).collect();
        if reverse {
            doc_rowids.reverse();
        }
        for rowid in doc_rowids {
            let (title, content, invariants): (String, String, String) = conn
                .query_row(
                    "SELECT section_title,content,invariants FROM doc_sections WHERE rowid=?1",
                    [rowid],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO doc_search(rowid,section_title,content,invariants) VALUES(?1,?2,?3,?4)",
                params![rowid, title, content, invariants],
            )
            .unwrap();
        }

        conn.execute_batch(
            "CREATE VIRTUAL TABLE temp.node_search_commit_vocab USING fts5vocab(main,node_search,'instance');",
        )
        .unwrap();
        let ordered = fts_postings(&conn, "node_search_commit_vocab");
        let streaming_postings_sorted =
            ordered.len() > 1 && ordered.windows(2).all(|pair| pair[0] <= pair[1]);
        assert!(
            streaming_postings_sorted,
            "{label}: streaming branch is exercised"
        );

        let root = seal(&conn).expect("initial sealing succeeds");
        verify(&conn).expect("the streaming result verifies");

        conn.execute_batch(
            "CREATE VIRTUAL TABLE temp.raw_node_vocab USING fts5vocab(main,node_search,'instance');
             DROP TABLE temp.node_search_commit_vocab;
             CREATE TEMP TABLE node_search_commit_vocab AS
               SELECT term,col,offset,doc FROM raw_node_vocab
               ORDER BY term DESC,doc DESC,col DESC,offset DESC;",
        )
        .unwrap();
        let reversed = conn
            .prepare("SELECT term,doc,col,offset FROM node_search_commit_vocab ORDER BY term DESC,doc DESC,col DESC,offset DESC")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let fallback_postings_reversed = reversed.windows(2).any(|pair| pair[0] > pair[1]);
        assert!(
            fallback_postings_reversed,
            "{label}: sorted fallback is exercised"
        );

        verify(&conn).expect("the fallback result verifies");
        let fallback_root = seal(&conn).expect("fallback sealing succeeds");
        assert_eq!(
            fallback_root, root,
            "{label}: fallback root must match initial root"
        );

        conn.execute(
            "UPDATE node_search_commit_vocab SET doc=9000001 WHERE rowid=(SELECT min(rowid) FROM node_search_commit_vocab)",
            [],
        )
        .unwrap();
        assert!(
            verify(&conn).is_err(),
            "{label}: missing posting owner must fail closed"
        );

        roots.push(root);
    }

    assert_eq!(
        roots[0], roots[1],
        "node_search and doc_search logical postings must seal identically regardless of insertion order"
    );
}

#[test]
fn sealing_rejects_missing_commitment_references() {
    #[derive(Clone, Copy)]
    enum MissingReference {
        EdgeOwner,
        EdgeSource,
        EdgeDestination,
        OccurrenceOwner,
        OccurrenceSource,
        OccurrenceDestination,
        DependencyOwner,
        SharedOwner,
        SharedKey,
    }

    let cases = [
        (
            MissingReference::EdgeOwner,
            "edge commitment references missing owner path",
        ),
        (
            MissingReference::EdgeSource,
            "edge commitment references missing source node",
        ),
        (
            MissingReference::EdgeDestination,
            "edge commitment references missing destination node",
        ),
        (
            MissingReference::OccurrenceOwner,
            "edge occurrence commitment references missing owner path",
        ),
        (
            MissingReference::OccurrenceSource,
            "edge occurrence commitment references missing source node",
        ),
        (
            MissingReference::OccurrenceDestination,
            "edge occurrence commitment references missing destination node",
        ),
        (
            MissingReference::DependencyOwner,
            "dependency commitment references missing owner path",
        ),
        (
            MissingReference::SharedOwner,
            "shared-owner commitment references missing owner path",
        ),
        (
            MissingReference::SharedKey,
            "shared-owner commitment references missing key",
        ),
    ];
    let conn = connection();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    let owner = owner_id(&conn, "src/a.rs");
    for id in ["fn:source", "fn:destination"] {
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id)
             VALUES(?1,'function',?1,?1,?2,1,1,0,'rust',0,'{}',?3,?2)",
            params![id, owner, stable_hash64(id)],
        )
        .unwrap();
    }
    let source_hash = stable_hash64("fn:source");
    let destination_hash = stable_hash64("fn:destination");
    let target_key_hash = stable_hash64("fn:target");
    conn.execute(
        "INSERT INTO shared_keys(key_hash,key) VALUES(?1,'fn:target')",
        [target_key_hash],
    )
    .unwrap();

    for (mutation, reason) in cases {
        conn.execute_batch("SAVEPOINT missing_commitment_reference;")
            .unwrap();
        let _ = match mutation {
            MissingReference::EdgeOwner => conn
                .execute(
                    "INSERT INTO edges VALUES(?1,'calls',?2,99,1,1,2,1)",
                    params![source_hash, destination_hash],
                )
                .unwrap(),
            MissingReference::EdgeSource => conn
                .execute(
                    "INSERT INTO edges VALUES(?1,'calls',?2,?3,1,1,2,1)",
                    params![stable_hash64("fn:missing-source"), destination_hash, owner],
                )
                .unwrap(),
            MissingReference::EdgeDestination => conn
                .execute(
                    "INSERT INTO edges VALUES(?1,'calls',?2,?3,1,1,2,1)",
                    params![source_hash, stable_hash64("fn:missing-destination"), owner],
                )
                .unwrap(),
            MissingReference::OccurrenceOwner => conn
                .execute(
                    "INSERT INTO edge_occurrences VALUES(99,0,?1,?2,'calls',1,1,2)",
                    params![source_hash, destination_hash],
                )
                .unwrap(),
            MissingReference::OccurrenceSource => conn
                .execute(
                    "INSERT INTO edge_occurrences VALUES(?1,0,?2,?3,'calls',1,1,2)",
                    params![owner, stable_hash64("fn:missing-source"), destination_hash],
                )
                .unwrap(),
            MissingReference::OccurrenceDestination => conn
                .execute(
                    "INSERT INTO edge_occurrences VALUES(?1,0,?2,?3,'calls',1,1,2)",
                    params![owner, source_hash, stable_hash64("fn:missing-destination")],
                )
                .unwrap(),
            MissingReference::DependencyOwner => conn
                .execute(
                    "INSERT INTO dependencies VALUES(99,0,?1,'imports','target','resolved')",
                    [stable_hash64("src/target.rs")],
                )
                .unwrap(),
            MissingReference::SharedOwner => conn
                .execute(
                    "INSERT INTO shared_owners VALUES(0,?1,99,0)",
                    [target_key_hash],
                )
                .unwrap(),
            MissingReference::SharedKey => conn
                .execute(
                    "INSERT INTO shared_owners VALUES(0,?1,?2,0)",
                    params![stable_hash64("fn:missing-target"), owner],
                )
                .unwrap(),
        };
        let sealing = seal(&conn);
        conn.execute_batch(
            "ROLLBACK TO missing_commitment_reference; RELEASE missing_commitment_reference;",
        )
        .unwrap();
        let error = sealing.expect_err("malformed commitment reference must fail sealing");
        assert!(error.to_string().contains(reason), "{reason}: {error:#}");
    }
}

#[test]
fn logical_fts_commitments_are_stable_and_reject_semantic_tampering() {
    let conn = connection();
    search_nodes(&conn, 2048);
    conn.execute_batch(
        "INSERT INTO node_search(node_search,rank) VALUES('pgsz',256);
         WITH RECURSIVE nums(x) AS (
             VALUES(1)
             UNION ALL SELECT x+1 FROM nums WHERE x<2048
         )
         INSERT INTO node_search(rowid,search_text)
         SELECT x,printf('unique%06d',x) FROM nums;",
    )
    .unwrap();
    for rowid in 1..=8 {
        conn.execute(
            "INSERT INTO doc_sections(doc_id,path,section_title,doc_type,content,invariants,referenced_symbols,mtime,size) \
             VALUES(?1,?2,?3,'guide',?4,?5,'',0,0)",
            params![
                format!("doc-{rowid}"),
                format!("docs/{rowid}.md"),
                format!("Section {rowid}"),
                format!("content search {rowid}"),
                format!("invariant {rowid}")
            ],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO doc_search(rowid,section_title,content,invariants) \
         SELECT rowid,section_title,content,invariants FROM doc_sections",
        [],
    )
    .unwrap();

    let defensive = conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, false)
        .unwrap();
    conn.execute_batch(
        "CREATE INDEX partial_order_probe ON node_search_idx(segid,pgno DESC,term);
         ANALYZE;",
    )
    .unwrap();
    let expected_root = seal(&conn).unwrap();

    let cases = [
        ("node_search_postings", "INSERT INTO node_search(rowid,search_text) VALUES(9000001,'logicalpostingtamper')", "node_search_postings"),
        ("node_search_docsize", "UPDATE node_search_docsize SET sz=CAST(sz || X'00' AS BLOB) WHERE id=(SELECT id FROM node_search_docsize ORDER BY id LIMIT 1)", "node_search_docsize"),
        ("node_search_config", "UPDATE node_search_config SET v=CAST(v || 'x' AS BLOB) WHERE k=(SELECT k FROM node_search_config ORDER BY k LIMIT 1)", "node_search_config"),
        ("doc_search_postings", "INSERT INTO doc_search(rowid,section_title,content,invariants) VALUES(9000001,'logical posting tamper','','')", "doc_search_postings"),
        ("doc_search_docsize", "UPDATE doc_search_docsize SET sz=CAST(sz || X'00' AS BLOB) WHERE id=(SELECT id FROM doc_search_docsize ORDER BY id LIMIT 1)", "doc_search_docsize"),
        ("doc_search_config", "UPDATE doc_search_config SET v=CAST(v || 'x' AS BLOB) WHERE k=(SELECT k FROM doc_search_config ORDER BY k LIMIT 1)", "doc_search_config"),
    ];
    for (table, sql, commitment) in cases {
        conn.execute_batch("SAVEPOINT fts_shadow_tamper").unwrap();
        let rows_changed = conn.execute(sql, []).unwrap();
        assert_eq!(rows_changed, 1, "{table}");
        let error = verify(&conn).expect_err("tampering must invalidate the stored logical seal");
        assert!(error.to_string().contains(commitment), "{error:#}");
        conn.execute_batch("ROLLBACK TO fts_shadow_tamper; RELEASE fts_shadow_tamper;")
            .unwrap();
        verify(&conn).expect("rolling back tampering restores verification");
    }

    conn.execute_batch("DROP INDEX partial_order_probe; ANALYZE;")
        .unwrap();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, defensive)
        .unwrap();
    verify(&conn).expect("cleanup restores verification");
    let cleanup_root = seal(&conn).expect("cleanup reseals");
    assert_eq!(cleanup_root, expected_root);

    conn.pragma_update(None, "reverse_unordered_selects", true)
        .unwrap();
    let reverse_unordered_root = seal(&conn).expect("reverse unordered selects reseal");
    assert_eq!(reverse_unordered_root, expected_root);

    conn.execute_batch("ANALYZE; VACUUM;").unwrap();
    let vacuum_root = seal(&conn).expect("ANALYZE and VACUUM reseal");
    assert_eq!(vacuum_root, expected_root);
    verify(&conn).expect("the maintained FTS state verifies");
}

#[test]
fn fts_sealing_survives_valid_rebuild_reindex_and_vacuum() {
    let conn = connection();
    search_nodes(&conn, 512);
    conn.execute_batch(
        "INSERT INTO node_search(node_search,rank) VALUES('pgsz',256);
         WITH RECURSIVE nums(x) AS (
             VALUES(1)
             UNION ALL SELECT x+1 FROM nums WHERE x<512
         )
         INSERT INTO node_search(rowid,search_text)
         SELECT x,printf('nodeword%06d shared',x) FROM nums;
         INSERT INTO doc_sections(doc_id,path,section_title,doc_type,content,invariants,referenced_symbols,mtime,size)
         VALUES('doc-1','docs/one.md','One','guide','shared document content','stable invariant','',0,0),
               ('doc-2','docs/two.md','Two','guide','other document content','stable invariant','',0,0);
         INSERT INTO doc_search(rowid,section_title,content,invariants)
         SELECT rowid,section_title,content,invariants FROM doc_sections;
         CREATE VIRTUAL TABLE node_search_vocab USING fts5vocab(node_search,'instance');
         CREATE VIRTUAL TABLE doc_search_vocab USING fts5vocab(doc_search,'instance');
         CREATE INDEX fts_maintenance_probe ON doc_sections(path);",
    )
    .unwrap();

    let node_postings_before = fts_postings(&conn, "node_search_vocab");
    let doc_postings_before = fts_postings(&conn, "doc_search_vocab");
    let initial_root = seal(&conn).unwrap();
    verify(&conn).expect("the original FTS state verifies");

    conn.execute_batch(
        "INSERT INTO node_search(node_search) VALUES('delete-all');
          WITH RECURSIVE nums(x) AS (
              VALUES(1)
              UNION ALL SELECT x+1 FROM nums WHERE x<512
          )
          INSERT INTO node_search(rowid,search_text)
          SELECT x,printf('nodeword%06d shared',x) FROM nums;
         INSERT INTO doc_search(doc_search) VALUES('rebuild');",
    )
    .unwrap();
    let node_postings_after_rebuild = fts_postings(&conn, "node_search_vocab");
    let doc_postings_after_rebuild = fts_postings(&conn, "doc_search_vocab");
    assert_eq!(node_postings_after_rebuild, node_postings_before);
    assert_eq!(doc_postings_after_rebuild, doc_postings_before);

    let rebuilt_root = seal(&conn).expect("the rebuilt FTS state seals");
    assert_eq!(
        rebuilt_root, initial_root,
        "rebuilding identical FTS postings must preserve this fixture's commitment root"
    );
    verify(&conn).expect("the rebuilt FTS state verifies");
    let resealed_rebuild_root = seal(&conn).expect("the rebuilt FTS state reseals");
    assert_eq!(resealed_rebuild_root, rebuilt_root);

    conn.execute_batch("REINDEX; ANALYZE; VACUUM;").unwrap();
    let node_postings_after_maintenance = fts_postings(&conn, "node_search_vocab");
    let doc_postings_after_maintenance = fts_postings(&conn, "doc_search_vocab");
    assert_eq!(
        node_postings_after_maintenance, node_postings_before,
        "maintenance must preserve semantic postings even if shadow storage is repacked"
    );
    assert_eq!(doc_postings_after_maintenance, doc_postings_before);

    let maintained_root = seal(&conn).expect("the maintained FTS state seals");
    assert_eq!(
        maintained_root, rebuilt_root,
        "REINDEX, ANALYZE, and VACUUM must preserve the logical FTS commitment root"
    );
    verify(&conn).expect("the maintained FTS state verifies");
    let resealed_maintenance_root = seal(&conn).expect("the maintained FTS state reseals");
    assert_eq!(resealed_maintenance_root, maintained_root);
}

#[test]
fn owner_language_expression_batches_preserve_repeatable_full_and_partial_roots() {
    let cases = [ExpressionOrder::Ascending, ExpressionOrder::Descending];
    let mut roots = Vec::with_capacity(2);
    for order in cases {
        let sql_order = match order {
            ExpressionOrder::Ascending => "ASC",
            ExpressionOrder::Descending => "DESC",
        };
        let conn = connection();
        let owner = owner_id(&conn, "src/owner_language.rs");
        conn.execute_batch(&format!(
            "WITH RECURSIVE n(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM n WHERE x<900)
             INSERT INTO coverage_expressions(expression)
             SELECT 'owner_expr_'||printf('%04d',x) FROM n ORDER BY x {};",
            sql_order
        ))
        .unwrap();
        let inserted_expressions = conn
            .execute(
                "INSERT INTO coverage_owner_language(path_id,line,expression_id,status,evidence_id,language)
                 SELECT ?1,
                        CAST(substr(expression,12) AS INTEGER)%17,
                        expression_id,
                        CASE CAST(substr(expression,12) AS INTEGER)%3
                            WHEN 0 THEN 'resolved'
                            WHEN 1 THEN 'external'
                            ELSE 'unresolved'
                        END,
                        1,
                        'javascript'
                 FROM coverage_expressions
                 WHERE expression LIKE 'owner_expr_%'",
                [owner],
            )
            .unwrap();
        assert_eq!(inserted_expressions, 901);
        let owner_rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM coverage_owner_language WHERE path_id=?1",
                [owner],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_rows, 901);

        let root = seal(&conn).unwrap();
        let repeat_root = seal(&conn).unwrap();
        assert_eq!(repeat_root, root);

        let selected = BTreeSet::from(["src/owner_language.rs".to_owned()]);
        let partial_root = seal_owners(&conn, Some(&selected)).unwrap();
        assert_eq!(partial_root, root);

        verify_owners(&conn, Some(&selected)).expect("selected owner verifies");
        verify(&conn).expect("full database verifies");

        roots.push(root);
    }
    assert_eq!(roots[0], roots[1]);
}

#[test]
fn manifest_input_digest_is_committed_and_tampering_is_detected() {
    let conn = connection();
    conn.execute(
        "INSERT INTO metadata VALUES('manifest_digest','before')",
        [],
    )
    .unwrap();
    let original_root = seal(&conn).unwrap();
    verify(&conn).expect("the original manifest digest verifies");

    conn.execute(
        "UPDATE metadata SET value='after' WHERE key='manifest_digest'",
        [],
    )
    .unwrap();
    assert!(verify(&conn).is_err());
    let tampered_root = seal(&conn).expect("tampered manifest digest can be resealed");
    assert_ne!(tampered_root, original_root);
}

#[test]
fn coverage_partial_owner_update_matches_full_seal() {
    let conn = connection();
    let selected_owner = owner_id(&conn, "src/owner_a.rs");
    owner_id(&conn, "src/owner_b.rs");
    conn.execute_batch(
        "WITH RECURSIVE n(x) AS (SELECT 0 UNION ALL SELECT x+1 FROM n WHERE x<63)
         INSERT INTO coverage_expressions(expression)
         SELECT 'item_'||printf('%02d',x) FROM n ORDER BY x DESC;
         INSERT OR IGNORE INTO coverage_evidence(evidence) VALUES('λz'),('a'),(''),(char(0)||'suffix');
         INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id)
         SELECT p.path_id,x.expression_id%2,x.expression_id,
             CASE e.evidence_id%4 WHEN 0 THEN 'resolved' WHEN 1 THEN 'external'
                 WHEN 2 THEN 'unresolved' ELSE 'ambiguous' END,e.evidence_id
         FROM path_dictionary p CROSS JOIN coverage_expressions x CROSS JOIN coverage_evidence e
         WHERE p.path LIKE 'src/owner_%';",
    )
    .unwrap();
    let initial_root = seal(&conn).unwrap();
    let selected_leaf_before = coverage_leaf(&conn, "src/owner_a.rs");
    let other_leaf_before = coverage_leaf(&conn, "src/owner_b.rs");
    conn.execute(
        "UPDATE resolution_coverage SET line=line+10 WHERE path_id=?1 AND line=0",
        [selected_owner],
    )
    .unwrap();
    let selected = BTreeSet::from(["src/owner_a.rs".to_owned()]);
    let partial_root = seal_owners(&conn, Some(&selected)).expect("partial owner sealing succeeds");
    let selected_leaf_after = coverage_leaf(&conn, "src/owner_a.rs");
    let other_leaf_after = coverage_leaf(&conn, "src/owner_b.rs");
    assert_ne!(partial_root, initial_root);
    assert_ne!(selected_leaf_after, selected_leaf_before);
    assert_eq!(other_leaf_after, other_leaf_before);

    let full_root_after = seal(&conn).expect("full sealing succeeds after the owner update");
    assert_eq!(full_root_after, partial_root);

    verify_owners(&conn, Some(&selected)).expect("selected owner verification succeeds");
    verify(&conn).expect("full verification succeeds");
}

#[test]
fn coverage_uses_dictionary_values_for_unicode_and_custom_status() {
    let conn = connection();
    let owner = owner_id(&conn, "src/owner_a.rs");
    conn.execute(
        "INSERT INTO coverage_expressions(expression) VALUES('λ.receiver')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO coverage_evidence(evidence) VALUES('line λ: resolved')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO resolution_coverage VALUES(?1,7,
         (SELECT expression_id FROM coverage_expressions WHERE expression='λ.receiver'),
         'custom-status',
         (SELECT evidence_id FROM coverage_evidence WHERE evidence='line λ: resolved'))",
        params![owner],
    )
    .unwrap();
    let original_root = seal(&conn).unwrap();
    let leaf_before = coverage_leaf(&conn, "src/owner_a.rs");
    assert_eq!(leaf_before.len(), 32);

    verify(&conn).expect("dictionary-backed Unicode evidence verifies");

    conn.execute(
        "UPDATE coverage_evidence SET evidence='line λ: changed' WHERE evidence='line λ: resolved'",
        [],
    )
    .unwrap();
    let root_after = seal(&conn).expect("coverage evidence mutation seals");
    assert_ne!(root_after, original_root);
}

#[test]
fn resolution_coverage_rejects_rows_with_missing_dictionary_entries() {
    #[derive(Clone, Copy)]
    enum MissingEntry {
        Path,
        Expression,
        Evidence,
    }

    let cases = [
        (MissingEntry::Path, "path"),
        (MissingEntry::Expression, "expression"),
        (MissingEntry::Evidence, "evidence"),
    ];
    let conn = connection();
    let owner = owner_id(&conn, "src/owner_a.rs");
    conn.execute(
        "INSERT INTO coverage_expressions(expression) VALUES('known.expression')",
        [],
    )
    .unwrap();
    let expression: i64 = conn
        .query_row(
            "SELECT expression_id FROM coverage_expressions WHERE expression='known.expression'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO resolution_coverage VALUES(?1,1,?2,'resolved',1)",
        params![owner, expression],
    )
    .unwrap();
    seal(&conn).expect("canonical coverage row seals before missing dictionary references");
    conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    let orphan = 10_000;
    for (entry, label) in cases {
        let (path_id, expression_id, evidence_id) = match entry {
            MissingEntry::Path => (orphan, expression, 1),
            MissingEntry::Expression => (owner, orphan, 1),
            MissingEntry::Evidence => (owner, expression, orphan),
        };
        conn.execute_batch("SAVEPOINT missing_coverage_dictionary;")
            .unwrap();
        conn.execute(
            "INSERT INTO resolution_coverage VALUES(?1,2,?2,'resolved',?3)",
            params![path_id, expression_id, evidence_id],
        )
        .unwrap();
        let verification = verify(&conn);
        let seal_after_orphan = seal(&conn);
        conn.execute_batch(
            "ROLLBACK TO missing_coverage_dictionary; RELEASE missing_coverage_dictionary;",
        )
        .unwrap();
        assert!(
            verification.is_err(),
            "verify accepted a resolution_coverage row with a missing {label} dictionary entry"
        );
        assert!(
            seal_after_orphan.is_err(),
            "seal accepted a resolution_coverage row with a missing {label} dictionary entry"
        );
    }
}

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
        .install(move || seal(connection))
        .expect("serial sealing produces the reference root");

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

    verify(&conn).expect("serial verification succeeds");

    for thread_count in [1, 2, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .unwrap();
        let connection = &mut conn;
        let full_seal = pool
            .install(move || seal(connection))
            .expect("full sealing succeeds for every pool");
        assert_eq!(
            full_seal, serial_root,
            "full sealing must match the serial root with {thread_count} threads"
        );

        let connection = &mut conn;
        let owner_paths = &owners;
        let owner_seal = pool
            .install(move || seal_owners(connection, Some(owner_paths)))
            .expect("selected-owner sealing succeeds for every pool");
        assert_eq!(
            owner_seal, serial_root,
            "owner sealing must match the serial root with {thread_count} threads"
        );

        let connection = &mut conn;
        pool.install(move || verify(connection))
            .expect("every pool's sealed database verifies");
    }
    {
        let conn = self::connection();
        search_nodes(&conn, 2);
        conn.execute_batch(
            "INSERT INTO node_search(rowid,search_text) VALUES(1,'alpha'),(2,'beta');",
        )
        .unwrap();
        let before = fts_digest_vocab(&conn, "node_search", "term,doc,col,offset").unwrap();
        assert_eq!(
            fts_digest_fastpath(&conn, "node_search").unwrap(),
            Some(before.clone())
        );
        let sizes: Vec<(i64, Vec<u8>)> = conn
            .prepare("SELECT id,sz FROM node_search_docsize ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        seal(&conn).unwrap();
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
        let after = fts_digest_vocab(&conn, "node_search", "term,doc,col,offset").unwrap();
        assert_eq!(fts_digest_fastpath(&conn, "node_search").unwrap(), None);
        assert_ne!(before, after);
        assert!(verify(&conn).is_err());
        conn.execute_batch("INSERT INTO node_search(node_search) VALUES('optimize');")
            .unwrap();
        assert_eq!(
            fts_digest_fastpath(&conn, "node_search").unwrap(),
            Some(after.clone())
        );
    }
    {
        let dir = acdd_core::fs::ScopedTempDir::create("acdd-fts-segment").unwrap();
        let path = dir.path().join("fts.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_DDL).unwrap();
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
            (FtsDomain::NodeSearch, "node_search"),
            (FtsDomain::DocSearch, "doc_search"),
        ] {
            let table = domain.as_str();
            assert_eq!(table, expected);
            assert_eq!(
                fts_digest_fastpath(&conn, table).unwrap(),
                Some(fts_digest_vocab(&conn, table, "term,doc,col,offset").unwrap()),
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
            assert_eq!(fts_digest_fastpath(&conn, "node_search").unwrap(), None);
            conn.execute_batch("ROLLBACK TO config_case; RELEASE config_case")
                .unwrap();
        }
        seal(&conn).unwrap();
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
            let fresh = Connection::open(&path).unwrap();
            let result = fts_digest_fastpath(&fresh, "node_search");
            if fastpath_errors {
                assert!(result.is_err(), "{sql}");
            } else {
                assert_eq!(result.unwrap(), None, "{sql}");
            }
            assert!(verify(&fresh).is_err(), "{sql}");
            drop(fresh);
            conn.execute(restore, [original]).unwrap();
        }
    }
}
