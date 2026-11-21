//! Read-only code-map SQLite admission, query execution, selectors, and projections.

mod diagnostics;
mod docs;
mod inspect;
mod open;
mod search;
mod selectors;
mod sql;

use acdd_core::response::QueryOptions;
use anyhow::{bail, Result};
use std::{fmt, str::FromStr};

pub use diagnostics::analyze_paged;
pub use docs::{
    document_contents_by_ids, get_doc_paged, resolve_component_prefix, search_docs_with_options,
};
pub use inspect::{
    explain_with_path_options, governing_invariants, governing_invariants_for_scope,
    inspect_paged_with_path, select,
};
pub use open::{
    decode_local_facts, inventory_snapshot, open, open_locked, open_raw_confined_readonly,
    open_with_diagnostics, read_policy, validate_database_header, validate_database_integrity,
    validate_workspace, LockedConnection,
};
pub use search::{documentation_hubs, overview_with_options};
pub use selectors::{directory_path, indexed_path_suggestions, path_bounds};
pub(crate) use selectors::{select_detail, select_detail_with_path};
pub(crate) use selectors::strip_selector_uri_prefix;
pub use sql::rows;
pub(crate) use sql::{rows_with_window_total, QueryBudget, RowMode};
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use sql::rows_with_mode;

/// Typed overview sections accepted by [`overview_with_options`] and host discovery adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverviewAspect {
    Counts,
    Components,
    Languages,
    CoverageByWorkspace,
    Cycles,
    Entrypoints,
    EntrypointsWithTests,
    Topology,
    CompiledProfiles,
    Metadata,
    Discovery,
    ManifestComponents,
    DocumentationHubs,
    Runs,
    Milestones,
}

impl OverviewAspect {
    /// All supported overview aspects in canonical wire order.
    pub const ALL: [Self; 15] = [
        Self::Counts, Self::Components, Self::Languages, Self::CoverageByWorkspace, Self::Cycles,
        Self::Entrypoints, Self::EntrypointsWithTests, Self::Topology, Self::CompiledProfiles,
        Self::Metadata, Self::Discovery, Self::ManifestComponents, Self::DocumentationHubs,
        Self::Runs, Self::Milestones,
    ];

    /// Returns the wire string for this overview aspect.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Counts => "counts", Self::Components => "components", Self::Languages => "languages",
            Self::CoverageByWorkspace => "coverage_by_workspace", Self::Cycles => "cycles",
            Self::Entrypoints => "entrypoints", Self::EntrypointsWithTests => "entrypoints_with_tests",
            Self::Topology => "topology", Self::CompiledProfiles => "compiled_profiles",
            Self::Metadata => "metadata", Self::Discovery => "discovery",
            Self::ManifestComponents => "manifest_components", Self::DocumentationHubs => "documentation_hubs",
            Self::Runs => "runs", Self::Milestones => "milestones",
        }
    }

    /// Returns `true` when this aspect is materialized by host discovery rather than SQLite.
    pub const fn is_discovery(&self) -> bool {
        matches!(self, Self::Discovery | Self::ManifestComponents | Self::DocumentationHubs | Self::Runs | Self::Milestones)
    }

    /// Returns `true` when this aspect is included in the default overview projection.
    pub const fn is_default_selected(&self) -> bool {
        !matches!(self, Self::Cycles | Self::Entrypoints | Self::EntrypointsWithTests | Self::Topology)
    }
}

impl FromStr for OverviewAspect {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::ALL.iter().copied().find(|a| a.as_str().eq_ignore_ascii_case(value)).ok_or_else(|| {
            let allowed = Self::ALL.iter().map(Self::as_str).collect::<Vec<_>>().join(", ");
            anyhow::anyhow!("unsupported overview aspect '{value}'; allowed aspects: {allowed}")
        })
    }
}

/// Database-owned aspects materialized directly by [`overview_with_options`].
pub const DATABASE_OVERVIEW_ASPECTS: [&str; 10] = [
    OverviewAspect::Counts.as_str(), OverviewAspect::Components.as_str(),
    OverviewAspect::Languages.as_str(), OverviewAspect::CoverageByWorkspace.as_str(),
    OverviewAspect::Cycles.as_str(), OverviewAspect::Entrypoints.as_str(),
    OverviewAspect::EntrypointsWithTests.as_str(), OverviewAspect::Topology.as_str(),
    OverviewAspect::CompiledProfiles.as_str(), OverviewAspect::Metadata.as_str(),
];

/// Host discovery aspects accepted by [`overview_with_options`] and composed by the discovery adapter.
pub const DISCOVERY_OVERVIEW_ASPECTS: [&str; 5] = [
    OverviewAspect::Discovery.as_str(), OverviewAspect::ManifestComponents.as_str(),
    OverviewAspect::DocumentationHubs.as_str(), OverviewAspect::Runs.as_str(),
    OverviewAspect::Milestones.as_str(),
];

/// Typed edge traversal direction for [`explain_with_path_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExplainDirection {
    #[default]
    Both,
    Incoming,
    Outgoing,
}

impl ExplainDirection {
    /// Returns the canonical wire string (`"both"`, `"incoming"`, or `"outgoing"`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
        }
    }

    /// Returns `true` when incoming edges are included.
    pub const fn includes_incoming(&self) -> bool {
        matches!(self, Self::Both | Self::Incoming)
    }

    /// Returns `true` when outgoing edges are included.
    pub const fn includes_outgoing(&self) -> bool {
        matches!(self, Self::Both | Self::Outgoing)
    }

    /// Parses an optional direction string, defaulting `None` to [`ExplainDirection::Both`].
    pub fn parse_optional(value: Option<&str>) -> Result<Self> {
        match value {
            None => Ok(Self::Both),
            Some(raw) => raw.parse(),
        }
    }
}

impl FromStr for ExplainDirection {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "both" => Ok(Self::Both),
            "incoming" | "inbound" => Ok(Self::Incoming),
            "outgoing" | "outbound" => Ok(Self::Outgoing),
            _ => bail!("direction must be both, incoming (inbound), or outgoing (outbound)"),
        }
    }
}

/// A selector failure that callers can handle without parsing diagnostic text.
#[derive(Debug)]
pub enum SelectorError {
    /// No indexed target matched the selector.
    NotFound {
        /// The requested selector.
        selector: String,
        /// Indexed directory paths that can narrow the request.
        suggestions: Vec<String>,
    },
    /// More than one indexed target matched the selector.
    Ambiguous {
        /// The requested selector.
        selector: String,
        /// Bounded indexed candidates that can disambiguate the target.
        candidates: Vec<AmbiguousCandidate>,
    },
    /// More than one indexed directory path matched a path alias.
    AmbiguousPaths {
        /// The requested path alias.
        selector: String,
        /// Matching workspace-relative paths.
        candidates: Vec<String>,
    },
    /// The selector cannot be interpreted safely.
    InvalidSyntax {
        /// The requested selector.
        selector: String,
        /// The syntax problem.
        reason: String,
    },
    /// A documentation path was supplied to a code selector.
    DocLink {
        /// The documentation target.
        target: String,
    },
}

/// Indexed metadata for one ambiguous selector candidate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AmbiguousCandidate {
    /// Exact indexed node ID.
    pub id: String,
    /// Workspace-relative file path.
    pub path: String,
    /// Source start line.
    pub line: i64,
    /// Qualified symbol name.
    pub qualname: String,
    /// Indexed declaration signature.
    pub signature: String,
}

impl fmt::Display for SelectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound {
                selector,
                suggestions,
            } if suggestions.is_empty() => write!(formatter, "selector not found: {selector}; use code_map_search to find an indexed symbol id, code_map_overview to inspect indexed paths, or get_doc for Markdown files. File paths are passed without a file: prefix"),
            Self::NotFound {
                selector,
                suggestions,
            } => write!(formatter, "selector not found: {selector}; indexed path suggestions: {}; use a workspace-relative directory or a code_map_search node id", serde_json::to_string(suggestions).map_err(|_| fmt::Error)?),
            Self::Ambiguous {
                selector,
                candidates,
            } => write!(formatter, "ambiguous selector {selector}; use an exact node id or workspace-relative path; candidates: {}", serde_json::to_string(&candidates.iter().map(|candidate| candidate.id.as_str()).collect::<Vec<_>>()).map_err(|_| fmt::Error)?),
            Self::AmbiguousPaths {
                selector,
                candidates,
            } => write!(formatter, "ambiguous selector path {selector}; use a workspace-relative path; candidates: {}", serde_json::to_string(candidates).map_err(|_| fmt::Error)?),
            Self::InvalidSyntax { selector, reason } if selector.trim().is_empty() => write!(formatter, "selector is empty; {reason}"),
            Self::InvalidSyntax { selector, reason } => write!(formatter, "invalid selector {selector}: {reason}"),
            Self::DocLink { target } => write!(formatter, "'{target}' is a Markdown file; use get_doc or search_docs to inspect documentation"),
        }
    }
}

impl std::error::Error for SelectorError {}

/// Error indicating that index schema or semantics changed and a full rebuild is required.
#[derive(Debug)]
pub struct IndexRebuildRequired(pub &'static str);

impl fmt::Display for IndexRebuildRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for IndexRebuildRequired {}

/// Selects overview sections and their common page.
pub struct OverviewOptions<'a> {
    /// Optional set of requested overview sections.
    pub aspects: Option<&'a [String]>,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Selects explanation direction, linked documentation, and paging.
pub struct ExplainOptions<'a> {
    /// Incoming, outgoing, or both directions.
    pub direction: ExplainDirection,
    /// Whether linked documentation is included.
    pub show_doc: bool,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Filters and paging for document search.
pub struct DocSearchOptions<'a> {
    /// Optional document type.
    pub doc_type: Option<&'a str>,
    /// Optional workspace component or alias.
    pub component: Option<&'a str>,
    /// Whether to include bounded matching excerpts.
    pub include_excerpt: bool,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        commitments::{write_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES},
        initialize_code_map_schema, paging,
    };
    use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
    use rusqlite::Connection;
    use serde_json::{json, Value};

    fn page(limit: usize, offset: usize, generation: Option<&str>) -> QueryOptions {
        QueryOptions::resolve(&ResponsePolicy::default(), Some(limit), offset, Some(Detail::Compact), generation.map(str::to_owned)).unwrap()
    }

    #[test]
    fn overview_aspect_and_explain_direction_contract_matrix() {
        assert_eq!(OverviewAspect::ALL.len(), 15);
        for aspect in OverviewAspect::ALL {
            assert_eq!(aspect.as_str().parse::<OverviewAspect>().unwrap(), aspect);
            assert_eq!(aspect.as_str().to_ascii_uppercase().parse::<OverviewAspect>().unwrap(), aspect);
            assert_eq!(aspect.is_discovery(), DISCOVERY_OVERVIEW_ASPECTS.contains(&aspect.as_str()));
        }
        let err = "bogus_aspect".parse::<OverviewAspect>().unwrap_err().to_string();
        assert!(err.contains("unsupported overview aspect 'bogus_aspect'") && OverviewAspect::ALL.iter().all(|a| err.contains(a.as_str())));
        assert_eq!(ExplainDirection::parse_optional(None).unwrap(), ExplainDirection::Both);
        for (raw, exp, inc, out, wire) in [
            ("both", ExplainDirection::Both, true, true, "both"),
            ("incoming", ExplainDirection::Incoming, true, false, "incoming"),
            ("inbound", ExplainDirection::Incoming, true, false, "incoming"),
            ("outgoing", ExplainDirection::Outgoing, false, true, "outgoing"),
            ("outbound", ExplainDirection::Outgoing, false, true, "outgoing"),
        ] {
            let parsed = ExplainDirection::parse_optional(Some(raw)).unwrap();
            assert_eq!((parsed, parsed.includes_incoming(), parsed.includes_outgoing(), parsed.as_str()), (exp, inc, out, wire));
        }
        assert!(ExplainDirection::parse_optional(Some("sideways")).is_err());
    }

    #[test]
    fn rows_with_mode_incremental_byte_accounting_matches_serde_and_enforces_8mib() {
        let conn = Connection::open_in_memory().unwrap();
        let sql = "SELECT NULL n,0 z,-42 neg,9223372036854775807 mxi,-9223372036854775808 mni,3.25 f,'plain' s1,'q\"\\'||char(10)||'🦀' s2,'{\"k\":[1,2]}' details,'raw' invariants,X'00ff10' b,1 dup,2 dup,999 node_hash UNION ALL SELECT 1,2,3,4,5,0.5,'b','c','{\"x\":true}','p',X'abcd',10,20,888";
        for mode in [RowMode::Structural, RowMode::Opaque] {
            let (res, bytes) = sql::rows_with_mode_and_bytes(&conn, sql, &[], 10, mode).unwrap();
            assert_eq!(bytes, serde_json::to_vec(&res).unwrap().len());
            assert!(bytes <= MAX_QUERY_RESULT_BYTES);
            assert_eq!(res[0]["dup"], 2);
            assert_eq!(res[0].get("node_hash").is_none(), mode == RowMode::Structural);
            assert_eq!(res[0]["details"].is_object(), mode == RowMode::Structural);
        }
        let big = "x".repeat(3 * 1024 * 1024);
        let err = rows_with_mode(&conn, "SELECT ?1 a UNION ALL SELECT ?1||'1' UNION ALL SELECT ?1||'2'", &[&big], 10, RowMode::Opaque).unwrap_err().to_string();
        assert!(err.contains("8 MiB row budget"), "{err}");
    }

    #[test]
    fn paging_query_single_pass_handles_empty_ordered_and_past_end_pages() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(&conn, MetadataKey::OutputRoot, "gen-test-0001").unwrap();
        conn.execute_batch("CREATE TABLE t_items(id INTEGER PRIMARY KEY,label TEXT);INSERT INTO t_items VALUES(1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');").unwrap();
        let empty = paging::query(&conn, "SELECT id FROM t_items WHERE id<0", &[], &page(2, 0, None)).unwrap();
        assert_eq!((&empty["total"], &empty["has_more"], &empty["items"]), (&json!(0), &json!(false), &json!([])));
        let cte = "WITH ordered AS (SELECT id,label FROM t_items ORDER BY id DESC) SELECT id,label FROM ordered";
        let p0 = paging::query(&conn, cte, &[], &page(2, 0, None)).unwrap();
        assert_eq!((&p0["total"], &p0["has_more"], &p0["next_offset"], p0["items"][0]["id"].as_i64()), (&json!(5), &json!(true), &json!(2), Some(5)));
        let past_end = paging::query(&conn, cte, &[], &page(2, 10, p0["generation"].as_str())).unwrap();
        assert_eq!((&past_end["total"], &past_end["has_more"], &past_end["items"]), (&json!(5), &json!(false), &Value::Array(Vec::new())));
    }
}
