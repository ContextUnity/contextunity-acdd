use anyhow::{bail, Result};
use std::{fmt, str::FromStr};

/// Typed overview sections accepted by [`crate::db::reader::overview_with_options`] and host discovery adapters.
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

/// Host discovery aspects accepted by [`crate::db::reader::overview_with_options`] and composed by the discovery adapter.
#[cfg(test)]
pub(crate) const DISCOVERY_OVERVIEW_ASPECTS: [&str; 5] = [
    OverviewAspect::Discovery.as_str(), OverviewAspect::ManifestComponents.as_str(),
    OverviewAspect::DocumentationHubs.as_str(), OverviewAspect::Runs.as_str(),
    OverviewAspect::Milestones.as_str(),
];

/// Typed edge traversal direction for [`crate::db::reader::explain_with_path_options`].
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
    pub page: &'a acdd_core::response::QueryOptions,
}

/// Selects explanation direction, linked documentation, and paging.
pub struct ExplainOptions<'a> {
    /// Incoming, outgoing, or both directions.
    pub direction: ExplainDirection,
    /// Whether linked documentation is included.
    pub show_doc: bool,
    /// Bounded page and generation contract.
    pub page: &'a acdd_core::response::QueryOptions,
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
    pub page: &'a acdd_core::response::QueryOptions,
}
