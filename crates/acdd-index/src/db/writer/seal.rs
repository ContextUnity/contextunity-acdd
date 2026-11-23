use super::ingest::{PreparedCoverage, PreparedDependency, PreparedGraph};
use super::node_domain::{
    finish_hashers, row_hasher, Leaves, OwnerHashers,
};
use crate::models::{compact_graph::CompactGraph, FileEntry};
use crate::typed_facts::TypedFacts;
use crate::{
    commitments::DOMAINS,
    db::commitments::{
        encode_int as encode_integer, encode_real, encode_text, hash_shared_owner_leaves,
        key as leaf_key, new_leaf_hasher, SharedOwnerKind, SharedOwnerRow,
    },
    models::{is_test, stable_hash64, Node, NodeDetailKey},
};
use anyhow::{ensure, Context, Result};
use hashbrown::HashMap;
use sha2::Digest;
use std::collections::BTreeMap;
use std::time::Instant;

type NodeLookupMaps<'a> = (HashMap<i64, &'a str>, HashMap<&'a str, &'a str>);

#[derive(Default)]
pub(super) struct ProjectionTimings {
    pub(super) file_domains_ms: f64,
    pub(super) node_domain_ms: f64,
    pub(super) graph_edges_ms: f64,
    pub(super) graph_occurrences_ms: f64,
    pub(super) graph_unique_edges_ms: f64,
    pub(super) graph_shared_owners_ms: f64,
    pub(super) graph_coverage_ms: f64,
    pub(super) graph_dependencies_ms: f64,
    pub(super) graph_domains_ms: f64,
}

struct ProjectedNode<'a> {
    owner: &'a str,
    node: &'a Node,
    node_id: i64,
}

struct DependencyLeafRow<'a> {
    target: Option<&'a str>,
    kind: &'a str,
    symbol: &'a str,
    resolution: &'a str,
}

struct GraphProjection<'a> {
    facts: &'a BTreeMap<String, TypedFacts>,
    graph: &'a CompactGraph,
    coverage: &'a PreparedCoverage<'a>,
    projected_nodes: &'a [ProjectedNode<'a>],
    prepared: &'a PreparedGraph<'a>,
    file_by_hash: &'a HashMap<i64, &'a str>,
    entry_languages: &'a HashMap<&'a str, &'a str>,
}

struct OwnerLanguageLeafRow<'a> {
    line: i64,
    expression: &'a str,
    status: &'a str,
    evidence: &'a str,
    language: &'a str,
}

pub(super) fn file_domain_leaves(
    file: &FileEntry,
    file_facts: &TypedFacts,
    reference_keys: &[String],
) -> Result<BTreeMap<crate::commitments::CommitmentDomain, Leaves>> {
    let mut domains = BTreeMap::new();
    let mut docs = OwnerHashers::new();
    hash_doc_sections(&file_facts.docs, &mut docs)?;
    domains.insert(
        crate::commitments::CommitmentDomain::DocSections,
        finish_hashers("doc_sections", docs)?,
    );
    let mut projected_nodes = Vec::with_capacity(file_facts.nodes.len());
    for (idx, node) in file_facts.nodes.iter().enumerate() {
        let node_id = i64::try_from(idx + 1).context("node ID overflow")?;
        projected_nodes.push(ProjectedNode {
            owner: &file.path,
            node,
            node_id,
        });
    }
    domains.insert(
        crate::commitments::CommitmentDomain::SharedOwners,
        project_shared_owner_entries(
            std::iter::once((file.path.as_str(), file_facts)),
            &projected_nodes,
            Some(reference_keys),
        )?,
    );
    Ok(domains)
}

/// Builds in-memory logical commitments that do not require database reads.
/// The `nodes` domain is produced while its exact serialized details are persisted.
pub(super) fn logical_domain_leaves(
    entries: &[FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
    graph: &CompactGraph,
    coverage: &PreparedCoverage<'_>,
    prepared: &PreparedGraph<'_>,
    timings: &mut ProjectionTimings,
    include_restored_file_domains: bool,
) -> Result<BTreeMap<&'static str, Leaves>> {
    let mut domains = BTreeMap::new();
    let started = Instant::now();
    let projected_nodes =
        project_file_domains(entries, facts, &mut domains, include_restored_file_domains)?;
    timings.file_domains_ms = started.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    let (file_by_hash, entry_languages) = node_lookup_maps(entries, facts)?;
    timings.node_domain_ms = started.elapsed().as_secs_f64() * 1000.;
    let started = Instant::now();
    project_graph_domains(
        GraphProjection {
            facts,
            graph,
            coverage,
            projected_nodes: &projected_nodes,
            prepared,
            file_by_hash: &file_by_hash,
            entry_languages: &entry_languages,
        },
        &mut domains,
        timings,
        include_restored_file_domains,
    )?;
    timings.graph_domains_ms = started.elapsed().as_secs_f64() * 1000.;
    for &domain in DOMAINS {
        if domain == "local_facts"
            || domain == "nodes"
            || (!include_restored_file_domains
                && matches!(domain, "doc_sections" | "shared_owners"))
        {
            continue;
        }
        ensure!(
            domains.contains_key(domain),
            "in-memory projection did not produce domain {domain}"
        );
    }
    Ok(domains)
}

fn project_file_domains<'a>(
    entries: &'a [FileEntry],
    facts: &'a BTreeMap<String, TypedFacts>,
    domains: &mut BTreeMap<&'static str, Leaves>,
    include_documents: bool,
) -> Result<Vec<ProjectedNode<'a>>> {
    let mut inventory = OwnerHashers::with_capacity(entries.len());
    let mut files = OwnerHashers::with_capacity(entries.len());
    let mut errors = OwnerHashers::new();
    let mut docs = OwnerHashers::new();
    let mut projected_nodes = if include_documents {
        Vec::with_capacity(facts.values().map(|f| f.nodes.len()).sum())
    } else {
        Vec::new()
    };
    let mut node_id = 0i64;

    for file in entries {
        let file_facts = facts
            .get(&file.path)
            .with_context(|| format!("missing extracted facts for {}", file.path))?;
        let size = i64::try_from(file.bytes).context("file size exceeds SQLite integer range")?;
        let test = i64::from(is_test(&file.path));
        let path_hash = stable_hash64(&file.path);
        {
            let hasher = row_hasher(&mut inventory, "source_inventory", &file.path);
            encode_text(hasher, &file.path);
            encode_text(hasher, "indexed");
            encode_text(hasher, &file.digest);
            encode_integer(hasher, size);
        }
        {
            let hasher = row_hasher(&mut files, "files", &file.path);
            encode_text(hasher, &file.path);
            encode_text(hasher, "indexed");
            encode_text(hasher, &file.digest);
            encode_integer(hasher, size);
            encode_text(hasher, &file.language);
            encode_integer(hasher, test);
            encode_integer(hasher, 0);
            encode_integer(hasher, path_hash);
        }

        let mut file_errors: Vec<_> = file_facts.errors.iter().collect();
        file_errors.sort_unstable_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.message.as_bytes().cmp(right.message.as_bytes()))
        });
        for error in file_errors {
            let line = i64::try_from(error.line).context("diagnostic line exceeds SQLite range")?;
            let hasher = row_hasher(&mut errors, "errors", &error.path);
            encode_text(hasher, &error.path);
            encode_integer(hasher, line);
            encode_text(hasher, &error.message);
        }

        if include_documents {
            hash_doc_sections(&file_facts.docs, &mut docs)?;
            for node in &file_facts.nodes {
                node_id = node_id.checked_add(1).context("node ID overflow")?;
                projected_nodes.push(ProjectedNode {
                    owner: &file.path,
                    node,
                    node_id,
                });
            }
        }
    }

    domains.insert(
        "source_inventory",
        finish_hashers("source_inventory", inventory)?,
    );
    domains.insert("files", finish_hashers("files", files)?);
    domains.insert("errors", finish_hashers("errors", errors)?);
    if include_documents {
        domains.insert("doc_sections", finish_hashers("doc_sections", docs)?);
    }
    Ok(projected_nodes)
}

fn node_lookup_maps<'a>(
    entries: &'a [FileEntry],
    facts: &BTreeMap<String, TypedFacts>,
) -> Result<NodeLookupMaps<'a>> {
    let mut file_by_hash = HashMap::with_capacity(entries.len());
    let mut entry_languages = HashMap::with_capacity(entries.len());
    for file in entries {
        file_by_hash.insert(stable_hash64(&file.path), file.path.as_str());
        if !file.language.is_empty() {
            entry_languages.insert(file.path.as_str(), file.language.as_str());
        }
        ensure!(
            facts.contains_key(&file.path),
            "missing facts for indexed path {}",
            file.path
        );
    }
    Ok((file_by_hash, entry_languages))
}

fn project_graph_domains<'a>(
    projection: GraphProjection<'a>,
    domains: &mut BTreeMap<&'static str, Leaves>,
    timings: &mut ProjectionTimings,
    include_shared_owners: bool,
) -> Result<()> {
    let GraphProjection {
        facts,
        graph,
        coverage,
        projected_nodes,
        prepared,
        file_by_hash,
        entry_languages,
    } = projection;
    let mut ordinals = HashMap::<&str, i64>::with_capacity(file_by_hash.len());
    let mut unique_edges: Vec<_> = prepared.unique_edges.iter().collect();
    let mut occurrence_hashers = OwnerHashers::with_capacity(file_by_hash.len());

    let edges_started = Instant::now();
    for edge in &graph.edges {
        let owner = edge.path.as_ref();
        let ordinal = ordinals.entry(owner).or_default();
        let src = edge.src.as_ref();
        let dst = edge.dst.as_ref();
        let occurrence_hasher = occurrence_hashers
            .entry(owner)
            .or_insert_with(|| new_leaf_hasher("edge_occurrences", owner));
        occurrence_hasher.update([0xff]);
        encode_text(occurrence_hasher, owner);
        encode_integer(occurrence_hasher, *ordinal);
        encode_text(occurrence_hasher, src);
        encode_text(occurrence_hasher, dst);
        encode_text(occurrence_hasher, edge.kind.as_str());
        encode_text(occurrence_hasher, owner);
        encode_integer(
            occurrence_hasher,
            i64::try_from(edge.line).context("edge line exceeds SQLite range")?,
        );
        encode_text(occurrence_hasher, edge.evidence.as_str());
        encode_text(occurrence_hasher, edge.confidence.as_str());
        *ordinal = (*ordinal)
            .checked_add(1)
            .context("edge occurrence ordinal overflow")?;
    }
    drop(ordinals);
    timings.graph_edges_ms = edges_started.elapsed().as_secs_f64() * 1000.;

    let occurrences_finished = Instant::now();
    domains.insert(
        "edge_occurrences",
        finish_hashers("edge_occurrences", occurrence_hashers)?,
    );
    timings.graph_occurrences_ms = occurrences_finished.elapsed().as_secs_f64() * 1000.;

    let (edges_result, (shared_result, (coverage_result, dependency_result))) = rayon::join(
        || -> Result<_> {
            let started = Instant::now();
            unique_edges.sort_unstable_by(|left, right| {
                left.edge
                    .path
                    .cmp(&right.edge.path)
                    .then_with(|| left.edge.src.cmp(&right.edge.src))
                    .then_with(|| left.edge.dst.cmp(&right.edge.dst))
                    .then_with(|| left.edge.kind.as_str().cmp(right.edge.kind.as_str()))
            });
            let mut edge_hashers = OwnerHashers::with_capacity(file_by_hash.len());
            for row in unique_edges {
                let owner = row.edge.path.as_ref();
                let hasher = edge_hashers
                    .entry(owner)
                    .or_insert_with(|| new_leaf_hasher("edges", owner));
                let edge = row.edge;
                hasher.update([0xff]);
                encode_text(hasher, row.edge.src.as_ref());
                encode_text(hasher, row.edge.dst.as_ref());
                encode_text(hasher, edge.kind.as_str());
                encode_text(hasher, owner);
                encode_integer(
                    hasher,
                    i64::try_from(edge.line).context("edge line exceeds SQLite range")?,
                );
                encode_text(hasher, edge.evidence.as_str());
                encode_text(hasher, edge.confidence.as_str());
                encode_integer(hasher, row.count);
            }
            Ok((
                finish_hashers("edges", edge_hashers)?,
                started.elapsed().as_secs_f64() * 1000.,
            ))
        },
        || {
            rayon::join(
                || -> Result<_> {
                    let started = Instant::now();
                    let leaves = if include_shared_owners {
                        Some(project_shared_owner_domain(facts, projected_nodes, None)?)
                    } else {
                        None
                    };
                    Ok((leaves, started.elapsed().as_secs_f64() * 1000.))
                },
                || {
                    rayon::join(
                        || -> Result<_> {
                            let started = Instant::now();
                            let domains = project_coverage_domains(coverage, entry_languages)?;
                            Ok((domains, started.elapsed().as_secs_f64() * 1000.))
                        },
                        || -> Result<_> {
                            let started = Instant::now();
                            let leaves =
                                project_dependency_domain(file_by_hash, &prepared.dependencies)?;
                            Ok((leaves, started.elapsed().as_secs_f64() * 1000.))
                        },
                    )
                },
            )
        },
    );
    let (edge_leaves, edge_ms) = edges_result?;
    domains.insert("edges", edge_leaves);
    timings.graph_unique_edges_ms = edge_ms;
    let (shared, shared_ms) = shared_result?;
    if let Some(shared) = shared {
        domains.insert("shared_owners", shared);
    }
    timings.graph_shared_owners_ms = shared_ms;
    let (coverage_domains, coverage_ms) = coverage_result?;
    domains.insert("resolution_coverage", coverage_domains.0);
    domains.insert("coverage_owner_language", coverage_domains.1);
    domains.insert("coverage_language_counts", coverage_domains.2);
    timings.graph_coverage_ms = coverage_ms;
    let (dependency_leaves, dependency_ms) = dependency_result?;
    domains.insert("dependencies", dependency_leaves);
    timings.graph_dependencies_ms = dependency_ms;
    Ok(())
}

fn hash_doc_sections<'a>(
    file_docs_slice: &'a [crate::models::DocSection],
    docs: &mut OwnerHashers<'a>,
) -> Result<()> {
    if file_docs_slice.is_empty() {
        return Ok(());
    }
    let mut file_docs: Vec<_> = file_docs_slice.iter().collect();
    file_docs.sort_unstable_by(|left, right| left.doc_id.cmp(&right.doc_id));
    for doc in file_docs {
        let size = i64::try_from(doc.size).context("document size exceeds SQLite range")?;
        let invariants = serde_json::to_string(&doc.invariants)?;
        let referenced = serde_json::to_string(&doc.referenced_symbols)?;
        let hasher = row_hasher(docs, "doc_sections", &doc.path);
        encode_text(hasher, &doc.doc_id);
        encode_text(hasher, &doc.path);
        encode_text(hasher, &doc.section_title);
        encode_text(hasher, &doc.doc_type);
        encode_text(hasher, &doc.content);
        encode_text(hasher, &invariants);
        encode_text(hasher, &referenced);
        encode_real(hasher, doc.mtime);
        encode_integer(hasher, size);
        encode_integer(hasher, i64::from(doc.is_invariant));
    }
    Ok(())
}

fn project_shared_owner_domain<'a>(
    facts: &'a BTreeMap<String, TypedFacts>,
    projected_nodes: &[ProjectedNode<'a>],
    prepared_references: Option<&'a [String]>,
) -> Result<Leaves> {
    project_shared_owner_entries(
        facts.iter().map(|(owner, f)| (owner.as_str(), f)),
        projected_nodes,
        prepared_references,
    )
}

fn project_shared_owner_entries<'a>(
    entries: impl Iterator<Item = (&'a str, &'a TypedFacts)>,
    projected_nodes: &[ProjectedNode<'a>],
    prepared_references: Option<&'a [String]>,
) -> Result<Leaves> {
    let mut key_by_hash = HashMap::<i64, &str>::new();
    let mut owner_rows = Vec::<(&str, SharedOwnerKind, i64, i64)>::new();
    let mut id_by_node_id = HashMap::<i64, &str>::new();
    let mut nodes_by_owner = BTreeMap::<&str, Vec<&ProjectedNode<'a>>>::new();
    for node in projected_nodes {
        nodes_by_owner.entry(node.owner).or_default().push(node);
        id_by_node_id.insert(node.node_id, node.node.id.as_str());
    }

    for (owner, file_facts) in entries {
        if let Some(owner_nodes) = nodes_by_owner.get(owner) {
            for projected in owner_nodes {
                let key_hash = register_shared_key(&mut key_by_hash, &projected.node.qualname)?;
                owner_rows.push((
                    owner,
                    SharedOwnerKind::Symbol,
                    key_hash,
                    projected.node_id,
                ));
            }
        }
        let keys = prepared_references.map_or_else(
            || file_facts.reference_keys(),
            |keys| keys.iter().map(String::as_str).collect(),
        );
        for key in keys {
            let key_hash = register_shared_key(&mut key_by_hash, key)?;
            owner_rows.push((owner, SharedOwnerKind::Reference, key_hash, 0));
        }
        if let Some(owner_nodes) = nodes_by_owner.get(owner) {
            for projected in owner_nodes {
                if projected.node.details[NodeDetailKey::DEFAULT_EXPORT] == true {
                    let key_hash = register_shared_key(&mut key_by_hash, &projected.node.id)?;
                    owner_rows.push((owner, SharedOwnerKind::DefaultExport, key_hash, 0));
                }
            }
        }
    }

    owner_rows.sort_unstable();
    owner_rows.dedup();

    let mut by_owner = BTreeMap::<&str, Vec<SharedOwnerRow<'_>>>::new();
    for (owner, kind, key_hash, ordinal) in owner_rows {
        let key = *key_by_hash
            .get(&key_hash)
            .with_context(|| format!("missing projected shared key {key_hash}"))?;
        let node_id = if kind == SharedOwnerKind::Symbol {
            Some(
                *id_by_node_id
                    .get(&ordinal)
                    .with_context(|| format!("missing projected shared symbol node {ordinal}"))?,
            )
        } else {
            None
        };
        by_owner.entry(owner).or_default().push(SharedOwnerRow {
            kind,
            key,
            ordinal,
            node_id,
        });
    }

    Ok(hash_shared_owner_leaves(by_owner))
}

fn register_shared_key<'a>(keys: &mut HashMap<i64, &'a str>, key: &'a str) -> Result<i64> {
    let hash = stable_hash64(key);
    if let Some(existing) = keys.get(&hash) {
        ensure!(*existing == key, "shared key hash collision detected");
    } else {
        keys.insert(hash, key);
    }
    Ok(hash)
}

type CoverageDomains = (Leaves, Leaves, Leaves);
fn project_coverage_domains<'a>(
    prepared: &'a PreparedCoverage<'a>,
    entry_languages: &HashMap<&'a str, &'a str>,
) -> Result<CoverageDomains> {
    let mut coverage_hashers = OwnerHashers::with_capacity(entry_languages.len());
    let mut owner_language_rows = BTreeMap::<&'a str, Vec<OwnerLanguageLeafRow<'a>>>::new();
    let mut language_counts = BTreeMap::<&'a str, BTreeMap<(&'a str, &'a str), i64>>::new();
    for &coverage in &prepared.rows {
        let owner = coverage.path.as_ref();
        let line = i64::try_from(coverage.line).context("coverage line exceeds SQLite range")?;
        let expression = coverage.expression.as_str();
        let status = coverage.status.as_str();
        let evidence = coverage.evidence.as_ref();
        let hasher = row_hasher(&mut coverage_hashers, "resolution_coverage", owner);
        encode_text(hasher, owner);
        encode_integer(hasher, line);
        encode_text(hasher, expression);
        encode_text(hasher, status);
        encode_text(hasher, evidence);

        let html_language = prepared.owner_language(coverage);
        if let Some(language) = html_language {
            owner_language_rows
                .entry(owner)
                .or_default()
                .push(OwnerLanguageLeafRow {
                    line,
                    expression,
                    status,
                    evidence,
                    language,
                });
        }

        let language = prepared.language(coverage, entry_languages.get(owner).copied());
        if let Some(language) = language {
            let count = language_counts
                .entry(owner)
                .or_default()
                .entry((language, status))
                .or_default();
            *count = count.checked_add(1).context("coverage count overflow")?;
        }
    }

    let mut owner_language = Leaves::new();
    for (owner, mut rows) in owner_language_rows {
        rows.sort_unstable_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.expression.as_bytes().cmp(right.expression.as_bytes()))
                .then_with(|| left.status.as_bytes().cmp(right.status.as_bytes()))
                .then_with(|| left.evidence.as_bytes().cmp(right.evidence.as_bytes()))
                .then_with(|| left.language.as_bytes().cmp(right.language.as_bytes()))
        });
        let mut hasher = new_leaf_hasher("coverage_owner_language", owner);
        for row in rows {
            hasher.update([0xff]);
            encode_text(&mut hasher, owner);
            encode_integer(&mut hasher, row.line);
            encode_text(&mut hasher, row.expression);
            encode_text(&mut hasher, row.status);
            encode_text(&mut hasher, row.evidence);
            encode_text(&mut hasher, row.language);
        }
        owner_language.insert(
            leaf_key("coverage_owner_language", owner),
            hex::encode(hasher.finalize()),
        );
    }

    let mut language_count_leaves = Leaves::new();
    for (owner, counts) in language_counts {
        let mut hasher = new_leaf_hasher("coverage_language_counts", owner);
        for ((language, status), count) in counts {
            hasher.update([0xff]);
            encode_text(&mut hasher, owner);
            encode_text(&mut hasher, language);
            encode_text(&mut hasher, status);
            encode_integer(&mut hasher, count);
        }
        language_count_leaves.insert(
            leaf_key("coverage_language_counts", owner),
            hex::encode(hasher.finalize()),
        );
    }

    Ok((
        finish_hashers("resolution_coverage", coverage_hashers)?,
        owner_language,
        language_count_leaves,
    ))
}

fn project_dependency_domain<'a>(
    file_by_hash: &'a HashMap<i64, &'a str>,
    dependencies: &[PreparedDependency<'a>],
) -> Result<Leaves> {
    let mut leaves = Leaves::new();
    let mut scratch_rows: Vec<DependencyLeafRow<'_>> = Vec::new();
    let mut idx = 0;
    while idx < dependencies.len() {
        let owner = dependencies[idx].owner;
        let group_start = idx;
        while idx < dependencies.len() && dependencies[idx].owner == owner {
            idx += 1;
        }
        let group = &dependencies[group_start..idx];

        scratch_rows.clear();
        scratch_rows.reserve(group.len());
        for dependency in group {
            let target = dependency
                .target_hash
                .and_then(|hash| file_by_hash.get(&hash).copied());
            scratch_rows.push(DependencyLeafRow {
                target,
                kind: dependency.kind,
                symbol: dependency.symbol,
                resolution: dependency.resolution,
            });
        }

        scratch_rows.sort_unstable_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.kind.cmp(right.kind))
                .then_with(|| left.symbol.cmp(right.symbol))
                .then_with(|| left.resolution.cmp(right.resolution))
        });
        let mut hasher = new_leaf_hasher("dependencies", owner);
        for row in &scratch_rows {
            hasher.update([0xff]);
            encode_text(&mut hasher, owner);
            if let Some(target) = row.target {
                encode_text(&mut hasher, target);
            } else {
                hasher.update(b"n");
            }
            encode_text(&mut hasher, row.kind);
            encode_text(&mut hasher, row.symbol);
            encode_text(&mut hasher, row.resolution);
        }
        leaves.insert(
            leaf_key("dependencies", owner),
            hex::encode(hasher.finalize()),
        );
    }
    Ok(leaves)
}
