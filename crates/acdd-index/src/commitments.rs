//! Merkle commitment calculations and domain state verification.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Standard hashing algorithm identifier.
pub const ALGORITHM: &str = "acdd-owner-sha256-v2";

/// Logical commitment domains and their stable wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitmentDomain {
    SourceInventory,
    Files,
    Nodes,
    Edges,
    LocalFacts,
    EdgeOccurrences,
    Dependencies,
    SharedOwners,
    Errors,
    ResolutionCoverage,
    CoverageOwnerLanguage,
    CoverageLanguageCounts,
    DocSections,
    NodeSearch,
    DocSearch,
}

impl CommitmentDomain {
    /// Returns the unchanged commitment wire name.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::SourceInventory => "source_inventory",
            Self::Files => "files",
            Self::Nodes => "nodes",
            Self::Edges => "edges",
            Self::LocalFacts => "local_facts",
            Self::EdgeOccurrences => "edge_occurrences",
            Self::Dependencies => "dependencies",
            Self::SharedOwners => "shared_owners",
            Self::Errors => "errors",
            Self::ResolutionCoverage => "resolution_coverage",
            Self::CoverageOwnerLanguage => "coverage_owner_language",
            Self::CoverageLanguageCounts => "coverage_language_counts",
            Self::DocSections => "doc_sections",
            Self::NodeSearch => "node_search",
            Self::DocSearch => "doc_search",
        }
    }

    /// Resolves a stable wire name to its domain.
    pub fn from_name(name: &str) -> Option<Self> {
        use CommitmentDomain::*;
        const ALL: &[CommitmentDomain] = &[
            SourceInventory, Files, Nodes, Edges, LocalFacts, EdgeOccurrences,
            Dependencies, SharedOwners, Errors, ResolutionCoverage,
            CoverageOwnerLanguage, CoverageLanguageCounts, DocSections, NodeSearch, DocSearch,
        ];
        ALL.iter().copied().find(|domain| domain.as_str() == name)
    }
}

/// Full-text search index domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FtsDomain {
    NodeSearch,
    DocSearch,
}

impl FtsDomain {
    /// Returns the unchanged index name.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NodeSearch => "node_search",
            Self::DocSearch => "doc_search",
        }
    }
}

/// The SQLite FTS index backing a commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsCommitmentTable {
    NodeSearch,
    DocSearch,
}

impl FtsCommitmentTable {
    /// Returns the SQLite index name.
    pub const fn table_name(&self) -> &'static str {
        match self {
            Self::NodeSearch => FtsDomain::NodeSearch.as_str(),
            Self::DocSearch => FtsDomain::DocSearch.as_str(),
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "node_search" => Some(Self::NodeSearch),
            "doc_search" => Some(Self::DocSearch),
            _ => None,
        }
    }
}

/// The logical table domain list for code graph commitments.
pub const DOMAINS: &[&str] = &[
    CommitmentDomain::SourceInventory.as_str(),
    CommitmentDomain::Files.as_str(),
    CommitmentDomain::Nodes.as_str(),
    CommitmentDomain::Edges.as_str(),
    CommitmentDomain::LocalFacts.as_str(),
    CommitmentDomain::EdgeOccurrences.as_str(),
    CommitmentDomain::Dependencies.as_str(),
    CommitmentDomain::SharedOwners.as_str(),
    CommitmentDomain::Errors.as_str(),
    CommitmentDomain::ResolutionCoverage.as_str(),
    CommitmentDomain::CoverageOwnerLanguage.as_str(),
    CommitmentDomain::CoverageLanguageCounts.as_str(),
    CommitmentDomain::DocSections.as_str(),
];

/// Returns a lowercase SHA-256 digest of the supplied bytes.
pub fn hash(bytes: &[u8]) -> String {
    acdd_core::format::sha256_hex(bytes)
}

/// Aggregates leaf hashes into a domain-level digest using SHA-256.
pub fn aggregate_domain_digest(table: &str, entries: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(ALGORITHM);
    h.update(table);
    for (k, v) in entries {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k);
        h.update(v);
    }
    hex::encode(h.finalize())
}

/// Computes the Merkle root digest over domain commitments and metadata in canonical order.
pub fn compute_merkle_root(
    domain_commitments: &[(String, String)],
    metadata_entries: &[(String, String)],
) -> String {
    let mut h = Sha256::new();
    h.update(ALGORITHM);
    let mut sorted_domains = domain_commitments.to_vec();
    sorted_domains.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in sorted_domains {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k);
        h.update(v);
    }
    let mut sorted_meta = metadata_entries.to_vec();
    sorted_meta.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in sorted_meta {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k);
        h.update((v.len() as u64).to_le_bytes());
        h.update(v);
    }
    hex::encode(h.finalize())
}
