//! SQLite persistence and indexing operations for ACDD.

use crate::db::commitments;
use crate::db::schema::SCHEMA_DDL;
use crate::models::compact_graph::Graph as CompactGraph;
use crate::models::*;
use crate::semantic::CatalogStoragePayload;
use crate::typed_facts::{encode_facts, TypedFacts};
use acdd_core::config::{self, Adapter};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    path::Path,
    sync::Arc,
    time::Instant,
};

mod batch;
/// Database extraction cache facilities.
pub mod cache;
pub mod delta;
#[doc(hidden)]
pub use delta as delta_persist;
mod dictionary;
pub(crate) mod entrypoints;
mod ingest;
mod seal;

use batch::*;
use cache::PreparedCatalogStorage;
use dictionary::*;
use ingest::*;

pub(crate) const MAX_MULTI_VALUE_BATCH_ROWS: usize = 500;

/// Bounded offset and length range for a staged payload blob in an on-disk spool.
#[derive(Debug, Clone, Copy)]
pub struct StagedPayloadBlob {
    /// Byte offset in the spool file.
    pub offset: u64,
    /// Byte length in the spool file.
    pub length: u64,
}

/// Staged on-disk payload locations for an indexed file entry.
#[derive(Debug)]
pub struct StagedFilePayload {
    /// Encoded facts payload blob location.
    pub encoded_facts: StagedPayloadBlob,
    /// Storage rows payload blob location if prepared.
    pub storage_rows: Option<StagedPayloadBlob>,
}

/// RAM-backed payloads indexed by their reserved spool offsets.
pub type RamStagedPayloads = std::sync::Mutex<BTreeMap<u64, Vec<u8>>>;

fn read_staged_payload(file: &File, payload: StagedPayloadBlob, ram: &RamStagedPayloads, consume: bool) -> Result<Vec<u8>> {
    if payload.offset & (1 << 63) != 0 {
        let mut entries = ram.lock().unwrap_or_else(|error| error.into_inner());
        let bytes = if consume { entries.remove(&payload.offset) } else { entries.get(&payload.offset).cloned() };
        let bytes = bytes.context("missing staged RAM payload")?;
        anyhow::ensure!(bytes.len() as u64 == payload.length, "staged RAM payload length mismatch");
        return Ok(bytes);
    }
    let length = usize::try_from(payload.length).context("staged payload length overflow")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .context("cannot reserve memory for staged payload")?;
    bytes.resize(length, 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut offset = payload.offset;
        let mut remaining = bytes.as_mut_slice();
        while !remaining.is_empty() {
            match file.read_at(remaining, offset) {
                Ok(0) => {
                    bail!(
                        "unexpected EOF reading staged payload at offset {} (remaining {} bytes)",
                        offset,
                        remaining.len()
                    );
                }
                Ok(read_bytes) => {
                    remaining = &mut remaining[read_bytes..];
                    offset = offset
                        .checked_add(read_bytes as u64)
                        .context("staged payload offset overflow")?;
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        static SPOOL_READ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = SPOOL_READ_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut reader = file;
        reader.seek(SeekFrom::Start(payload.offset))?;
        reader.read_exact(&mut bytes)?;
    }
    Ok(bytes)
}

fn decode_prepared_storage(bytes: &[u8]) -> Result<PreparedCatalogStorage> {
    cache::decode_prepared_catalog_storage_payload(bytes)
}

/// Prepares encoded catalog storage rows for a file using extracted typed facts.
pub fn prepare_catalog_storage_with_facts(
    file: &FileEntry,
    facts: &TypedFacts,
) -> Result<Vec<u8>> {
    let (search_texts, reference_keys, leaves) = catalog_metadata(file, facts)?;
    let mut bytes = cache::encode_prepared_catalog_storage_with_facts(facts, &search_texts, &reference_keys, &leaves);
    bytes.shrink_to_fit();
    Ok(bytes)
}

type CatalogMetadata = (Vec<String>, Vec<String>, BTreeMap<String, BTreeMap<String, String>>);

fn catalog_metadata(file: &FileEntry, facts: &TypedFacts) -> Result<CatalogMetadata> {
    let search_texts = facts
        .nodes
        .iter()
        .map(|node| {
            let mut text = String::new();
            write_node_search_text(&mut text, node, facts);
            text
        })
        .collect::<Vec<_>>();
    let reference_keys = facts
        .reference_keys()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let leaves = seal::file_domain_leaves(file, facts, &reference_keys)?;
    Ok((search_texts, reference_keys, leaves.into_iter().map(|(domain, leaves)| (domain.to_owned(), leaves)).collect()))
}

fn retained_node_details(node: &Node) -> Value {
            match &node.details {
                Value::Object(details) if !details.is_empty() => {
                    let has_retained = details.iter().any(|(key, value)| {
                        (key.as_str() == "default_export" && *value == true)
                            || matches!(key.as_str(), "doc" | "docstring")
                    });
                    if has_retained {
                        let details = details
                            .iter()
                            .filter(|(key, value)| {
                                (key.as_str() == "default_export" && *value == true)
                                    || matches!(key.as_str(), "doc" | "docstring")
                            })
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect::<serde_json::Map<_, _>>();
                        Value::Object(details)
                    } else {
                        Value::Null
                    }
                }
                _ => Value::Null,
            }
}

fn prepare_catalog_rows(file: &FileEntry, mut facts: TypedFacts) -> Result<PreparedCatalogStorage> {
    let (search_texts, reference_keys, leaves) = catalog_metadata(file, &facts)?;
    let nodes = std::mem::take(&mut facts.facts.nodes).into_iter().map(|mut node| {
        node.details = retained_node_details(&node);
        node.native_semantic = None;
        node
    }).collect::<Vec<_>>();
    let mut storage_facts = Facts {
        nodes,
        docs: std::mem::take(&mut facts.facts.docs),
        errors: std::mem::take(&mut facts.facts.errors),
        ..Default::default()
    };
    storage_facts.semantic.mark_complete();
    let storage = TypedFacts {
        facts: storage_facts,
        flows: Default::default(),
    };
    let prepared = PreparedCatalogStorage {
        facts: storage,
        search_texts,
        reference_keys,
        leaves,
    };
    Ok(prepared)
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

/// Populates index database taking ownership of extracted facts and staged payloads.
#[allow(clippy::too_many_arguments)]
pub fn populate_owned(
    conn: &mut Connection,
    root: &Path,
    adapter: &Adapter,
    entries: &[FileEntry],
    facts: BTreeMap<String, TypedFacts>,
    graph: &CompactGraph,
    build_inputs: BuildInputs<'_>,
    staged_payloads: &mut BTreeMap<String, StagedFilePayload>,
    payload_spool_path: Option<&Path>,
) -> Result<Value> {
    populate_owned_with_ram(conn, root, adapter, entries, facts, graph, build_inputs, staged_payloads, payload_spool_path, &RamStagedPayloads::default())
}

/// Populates an index with optional RAM-backed staged payloads.
#[allow(clippy::too_many_arguments)]
pub fn populate_owned_with_ram(
    conn: &mut Connection,
    root: &Path,
    adapter: &Adapter,
    entries: &[FileEntry],
    mut facts: BTreeMap<String, TypedFacts>,
    graph: &CompactGraph,
    build_inputs: BuildInputs<'_>,
    staged_payloads: &mut BTreeMap<String, StagedFilePayload>,
    payload_spool_path: Option<&Path>,
    ram: &RamStagedPayloads,
) -> Result<Value> {
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
        for (k, v) in [
            ("schema_version", config::ENGINE_SCHEMA_VERSION.to_owned()),
            (
                "index_semantics_version",
                crate::INDEX_SEMANTICS_VERSION.to_owned(),
            ),
            ("indexer_engine", "contextunity-acdd-rust".to_owned()),
            ("workspace_root", root.to_string_lossy().into_owned()),
            ("adapter", config::policy(root, adapter).to_string()),
            (
                "adapter_version",
                adapter.adapter_version.clone().unwrap_or_default(),
            ),
            ("adapter_digest", adapter.digest.clone()),
            ("manifest_digest", build_inputs.manifest_digest.to_owned()),
            (
                "adapter_linked_workspaces",
                serde_json::to_string(&adapter.linked_workspaces)?,
            ),
            ("inventory_snapshot", serde_json::to_string(entries)?),
            (
                "corpus_hash",
                acdd_core::format::sha256_serde_json_hex(entries)?,
            ),
        ] {
            cached(
                &tx,
                "INSERT OR REPLACE INTO metadata VALUES(?1,?2)",
                params![k, v],
            )?;
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
        let composed_sources = facts
            .values()
            .filter(|_| streamed_payloads)
            .flat_map(|facts| &facts.nodes)
            .filter(|node| node.details.get("included_from").is_some())
            .filter_map(|node| node.details["source_path"].as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|path| {
                let staged = staged_payloads
                    .get(path)
                    .context("missing composed source payload")?;
                let encoded_facts = read_staged_payload(
                    payload_spool
                        .as_ref()
                        .context("missing staged payload spool")?,
                    staged.encoded_facts,
                    ram,
                    false,
                )?;
                Ok((path.to_owned(), catalog_payload(path, &encoded_facts)?))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let graph_ranks = entrypoints::cold_ranks(graph, &facts);
        let file_timings = if streamed_payloads {
            persist_catalog_files(
                &tx,
                &mut path_cache,
                entries,
                &facts,
                &mut |chunk, facts| {
                    let staged = chunk
                        .iter()
                        .map(|file| {
                            Ok((
                                file,
                                facts.get(&file.path).context("missing catalog file")?,
                                staged_payloads
                                    .remove(&file.path)
                                    .context("missing staged file payload")?,
                            ))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let spool = payload_spool
                        .as_ref()
                        .context("missing staged payload spool")?;
                    let prepared = staged
                        .into_par_iter()
                        .map(|(file, lean, staged)| {
                            let restoring = Instant::now();
                            let encoded_facts = read_staged_payload(spool, staged.encoded_facts, ram, true)?;
                            let (storage, encoded) = if let Some(storage_blob) = staged.storage_rows
                            {
                                let storage_bytes = read_staged_payload(spool, storage_blob, ram, true)?;
                                let storage = decode_prepared_storage(&storage_bytes)?;
                                (storage, encoded_facts)
                            } else {
                                let full = restore_catalog_file(
                                    file,
                                    lean,
                                    &encoded_facts,
                                    &composed_sources,
                                )?;
                                let encoded = encode_facts(&full)?;
                                (prepare_catalog_rows(file, full)?, encoded)
                            };
                            let local_facts_leaf = seal::local_facts_leaf(
                                &file.path,
                                &file.digest,
                                &file.language,
                                is_test(&file.path),
                                &encoded,
                            )?;
                            Ok((
                                LoadedCatalogFile {
                                    facts: storage.facts,
                                    encoded,
                                    local_facts_leaf,
                                    search_texts: storage.search_texts,
                                    reference_keys: storage.reference_keys,
                                },
                                storage.leaves,
                                restoring.elapsed().as_secs_f64() * 1000.,
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
                        restore_payload_ms += elapsed_ms;
                        loaded.push(file);
                    }
                    Ok(loaded)
                },
                &graph_ranks,
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
                retain_graph_storage_facts(file_facts);
            }
        }
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            // SAFETY: glibc synchronizes allocator arenas; no pointers are passed.
            libc::malloc_trim(0);
        }
        let coverage = ingest::PreparedCoverage::new(graph, &facts, Some(entries));
        let (mut domains, graph_timings, persist_graph_ms, graph_projection_ms) =
            std::thread::scope(|scope| -> Result<_> {
                let projection = scope.spawn(|| {
                    let started = Instant::now();
                    let domains = seal::logical_domain_leaves(
                        entries,
                        &facts,
                        graph,
                        &coverage,
                        &mut projection_timings,
                        !streamed_payloads,
                    );
                    #[cfg(all(target_os = "linux", target_env = "gnu"))]
                    unsafe {
                        // SAFETY: projection scratch buffers have dropped; arenas are synchronized.
                        libc::malloc_trim(0);
                    }
                    (domains, started.elapsed().as_secs_f64() * 1000.)
                });
                let mut node_paths: hashbrown::HashMap<
                    std::borrow::Cow<'_, str>,
                    Option<CachedNodePath>,
                > = hashbrown::HashMap::with_capacity(
                    facts
                        .values()
                        .map(|file_facts| file_facts.nodes.len())
                        .sum(),
                );
                let mut shared_paths = hashbrown::HashMap::<&str, Arc<str>>::new();
                for file_facts in facts.values() {
                    for node in &file_facts.nodes {
                        let shared_path = shared_paths
                            .entry(node.path.as_str())
                            .or_insert_with(|| Arc::<str>::from(node.path.as_str()));
                        node_paths.insert(
                            std::borrow::Cow::Borrowed(node.id.as_str()),
                            Some(CachedNodePath::from_shared(
                                &node.id,
                                Arc::clone(shared_path),
                            )),
                        );
                    }
                }
                drop(shared_paths);
                let started = Instant::now();
                let stored = ingest::persist_graph_prepared(
                    &tx,
                    &mut path_cache,
                    graph,
                    &facts,
                    None,
                    &coverage,
                    &mut node_paths,
                    true,
                );
                drop(node_paths);
                let persist_ms = started.elapsed().as_secs_f64() * 1000.;
                let (domains, projection_ms) = projection
                    .join()
                    .map_err(|_| anyhow::anyhow!("graph commitment projection panicked"))?;
                Ok((domains?, stored?, persist_ms, projection_ms))
            })?;
        drop(coverage);
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
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            // SAFETY: glibc synchronizes allocator arenas; no pointers are passed.
            libc::malloc_trim(0);
        }
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
        if node.details.get("included_from").is_some() {
            if let Some(source) = node.details["source_path"].as_str() {
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
    Ok(crate::semantic::SymbolCatalog::default().insert_typed(path.to_owned(), facts, true))
}

fn retain_graph_storage_facts(facts: &mut TypedFacts) {
    let html = facts
        .nodes
        .iter()
        .any(|node| node.kind == "module" && node.language == "html");
    let mut flows = crate::typed_facts::FlowStore::default();
    for node in &mut facts.facts.nodes {
        if let Some(flow) = facts.flows.get_mut(&node.id) {
            let template_contexts: Vec<_> = flow
                .framework
                .take()
                .into_iter()
                .flat_map(|framework| framework.facts)
                .filter_map(|fact| match fact {
                    crate::semantic::FrameworkFact::TemplateContext(context) => Some(context),
                    _ => None,
                })
                .collect();
            if !template_contexts.is_empty() {
                flows.insert(
                    node.id.clone(),
                    crate::semantic::ValueFlowFacts {
                        framework: Some(Box::new(crate::semantic::FrameworkScopeFacts {
                            facts: template_contexts
                                .into_iter()
                                .map(crate::semantic::FrameworkFact::TemplateContext)
                                .collect(),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                );
            }
        }
        node.details = Value::Null;
        node.native_semantic = None;
        node.name = String::new();
        node.qualname = String::new();
    }
    facts.flows = flows;
    facts.facts.edges = Vec::new();
    facts.facts.docs = Vec::new();
    if !html {
        facts.facts.references = Vec::new();
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
