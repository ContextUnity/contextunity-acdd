//! Incremental delta persistence operations.

use anyhow::{bail, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

use super::{
    batch::BorrowedSqlValue,
    dictionary::PathDictionaryCache,
    ingest::{persist_delta_files, persist_graph, CachedNodePath, PersistGraphParams},
};
use crate::db::commitments::{self, MetadataKey};
use crate::models::{compact_graph::CompactGraph, stable_hash64, FileEntry};
use crate::typed_facts::TypedFacts;

fn cached(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize> {
    conn.prepare_cached(sql)?.execute(params)
}

fn node_search_rows(facts: Option<&TypedFacts>) -> Vec<(i64, String)> {
    let Some(facts) = facts else {
        return Vec::new();
    };
    let mut rows = Vec::with_capacity(facts.nodes.len());
    for node in &facts.nodes {
        let mut text = String::new();
        super::ingest::append_node_search_text(&mut text, node, facts);
        rows.push((stable_hash64(&node.id), text));
    }
    rows.sort_unstable();
    rows
}

fn node_search_changed(
    conn: &Connection,
    owner_id: i64,
    path: &str,
    new: Option<&TypedFacts>,
) -> Result<bool> {
    let nodes: Vec<(i64, String)> = conn
        .prepare_cached(
            "SELECT node_hash,id FROM nodes WHERE owner_path_id=?1 ORDER BY node_hash,id",
        )?
        .query_map([owner_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT facts_blob FROM local_facts WHERE path=?1",
            [path],
            |r| r.get(0),
        )
        .optional()?;
    let old = match blob {
        Some(blob) => match crate::typed_facts::decode_facts(&blob) {
            Ok(facts) => Some(facts),
            Err(_) => return Ok(true),
        },
        None if !nodes.is_empty() => return Ok(true),
        None => None,
    };
    let mut old_nodes: Vec<_> = old
        .iter()
        .flat_map(|f| &f.nodes)
        .map(|n| (stable_hash64(&n.id), n.id.clone()))
        .collect();
    old_nodes.sort_unstable();
    Ok(nodes != old_nodes || node_search_rows(old.as_ref()) != node_search_rows(new))
}

fn doc_search_changed(conn: &Connection, path: &str, new: Option<&TypedFacts>) -> Result<bool> {
    let old: Vec<(String, String, String, String, String)> = conn.prepare_cached("SELECT doc_id,path,section_title,content,invariants FROM doc_sections WHERE path=?1 ORDER BY doc_id,path,section_title,content,invariants")?
        .query_map([path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut rows = Vec::new();
    if let Some(facts) = new {
        for d in &facts.docs {
            rows.push((
                d.doc_id.clone(),
                d.path.clone(),
                d.section_title.clone(),
                d.content.clone(),
                serde_json::to_string(&d.invariants)?,
            ));
        }
    }
    rows.sort_unstable();
    Ok(old != rows)
}

fn delete_file(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    path: &str,
    dirty_fts: &commitments::FtsDirtySet,
) -> Result<()> {
    let owner_id = path_cache.get_or_insert(tx, path)?;
    if dirty_fts.node_search {
        cached(
        tx,
        "DELETE FROM node_search WHERE rowid IN(SELECT node_hash FROM nodes WHERE owner_path_id=?1)",
        [owner_id],
    )?;
    }
    if dirty_fts.doc_search {
        cached(
        tx,
        "INSERT INTO doc_search(doc_search,rowid,section_title,content,invariants)SELECT 'delete',rowid,section_title,content,invariants FROM doc_sections WHERE path=?1",
        [path],
    )?;
    }
    cached(
        tx,
        "UPDATE shared_keys SET key=(SELECT qualname FROM nodes WHERE node_id=shared_keys.key) WHERE typeof(key)='integer' AND key IN(SELECT node_id FROM nodes WHERE owner_path_id=?1)",
        [owner_id],
    )?;
    cached(tx, "DELETE FROM nodes WHERE owner_path_id=?1", [owner_id])?;
    for (table, column) in [
        ("source_inventory", "path"),
        ("files", "path"),
        ("local_facts", "path"),
        ("shared_owners", "owner_id"),
        ("errors", "path"),
        ("doc_sections", "path"),
    ] {
        if table == "doc_sections" && !dirty_fts.doc_search {
            continue;
        }
        let value = if column == "owner_id" {
            rusqlite::types::Value::Integer(owner_id)
        } else {
            rusqlite::types::Value::Text(path.to_owned())
        };
        cached(
            tx,
            &format!("DELETE FROM {table} WHERE {column}=?1"),
            [value],
        )?;
    }
    Ok(())
}

/// Inputs for delta SQLite database updates.
pub struct DeltaPersistenceInputs<'a> {
    /// Target SQLite database path.
    pub db: &'a Path,
    /// Workspace root directory.
    pub root: &'a Path,
    /// Expected previous output root commitment hash.
    pub expected: &'a str,
    /// Admitted database generation identity.
    pub admitted_identity: crate::db::cache::Identity,
    /// Workspace file entry inventory.
    pub entries: &'a [FileEntry],
    /// Affected file paths.
    pub affected: &'a BTreeSet<String>,
    /// Files whose facts must be replaced in persistence.
    pub replace_files: &'a BTreeSet<String>,
    /// Extracted typed facts.
    pub facts: &'a BTreeMap<String, TypedFacts>,
    /// Linked compact graph.
    pub graph: &'a CompactGraph,
    /// Manifest commitment input digest.
    pub manifest_digest: &'a str,
}

/// Timings and output metadata resulting from delta persistence.
pub struct DeltaPersistResult {
    /// Time spent deleting affected rows in milliseconds.
    pub delete_ms: f64,
    /// Time spent persisting file entries in milliseconds.
    pub files_ms: f64,
    /// Time spent persisting graph nodes and edges in milliseconds.
    pub graph_ms: f64,
    /// Time spent computing aggregate facts in milliseconds.
    pub aggregate_ms: f64,
    /// Time spent sealing the database in milliseconds.
    pub seal_ms: f64,
    /// Time spent verifying the seal in milliseconds.
    pub verify_ms: f64,
    /// Time spent committing the transaction in milliseconds.
    pub commit_ms: f64,
    /// Total persistence time in milliseconds.
    pub persist_ms: f64,
    /// Resulting output root commitment hash.
    pub output_root: String,
    /// Whether verification cache was written.
    pub verification_cache: bool,
    /// Count of rewritten files.
    pub rewritten_files: usize,
}

/// Persists incremental delta changes into SQLite database.
pub fn persist_delta(
    inputs: DeltaPersistenceInputs<'_>,
    check_snapshot: impl FnOnce(f64, f64, f64, f64) -> Result<()>,
) -> Result<DeltaPersistResult> {
    let DeltaPersistenceInputs {
        db,
        root,
        expected,
        admitted_identity,
        entries,
        affected,
        replace_files,
        facts,
        graph,
        manifest_digest,
    } = inputs;

    let persisting = Instant::now();
    let current_identity = crate::db::cache::identity(db)?;
    if current_identity != admitted_identity {
        bail!(
            "database changed during delta admission ({})",
            crate::db::cache::describe_diff(&admitted_identity, &current_identity)
        );
    }
    let open_path = acdd_core::fs::confined_database_open_path(db)?;
    let mut conn = Connection::open_with_flags(
        &open_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.set_prepared_statement_cache_capacity(128);
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-64000; PRAGMA mmap_size=268435456;",
    )?;
    let precheck = commitments::read_metadata(&conn, MetadataKey::OutputRoot)?;
    if precheck != expected {
        bail!("generation changed during delta admission");
    }
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = commitments::read_metadata(&tx, MetadataKey::OutputRoot)?;
    if current != expected {
        bail!("generation changed during delta admission");
    }
    let deleting = Instant::now();
    let mut path_cache = PathDictionaryCache::new();
    path_cache.load_from_db(&tx)?;
    let mut dirty_fts = commitments::FtsDirtySet {
        node_search: false,
        doc_search: false,
    };
    for path in replace_files {
        let owner_id = path_cache.get_or_insert(&tx, path)?;
        if !dirty_fts.node_search {
            dirty_fts.node_search =
                node_search_changed(&tx, owner_id, path, facts.get(path)).unwrap_or(true);
        }
        if !dirty_fts.doc_search {
            dirty_fts.doc_search = doc_search_changed(&tx, path, facts.get(path))?;
        }
    }
    let orphan_table = super::temp_table::TempTableGuard::new(
        &tx,
        "candidate_orphan_evidence",
        "(evidence_id INTEGER PRIMARY KEY) WITHOUT ROWID",
    )?;
    let changed_table = super::temp_table::TempTableGuard::new(
        &tx,
        "changed_edge_keys",
        "(src_hash INTEGER,dst_hash INTEGER,kind TEXT,PRIMARY KEY(src_hash,dst_hash,kind))",
    )?;
    for path in affected {
        let owner_id = path_cache.get_or_insert(&tx, path)?;
        cached(
            &tx,
            "INSERT OR IGNORE INTO changed_edge_keys SELECT src_hash,dst_hash,kind FROM edge_occurrences WHERE owner_id=?1",
            [owner_id],
        )?;
    }
    let mut changed_edge_batch = changed_table
        .batch::<3, BorrowedSqlValue<'_>>("INSERT OR IGNORE INTO changed_edge_keys VALUES")?;
    for e in &graph.edges {
        changed_edge_batch.push([
            BorrowedSqlValue::Integer(stable_hash64(&e.src)),
            BorrowedSqlValue::Integer(stable_hash64(&e.dst)),
            BorrowedSqlValue::Text(e.kind.as_str()),
        ])?;
    }
    changed_edge_batch.flush()?;
    drop(changed_edge_batch);
    let replaced_table = super::temp_table::TempTableGuard::new(
        &tx,
        "replaced_nodes",
        "(id TEXT PRIMARY KEY,node_hash INTEGER NOT NULL)",
    )?;
    tx.execute("INSERT OR IGNORE INTO replaced_nodes SELECT n.id,n.node_hash FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE owner.path IN(SELECT value FROM json_each(?1))", [serde_json::to_string(&replace_files)?])?;
    let candidates_table = super::entrypoints::prepare_candidates(&tx)?;
    super::entrypoints::add_delta_candidates(&tx, replace_files)?;
    let mut commitment_owners = affected.clone();
    commitment_owners.extend(
        tx.prepare_cached("SELECT DISTINCT p.path FROM edges e JOIN path_dictionary p ON p.path_id=e.path_id WHERE (e.src_hash,e.dst_hash,e.kind)IN(SELECT src_hash,dst_hash,kind FROM changed_edge_keys)")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    for column in ["evidence_id", "confidence_id"] {
        tx.execute(&format!("INSERT OR IGNORE INTO candidate_orphan_evidence SELECT {column} FROM edges WHERE {column} IS NOT NULL AND (src_hash,dst_hash,kind) IN (SELECT src_hash,dst_hash,kind FROM changed_edge_keys)"), [])?;
    }
    tx.execute("DELETE FROM edges WHERE (src_hash,dst_hash,kind)IN(SELECT src_hash,dst_hash,kind FROM changed_edge_keys)", [])?;
    for path in affected {
        let owner_id = path_cache.get_or_insert(&tx, path)?;
        for (table, owner) in [
            ("edge_occurrences", "owner_id"),
            ("resolution_coverage", "path_id"),
        ] {
            for column in ["evidence_id", "confidence_id"] {
                if table == "resolution_coverage" && column == "confidence_id" {
                    continue;
                }
                cached(&tx, &format!("INSERT OR IGNORE INTO candidate_orphan_evidence SELECT {column} FROM {table} WHERE {owner}=?1 AND {column} IS NOT NULL"), [owner_id])?;
            }
        }
        for (table, column) in [
            ("edge_occurrences", "owner_id"),
            ("dependencies", "owner_id"),
            ("resolution_coverage", "path_id"),
            ("coverage_owner_language", "path_id"),
            ("coverage_language_counts", "path_id"),
        ] {
            cached(
                &tx,
                &format!("DELETE FROM {table} WHERE {column}=?1"),
                [owner_id],
            )?;
        }
    }
    for path in replace_files {
        delete_file(&tx, &mut path_cache, path, &dirty_fts)?;
    }
    let delete_ms = deleting.elapsed().as_secs_f64() * 1000.;
    let changed_entries: Vec<_> = entries
        .iter()
        .filter(|f| replace_files.contains(&f.path))
        .cloned()
        .collect();
    let files_writing = Instant::now();
    persist_delta_files(&tx, &mut path_cache, &changed_entries, facts, dirty_fts)?;
    let files_ms = files_writing.elapsed().as_secs_f64() * 1000.;
    let mut node_paths: hashbrown::HashMap<std::borrow::Cow<'_, str>, Option<CachedNodePath>> =
        hashbrown::HashMap::with_capacity(
            facts
                .values()
                .map(|file_facts| file_facts.nodes.len())
                .sum(),
        );
    let mut shared_paths = hashbrown::HashMap::<&str, std::sync::Arc<str>>::new();
    for file_facts in facts.values() {
        for node in &file_facts.nodes {
            let shared_path = shared_paths
                .entry(node.path.as_str())
                .or_insert_with(|| std::sync::Arc::<str>::from(node.path.as_str()));
            node_paths.insert(
                std::borrow::Cow::Borrowed(node.id.as_str()),
                Some(CachedNodePath::from_shared(
                    &node.id,
                    std::sync::Arc::clone(shared_path),
                )),
            );
        }
    }
    drop(shared_paths);
    let graph_writing = Instant::now();
    let graph_result = persist_graph(
        &tx,
        &mut path_cache,
        &mut node_paths,
        PersistGraphParams {
            graph,
            facts,
            dependency_owners: Some(affected),
            materialize_edges: false,
        },
        Some(entries),
    );
    drop(node_paths);
    graph_result?;
    let graph_ms = graph_writing.elapsed().as_secs_f64() * 1000.;
    let aggregating = Instant::now();
    let dangling: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM replaced_nodes r WHERE NOT EXISTS(SELECT 1 FROM nodes n WHERE n.id=r.id AND n.node_hash=r.node_hash) AND EXISTS(SELECT 1 FROM edges e WHERE e.src_hash=r.node_hash OR e.dst_hash=r.node_hash))", [], |r| r.get(0))?;
    if dangling {
        bail!("delta leaves an edge with a missing endpoint; rebuild index");
    }
    tx.execute(
        "WITH ranked AS (
            SELECT e.*,e.owner_id AS path_id,p.path AS owner_path,
                   row_number() OVER edge_group AS representative,
                   count(*) OVER edge_group AS occurrences
            FROM changed_edge_keys k JOIN edge_occurrences e
              ON k.src_hash=e.src_hash AND k.dst_hash=e.dst_hash AND k.kind=e.kind
            JOIN path_dictionary p ON p.path_id=e.owner_id
            WINDOW edge_group AS (
                PARTITION BY e.src_hash,e.dst_hash,e.kind ORDER BY p.path,e.ordinal
                ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
            )
        )
        INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count)
        SELECT src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrences
        FROM ranked WHERE representative=1",
        [],
    )?;
    for path in affected {
        let owner_id = path_cache.get_or_insert(&tx, path)?;
        cached(&tx, "DELETE FROM candidate_orphan_evidence WHERE evidence_id IN (SELECT evidence_id FROM resolution_coverage WHERE path_id=?1 UNION ALL SELECT evidence_id FROM edges WHERE path_id=?1 UNION ALL SELECT confidence_id FROM edges WHERE path_id=?1 UNION ALL SELECT evidence_id FROM edge_occurrences WHERE owner_id=?1 UNION ALL SELECT confidence_id FROM edge_occurrences WHERE owner_id=?1)", [owner_id])?;
    }
    let candidates: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM candidate_orphan_evidence)",
        [],
        |r| r.get(0),
    )?;
    if candidates {
        tx.execute_batch("DELETE FROM candidate_orphan_evidence WHERE EXISTS (SELECT 1 FROM resolution_coverage r WHERE r.evidence_id=candidate_orphan_evidence.evidence_id) OR EXISTS (SELECT 1 FROM edges e WHERE e.evidence_id=candidate_orphan_evidence.evidence_id OR e.confidence_id=candidate_orphan_evidence.evidence_id) OR EXISTS (SELECT 1 FROM edge_occurrences o WHERE o.evidence_id=candidate_orphan_evidence.evidence_id OR o.confidence_id=candidate_orphan_evidence.evidence_id); DELETE FROM coverage_evidence WHERE evidence_id IN (SELECT evidence_id FROM candidate_orphan_evidence);")?;
    }
    drop(orphan_table);
    let dangling_changed: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM changed_edge_keys k
             JOIN edges e ON e.src_hash=k.src_hash AND e.kind=k.kind AND e.dst_hash=k.dst_hash
             LEFT JOIN nodes s ON s.node_hash=e.src_hash
             LEFT JOIN nodes t ON t.node_hash=e.dst_hash
             WHERE s.node_hash IS NULL OR t.node_hash IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if dangling_changed {
        bail!("delta leaves an edge with a missing endpoint; rebuild index");
    }
    super::entrypoints::add_delta_candidates(&tx, replace_files)?;
    commitment_owners.extend(super::entrypoints::project(&tx)?);
    commitment_owners.extend(
        tx.prepare_cached("SELECT DISTINCT p.path FROM edges e JOIN path_dictionary p ON p.path_id=e.path_id WHERE (e.src_hash,e.dst_hash,e.kind)IN(SELECT src_hash,dst_hash,kind FROM changed_edge_keys)")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    let aggregate_ms = aggregating.elapsed().as_secs_f64() * 1000.;
    commitments::write_metadata(
        &tx,
        MetadataKey::CorpusHash,
        &acdd_core::format::sha256_serde_json_hex(entries)?,
    )?;
    commitments::write_metadata(
        &tx,
        MetadataKey::InventorySnapshot,
        &serde_json::to_string(entries)?,
    )?;
    commitments::write_metadata(&tx, MetadataKey::ManifestDigest, manifest_digest)?;
    let persist_ms = persisting.elapsed().as_secs_f64() * 1000.;
    let sealing = Instant::now();
    let seal = commitments::seal_owners_with_dirty_fts(&tx, Some(&commitment_owners), dirty_fts)?;
    let seal_ms = sealing.elapsed().as_secs_f64() * 1000.;
    let verifying = Instant::now();
    commitments::verify_delta_seal(&tx, &seal)?;

    let verify_ms = verifying.elapsed().as_secs_f64() * 1000.;
    check_snapshot(persist_ms, seal_ms, verify_ms, verify_ms)?;

    let committing = Instant::now();
    drop((candidates_table, replaced_table, changed_table));
    tx.commit()?;
    crate::db::cache::checkpoint(&conn, true)
        .map_err(|err| anyhow::anyhow!("delta committed but checkpoint failed: {err:#}"))?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=DELETE;", [], |r| r.get(0))
        .map_err(|err| {
            anyhow::anyhow!("delta committed but DELETE journal_mode transition failed: {err:#}")
        })?;
    anyhow::ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "delta committed but journal_mode remained '{mode}'"
    );
    let commit_ms = committing.elapsed().as_secs_f64() * 1000.;
    drop(conn);
    let verification_cache = match crate::db::cache::publish_verified(db, root, &seal) {
        Ok(true) => true,
        Ok(false) => {
            eprintln!(
                "delta committed; verified receipt publication refused for {}",
                db.display()
            );
            false
        }
        Err(err) => {
            eprintln!(
                "delta committed; verified receipt publication failed for {}: {err:#}",
                db.display()
            );
            false
        }
    };

    Ok(DeltaPersistResult {
        delete_ms,
        files_ms,
        graph_ms,
        aggregate_ms,
        seal_ms,
        verify_ms,
        commit_ms,
        persist_ms,
        output_root: seal,
        verification_cache,
        rewritten_files: changed_entries.len(),
    })
}
