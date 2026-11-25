//! SQLite persistence and indexing operations for ACDD.

use crate::db::commitments;
use crate::db::schema::SCHEMA_DDL;
use crate::models::compact_graph::CompactGraph;
use crate::models::*;
use crate::typed_facts::{encode_facts, CatalogStoragePayload, SymbolCatalog, TypedFacts};
use acdd_core::config::{self, Adapter};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs::File, path::Path, time::Instant};

mod batch;
/// Database extraction cache facilities.
pub mod cache;
pub mod delta;
#[doc(hidden)]
pub use delta as delta_persist;
mod dictionary;
pub(crate) mod entrypoints;
mod ingest;
mod node_domain;
mod seal;
pub mod temp_table;

use cache::PreparedCatalogStorage;
use dictionary::*;
use ingest::*;

mod payload_store;
pub use ingest::LocalFactsBatchWriter;
pub use payload_store::PayloadStore;
use payload_store::StagedPreparedStorage;

/// Staged payloads for an indexed file entry.
#[derive(Debug)]
pub struct StagedFilePayload {
    /// Prepared storage rows, when available.
    pub storage_rows: Option<StagedPreparedStorage>,
    /// Commitment leaf entry for the streamed `local_facts` row, when pre-persisted.
    pub local_facts_leaf: Option<(String, String)>,
}

/// Computes the deterministic commitment leaf entry for a file's compressed local facts blob.
pub fn local_facts_leaf(file: &FileEntry, facts_blob: &[u8]) -> Result<(String, String)> {
    node_domain::local_facts_leaf(
        &file.path,
        &file.digest,
        &file.language,
        is_test(&file.path),
        facts_blob,
    )
}

fn decode_prepared_storage(bytes: &[u8]) -> Result<PreparedCatalogStorage> {
    cache::decode_prepared_catalog_storage_payload(bytes)
}

fn non_empty_detail(value: &Value) -> bool {
    !value.is_null()
        && !value.as_str().is_some_and(str::is_empty)
        && !value.as_array().is_some_and(Vec::is_empty)
}

/// Prepares delta catalog storage rows for a file using extracted typed facts.
pub fn prepare_catalog_storage_with_facts(
    file: &FileEntry,
    facts: &TypedFacts,
) -> Result<PreparedCatalogStorage> {
    let mut search_texts = cache::PackedStrings::default();
    let mut node_docs = Vec::new();
    for (index, node) in facts.nodes.iter().enumerate() {
        search_texts.append_with(|buf| append_node_search_text(buf, node, facts))?;
        if let Some(details) = node.details.as_object() {
            let doc = details
                .get(NodeDetailKey::DOC)
                .filter(|value| non_empty_detail(value))
                .cloned();
            let docstring = details
                .get(NodeDetailKey::DOCSTRING)
                .filter(|value| non_empty_detail(value))
                .cloned();
            if doc.is_some() || docstring.is_some() {
                node_docs.push(cache::NodeDocumentation {
                    index: u32::try_from(index).context("node index exceeds u32")?,
                    doc,
                    docstring,
                });
            }
        }
    }
    let doc_bodies = facts
        .docs
        .iter()
        .map(|doc| (doc.content.clone(), doc.invariants.clone()))
        .collect();
    let reference_keys = facts
        .reference_keys()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let leaves = seal::file_domain_leaves(file, facts, &reference_keys)?
        .into_iter()
        .map(|(domain, leaves)| (domain.as_str().to_owned(), leaves))
        .collect();
    Ok(PreparedCatalogStorage {
        node_docs,
        doc_bodies,
        search_texts,
        reference_keys,
        leaves,
    })
}

/// Inputs supplied to pipeline index persistence.
#[derive(Clone, Copy)]
pub struct BuildInputs<'a> {
    /// Manifest digest across workspace dependencies.
    pub manifest_digest: &'a str,
    /// Allocated memory budget in bytes.
    pub memory_budget: u64,
    /// Whether schema DDL tables have already been prepared on the connection.
    pub schema_ready: bool,
}

/// Prepares pipeline SQLite schema with appropriate PRAGMAs and tables.
pub fn prepare_pipeline_schema(
    conn: &mut Connection,
    memory_budget: u64,
    test_consumer_failure: bool,
) -> Result<()> {
    let cache_kb = sqlite_cache_size_kib(memory_budget);
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-{cache_kb}; PRAGMA cache_spill=ON;"
    ))?;
    let (tables, _indexes) = SCHEMA_DDL
        .split_once("CREATE INDEX")
        .context("schema indexes missing")?;
    conn.execute_batch(tables)?;
    if test_consumer_failure {
        conn.execute_batch(
            "CREATE TEMP TRIGGER pipeline_test_consumer_failure
             BEFORE INSERT ON main.source_inventory
             WHEN NEW.path='src/unit_00.rs'
             BEGIN SELECT RAISE(ABORT,'injected pipeline SQLite consumer failure'); END;",
        )?;
    }
    Ok(())
}

pub(crate) fn sqlite_cache_size_kib(memory_budget: u64) -> usize {
    let raw = memory_budget / 8 / 1024;
    raw.clamp(4_096, 32_768) as usize
}

/// Inputs and mutable staging state for [`populate_owned`].
pub struct PopulateOwnedParams<'a> {
    pub root: &'a Path,
    pub adapter: &'a Adapter,
    pub entries: &'a [FileEntry],
    pub facts: BTreeMap<String, TypedFacts>,
    pub graph: &'a CompactGraph,
    pub build_inputs: BuildInputs<'a>,
    pub staged_payloads: &'a mut BTreeMap<String, StagedFilePayload>,
    pub payload_spool_path: Option<&'a Path>,
}

fn load_composed_sources(
    tx: &Connection,
    facts: &BTreeMap<String, TypedFacts>,
) -> Result<BTreeMap<String, CatalogStoragePayload>> {
    let source_paths = facts
        .values()
        .flat_map(|facts| &facts.nodes)
        .filter(|node| node.detail_get(NodeDetailKey::INCLUDED_FROM).is_some())
        .filter_map(|node| node.detail_entry(NodeDetailKey::SOURCE_PATH).as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut sources = BTreeMap::new();
    if source_paths.is_empty() {
        return Ok(sources);
    }
    let mut stmt = tx.prepare_cached("SELECT facts_blob FROM local_facts WHERE path=?1")?;
    for path in source_paths {
        let encoded_facts: Vec<u8> = stmt
            .query_row([path], |row| row.get(0))
            .with_context(|| format!("missing composed source local_facts for {path}"))?;
        sources.insert(path.to_owned(), catalog_payload(path, &encoded_facts)?);
    }
    Ok(sources)
}

#[allow(clippy::too_many_arguments)]
fn load_staged_catalog_chunk(
    chunk_tx: &Connection,
    chunk: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    staged_payloads: &mut BTreeMap<String, StagedFilePayload>,
    payload_spool: Option<&File>,
    composed_sources: &BTreeMap<String, CatalogStoragePayload>,
    logical_commitments: &mut BTreeMap<&'static str, BTreeMap<String, String>>,
    restore_payload_ms: &mut f64,
) -> Result<Vec<LoadedCatalogFile>> {
    let mut db_blobs = hashbrown::HashMap::new();
    let mut staged = Vec::with_capacity(chunk.len());
    for file in chunk {
        let lean = facts.get(&file.path).context("missing catalog file")?;
        let payload = staged_payloads
            .remove(&file.path)
            .context("missing staged file payload")?;
        if payload.storage_rows.is_none() {
            let blob: Vec<u8> = chunk_tx
                .prepare_cached("SELECT facts_blob FROM local_facts WHERE path=?1")?
                .query_row([&file.path], |row| row.get(0))
                .with_context(|| format!("missing local_facts blob for {}", file.path))?;
            db_blobs.insert(file.path.as_str(), blob);
        }
        staged.push((file, lean, payload));
    }
    let spool = payload_spool.context("missing staged payload spool")?;
    let prepared = staged
        .into_par_iter()
        .map(|(file, lean, staged)| {
            let restoring = Instant::now();
            let (storage, local_facts_leaf, updated_local_facts, elapsed_ms) =
                if let Some(storage) = staged.storage_rows {
                    let decoded = match storage {
                        StagedPreparedStorage::InMemory(storage) => (*storage, 0.0),
                        StagedPreparedStorage::Spilled(slice) => {
                            let bytes = payload_store::read_spool(spool, slice)?;
                            (
                                decode_prepared_storage(&bytes)?,
                                restoring.elapsed().as_secs_f64() * 1000.,
                            )
                        }
                    };
                    let leaf = staged
                        .local_facts_leaf
                        .context("missing staged local_facts leaf")?;
                    (decoded.0, leaf, None, decoded.1)
                } else {
                    let encoded_facts = db_blobs
                        .get(file.path.as_str())
                        .context("missing composed local_facts blob")?;
                    let full = restore_catalog_file(file, lean, encoded_facts, composed_sources)?;
                    let encoded = encode_facts(&full)?;
                    let leaf = local_facts_leaf(file, &encoded)?;
                    let storage = prepare_catalog_storage_with_facts(file, &full)?;
                    (
                        storage,
                        leaf,
                        Some(encoded),
                        restoring.elapsed().as_secs_f64() * 1000.,
                    )
                };
            Ok((
                LoadedCatalogFile {
                    node_docs: storage.node_docs,
                    doc_bodies: storage.doc_bodies,
                    local_facts_leaf,
                    updated_local_facts,
                    search_texts: storage.search_texts,
                    reference_keys: storage.reference_keys,
                },
                storage.leaves,
                elapsed_ms,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut loaded = Vec::with_capacity(prepared.len());
    for (file, mut leaves, elapsed_ms) in prepared {
        for domain in ["doc_sections", "shared_owners"] {
            logical_commitments
                .get_mut(domain)
                .context("missing storage commitment domain")?
                .extend(
                    leaves
                        .remove(domain)
                        .context("missing prepared commitment domain")?,
                );
        }
        *restore_payload_ms += elapsed_ms;
        loaded.push(file);
    }
    Ok(loaded)
}

/// Populates an index with staged payloads.
pub fn populate_owned(conn: &mut Connection, params: PopulateOwnedParams<'_>) -> Result<Value> {
    let PopulateOwnedParams {
        root,
        adapter,
        entries,
        mut facts,
        graph,
        build_inputs,
        staged_payloads,
        payload_spool_path,
    } = params;
    let streamed_payloads = !staged_payloads.is_empty();
    let payload_spool = if streamed_payloads {
        Some(File::open(
            payload_spool_path.context("missing staged payload spool path")?,
        )?)
    } else {
        None
    };
    let cache_kb = sqlite_cache_size_kib(build_inputs.memory_budget);
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-{cache_kb}; PRAGMA cache_spill=ON;"
    ))?;
    conn.set_prepared_statement_cache_capacity(128);
    let (tables, indexes) = SCHEMA_DDL
        .split_once("CREATE INDEX")
        .context("schema indexes missing")?;
    if !build_inputs.schema_ready {
        conn.execute_batch(tables)?;
    }
    {
        let mut projection_timings = seal::ProjectionTimings::default();
        let mut logical_commitments = BTreeMap::from([
            ("nodes", BTreeMap::new()),
            ("doc_sections", BTreeMap::new()),
            ("shared_owners", BTreeMap::new()),
        ]);
        let mut seal_projection_ms = 0.0;
        let writing = Instant::now();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !build_inputs.schema_ready {
            cached(
                &tx,
                "INSERT INTO node_search(node_search) VALUES('delete-all')",
                [],
            )?;
            cached(
                &tx,
                "INSERT INTO doc_search(doc_search) VALUES('delete-all')",
                [],
            )
            .ok();
            cached(&tx, "DELETE FROM shared_keys", [])?;
            for table in commitments::DOMAINS {
                cached(&tx, &format!("DELETE FROM {table}"), [])?;
            }
            cached(&tx, "DELETE FROM path_dictionary", [])?;
            cached(&tx, "DELETE FROM domain_commitments", [])?;
            cached(&tx, "DELETE FROM coverage_evidence", [])?;
            cached(&tx, "DELETE FROM coverage_expressions", [])?;
        }
        use commitments::MetadataKey;
        for (k, v) in [
            (
                MetadataKey::SchemaVersion,
                config::ENGINE_SCHEMA_VERSION.to_owned(),
            ),
            (
                MetadataKey::IndexSemanticsVersion,
                crate::models::INDEX_SEMANTICS_VERSION.to_owned(),
            ),
            (
                MetadataKey::IndexerEngine,
                "contextunity-acdd-rust".to_owned(),
            ),
            (
                MetadataKey::WorkspaceRoot,
                root.to_string_lossy().into_owned(),
            ),
            (
                MetadataKey::Adapter,
                config::policy(root, adapter).to_string(),
            ),
            (
                MetadataKey::AdapterVersion,
                adapter.adapter_version.clone().unwrap_or_default(),
            ),
            (MetadataKey::AdapterDigest, adapter.digest.clone()),
            (
                MetadataKey::ManifestDigest,
                build_inputs.manifest_digest.to_owned(),
            ),
            (
                MetadataKey::AdapterLinkedWorkspaces,
                serde_json::to_string(&adapter.linked_workspaces)?,
            ),
            (
                MetadataKey::InventorySnapshot,
                serde_json::to_string(entries)?,
            ),
            (
                MetadataKey::CorpusHash,
                acdd_core::format::sha256_serde_json_hex(entries)?,
            ),
        ] {
            commitments::write_metadata(&tx, k, &v)?;
        }
        let mut path_cache = PathDictionaryCache::new();
        let all_paths = entries
            .iter()
            .map(|e| e.path.as_str())
            .chain(graph.edges.iter().map(|e| e.path.as_ref()))
            .chain(graph.coverage.iter().map(|c| c.path.as_ref()))
            .chain(
                entries
                    .iter()
                    .filter_map(|entry| facts.get(&entry.path))
                    .flat_map(|file_facts| file_facts.nodes.iter().map(|node| node.path.as_str())),
            );
        let t_bulk = Instant::now();
        path_cache.bulk_insert(&tx, all_paths)?;
        let bulk_paths_ms = t_bulk.elapsed().as_secs_f64() * 1000.;
        let t_files = Instant::now();
        let mut restore_payload_ms = 0.0;
        let composed_sources = if streamed_payloads {
            load_composed_sources(&tx, &facts)?
        } else {
            BTreeMap::new()
        };
        let graph_ranks = entrypoints::cold_ranks(graph, &facts);
        if streamed_payloads {
            for (path, file_facts) in &mut facts {
                if staged_payloads
                    .get(path)
                    .is_some_and(|staged| staged.storage_rows.is_none())
                {
                    continue;
                }
                prune_facts_for_stage(file_facts, true);
            }
        }
        let file_timings = if streamed_payloads {
            persist_catalog_files(
                &tx,
                &mut path_cache,
                entries,
                &facts,
                &mut |chunk_tx, chunk, facts| {
                    load_staged_catalog_chunk(
                        chunk_tx,
                        chunk,
                        facts,
                        staged_payloads,
                        payload_spool.as_ref(),
                        &composed_sources,
                        &mut logical_commitments,
                        &mut restore_payload_ms,
                    )
                },
                &graph_ranks,
                build_inputs.schema_ready,
            )?
        } else {
            persist_files_with_ranks(&tx, &mut path_cache, entries, &facts, Some(&graph_ranks))?
        };
        drop(graph_ranks);
        logical_commitments.insert("nodes", file_timings.node_leaves);
        drop(composed_sources);
        let node_id = file_timings.node_id;
        let node_row_count = file_timings.node_row_count;
        let node_row_insert_ms = file_timings
            .node_row_insert_ms
            .context("cold file persistence must report node row insertion time")?;
        let docs_count = file_timings.docs_count;
        let fts_insert_ms = file_timings.fts_insert_ms;
        let encode_facts_ms = file_timings.encode_facts_ms;
        let local_facts_batch_rows = file_timings.local_facts_batch_rows;
        let persist_files_ms = t_files.elapsed().as_secs_f64() * 1000. - fts_insert_ms;
        let t_doc_fts = Instant::now();
        if docs_count > 0 {
            tx.execute(
            "INSERT INTO doc_search(rowid,section_title,content,invariants) SELECT rowid,section_title,content,invariants FROM doc_sections",
            [],
        )?;
        }
        let doc_fts_insert_ms = t_doc_fts.elapsed().as_secs_f64() * 1000.;

        if streamed_payloads {
            for file_facts in facts.values_mut() {
                prune_facts_for_stage(file_facts, false);
            }
        }
        let coverage = ingest::PreparedCoverage::new(graph, &facts, Some(entries));
        let prepared = ingest::PreparedGraph::new(graph, &facts)?;
        let (mut domains, graph_timings, persist_graph_ms, graph_projection_ms) =
            std::thread::scope(|scope| -> Result<_> {
                let projection = scope.spawn(|| {
                    let started = Instant::now();
                    let domains = seal::logical_domain_leaves(
                        entries,
                        &facts,
                        graph,
                        &coverage,
                        &prepared,
                        &mut projection_timings,
                        !streamed_payloads,
                    );
                    (domains, started.elapsed().as_secs_f64() * 1000.)
                });
                let mut node_paths: hashbrown::HashMap<
                    std::borrow::Cow<'_, str>,
                    Option<CachedNodePath>,
                > = hashbrown::HashMap::new();
                let started = Instant::now();
                let stored = ingest::persist_graph_prepared(
                    &tx,
                    &mut path_cache,
                    &mut node_paths,
                    ingest::PersistGraphParams {
                        graph,
                        facts: &facts,
                        dependency_owners: None,
                        materialize_edges: true,
                    },
                    &coverage,
                    Some(&prepared),
                );
                drop(node_paths);
                let persist_ms = started.elapsed().as_secs_f64() * 1000.;
                let (domains, projection_ms) = projection
                    .join()
                    .map_err(|_| anyhow::anyhow!("graph commitment projection panicked"))?;
                Ok((domains?, stored?, persist_ms, projection_ms))
            })?;
        drop(coverage);
        drop(prepared);
        seal_projection_ms += graph_projection_ms;
        if streamed_payloads {
            for (domain, leaves) in logical_commitments {
                domains.insert(domain, leaves);
            }
        } else {
            domains.insert(
                "nodes",
                logical_commitments
                    .remove("nodes")
                    .context("missing persisted node commitments")?,
            );
        }
        let mut logical_commitments = domains;

        let rows_ms = writing.elapsed().as_secs_f64() * 1000.;
        tx.commit()?;
        // Rows and their logical commitment leaves are now independent of the
        // extracted facts. Release those facts before index and FTS construction.
        drop(facts);
        acdd_core::sys::trim_allocator();
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let index_and_seal = Instant::now();
        let index_sql = format!("CREATE INDEX{indexes}");
        let index_candidate = |conn: &mut Connection| -> Result<f64> {
            let started = Instant::now();
            conn.execute_batch(&format!(
                "PRAGMA cache_size=-{cache_kb}; PRAGMA temp_store=MEMORY;"
            ))?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute_batch(&index_sql)?;
            tx.commit()?;
            Ok(started.elapsed().as_secs_f64() * 1000.)
        };
        let indexes_ms = index_candidate(conn)?;
        let t_fts = Instant::now();
        let fts_digests = commitments::fts_commitment_digests(conn)?;
        let seal_fts_ms = t_fts.elapsed().as_secs_f64() * 1000.;
        logical_commitments.insert("local_facts", file_timings.local_facts_leaves);
        let seal_started = Instant::now();
        let snapshot_started = Instant::now();
        let (seal, snapshot_timings): (String, commitments::SnapshotTimings) =
            commitments::seal_snapshot_from_projection(conn, logical_commitments, fts_digests)?;
        let seal_snapshot_ms = snapshot_started.elapsed().as_secs_f64() * 1000.;
        let seal_ms = seal_started.elapsed().as_secs_f64() * 1000.;
        conn.execute_batch(&format!("PRAGMA cache_size=-{cache_kb};"))?;
        let index_and_seal_ms = index_and_seal.elapsed().as_secs_f64() * 1000.;
        let persist_ms = writing.elapsed().as_secs_f64() * 1000.;
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")?;
        let mut report = json!({"files":entries.len(),"nodes":node_id,"node_row_count":node_row_count,"node_row_insert_ms":node_row_insert_ms,"edges":graph.edges.len(),"doc_sections":docs_count,"output_root":seal,"schema_version":config::ENGINE_SCHEMA_VERSION,"link_ms":0.0,"persist_ms":persist_ms,"rows_ms":rows_ms,"bulk_paths_ms":bulk_paths_ms,"persist_files_ms":persist_files_ms,"fts_insert_ms":fts_insert_ms,"doc_fts_insert_ms":doc_fts_insert_ms,"persist_graph_ms":persist_graph_ms,"indexes_ms":indexes_ms,"seal_ms":seal_ms,"seal_projection_ms":seal_projection_ms,"seal_fts_ms":seal_fts_ms,"seal_file_domains_ms":projection_timings.file_domains_ms,"seal_node_domain_ms":projection_timings.node_domain_ms,"seal_graph_edges_ms":projection_timings.graph_edges_ms,"seal_graph_occurrences_ms":projection_timings.graph_occurrences_ms,"seal_graph_unique_edges_ms":projection_timings.graph_unique_edges_ms,"seal_graph_shared_owners_ms":projection_timings.graph_shared_owners_ms,"seal_graph_coverage_ms":projection_timings.graph_coverage_ms,"seal_graph_dependencies_ms":projection_timings.graph_dependencies_ms,"seal_graph_domains_ms":projection_timings.graph_domains_ms,"seal_snapshot_ms":seal_snapshot_ms,"seal_snapshot_logical_domains_ms":snapshot_timings.logical_domains_ms,"seal_snapshot_install_ms":snapshot_timings.install_ms,"encode_facts_ms":encode_facts_ms,"sqlite_ingestion":{"mode":"deferred-index-batched","secondary_indexes_deferred":true,"batch_rows":local_facts_batch_rows}});
        report["graph_phases_ms"] = json!({"occurrences_and_dependencies":graph_timings.occurrences,"unique_edges":graph_timings.edges,"coverage_dictionaries":graph_timings.dictionaries,"coverage_and_unresolved":graph_timings.coverage});
        report["index_and_seal_ms"] = json!(index_and_seal_ms);
        report["restore_payload_ms"] = json!(restore_payload_ms);
        report["commitment_seal_mode"] = json!("in-memory-projection");
        Ok(report)
    }
}

fn restore_catalog_file(
    file: &FileEntry,
    lean: &TypedFacts,
    encoded: &[u8],
    composed_sources: &BTreeMap<String, CatalogStoragePayload>,
) -> Result<TypedFacts> {
    let mut full = lean.clone();
    catalog_payload(&file.path, encoded)?.restore(&mut full);
    for node in &mut full.nodes {
        if node.detail_get(NodeDetailKey::INCLUDED_FROM).is_some() {
            if let Some(source) = node.detail_entry(NodeDetailKey::SOURCE_PATH).as_str() {
                let payload = composed_sources
                    .get(source)
                    .context("missing composed source")?;
                payload.restore_composed_node(node);
            }
        }
    }
    Ok(full)
}

fn catalog_payload(path: &str, encoded: &[u8]) -> Result<CatalogStoragePayload> {
    let facts = crate::typed_facts::decode_facts(encoded)?;
    Ok(SymbolCatalog::default().insert_typed_with_payload(path.to_owned(), facts))
}

fn prune_facts_for_stage(facts: &mut TypedFacts, keep_node_navigation_data: bool) {
    let html = facts.nodes.iter().any(|node| {
        node.kind == crate::models::tags::NodeKind::Module
            && node.language == crate::models::tags::LanguageId::Html
    });
    facts
        .flows
        .retain_persistence_facts(keep_node_navigation_data);
    facts.facts.edges = Vec::new();
    facts.facts.semantic = Default::default();
    facts.facts.pending_flow_nodes = Vec::new();
    if !html {
        facts.facts.references = Vec::new();
    }
    if !keep_node_navigation_data {
        facts.facts.docs = Vec::new();
        for node in &mut facts.facts.nodes {
            node.details = Value::Null;
            node.native_semantic = None;
            node.name = String::new();
            node.qualname = String::new();
        }
    }
}

fn cached(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize> {
    conn.prepare_cached(sql)?.execute(params)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sqlite_cache_size_is_clamped_in_kibibytes() {
        for (budget, expected) in [(0, 4_096), (64 * 1024 * 1024, 8_192), (u64::MAX, 32_768)] {
            assert_eq!(super::sqlite_cache_size_kib(budget), expected);
        }
    }
}
