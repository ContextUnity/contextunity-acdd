use super::super::{paging, reader};
use crate::models::tags::EdgeKind;
use acdd_core::response::{CoverageOptions, QueryOptions};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::str::FromStr;

pub(super) const MAX_DEEP_TRAVERSAL_FRONTIER: usize = 1000;

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

/// Typed operation dispatched by [`super::query_with_path_options`].
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

pub(super) fn decode_edge_descriptor(value: Value) -> Result<Value> {
    match value {
        Value::String(encoded) => {
            serde_json::from_str(&encoded).context("invalid persisted impact edge descriptor")
        }
        other => Ok(other),
    }
}

pub(super) fn resolve_traversal_kinds(
    mode: TraversalMode,
    edge_types: Option<&[String]>,
    structural: bool,
) -> (String, String) {
    if let Some(types) = edge_types {
        if !types.is_empty() {
            let mut forward = Vec::new();
            let mut reverse = Vec::new();
            for t in types {
                let lower = t.to_ascii_lowercase();
                if lower == EdgeKind::Decorates.as_str() {
                    reverse.push(format!("'{lower}'"));
                } else {
                    forward.push(format!("'{lower}'"));
                }
            }
            let f_str = if forward.is_empty() {
                "'__none__'".into()
            } else {
                forward.join(",")
            };
            return (f_str, reverse.join(","));
        }
    }

    match mode {
        TraversalMode::DataFlow => (
            format!(
                "'{}','{}','{}'",
                EdgeKind::Mutates.as_str(),
                EdgeKind::References.as_str(),
                EdgeKind::Handles.as_str()
            ),
            String::new(),
        ),
        TraversalMode::All => (
            format!("{}{}", paging::FORWARD_DEPENDENCIES, paging::STRUCTURAL_DEPENDENCIES),
            paging::REVERSE_DEPENDENCIES.into(),
        ),
        TraversalMode::Calls => {
            let extra = if structural {
                paging::STRUCTURAL_DEPENDENCIES
            } else {
                ""
            };
            (
                format!("{}{extra}", paging::FORWARD_DEPENDENCIES),
                paging::REVERSE_DEPENDENCIES.into(),
            )
        }
    }
}
