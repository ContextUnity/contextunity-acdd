//! Graph traversal, directory slice, Git change impact, duplicate detection, and dead-tail queries.

mod change_impact;
mod dead_tails;
mod duplicates;
mod impact;
mod slice;

use super::{paging, reader, symbols};
use crate::models::{
    details::NodeDetailKey,
    tags::{EdgeKind, NodeKind},
};
use acdd_core::response::{CoverageOptions, QueryOptions};
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::str::FromStr;

pub use super::cycles::cycles;
pub use dead_tails::dead_tails_paged;
pub use duplicates::duplicates_paged;
pub use impact::{
    impact_inbound, impact_with_path_options, removal_paged_with_path, traverse_with_path_options,
};
pub(crate) use impact::immediate_links;

/// Typed edge direction for impact and traversal queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImpactDirection {
    /// Follow incoming dependents/callers (`dst_hash -> src_hash`).
    #[default]
    Inbound,
    /// Follow outgoing dependencies/callees (`src_hash -> dst_hash`).
    Outbound,
}

impl ImpactDirection {
    /// Returns the canonical wire string (`"inbound"` or `"outbound"`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    /// Returns `true` for [`ImpactDirection::Inbound`].
    pub const fn is_inbound(&self) -> bool {
        matches!(self, Self::Inbound)
    }

    /// Converts a boolean `inbound` flag into [`ImpactDirection`].
    pub const fn from_inbound(inbound: bool) -> Self {
        if inbound {
            Self::Inbound
        } else {
            Self::Outbound
        }
    }

    /// Parses an optional direction string, defaulting `None` to [`ImpactDirection::Inbound`].
    pub fn parse_optional(value: Option<&str>) -> Result<Self> {
        match value {
            None => Ok(Self::Inbound),
            Some(raw) => raw.parse(),
        }
    }
}

impl FromStr for ImpactDirection {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "inbound" => Ok(Self::Inbound),
            "outbound" => Ok(Self::Outbound),
            _ => bail!("impact direction must be inbound or outbound"),
        }
    }
}

/// Typed relationship mode for graph traversal queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TraversalMode {
    /// Call/invocation and reference dependency graph (default).
    #[default]
    Calls,
    /// Parameter, assignment, and mutation data-flow edges.
    DataFlow,
    /// All dependency edges including structural containment/imports/docs.
    All,
}

impl TraversalMode {
    /// Returns the canonical wire string (`"calls"`, `"data-flow"`, or `"all"`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::DataFlow => "data-flow",
            Self::All => "all",
        }
    }

    /// Parses an optional traversal mode, defaulting unknown or omitted values to [`TraversalMode::Calls`].
    pub fn parse_optional(value: Option<&str>) -> Self {
        match value {
            Some("data-flow") | Some("dataflow") => Self::DataFlow,
            Some("all") => Self::All,
            _ => Self::Calls,
        }
    }
}

impl FromStr for TraversalMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "calls" => Ok(Self::Calls),
            "data-flow" | "dataflow" => Ok(Self::DataFlow),
            "all" => Ok(Self::All),
            _ => bail!("traversal mode must be calls, data-flow, or all"),
        }
    }
}

/// Typed operation dispatched by [`query_with_path_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphQueryOp {
    Overview,
    Inspect,
    Explain,
    Impact,
    Slice,
    Untested,
    Implementations,
    Unwired,
    Sql,
    Doctor,
    Search,
    Duplicates,
    DeadTails,
}

impl FromStr for GraphQueryOp {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "overview" => Ok(Self::Overview),
            "inspect" => Ok(Self::Inspect),
            "explain" => Ok(Self::Explain),
            "impact" => Ok(Self::Impact),
            "slice" => Ok(Self::Slice),
            "untested" => Ok(Self::Untested),
            "implementations" => Ok(Self::Implementations),
            "unwired" => Ok(Self::Unwired),
            "sql" => Ok(Self::Sql),
            "doctor" => Ok(Self::Doctor),
            "search" | "discover" | "find" => Ok(Self::Search),
            "duplicates" => Ok(Self::Duplicates),
            "dead_tails" => Ok(Self::DeadTails),
            _ => bail!("unknown operation; supported: overview,inspect,explain,impact,slice,unwired,sql,untested,implementations,duplicates,dead_tails"),
        }
    }
}

/// Bounds and filters a graph traversal.
pub struct TraversalOptions<'a> {
    /// Maximum graph depth.
    pub depth: u32,
    /// Whether to follow incoming relationships.
    pub inbound: bool,
    /// Optional relationship mode.
    pub mode: Option<&'a str>,
    /// Optional exact relationship kinds.
    pub edge_types: Option<&'a [String]>,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Bounds a generic graph query.
pub struct GraphQueryOptions<'a> {
    /// Maximum graph depth for traversal operations.
    pub depth: u32,
    /// Incoming or outgoing impact direction.
    pub direction: Option<&'a str>,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

pub(super) fn normalize_scope(
    target: Option<&str>,
    path: Option<&str>,
    label: &str,
) -> Result<String> {
    if let (Some(target), Some(path)) = (target, path) {
        anyhow::ensure!(target == path, "{label} conflicts with path");
    }
    let raw = reader::strip_selector_uri_prefix(target.or(path).unwrap_or("").trim())
        .trim_end_matches('/');
    let scope = if raw == "." { "" } else { raw };
    anyhow::ensure!(
        !scope.starts_with('/')
            && !scope
                .split('/')
                .any(|s| s == ".." || s == "." || (s.is_empty() && !scope.is_empty())),
        "{label} must be workspace-relative"
    );
    Ok(scope.to_owned())
}

pub(super) const fn edge_endpoints(direction: ImpactDirection) -> (&'static str, &'static str) {
    if direction.is_inbound() {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    }
}

pub(super) fn page_bounds<'a, T>(items: &'a [T], options: &QueryOptions) -> (usize, &'a [T]) {
    let total = items.len();
    let start = options.offset.min(total);
    let end = start.saturating_add(options.limit).min(total);
    (total, &items[start..end])
}

fn sql_selector(selector: Option<&str>) -> Result<&str> {
    selector
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("sql operation requires a SELECT or WITH selector"))
}

/// Executes graph traversal query with path scoping and graph query options.
pub fn query_with_path_options(
    conn: &Connection,
    operation: &str,
    selector: Option<&str>,
    path: Option<&str>,
    request: &GraphQueryOptions<'_>,
) -> Result<Value> {
    let depth = request.depth;
    let direction = request.direction;
    let coverage = request.coverage;
    let options = request.page;
    let op = operation.trim().parse::<GraphQueryOp>()?;
    match op {
        GraphQueryOp::Overview | GraphQueryOp::Doctor => reader::overview_with_options(
            conn,
            &reader::OverviewOptions {
                aspects: None,
                page: options,
            },
        ),
        GraphQueryOp::Inspect => symbols::inspect_paged_response(
            conn,
            selector.unwrap_or(""),
            path,
            true,
            options,
            coverage,
        ),
        GraphQueryOp::Explain => symbols::explain_paged_response(
            conn,
            selector.unwrap_or(""),
            path,
            &reader::ExplainOptions {
                direction: reader::ExplainDirection::Both,
                show_doc: true,
                page: options,
            },
            coverage,
        ),
        GraphQueryOp::Impact => traverse_with_path_options(
            conn,
            selector.unwrap_or(""),
            path,
            &TraversalOptions {
                depth,
                inbound: ImpactDirection::parse_optional(direction)?.is_inbound(),
                mode: None,
                edge_types: None,
                page: options,
            },
        ),
        GraphQueryOp::Slice => slice::slice_paged(conn, selector.unwrap_or(""), path, depth, options),
        GraphQueryOp::Untested => impact::untested_paged(conn, selector, path, options),
        GraphQueryOp::Implementations => {
            impact::implementations_paged(conn, selector.unwrap_or(""), path, options)
        }
        GraphQueryOp::Unwired => {
            let candidates = format!(
                "n.kind IN('{function}','{method}') AND NOT EXISTS(SELECT 1 FROM edges e WHERE e.dst_hash=n.node_hash AND e.kind='{calls}')",
                function = NodeKind::Function.as_str(),
                method = NodeKind::Method.as_str(),
                calls = EdgeKind::Calls.as_str(),
            );
            let entrypoint_key = NodeDetailKey::ENTRYPOINT;
            let excluded = paging::count(
                conn,
                &format!(
                    "SELECT count(*) FROM nodes n WHERE {candidates} AND json_extract(n.details,'$.{entrypoint_key}') IS NOT NULL"
                ),
                &[],
            )?;
            Ok(json!({
                "nodes": paging::query(
                    conn,
                    &format!(
                        "SELECT {} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE {candidates} AND json_extract(n.details,'$.{entrypoint_key}') IS NULL ORDER BY p.path,n.line,n.id",
                        paging::nodes_with_path("n", options.detail, "p.path")
                    ),
                    &[],
                    options,
                )?,
                "summary": { "excluded_entrypoints_count": excluded },
                "meaning": "no indexed static caller; not a dead-code proof"
            }))
        }
        GraphQueryOp::Sql => reader::analyze_paged(conn, sql_selector(selector)?, None, options),
        GraphQueryOp::Search => symbols::search_with_options(
            conn,
            selector.unwrap_or(""),
            &symbols::SearchOptions {
                kind: None,
                path,
                include_docs: false,
                exact: false,
                page: options,
            },
        ),
        GraphQueryOp::Duplicates => {
            let (target, level) = match selector {
                Some("file") | Some("symbol") => (None, selector),
                _ => (selector, direction),
            };
            duplicates_paged(
                conn,
                target,
                path,
                level,
                (depth > 0 && depth != 2).then_some(depth as usize),
                options,
            )
        }
        GraphQueryOp::DeadTails => dead_tails_paged(conn, selector, path, options),
    }
}

#[cfg(test)]
mod traversal_seam_tests {
    use super::*;
    use crate::db::SCHEMA_DDL;
    use acdd_core::response::{QueryOptions, ResponsePolicy};
    use rusqlite::Connection;

    #[test]
    fn boundary_enums_parse_accepted_and_rejected_values() {
        assert_eq!(ImpactDirection::parse_optional(None).unwrap(), ImpactDirection::Inbound);
        assert_eq!(ImpactDirection::parse_optional(Some("outbound")).unwrap(), ImpactDirection::Outbound);
        assert!(ImpactDirection::parse_optional(Some("sideways")).is_err());

        assert_eq!(TraversalMode::parse_optional(None), TraversalMode::Calls);
        assert_eq!(TraversalMode::parse_optional(Some("dataflow")), TraversalMode::DataFlow);
        assert_eq!(TraversalMode::parse_optional(Some("all")), TraversalMode::All);
        assert!(matches!("dead_tails".parse::<GraphQueryOp>(), Ok(GraphQueryOp::DeadTails)));
        assert!("cypher".parse::<GraphQueryOp>().is_err());
    }

    #[test]
    fn duplicates_paged_symbol_clone_matrix() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "{SCHEMA_DDL}INSERT INTO metadata(key,value) VALUES ('output_root','test-generation'),('workspace_root','/tmp');"
        ))
        .unwrap();
        let options =
            QueryOptions::resolve(&ResponsePolicy::default(), Some(10), 0, None, None).unwrap();
        let result = duplicates_paged(&conn, None, None, None, None, &options).unwrap();
        assert_eq!(result["min_lines"], 5);
    }
}
