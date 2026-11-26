//! Pure read-only graph and PageRank diagnostic analyzers over `code-map.sqlite`.

mod boundary;
mod common;
mod entrypoints;
pub mod hotspots;
mod path;

use crate::db::{paging::QueryBudget, reader::diagnostics::analyze_summary_paged};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, str::FromStr};

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
}

impl AnalyzeMode {
    pub const ALL: &'static [Self] = &[
        Self::Summary,
        Self::Hotspots,
        Self::Boundary,
        Self::Path,
        Self::Entrypoints,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Hotspots => "hotspots",
            Self::Boundary => "boundary",
            Self::Path => "path",
            Self::Entrypoints => "entrypoints",
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
            .ok_or_else(|| anyhow::anyhow!("unsupported analyze mode '{lower}'; expected one of: summary, hotspots, boundary, path, entrypoints"))
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
}

/// Dispatches a read-only code-map analysis mode over already-indexed SQLite facts.
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
    }?;
    budget.check()?;
    Ok(result)
}
