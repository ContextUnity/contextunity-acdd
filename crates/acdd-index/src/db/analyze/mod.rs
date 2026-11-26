//! Pure read-only graph, PageRank, diff-impact, and Git co-change analyzers over `code-map.sqlite`.

mod boundary;
mod cochange;
mod common;
mod entrypoints;
pub mod hotspots;
mod path;

use crate::db::{
    paging::QueryBudget, reader::diagnostics::analyze_summary_paged,
    traversal::change_impact::diff_impact_with_options,
};
use acdd_core::{path_policy::WorkspacePathPolicy, response::QueryOptions};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, path::Path, str::FromStr};

/// Closed analysis mode enum dispatched by [`analyze_with_mode`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzeMode {
    #[default]
    Summary,
    Hotspots,
    Boundary,
    Path,
    Entrypoints,
    DiffImpact,
    Cochange,
}

impl AnalyzeMode {
    pub const ALL: &'static [Self] = &[
        Self::Summary,
        Self::Hotspots,
        Self::Boundary,
        Self::Path,
        Self::Entrypoints,
        Self::DiffImpact,
        Self::Cochange,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Hotspots => "hotspots",
            Self::Boundary => "boundary",
            Self::Path => "path",
            Self::Entrypoints => "entrypoints",
            Self::DiffImpact => "diff_impact",
            Self::Cochange => "cochange",
        }
    }
}

impl FromStr for AnalyzeMode {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        let lower = value.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.as_str() == lower)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported analyze mode '{lower}'; expected one of: summary, hotspots, boundary, path, entrypoints, diff_impact, cochange"
                )
            })
    }
}

impl fmt::Display for AnalyzeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Request parameters for [`analyze_with_mode`].
#[derive(Debug, Clone, Copy)]
pub struct AnalyzeRequest<'a> {
    pub mode: AnalyzeMode,
    pub target: &'a str,
    pub to: Option<&'a str>,
    pub include_cycles: Option<bool>,
    pub page: &'a QueryOptions,
    pub root: Option<&'a Path>,
    pub base: Option<&'a str>,
    pub policy: Option<&'a WorkspacePathPolicy>,
}

/// Dispatches any read-only code-map analysis mode over already-indexed SQLite facts.
pub fn analyze_with_mode(conn: &Connection, req: &AnalyzeRequest<'_>) -> Result<Value> {
    let budget = QueryBudget::new(conn);
    budget.check()?;
    let result = match req.mode {
        AnalyzeMode::Summary => {
            analyze_summary_paged(conn, req.target, req.include_cycles, req.page)
        }
        AnalyzeMode::Hotspots => {
            let cfg = hotspots::PageRankConfig::default();
            let pr = hotspots::pagerank_centrality(conn, &cfg)?;
            hotspots::analyze_hotspots(conn, req.target, &cfg, &pr, req.page, &budget)
        }
        AnalyzeMode::Boundary => boundary::analyze_boundary(conn, req.target, req.page, &budget),
        AnalyzeMode::Path => path::analyze_path(conn, req.target, req.to, req.page, &budget),
        AnalyzeMode::Entrypoints => {
            entrypoints::analyze_entrypoints(conn, req.target, req.page, &budget)
        }
        AnalyzeMode::DiffImpact => {
            let root = req
                .root
                .context("diff_impact mode requires a workspace root")?;
            diff_impact_with_options(
                conn, root, req.target, req.base, req.policy, req.page, &budget,
            )
        }
        AnalyzeMode::Cochange => {
            let root = req
                .root
                .context("cochange mode requires a workspace root")?;
            cochange::analyze_cochange(
                conn, root, req.target, req.base, req.policy, req.page, &budget,
            )
        }
    }?;
    budget.check()?;
    Ok(result)
}
