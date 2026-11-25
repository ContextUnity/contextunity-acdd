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
        Some(match name {
            "source_inventory" => Self::SourceInventory,
            "files" => Self::Files,
            "nodes" => Self::Nodes,
            "edges" => Self::Edges,
            "local_facts" => Self::LocalFacts,
            "edge_occurrences" => Self::EdgeOccurrences,
            "dependencies" => Self::Dependencies,
            "shared_owners" => Self::SharedOwners,
            "errors" => Self::Errors,
            "resolution_coverage" => Self::ResolutionCoverage,
            "coverage_owner_language" => Self::CoverageOwnerLanguage,
            "coverage_language_counts" => Self::CoverageLanguageCounts,
            "doc_sections" => Self::DocSections,
            "node_search" => Self::NodeSearch,
            "doc_search" => Self::DocSearch,
            _ => return None,
        })
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

fn update_sorted_pairs(h: &mut Sha256, pairs: &[(String, String)], prefix_value_len: bool) {
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in sorted {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k);
        if prefix_value_len {
            h.update((v.len() as u64).to_le_bytes());
        }
        h.update(v);
    }
}

/// Computes the Merkle root digest over domain commitments and metadata in canonical order.
pub fn compute_merkle_root(
    domain_commitments: &[(String, String)],
    metadata_entries: &[(String, String)],
) -> String {
    let mut h = Sha256::new();
    h.update(ALGORITHM);
    update_sorted_pairs(&mut h, domain_commitments, false);
    update_sorted_pairs(&mut h, metadata_entries, true);
    hex::encode(h.finalize())
}
