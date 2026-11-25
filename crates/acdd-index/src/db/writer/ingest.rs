use super::batch::{multi_value_batch_rows, BorrowedSqlValue, MultiValueBatch};
use super::dictionary::*;
use super::node_domain::{local_facts_leaf, NodeDomain};
use crate::db::commitments;
use crate::models::compact_graph::CompactGraph;
use crate::models::*;
use crate::typed_facts::{encode_facts, TypedFacts};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

const COLD_LOCAL_FACTS_BATCH_ROWS: usize = 500;

fn compare_coverage_rows(
    left: &crate::models::compact_graph::CompactCoverage,
    right: &crate::models::compact_graph::CompactCoverage,
) -> std::cmp::Ordering {
    left.path
        .as_bytes()
        .cmp(right.path.as_bytes())
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left.expression.as_bytes().cmp(right.expression.as_bytes()))
        .then_with(|| left.status.cmp(&right.status))
        .then_with(|| left.evidence.as_bytes().cmp(right.evidence.as_bytes()))
}

type HtmlCoverageOwners<'a> =
    hashbrown::HashMap<&'a str, hashbrown::HashMap<(usize, &'a str), Option<&'a str>>>;

/// Canonical borrowed coverage input shared by SQL persistence and sealing.
pub(super) struct PreparedCoverage<'a> {
    pub(super) rows: Vec<&'a crate::models::compact_graph::CompactCoverage>,
    file_languages: hashbrown::HashMap<&'a str, &'a str>,
    html_owners: HtmlCoverageOwners<'a>,
    html_islands: hashbrown::HashMap<&'a str, Vec<(usize, usize)>>,
}

/// Graph endpoints and unique edges shared by cold persistence and sealing.
pub(super) struct PreparedGraph<'a> {
    pub(super) edge_hashes: Vec<(i64, i64)>,
    pub(super) unique_edges: Vec<PreparedEdge<'a>>,
    pub(super) dependencies: Vec<PreparedDependency<'a>>,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct PreparedDependency<'a> {
    pub(super) owner: &'a str,
    pub(super) target_hash: Option<i64>,
    pub(super) kind: &'a str,
    pub(super) symbol: &'a str,
    pub(super) resolution: &'a str,
}

pub(super) struct PreparedEdge<'a> {
    pub(super) edge: &'a crate::models::compact_graph::CompactEdge,
    pub(super) src_hash: i64,
    pub(super) dst_hash: i64,
    pub(super) count: i64,
}

impl<'a> PreparedGraph<'a> {
    pub(super) fn new(
        graph: &'a CompactGraph,
        facts: &'a BTreeMap<String, TypedFacts>,
    ) -> Result<Self> {
        let total_nodes: usize = facts
            .values()
            .map(|file_facts| file_facts.nodes.len())
            .sum();
        let mut node_by_id = hashbrown::HashMap::with_capacity(total_nodes);
        for file_facts in facts.values() {
            for node in &file_facts.nodes {
                node_by_id.insert(
                    node.id.as_str(),
                    (node.path.as_str(), stable_hash64(&node.id)),
                );
            }
        }
        let mut indexes = hashbrown::HashMap::with_capacity(graph.edges.len());
        let mut edge_hashes = Vec::with_capacity(graph.edges.len());
        let mut unique_edges: Vec<PreparedEdge<'a>> = Vec::with_capacity(graph.edges.len());
        let mut resolved_edge_paths = Vec::with_capacity(graph.edges.len());
        for edge in &graph.edges {
            let &(src_path, src_hash) = node_by_id.get(edge.src.as_ref()).with_context(|| {
                format!(
                    "edge {} has missing source endpoint {}",
                    edge.kind.as_str(),
                    edge.src
                )
            })?;
            let &(dst_path, dst_hash) = node_by_id.get(edge.dst.as_ref()).with_context(|| {
                format!(
                    "edge {} has missing destination endpoint {}",
                    edge.kind.as_str(),
                    edge.dst
                )
            })?;
            edge_hashes.push((src_hash, dst_hash));
            resolved_edge_paths.push((
                (src_path != edge.path.as_ref()).then(|| stable_hash64(src_path)),
                (dst_path != edge.path.as_ref()).then(|| stable_hash64(dst_path)),
            ));
            let key = (src_hash, edge.kind.as_str(), dst_hash);
            if let Some(&index) = indexes.get(&key) {
                let row: &mut PreparedEdge<'a> = &mut unique_edges[index];
                row.count = row
                    .count
                    .checked_add(1)
                    .context("edge occurrence count overflow")?;
            } else {
                indexes.insert(key, unique_edges.len());
                unique_edges.push(PreparedEdge {
                    edge,
                    src_hash,
                    dst_hash,
                    count: 1,
                });
            }
        }
        drop(node_by_id);
        drop(indexes);
        let dependencies = prepare_dependencies(graph, facts, None, &resolved_edge_paths)?;
        Ok(Self {
            edge_hashes,
            unique_edges,
            dependencies,
        })
    }
}

impl<'a> PreparedCoverage<'a> {
    pub(super) fn new(
        graph: &'a CompactGraph,
        facts: &'a BTreeMap<String, TypedFacts>,
        entries: Option<&'a [FileEntry]>,
    ) -> Self {
        let mut rows: Vec<_> = graph.coverage.iter().collect();
        if !rows
            .windows(2)
            .all(|pair| compare_coverage_rows(pair[0], pair[1]).is_le())
        {
            if rows.len() >= 8192 {
                rows.par_sort_unstable_by(|left, right| compare_coverage_rows(left, right));
            } else {
                rows.sort_unstable_by(|left, right| compare_coverage_rows(left, right));
            }
        }
        rows.dedup_by(|later, first| compare_coverage_rows(first, later).is_eq());
        let mut prepared = Self {
            rows,
            file_languages: entries
                .into_iter()
                .flatten()
                .filter(|entry| !entry.language.is_empty())
                .map(|entry| (entry.path.as_str(), entry.language.as_str()))
                .collect(),
            html_owners: hashbrown::HashMap::new(),
            html_islands: hashbrown::HashMap::new(),
        };
        for (path, file_facts) in facts {
            if let Some(module) = file_facts
                .nodes
                .iter()
                .find(|node| node.kind == crate::models::tags::NodeKind::Module)
            {
                prepared
                    .file_languages
                    .insert(path.as_str(), module.language.as_str());
            }
            if !file_facts.nodes.iter().any(|node| {
                node.kind == crate::models::tags::NodeKind::Module
                    && node.language == crate::models::tags::LanguageId::Html
            }) {
                continue;
            }
            let node_languages: hashbrown::HashMap<_, _> = file_facts
                .nodes
                .iter()
                .map(|node| (node.id.as_str(), node.language.as_str()))
                .collect();
            let islands: Vec<_> = file_facts
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == crate::models::tags::NodeKind::IslandScope
                        && node.language == crate::models::tags::LanguageId::Javascript
                })
                .map(|node| (node.line, node.end_line))
                .collect();
            let mut owners = hashbrown::HashMap::new();
            for reference in &file_facts.references {
                let Some(language) = node_languages.get(reference.source.as_str()).copied() else {
                    continue;
                };
                let entry = owners
                    .entry((reference.line, reference.expression.as_str()))
                    .or_insert(Some(language));
                if *entry != Some(language) {
                    *entry = None;
                }
            }
            owners.retain(|(line, _), language| {
                let whole_line_js = islands
                    .iter()
                    .any(|(first, last)| first <= line && line <= last);
                match language {
                    Some("javascript") => !whole_line_js,
                    Some("html") | None => whole_line_js,
                    _ => false,
                }
            });
            if !owners.is_empty() {
                prepared.html_owners.insert(path.as_str(), owners);
            }
            if !islands.is_empty() {
                prepared.html_islands.insert(path.as_str(), islands);
            }
        }
        prepared
    }

    pub(super) fn file_language(&self, path: &str) -> Option<&'a str> {
        self.file_languages.get(path).copied()
    }

    pub(super) fn owner_language(
        &self,
        row: &crate::models::compact_graph::CompactCoverage,
    ) -> Option<&'a str> {
        self.html_owners
            .get(row.path.as_ref())
            .and_then(|owners| owners.get(&(row.line, row.expression.as_str())))
            .map(|language| language.unwrap_or("html"))
    }

    pub(super) fn language<'b>(
        &'b self,
        row: &crate::models::compact_graph::CompactCoverage,
        fallback: Option<&'b str>,
    ) -> Option<&'b str> {
        if let Some(language) = self.owner_language(row) {
            return (!language.is_empty()).then_some(language);
        }
        let base = self
            .file_language(row.path.as_ref())
            .or_else(|| fallback.filter(|lang| !lang.is_empty()));
        if let Some(islands) = self.html_islands.get(row.path.as_ref()) {
            if islands
                .iter()
                .any(|(first, last)| *first <= row.line && row.line <= *last)
            {
                Some("javascript")
            } else {
                base.or(Some("html"))
            }
        } else {
            base
        }
    }
}

fn flush_owner_language_counts<'a>(
    path_id: i64,
    pending: &mut Vec<(&'a str, &'a str, i64)>,
    language_counts: &mut BTreeMap<(i64, &'a str, &'a str), i64>,
) {
    for (language, status, count) in pending.drain(..) {
        *language_counts
            .entry((path_id, language, status))
            .or_default() += count;
    }
}

pub struct LocalFactsBatchWriter<'conn> {
    rows: MultiValueBatch<'conn, 6, BorrowedSqlValue<'static>>,
}

impl<'conn> LocalFactsBatchWriter<'conn> {
    fn with_prefix(conn: &'conn Connection, prefix: &'static str) -> Result<Self> {
        Ok(Self {
            rows: MultiValueBatch::new(conn, prefix)?,
        })
    }

    pub fn new(conn: &'conn Connection) -> Result<Self> {
        Self::with_prefix(conn, "INSERT INTO local_facts VALUES")
    }

    pub(super) fn new_replace(conn: &'conn Connection) -> Result<Self> {
        Self::with_prefix(conn, "INSERT OR REPLACE INTO local_facts VALUES")
    }

    pub fn push(&mut self, file: &FileEntry, blob: Vec<u8>) -> Result<()> {
        self.rows.push([
            BorrowedSqlValue::OwnedText(file.path.clone()),
            BorrowedSqlValue::OwnedText(file.digest.clone()),
            BorrowedSqlValue::OwnedText(file.language.clone()),
            BorrowedSqlValue::Integer(i64::from(is_test(&file.path))),
            BorrowedSqlValue::Integer(0),
            BorrowedSqlValue::OwnedBlob(blob),
        ])
    }

    pub fn flush(&mut self) -> Result<()> {
        self.rows.flush()
    }
}

#[derive(Debug)]
pub(super) struct FilePersistenceTimings {
    pub(super) node_id: i64,
    pub(super) node_row_count: usize,
    pub(super) docs_count: usize,
    pub(super) fts_insert_ms: f64,
    pub(super) node_row_insert_ms: Option<f64>,
    pub(super) encode_facts_ms: f64,
    pub(super) local_facts_batch_rows: usize,
    pub(super) local_facts_leaves: std::collections::BTreeMap<String, String>,
    pub(super) node_leaves: std::collections::BTreeMap<String, String>,
}

pub(super) struct PersistFilesParams<'a, 'r> {
    pub(super) entries: &'a [FileEntry],
    pub(super) facts: &'a BTreeMap<String, TypedFacts>,
    pub(super) loader: Option<ChunkLoader<'a>>,
    pub(super) graph_ranks: Option<&'a hashbrown::HashMap<&'r str, Option<i64>>>,
    pub(super) dirty_fts: Option<crate::db::commitments::FtsDirtySet>,
    pub(super) inventory_ready: bool,
}

pub(super) fn persist_delta_files(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    entries: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    dirty_fts: crate::db::commitments::FtsDirtySet,
) -> Result<FilePersistenceTimings> {
    persist_files_impl(
        tx,
        path_cache,
        PersistFilesParams {
            entries,
            facts,
            loader: None,
            graph_ranks: None,
            dirty_fts: Some(dirty_fts),
            inventory_ready: false,
        },
    )
}

#[cfg(test)]
pub(super) fn persist_files(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    entries: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    bulk_search: bool,
) -> Result<FilePersistenceTimings> {
    let dirty = (!bulk_search).then_some(crate::db::commitments::FtsDirtySet {
        node_search: true,
        doc_search: true,
    });
    persist_files_impl(
        tx,
        path_cache,
        PersistFilesParams {
            entries,
            facts,
            loader: None,
            graph_ranks: None,
            dirty_fts: dirty,
            inventory_ready: false,
        },
    )
}

pub(super) fn persist_files_with_ranks(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    entries: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    graph_ranks: Option<&hashbrown::HashMap<&str, Option<i64>>>,
) -> Result<FilePersistenceTimings> {
    persist_files_impl(
        tx,
        path_cache,
        PersistFilesParams {
            entries,
            facts,
            loader: None,
            graph_ranks,
            dirty_fts: None,
            inventory_ready: false,
        },
    )
}

pub(super) struct LoadedCatalogFile {
    pub(super) node_docs: Vec<super::cache::NodeDocumentation>,
    pub(super) doc_bodies: Vec<(String, Vec<String>)>,
    pub(super) local_facts_leaf: (String, String),
    pub(super) updated_local_facts: Option<Vec<u8>>,
    pub(super) search_texts: super::cache::PackedStrings,
    pub(super) reference_keys: Vec<String>,
}

type ChunkLoader<'a> = &'a mut dyn FnMut(
    &Connection,
    &[FileEntry],
    &BTreeMap<String, TypedFacts>,
) -> Result<Vec<LoadedCatalogFile>>;

pub(super) fn persist_catalog_files(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    entries: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    loader: ChunkLoader<'_>,
    graph_ranks: &hashbrown::HashMap<&str, Option<i64>>,
    inventory_ready: bool,
) -> Result<FilePersistenceTimings> {
    persist_files_impl(
        tx,
        path_cache,
        PersistFilesParams {
            entries,
            facts,
            loader: Some(loader),
            graph_ranks: Some(graph_ranks),
            dirty_fts: None,
            inventory_ready,
        },
    )
}

struct EncodedLocalFactsEntry {
    blob: Option<Vec<u8>>,
    leaf: (String, String),
}

fn encode_chunk_local_facts(
    file_chunk: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    loaded: Option<&mut Vec<LoadedCatalogFile>>,
    encode_facts_ms: &mut f64,
) -> Result<Vec<EncodedLocalFactsEntry>> {
    if let Some(loaded) = loaded {
        return Ok(loaded
            .iter_mut()
            .map(|file| EncodedLocalFactsEntry {
                blob: std::mem::take(&mut file.updated_local_facts),
                leaf: std::mem::take(&mut file.local_facts_leaf),
            })
            .collect());
    }
    let started = Instant::now();
    let encode = |file: &FileEntry| -> Result<EncodedLocalFactsEntry> {
        let file_facts = facts.get(&file.path).context("missing extracted facts")?;
        let blob = encode_facts(file_facts)?;
        let leaf = local_facts_leaf(
            &file.path,
            &file.digest,
            &file.language,
            is_test(&file.path),
            &blob,
        )?;
        Ok(EncodedLocalFactsEntry {
            blob: Some(blob),
            leaf,
        })
    };
    let encoded = if file_chunk.len() == 1 {
        file_chunk.iter().map(encode).collect::<Result<Vec<_>>>()?
    } else {
        file_chunk
            .par_iter()
            .map(encode)
            .collect::<Result<Vec<_>>>()?
    };
    *encode_facts_ms += started.elapsed().as_secs_f64() * 1000.0;
    Ok(encoded)
}

type PreparedNodeDetails = Vec<(usize, Vec<u8>)>;

fn prepare_chunk_node_details(
    file_chunk: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    loaded: Option<&[LoadedCatalogFile]>,
    graph_ranks: Option<&hashbrown::HashMap<&str, Option<i64>>>,
) -> Result<Vec<PreparedNodeDetails>> {
    file_chunk
        .par_iter()
        .enumerate()
        .map(|(chunk_index, file)| -> Result<PreparedNodeDetails> {
            let detail_facts = facts
                .get(&file.path)
                .context("missing navigation facts for node details")?;
            let node_docs = loaded.map(|loaded| loaded[chunk_index].node_docs.as_slice());
            if let Some(docs) = node_docs {
                anyhow::ensure!(
                    docs.last()
                        .is_none_or(|last| (last.index as usize) < detail_facts.nodes.len()),
                    "prepared node documentation index out of bounds"
                );
            }
            let mut node_order = (0..detail_facts.nodes.len()).collect::<Vec<_>>();
            node_order.sort_unstable_by(|&left, &right| {
                detail_facts.nodes[left]
                    .id
                    .cmp(&detail_facts.nodes[right].id)
                    .then_with(|| left.cmp(&right))
            });
            node_order
                .into_iter()
                .map(|node_index| -> Result<(usize, Vec<u8>)> {
                    let navigation_node = &detail_facts.nodes[node_index];
                    let return_type = if matches!(
                        navigation_node.kind.as_str(),
                        "function" | "method" | "constructor"
                    ) {
                        declared_return_type(navigation_node, detail_facts)
                    } else {
                        None
                    };
                    let entrypoint_base =
                        navigation_node.detail_get(NodeDetailKey::ENTRYPOINT_BASE);
                    let intrinsic = entrypoint_base
                        .and_then(Value::as_str)
                        .and_then(super::entrypoints::rank);
                    let graph = graph_ranks
                        .and_then(|ranks| ranks.get(navigation_node.id.as_str()))
                        .copied()
                        .flatten();
                    let category = intrinsic
                        .into_iter()
                        .chain(graph)
                        .min()
                        .and_then(super::entrypoints::category);
                    let intrinsic_candidate = entrypoint_base.is_some_and(|value| {
                        !value.is_null()
                            && !value.as_str().is_some_and(str::is_empty)
                            && !value.as_array().is_some_and(Vec::is_empty)
                    });
                    let clear_entrypoint = graph_ranks.is_some()
                        && category.is_none()
                        && (intrinsic_candidate
                            || graph_ranks.is_some_and(|ranks| {
                                ranks.contains_key(navigation_node.id.as_str())
                            }));
                    let navigation = if let Some(docs) = node_docs {
                        let doc_entry = u32::try_from(node_index)
                            .ok()
                            .and_then(|idx| {
                                docs.binary_search_by_key(&idx, |entry| entry.index).ok()
                            })
                            .map(|position| &docs[position]);
                        navigation_node.navigation_details_with_doc_fields(
                            doc_entry.and_then(|entry| entry.doc.as_ref()),
                            doc_entry.and_then(|entry| entry.docstring.as_ref()),
                        )
                    } else {
                        navigation_node
                            .navigation_details_with_documentation(&navigation_node.details)
                    }
                    .persisted(
                        return_type.as_deref(),
                        category,
                        clear_entrypoint,
                    );
                    Ok((node_index, serde_json::to_vec(&navigation)?))
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()
}

fn validate_staged_file_counts(
    file_path: &str,
    facts: &TypedFacts,
    search_texts: Option<&super::cache::PackedStrings>,
    doc_bodies: Option<&Vec<(String, Vec<String>)>>,
) -> Result<()> {
    if let Some(search_texts) = search_texts {
        anyhow::ensure!(
            search_texts.len() == facts.nodes.len(),
            "prepared search text count {} mismatches node count {} for {}",
            search_texts.len(),
            facts.nodes.len(),
            file_path
        );
    }
    if let Some(bodies) = doc_bodies {
        anyhow::ensure!(
            bodies.len() == facts.docs.len(),
            "prepared doc bodies count {} mismatches doc count {} for {}",
            bodies.len(),
            facts.docs.len(),
            file_path
        );
    }
    Ok(())
}

fn persist_local_facts_entry<'tx>(
    tx: &'tx Connection,
    file: &FileEntry,
    entry: &mut EncodedLocalFactsEntry,
    local_facts_leaves: &mut std::collections::BTreeMap<String, String>,
    insert_writer: &mut Option<LocalFactsBatchWriter<'tx>>,
    replace_writer: &mut Option<LocalFactsBatchWriter<'tx>>,
) -> Result<()> {
    local_facts_leaves.insert(
        std::mem::take(&mut entry.leaf.0),
        std::mem::take(&mut entry.leaf.1),
    );
    let Some(blob) = entry.blob.take() else {
        return Ok(());
    };
    if let Some(writer) = insert_writer.as_mut() {
        writer.push(file, blob)?;
    } else {
        replace_writer
            .get_or_insert(LocalFactsBatchWriter::new_replace(tx)?)
            .push(file, blob)?;
    }
    Ok(())
}

fn push_node_search_row(
    tx: &Connection,
    owned_search_batch: &mut Vec<SearchRow>,
    reusable_search_buffers: &mut Vec<String>,
    owned_search_batch_rows: usize,
    fts_insert_ms: &mut f64,
    search_id: i64,
    text: String,
) -> Result<()> {
    owned_search_batch.push(SearchRow { search_id, text });
    if owned_search_batch.len() == owned_search_batch_rows {
        *fts_insert_ms +=
            flush_owned_search_batch(tx, owned_search_batch, reusable_search_buffers)?;
    }
    Ok(())
}

fn persist_file_owners(
    f: &TypedFacts,
    owner_id: i64,
    prepared_references: Option<&Vec<String>>,
    shared_key_cache: &mut SharedKeyCache<'_>,
    shared_owner_rows: &mut Vec<[i64; 4]>,
    shared_owner_batch: &mut MultiValueBatch<'_, 4>,
) -> Result<()> {
    let keys = prepared_references.map_or_else(
        || f.reference_keys(),
        |keys| keys.iter().map(String::as_str).collect(),
    );
    for key in keys {
        let key_hash = shared_key_cache.get_or_insert(key)?;
        shared_owner_rows.push([
            commitments::SharedOwnerKind::Reference as i64,
            key_hash,
            owner_id,
            0,
        ]);
    }
    for n in &f.nodes {
        if *n.detail_entry(NodeDetailKey::DEFAULT_EXPORT) == true {
            let key_hash = shared_key_cache.get_or_insert(&n.id)?;
            shared_owner_rows.push([
                commitments::SharedOwnerKind::DefaultExport as i64,
                key_hash,
                owner_id,
                0,
            ]);
        }
    }
    if shared_owner_rows.len() >= 8192 {
        shared_owner_rows.par_sort_unstable();
    } else {
        shared_owner_rows.sort_unstable();
    }
    for row in shared_owner_rows.drain(..) {
        shared_owner_batch.push(row.map(SqlValue::Integer))?;
    }
    Ok(())
}

fn persist_file_docs_and_errors(
    file: &FileEntry,
    f: &TypedFacts,
    mut doc_bodies: Option<&mut Vec<(String, Vec<String>)>>,
    doc_sections_batch: &mut MultiValueBatch<'_, 10, BorrowedSqlValue<'static>>,
    stmt_doc_search: &mut Option<rusqlite::Statement<'_>>,
    stmt_errors: &mut rusqlite::Statement<'_>,
    docs_count: &mut usize,
) -> Result<()> {
    for (doc_index, d) in f.docs.iter().enumerate() {
        *docs_count += 1;
        let (content, invariants) = if let Some(bodies) = doc_bodies.as_deref_mut() {
            let (content, invariants) = std::mem::take(&mut bodies[doc_index]);
            (content, serde_json::to_string(&invariants)?)
        } else {
            (d.content.clone(), serde_json::to_string(&d.invariants)?)
        };
        doc_sections_batch.push([
            BorrowedSqlValue::OwnedText(d.doc_id.clone()),
            BorrowedSqlValue::OwnedText(d.path.clone()),
            BorrowedSqlValue::OwnedText(d.section_title.clone()),
            BorrowedSqlValue::OwnedText(d.doc_type.clone()),
            BorrowedSqlValue::OwnedText(content),
            BorrowedSqlValue::OwnedText(invariants),
            BorrowedSqlValue::OwnedText(serde_json::to_string(&d.referenced_symbols)?),
            BorrowedSqlValue::Real(d.mtime),
            BorrowedSqlValue::Integer(i64::try_from(d.size)?),
            BorrowedSqlValue::Integer(i64::from(d.is_invariant)),
        ])?;
    }
    if !f.docs.is_empty() {
        if let Some(stmt) = stmt_doc_search.as_mut() {
            doc_sections_batch.flush()?;
            stmt.execute([&file.path])?;
        }
    }
    for e in &f.errors {
        stmt_errors.execute(params![e.path, e.line, e.message])?;
    }
    Ok(())
}

fn persist_file_record<'data, 'tx>(
    tx: &'tx Connection,
    path_cache: &mut PathDictionaryCache,
    file_batch: &mut MultiValueBatch<'tx, 8, BorrowedSqlValue<'data>>,
    source_inventory_batch: Option<&mut MultiValueBatch<'tx, 4, BorrowedSqlValue<'data>>>,
    file: &'data FileEntry,
) -> Result<i64> {
    let owner_id = path_cache.get_or_insert(tx, &file.path)?;
    if let Some(batch) = source_inventory_batch {
        batch.push([
            BorrowedSqlValue::Text(&file.path),
            BorrowedSqlValue::Text("indexed"),
            BorrowedSqlValue::Text(&file.digest),
            BorrowedSqlValue::Integer(i64::try_from(file.bytes)?),
        ])?;
    }
    file_batch.push([
        BorrowedSqlValue::Text(&file.path),
        BorrowedSqlValue::Text("indexed"),
        BorrowedSqlValue::Text(&file.digest),
        BorrowedSqlValue::Integer(i64::try_from(file.bytes)?),
        BorrowedSqlValue::Text(&file.language),
        BorrowedSqlValue::Integer(i64::from(is_test(&file.path))),
        BorrowedSqlValue::Integer(0),
        BorrowedSqlValue::Integer(stable_hash64(&file.path)),
    ])?;
    Ok(owner_id)
}

fn persist_files_impl<'data>(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    params: PersistFilesParams<'data, '_>,
) -> Result<FilePersistenceTimings> {
    let PersistFilesParams {
        entries,
        facts,
        mut loader,
        graph_ranks,
        dirty_fts,
        inventory_ready,
    } = params;
    let bulk_search = dirty_fts.is_none();
    let initial_node_id: i64 =
        tx.query_row("SELECT coalesce(max(node_id),0)FROM nodes", [], |r| {
            r.get(0)
        })?;
    let mut encode_facts_ms = 0.0;
    let mut local_facts_leaves = std::collections::BTreeMap::new();

    let mut shared_owner_batch = MultiValueBatch::<4>::new(
        tx,
        "INSERT OR IGNORE INTO shared_owners(kind_id,key_hash,owner_id,ordinal) VALUES",
    )?;
    let mut shared_key_cache = if bulk_search {
        SharedKeyCache::new_batched(tx)?
    } else {
        SharedKeyCache::new_batched_delta(tx)?
    };
    let mut stmt_doc_search = (!bulk_search && dirty_fts.as_ref().is_none_or(|d| d.doc_search))
        .then(|| {
            tx.prepare(
                "INSERT INTO doc_search(rowid,section_title,content,invariants)SELECT rowid,section_title,content,invariants FROM doc_sections WHERE path=?1",
            )
        })
        .transpose()?;
    let mut stmt_errors = tx.prepare("INSERT INTO errors VALUES(?1,?2,?3)")?;

    let mut source_inventory_batch = (!inventory_ready)
        .then(|| {
            MultiValueBatch::<4, BorrowedSqlValue<'_>>::new(
                tx,
                "INSERT INTO source_inventory VALUES",
            )
        })
        .transpose()?;
    let local_facts_batch_rows = multi_value_batch_rows::<6>(tx)?.min(COLD_LOCAL_FACTS_BATCH_ROWS);
    let mut insert_local_facts_writer = loader
        .is_none()
        .then(|| LocalFactsBatchWriter::new(tx))
        .transpose()?;
    let mut replace_local_facts_writer: Option<LocalFactsBatchWriter<'_>> = None;
    let mut file_batch =
        MultiValueBatch::<8, BorrowedSqlValue<'_>>::new(tx, "INSERT INTO files VALUES")?;
    let mut owned_search_batch = Vec::<SearchRow>::new();
    let mut reusable_search_buffers = Vec::<String>::new();
    let suffix = if dirty_fts.as_ref().is_some_and(|d| !d.doc_search) {
        " ON CONFLICT(doc_id) DO UPDATE SET path=excluded.path,section_title=excluded.section_title,doc_type=excluded.doc_type,content=excluded.content,invariants=excluded.invariants,referenced_symbols=excluded.referenced_symbols,mtime=excluded.mtime,size=excluded.size,is_invariant=excluded.is_invariant"
    } else {
        ""
    };
    let mut doc_sections_batch = MultiValueBatch::<10, BorrowedSqlValue<'static>>::with_suffix(
        tx,
        "INSERT INTO doc_sections VALUES",
        suffix,
    )?;
    let mut shared_owner_rows = Vec::<[i64; 4]>::new();
    let mut node_domain = NodeDomain::new();
    let owned_search_batch_rows = multi_value_batch_rows::<2>(tx)?;
    let update_node_search = dirty_fts.as_ref().is_none_or(|d| d.node_search);

    let mut node_id = initial_node_id;
    let mut docs_count = 0usize;
    let mut fts_insert_ms = 0.0;
    let mut node_row_insert_ms = bulk_search.then_some(0.0);

    for file_chunk in entries.chunks(local_facts_batch_rows) {
        let mut loaded = loader
            .as_mut()
            .map(|loader| loader(tx, file_chunk, facts))
            .transpose()?;
        let mut encoded_chunk =
            encode_chunk_local_facts(file_chunk, facts, loaded.as_mut(), &mut encode_facts_ms)?;
        let mut prepared_node_details =
            prepare_chunk_node_details(file_chunk, facts, loaded.as_deref(), graph_ranks)?;
        let mut node_batch =
            MultiValueBatch::<14, BorrowedSqlValue<'_>>::new(tx, "INSERT INTO nodes VALUES")?;
        for (chunk_index, file) in file_chunk.iter().enumerate() {
            let f = facts.get(&file.path).context("missing extracted facts")?;
            let (search_texts, prepared_references, doc_bodies) =
                if let Some(loaded) = loaded.as_mut() {
                    let item = &mut loaded[chunk_index];
                    (
                        Some(&item.search_texts),
                        Some(&item.reference_keys),
                        Some(&mut item.doc_bodies),
                    )
                } else {
                    (None, None, None)
                };
            validate_staged_file_counts(&file.path, f, search_texts, doc_bodies.as_deref())?;
            let owner_id = persist_file_record(
                tx,
                path_cache,
                &mut file_batch,
                source_inventory_batch.as_mut(),
                file,
            )?;
            persist_local_facts_entry(
                tx,
                file,
                &mut encoded_chunk[chunk_index],
                &mut local_facts_leaves,
                &mut insert_local_facts_writer,
                &mut replace_local_facts_writer,
            )?;
            let node_id_start = node_id;
            node_id = node_id
                .checked_add(i64::try_from(f.nodes.len())?)
                .context("node ID overflow")?;
            for (node_index, details_bytes) in
                std::mem::take(&mut prepared_node_details[chunk_index])
            {
                let n = &f.nodes[node_index];
                let row_node_id = node_id_start
                    .checked_add(i64::try_from(node_index + 1)?)
                    .context("node ID overflow")?;
                let search_id = stable_hash64(&n.id);
                let node_path_id = if n.path == file.path {
                    owner_id
                } else {
                    path_cache.get_or_insert(tx, &n.path)?
                };
                node_domain.push(&file.path, n, &details_bytes)?;
                let started = Instant::now();
                node_batch.push([
                    BorrowedSqlValue::Integer(row_node_id),
                    BorrowedSqlValue::Text(n.id.as_str()),
                    BorrowedSqlValue::Text(n.kind.as_str()),
                    BorrowedSqlValue::Text(n.name.as_str()),
                    BorrowedSqlValue::Text(n.qualname.as_str()),
                    BorrowedSqlValue::Integer(node_path_id),
                    BorrowedSqlValue::Integer(i64::try_from(n.line)?),
                    BorrowedSqlValue::Integer(i64::try_from(n.end_line)?),
                    BorrowedSqlValue::Integer(i64::from(n.is_test)),
                    BorrowedSqlValue::Text(n.language.as_str()),
                    BorrowedSqlValue::Integer(i64::from(n.generated)),
                    BorrowedSqlValue::OwnedJsonText(details_bytes),
                    BorrowedSqlValue::Integer(search_id),
                    BorrowedSqlValue::Integer(owner_id),
                ])?;
                if let Some(total_ms) = node_row_insert_ms.as_mut() {
                    *total_ms += started.elapsed().as_secs_f64() * 1000.;
                }
                if update_node_search {
                    let mut search = reusable_search_buffers.pop().unwrap_or_default();
                    if let Some(prepared) = search_texts {
                        let text = prepared
                            .get(node_index)
                            .context("prepared search text index out of bounds")?;
                        search.clear();
                        search.push_str(text);
                    } else {
                        write_node_search_text(&mut search, n, f);
                    }
                    push_node_search_row(
                        tx,
                        &mut owned_search_batch,
                        &mut reusable_search_buffers,
                        owned_search_batch_rows,
                        &mut fts_insert_ms,
                        search_id,
                        search,
                    )?;
                }
                let key_hash = shared_key_cache.get_or_insert_node(&n.qualname, row_node_id)?;
                shared_owner_rows.push([
                    commitments::SharedOwnerKind::Symbol as i64,
                    key_hash,
                    owner_id,
                    row_node_id,
                ]);
            }
            persist_file_owners(
                f,
                owner_id,
                prepared_references,
                &mut shared_key_cache,
                &mut shared_owner_rows,
                &mut shared_owner_batch,
            )?;
            persist_file_docs_and_errors(
                file,
                f,
                doc_bodies,
                &mut doc_sections_batch,
                &mut stmt_doc_search,
                &mut stmt_errors,
                &mut docs_count,
            )?;
        }
        let started = Instant::now();
        node_batch.flush()?;
        let insert_ms = started.elapsed().as_secs_f64() * 1000.;
        node_row_insert_ms = node_row_insert_ms.map(|total| total + insert_ms);
    }

    if let Some(writer) = insert_local_facts_writer.as_mut() {
        writer.flush()?;
    }
    if let Some(writer) = replace_local_facts_writer.as_mut() {
        writer.flush()?;
    }
    doc_sections_batch.flush()?;
    let node_leaves = node_domain.finish()?;
    file_batch.flush()?;
    fts_insert_ms +=
        flush_owned_search_batch(tx, &mut owned_search_batch, &mut reusable_search_buffers)?;
    if let Some(batch) = source_inventory_batch.as_mut() {
        batch.flush()?;
    }

    shared_key_cache.flush()?;
    shared_owner_batch.flush()?;
    Ok(FilePersistenceTimings {
        node_id,
        node_row_count: usize::try_from(node_id - initial_node_id)?,
        docs_count,
        fts_insert_ms,
        node_row_insert_ms,
        encode_facts_ms,
        local_facts_batch_rows,
        local_facts_leaves,
        node_leaves,
    })
}

pub(super) fn write_node_search_text(search: &mut String, node: &Node, facts: &TypedFacts) {
    search.clear();
    append_node_search_text(search, node, facts);
}

pub(super) fn append_node_search_text(search: &mut String, node: &Node, facts: &TypedFacts) {
    let doc = node
        .detail_get(NodeDetailKey::DOC)
        .and_then(Value::as_str)
        .unwrap_or("");
    search.reserve(node.name.len() + node.qualname.len() + node.path.len() + doc.len() + 16);
    search.push_str(&node.name);
    search.push(' ');
    search.push_str(&node.qualname);
    search.push(' ');
    search.push_str(&node.path);
    if !doc.is_empty() {
        search.push(' ');
        search.push_str(doc);
    }
    if matches!(node.kind.as_str(), "function" | "method" | "constructor") {
        for (name, ty) in node.parameter_types() {
            if let Some(ty) = ty {
                search.push(' ');
                search.push_str(name);
                search.push(':');
                search.push_str(ty);
                write_type_tokens(search, "acddtakes", ty);
            }
        }
        if let Some(ty) = declared_return_type(node, facts) {
            search.push_str(" returns:");
            search.push_str(&ty);
            write_type_tokens(search, "acddreturns", &ty);
        }
    }
    if let Some(decorators) = node
        .detail_get(NodeDetailKey::DECORATORS)
        .and_then(Value::as_array)
    {
        for decorator in decorators.iter().filter_map(Value::as_str) {
            search.push(' ');
            search.push_str(decorator);
        }
    }
    if node.kind == crate::models::tags::NodeKind::Module {
        let mut expressions: Vec<_> = facts
            .references
            .iter()
            .map(|reference| reference.expression.as_str())
            .collect();
        if expressions.len() >= 8192 {
            expressions.par_sort_unstable();
        } else {
            expressions.sort_unstable();
        }
        expressions.dedup();
        for expression in expressions {
            search.push(' ');
            search.push_str(expression);
        }
    }
}

fn write_type_tokens(search: &mut String, prefix: &str, text: &str) {
    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        search.push(' ');
        search.push_str(prefix);
        search.push_str(token);
    }
}

fn signature_return_annotation<'a>(signature: &'a str, language: &str) -> Option<&'a str> {
    let mut depth = 0usize;
    let close_offset = signature.char_indices().find_map(|(offset, ch)| match ch {
        '(' => {
            depth += 1;
            None
        }
        ')' if depth > 0 => {
            depth -= 1;
            (depth == 0).then_some(offset)
        }
        _ => None,
    })?;
    let tail = signature[close_offset + 1..].trim_start();
    let annotation = tail.strip_prefix("->").or_else(|| {
        (language == "typescript" || language == "javascript")
            .then(|| tail.strip_prefix(':'))
            .flatten()
    })?;
    let trimmed = annotation
        .split_once(" where ")
        .map_or(annotation, |(result, _)| result)
        .trim()
        .trim_end_matches(':')
        .trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn declared_return_type<'a>(node: &'a Node, facts: &TypedFacts) -> Option<Cow<'a, str>> {
    if let Some(signature) = node
        .detail_get(NodeDetailKey::SIGNATURE)
        .and_then(Value::as_str)
    {
        if let Some(annotation) = signature_return_annotation(signature, node.language.as_str()) {
            return Some(Cow::Borrowed(annotation));
        }
    }
    let flow = facts.flows.get(node)?;
    let ty = flow
        .return_type
        .as_ref()
        .or(flow.async_return_type.as_ref())?;
    let mut output = String::new();
    write_return_type(&mut output, ty);
    (!output.is_empty()).then_some(Cow::Owned(output))
}

fn write_return_type(output: &mut String, ty: &crate::semantic::TypeExpr) {
    use crate::semantic::TypeExpr;
    match ty {
        TypeExpr::Named { name } | TypeExpr::Applied { base: name, .. } => {
            output.push_str(name);
        }
        TypeExpr::RustRange { item } | TypeExpr::RustArray { item } => {
            write_return_type(output, item)
        }
        TypeExpr::RustCallArguments { args } => {
            for arg in args {
                write_return_type(output, arg);
            }
        }
        TypeExpr::Unknown => {}
    }
    if let TypeExpr::Applied { args, .. } = ty {
        output.push('<');
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                output.push_str(", ");
            }
            write_return_type(output, arg);
        }
        output.push('>');
    }
}

struct SearchRow {
    search_id: i64,
    text: String,
}

fn flush_owned_search_batch(
    tx: &Connection,
    batch: &mut Vec<SearchRow>,
    reusable_buffers: &mut Vec<String>,
) -> Result<f64> {
    batch.sort_unstable_by_key(|row| row.search_id);
    let started = Instant::now();
    let mut rows = MultiValueBatch::<2, BorrowedSqlValue<'_>>::new(
        tx,
        "INSERT INTO node_search(rowid,search_text) VALUES",
    )?;
    for row in batch.iter() {
        rows.push([
            BorrowedSqlValue::Integer(row.search_id),
            BorrowedSqlValue::Text(&row.text),
        ])?;
    }
    rows.flush()?;
    drop(rows);
    let fts_ms = started.elapsed().as_secs_f64() * 1000.;
    for row in batch.drain(..) {
        reusable_buffers.push(row.text);
    }
    Ok(fts_ms)
}

pub(super) struct CachedNodePath {
    path: Arc<str>,
    node_hash: i64,
    path_hash: i64,
}

impl CachedNodePath {
    pub(super) fn new(id: &str, path: String) -> Self {
        Self::from_shared(id, Arc::<str>::from(path))
    }

    pub(super) fn from_shared(id: &str, path: Arc<str>) -> Self {
        Self {
            node_hash: stable_hash64(id),
            path_hash: stable_hash64(&path),
            path,
        }
    }
}

pub(super) fn graph_endpoint_hashes(
    node_paths: &mut hashbrown::HashMap<Cow<'_, str>, Option<CachedNodePath>>,
    stmt_select_path: &mut rusqlite::Statement<'_>,
    endpoint: &str,
    owner: &str,
    kind: &str,
) -> Result<(i64, Option<i64>)> {
    if let Some(node) = node_paths.get(endpoint) {
        let node = node
            .as_ref()
            .with_context(|| format!("edge {kind} has missing endpoint {endpoint}"))?;
        return Ok((
            node.node_hash,
            (node.path.as_ref() != owner).then_some(node.path_hash),
        ));
    }
    let path: Option<String> = stmt_select_path
        .query_row([endpoint], |row| row.get(0))
        .optional()?;
    let node = node_paths
        .entry(Cow::Owned(endpoint.to_owned()))
        .or_insert_with(|| path.map(|path| CachedNodePath::new(endpoint, path)));
    let node = node
        .as_ref()
        .with_context(|| format!("edge {kind} has missing endpoint {endpoint}"))?;
    Ok((
        node.node_hash,
        (node.path.as_ref() != owner).then_some(node.path_hash),
    ))
}

struct StoredEdge<'a> {
    src: i64,
    dst: i64,
    kind: &'a str,
    path_id: i64,
    line: usize,
    evidence: i64,
    confidence: i64,
    count: i64,
    first: usize,
}

fn prepare_dependencies<'a>(
    graph: &'a CompactGraph,
    facts: &'a BTreeMap<String, TypedFacts>,
    dependency_owners: Option<&std::collections::BTreeSet<String>>,
    resolved_edge_paths: &[(Option<i64>, Option<i64>)],
) -> Result<Vec<PreparedDependency<'a>>> {
    anyhow::ensure!(
        graph.edges.len() == resolved_edge_paths.len(),
        "missing resolved edge endpoint paths"
    );
    let mut dependencies = Vec::new();
    for (edge, (src, dst)) in graph.edges.iter().zip(resolved_edge_paths) {
        for target in [src, dst].into_iter().flatten() {
            dependencies.push(PreparedDependency {
                owner: edge.path.as_ref(),
                target_hash: Some(*target),
                kind: edge.kind.as_str(),
                symbol: edge.evidence.as_str(),
                resolution: "resolved",
            });
        }
    }
    for row in &graph.coverage {
        if row.status != "resolved" {
            dependencies.push(PreparedDependency {
                owner: row.path.as_ref(),
                target_hash: None,
                kind: "unresolved",
                symbol: &row.expression,
                resolution: row.status.as_str(),
            });
        }
    }
    #[cfg(feature = "lang-html")]
    for (path, file) in facts {
        if dependency_owners.is_some_and(|owners| !owners.contains(path)) {
            continue;
        }
        for node in file.nodes.iter().filter(|node| {
            node.language == crate::models::tags::LanguageId::Python
                && matches!(node.kind.as_str(), "module" | "function" | "method")
        }) {
            let Some(flow) = file.flows.get(node) else {
                continue;
            };
            for context in flow.template_contexts() {
                let Some(target) = context.target_path.as_deref() else {
                    continue;
                };
                if acdd_core::config::workspace_path(path).0
                    == acdd_core::config::workspace_path(target).0
                {
                    dependencies.push(PreparedDependency {
                        owner: path,
                        target_hash: Some(stable_hash64(target)),
                        kind: "renders",
                        symbol: &context.target,
                        resolution: "resolved",
                    });
                }
            }
        }
    }
    #[cfg(not(feature = "lang-html"))]
    let _ = (facts, dependency_owners);
    dependencies.sort_unstable();
    dependencies.dedup();
    Ok(dependencies)
}

fn persist_dependencies(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    graph: &CompactGraph,
    facts: &BTreeMap<String, TypedFacts>,
    dependency_owners: Option<&std::collections::BTreeSet<String>>,
    resolved_edge_paths: &[(Option<i64>, Option<i64>)],
    prepared: Option<&PreparedGraph<'_>>,
) -> Result<()> {
    let generated;
    let dependencies = if let Some(prepared) = prepared {
        &prepared.dependencies
    } else {
        generated = prepare_dependencies(graph, facts, dependency_owners, resolved_edge_paths)?;
        &generated
    };
    let mut batch =
        MultiValueBatch::<6, BorrowedSqlValue<'_>>::new(tx, "INSERT INTO dependencies VALUES")?;
    for rows in dependencies.chunk_by(|a, b| a.owner == b.owner) {
        let owner = path_cache.get_or_insert(tx, rows[0].owner)?;
        for (ordinal, row) in rows.iter().enumerate() {
            batch.push([
                BorrowedSqlValue::Integer(owner),
                BorrowedSqlValue::Integer(
                    i64::try_from(ordinal).context("dependency ordinal overflow")?,
                ),
                row.target_hash
                    .map_or(BorrowedSqlValue::Null, BorrowedSqlValue::Integer),
                BorrowedSqlValue::Text(row.kind),
                BorrowedSqlValue::Text(row.symbol),
                BorrowedSqlValue::Text(row.resolution),
            ])?;
        }
    }
    batch.flush()
}

pub(super) struct GraphPersistenceTimings {
    pub(super) occurrences: f64,
    pub(super) edges: f64,
    pub(super) dictionaries: f64,
    pub(super) coverage: f64,
}

pub(super) struct PersistGraphParams<'a> {
    pub(super) graph: &'a CompactGraph,
    pub(super) facts: &'a BTreeMap<String, TypedFacts>,
    pub(super) dependency_owners: Option<&'a std::collections::BTreeSet<String>>,
    pub(super) materialize_edges: bool,
}

pub(super) fn persist_graph(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    node_paths: &mut hashbrown::HashMap<Cow<'_, str>, Option<CachedNodePath>>,
    params: PersistGraphParams<'_>,
    known_file_entries: Option<&[FileEntry]>,
) -> Result<GraphPersistenceTimings> {
    let coverage = PreparedCoverage::new(params.graph, params.facts, known_file_entries);
    persist_graph_prepared(tx, path_cache, node_paths, params, &coverage, None)
}

type EdgeOccurrencesOutput<'a> = (Vec<StoredEdge<'a>>, Vec<(Option<i64>, Option<i64>)>, f64);

fn persist_edge_occurrences_phase<'a>(
    tx: &Connection,
    graph: &'a CompactGraph,
    evidence_ids: &hashbrown::HashMap<&str, i64>,
    materialize_edges: bool,
    prepared: Option<&PreparedGraph<'a>>,
    path_cache: &mut PathDictionaryCache,
    node_paths: &mut hashbrown::HashMap<Cow<'_, str>, Option<CachedNodePath>>,
) -> Result<EdgeOccurrencesOutput<'a>> {
    let phase = Instant::now();
    let mut ordinals: hashbrown::HashMap<&str, usize> =
        hashbrown::HashMap::with_capacity(graph.edges.len().min(512));
    let mut current_path: Option<&str> = None;
    let mut current_path_id = None;
    let mut current_ordinal = 0usize;
    let mut occurrence_batch =
        MultiValueBatch::<8, BorrowedSqlValue<'_>>::new(tx, "INSERT INTO edge_occurrences VALUES")?;
    let mut unique_edges =
        Vec::<StoredEdge<'a>>::with_capacity(if materialize_edges && prepared.is_none() {
            graph.edges.len()
        } else {
            0
        });
    let mut stmt_select_path = prepared
        .is_none()
        .then(|| {
            tx.prepare_cached(
                "SELECT p.path FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id=?1",
            )
        })
        .transpose()?;
    let mut resolved_edge_paths = Vec::with_capacity(if prepared.is_none() {
        graph.edges.len()
    } else {
        0
    });
    for (first, e) in graph.edges.iter().enumerate() {
        if current_path != Some(e.path.as_ref()) {
            if let Some(previous_path) = current_path {
                ordinals.insert(previous_path, current_ordinal);
            }
            current_path = Some(e.path.as_ref());
            current_path_id = Some(path_cache.get_or_insert(tx, &e.path)?);
            current_ordinal = ordinals.get(e.path.as_ref()).copied().unwrap_or_default();
        }
        let owner_id = current_path_id.context("edge path ID cache is missing the current path")?;
        let evidence_id = *evidence_ids
            .get(e.evidence.as_str())
            .context("missing edge evidence")?;
        let confidence_id = *evidence_ids
            .get(e.confidence.as_str())
            .context("missing edge confidence")?;
        let (src_hash, dst_hash) = if let Some(prepared) = prepared {
            prepared.edge_hashes[first]
        } else {
            let stmt = stmt_select_path
                .as_mut()
                .context("missing edge endpoint path lookup statement")?;
            let (src_hash, src_target) =
                graph_endpoint_hashes(node_paths, stmt, &e.src, &e.path, e.kind.as_str())?;
            let (dst_hash, dst_target) =
                graph_endpoint_hashes(node_paths, stmt, &e.dst, &e.path, e.kind.as_str())?;
            resolved_edge_paths.push((src_target, dst_target));
            (src_hash, dst_hash)
        };
        occurrence_batch.push([
            BorrowedSqlValue::Integer(owner_id),
            BorrowedSqlValue::Integer(i64::try_from(current_ordinal)?),
            BorrowedSqlValue::Integer(src_hash),
            BorrowedSqlValue::Integer(dst_hash),
            BorrowedSqlValue::Text(e.kind.as_str()),
            BorrowedSqlValue::Integer(i64::try_from(e.line)?),
            BorrowedSqlValue::Integer(evidence_id),
            BorrowedSqlValue::Integer(confidence_id),
        ])?;
        current_ordinal = current_ordinal
            .checked_add(1)
            .context("edge occurrence ordinal overflow")?;

        if materialize_edges && prepared.is_none() {
            unique_edges.push(StoredEdge {
                src: src_hash,
                dst: dst_hash,
                kind: e.kind.as_str(),
                path_id: owner_id,
                line: e.line,
                evidence: evidence_id,
                confidence: confidence_id,
                count: 1,
                first,
            });
        }
    }
    occurrence_batch.flush()?;
    Ok((
        unique_edges,
        resolved_edge_paths,
        phase.elapsed().as_secs_f64() * 1000.,
    ))
}

fn persist_unique_edges_phase<'a>(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    evidence_ids: &hashbrown::HashMap<&str, i64>,
    mut unique_edges: Vec<StoredEdge<'a>>,
    prepared: Option<&PreparedGraph<'a>>,
) -> Result<f64> {
    let phase = Instant::now();
    let mut edge_batch = MultiValueBatch::<8, BorrowedSqlValue<'_>>::new(
        tx,
        "INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES",
    )?;
    if let Some(prepared) = prepared {
        for row in &prepared.unique_edges {
            let edge = row.edge;
            edge_batch.push([
                BorrowedSqlValue::Integer(row.src_hash),
                BorrowedSqlValue::Text(edge.kind.as_str()),
                BorrowedSqlValue::Integer(row.dst_hash),
                BorrowedSqlValue::Integer(path_cache.get_or_insert(tx, &edge.path)?),
                BorrowedSqlValue::Integer(i64::try_from(edge.line)?),
                BorrowedSqlValue::Integer(
                    *evidence_ids
                        .get(edge.evidence.as_str())
                        .context("missing edge evidence")?,
                ),
                BorrowedSqlValue::Integer(
                    *evidence_ids
                        .get(edge.confidence.as_str())
                        .context("missing edge confidence")?,
                ),
                BorrowedSqlValue::Integer(row.count),
            ])?;
        }
    } else {
        if unique_edges.len() >= 8192 {
            unique_edges
                .par_sort_unstable_by_key(|edge| (edge.src, edge.kind, edge.dst, edge.first));
        } else {
            unique_edges.sort_unstable_by_key(|edge| (edge.src, edge.kind, edge.dst, edge.first));
        }
        unique_edges.dedup_by(|later, first| {
            if (later.src, later.kind, later.dst) == (first.src, first.kind, first.dst) {
                first.count += later.count;
                true
            } else {
                false
            }
        });
        for edge in unique_edges {
            edge_batch.push([
                BorrowedSqlValue::Integer(edge.src),
                BorrowedSqlValue::Text(edge.kind),
                BorrowedSqlValue::Integer(edge.dst),
                BorrowedSqlValue::Integer(edge.path_id),
                BorrowedSqlValue::Integer(i64::try_from(edge.line)?),
                BorrowedSqlValue::Integer(edge.evidence),
                BorrowedSqlValue::Integer(edge.confidence),
                BorrowedSqlValue::Integer(edge.count),
            ])?;
        }
    }
    edge_batch.flush()?;
    Ok(phase.elapsed().as_secs_f64() * 1000.)
}

pub(super) fn persist_graph_prepared(
    tx: &Connection,
    path_cache: &mut PathDictionaryCache,
    node_paths: &mut hashbrown::HashMap<Cow<'_, str>, Option<CachedNodePath>>,
    params: PersistGraphParams<'_>,
    coverage: &PreparedCoverage<'_>,
    prepared: Option<&PreparedGraph<'_>>,
) -> Result<GraphPersistenceTimings> {
    let PersistGraphParams {
        graph,
        facts,
        dependency_owners,
        materialize_edges,
    } = params;
    let phase = Instant::now();
    let evidence_ids = CoverageDictionaryCache::prefill_evidence(tx, graph)?;
    let evidence_dictionary_ms = phase.elapsed().as_secs_f64() * 1000.;
    let (unique_edges, resolved_edge_paths, occurrences) = persist_edge_occurrences_phase(
        tx,
        graph,
        &evidence_ids,
        materialize_edges,
        prepared,
        path_cache,
        node_paths,
    )?;
    let edges = if materialize_edges {
        persist_unique_edges_phase(tx, path_cache, &evidence_ids, unique_edges, prepared)?
    } else {
        0.0
    };
    let phase = Instant::now();
    let expression_ids =
        CoverageDictionaryCache::prefill_expressions(tx, graph, materialize_edges)?;
    let dictionaries = evidence_dictionary_ms + phase.elapsed().as_secs_f64() * 1000.;
    let phase = Instant::now();
    let mut coverage_batch = MultiValueBatch::<5, BorrowedSqlValue<'_>>::new(
        tx,
        "INSERT OR IGNORE INTO resolution_coverage(path_id,line,expression_id,status,evidence_id) VALUES",
    )?;
    let mut owner_language_batch = MultiValueBatch::<6, BorrowedSqlValue<'_>>::new(
        tx,
        "INSERT OR IGNORE INTO coverage_owner_language(path_id,line,expression_id,status,evidence_id,language) VALUES",
    )?;
    let coverage_rows = &coverage.rows;
    let mut owned_languages: hashbrown::HashMap<i64, String> = hashbrown::HashMap::new();
    let mut path_ids: hashbrown::HashMap<&str, i64> =
        hashbrown::HashMap::with_capacity(coverage_rows.len().min(facts.len()));
    let mut missing_owners = Vec::new();
    let mut previous_path = None;
    for c in coverage_rows {
        if previous_path != Some(c.path.as_ref()) {
            let owner_id = path_cache.get_or_insert(tx, &c.path)?;
            path_ids.insert(c.path.as_ref(), owner_id);
            previous_path = Some(c.path.as_ref());
            if coverage.file_language(c.path.as_ref()).is_none() {
                missing_owners.push(owner_id);
            }
        }
    }
    if !missing_owners.is_empty() {
        let encoded = serde_json::to_string(&missing_owners)?;
        let mut statement = tx.prepare_cached(
            "SELECT p.path_id, f.language FROM files f JOIN path_dictionary p ON p.path=f.path WHERE p.path_id IN (SELECT value FROM json_each(?1))",
        )?;
        let mut rows = statement.query([encoded.as_str()])?;
        while let Some(row) = rows.next()? {
            let owner_id: i64 = row.get(0)?;
            let language: String = row.get(1)?;
            if !language.is_empty() {
                owned_languages.insert(owner_id, language);
            }
        }
    }

    let mut language_counts: BTreeMap<(i64, &str, &str), i64> = BTreeMap::new();
    let mut current_path = None;
    let mut current_owner_id = None;
    let mut owner_language_counts = Vec::new();
    for c in coverage_rows {
        let owner_id = if current_path == Some(c.path.as_ref()) {
            current_owner_id.context("coverage path ID cache is missing the current path")?
        } else {
            if let Some(previous_owner_id) = current_owner_id {
                flush_owner_language_counts(
                    previous_owner_id,
                    &mut owner_language_counts,
                    &mut language_counts,
                );
            }
            let owner_id = *path_ids
                .get(c.path.as_ref())
                .context("missing preloaded coverage path ID")?;
            current_path = Some(c.path.as_ref());
            current_owner_id = Some(owner_id);
            owner_id
        };
        let expression_id = *expression_ids
            .get(c.expression.as_str())
            .context("missing coverage expression")?;
        let evidence_id = *evidence_ids
            .get(c.evidence.as_ref())
            .context("missing coverage evidence")?;
        coverage_batch.push([
            BorrowedSqlValue::Integer(owner_id),
            BorrowedSqlValue::Integer(i64::try_from(c.line)?),
            BorrowedSqlValue::Integer(expression_id),
            BorrowedSqlValue::Text(c.status.as_str()),
            BorrowedSqlValue::Integer(evidence_id),
        ])?;
        if let Some(language) = coverage.owner_language(c) {
            owner_language_batch.push([
                BorrowedSqlValue::Integer(owner_id),
                BorrowedSqlValue::Integer(i64::try_from(c.line)?),
                BorrowedSqlValue::Integer(expression_id),
                BorrowedSqlValue::Text(c.status.as_str()),
                BorrowedSqlValue::Integer(evidence_id),
                BorrowedSqlValue::Text(language),
            ])?;
        }
        if let Some(item_language) =
            coverage.language(c, owned_languages.get(&owner_id).map(String::as_str))
        {
            if let Some((_, _, count)) =
                owner_language_counts
                    .iter_mut()
                    .find(|(language, status, _)| {
                        *language == item_language && *status == c.status.as_str()
                    })
            {
                *count += 1;
            } else {
                owner_language_counts.push((item_language, c.status.as_str(), 1));
            }
        }
    }
    if let Some(owner_id) = current_owner_id {
        flush_owner_language_counts(owner_id, &mut owner_language_counts, &mut language_counts);
    }

    coverage_batch.flush()?;
    owner_language_batch.flush()?;
    let mut language_count_batch = MultiValueBatch::<4, BorrowedSqlValue<'_>>::new(
        tx,
        "INSERT INTO coverage_language_counts(path_id,language,status,records) VALUES",
    )?;
    for ((path_id, language, status), count) in language_counts {
        language_count_batch.push([
            BorrowedSqlValue::Integer(path_id),
            BorrowedSqlValue::Text(language),
            BorrowedSqlValue::Text(status),
            BorrowedSqlValue::Integer(count),
        ])?;
    }
    language_count_batch.flush()?;
    persist_dependencies(
        tx,
        path_cache,
        graph,
        facts,
        dependency_owners,
        &resolved_edge_paths,
        prepared,
    )?;
    Ok(GraphPersistenceTimings {
        occurrences,
        edges,
        dictionaries,
        coverage: phase.elapsed().as_secs_f64() * 1000.,
    })
}
