//! Shared read-only SQLite node/edge loaders and scope helpers for `db::analyze`.

use crate::db::{
    cycles::{is_archived_path, is_test_context_path},
    paging::QueryBudget,
    reader::selectors::{path_bounds, strip_selector_uri_prefix},
};
use crate::models::details::{NodeDetailFlags, NodeDetailKey};
use acdd_core::response::QueryOptions;
use anyhow::{bail, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) const NON_DEP_EDGES: &str =
    "'contains','documents','governs','cites_doc','cited_by_doc','references_doc','governed_by'";
pub(super) const NODE_COLS: &str =
    "n.node_hash,n.id,n.kind,n.name,n.qualname,p.path,n.line,n.end_line,n.language,n.details,\
     json_extract(n.details,'$.entrypoint'),json_extract(n.details,'$.cognitive_complexity'),n.is_test,n.generated";

#[derive(Clone, Debug)]
pub(super) struct NodeSummary {
    pub(super) hash: i64,
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) qualname: String,
    pub(super) path: String,
    pub(super) line: i64,
    pub(super) end_line: i64,
    pub(super) language: String,
    pub(super) is_public: bool,
    pub(super) is_prod: bool,
    pub(super) entrypoint: Option<String>,
    pub(super) cognitive_complexity: Option<usize>,
}

impl NodeSummary {
    pub(super) fn loc(&self) -> usize {
        usize::try_from(self.end_line.saturating_sub(self.line).saturating_add(1)).unwrap_or(1)
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "name": self.name, "qualname": self.qualname,
            "path": self.path, "line": self.line, "end_line": self.end_line, "language": self.language,
        })
    }

    pub(super) fn with_fields(&self, extra: Vec<(&str, Value)>) -> Value {
        let mut item = self.to_json();
        if let Some(obj) = item.as_object_mut() {
            obj.extend(extra.into_iter().map(|(k, v)| (k.to_owned(), v)));
        }
        item
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct GraphEdge {
    pub(super) src: i64,
    pub(super) dst: i64,
    pub(super) kind: String,
    pub(super) path: String,
    pub(super) line: i64,
    pub(super) occurrence_count: i64,
}

pub(super) fn is_non_prod_raw(path: &str, details: &str) -> bool {
    is_archived_path(path)
        || is_test_context_path(path)
        || (details.contains(NodeDetailKey::DECORATORS)
            && NodeDetailFlags::from_json(details).has_cfg_test)
}

pub(super) fn is_symbol_kind(kind: &str) -> bool {
    const NON_SYMBOLS: [&str; 8] = [
        "module",
        "document",
        "invariant",
        "namespace",
        "template_scope",
        "island_scope",
        "template_loader_root",
        "module_declaration",
    ];
    !NON_SYMBOLS.contains(&kind)
}

pub(super) fn path_in_scope(path: &str, scope: &str) -> bool {
    scope.is_empty()
        || path == scope
        || (path.starts_with(scope) && path.as_bytes().get(scope.len()) == Some(&b'/'))
}

pub(super) fn resolve_indexed_scope(conn: &Connection, target: &str) -> Result<String> {
    let cleaned = strip_selector_uri_prefix(target.trim()).trim_end_matches('/');
    if cleaned.is_empty() {
        return Ok(String::new());
    }
    let (prefix, end) = path_bounds(cleaned);
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1 OR (path>=?2 AND path<?3))",
        rusqlite::params![cleaned, prefix, end],
        |r| r.get(0),
    )?;
    if !exists {
        bail!("PATH_NOT_INDEXED: '{cleaned}' was not found in the indexed inventory");
    }
    Ok(cleaned.to_owned())
}

pub(super) fn page_slice<'a, T>(items: &'a [T], options: &QueryOptions) -> &'a [T] {
    let start = options.offset.min(items.len());
    &items[start..items.len().min(start.saturating_add(options.limit))]
}

pub(super) fn load_nodes(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    max_nodes: Option<usize>,
    prod_only: bool,
    budget: &QueryBudget<'_>,
) -> Result<Vec<NodeSummary>> {
    let mut stmt = conn.prepare(sql)?;
    let mut out = Vec::new();
    for row in stmt.query_map(params, |r| {
        let details: String = r.get(9)?;
        let flags = NodeDetailFlags::from_json(&details);
        let path: String = r.get(5)?;
        let is_prod = r.get::<_, i64>(12)? == 0
            && r.get::<_, i64>(13)? == 0
            && !flags.has_cfg_test
            && !is_archived_path(&path)
            && !is_test_context_path(&path);
        Ok(NodeSummary {
            hash: r.get(0)?,
            id: r.get(1)?,
            kind: r.get(2)?,
            name: r.get(3)?,
            qualname: r.get(4)?,
            path,
            line: r.get(6)?,
            end_line: r.get(7)?,
            language: r.get(8)?,
            is_public: flags.is_public,
            is_prod,
            entrypoint: r.get(10)?,
            cognitive_complexity: r
                .get::<_, Option<i64>>(11)?
                .and_then(|v| usize::try_from(v).ok()),
        })
    })? {
        budget.check()?;
        let node = row?;
        if prod_only && !node.is_prod {
            continue;
        }
        out.push(node);
        if let Some(limit) = max_nodes.filter(|&l| out.len() > l) {
            bail!("PageRank graph exceeds {limit} node budget");
        }
    }
    budget.check()?;
    Ok(out)
}

pub(super) fn load_nodes_by_hashes(
    conn: &Connection,
    hashes: &[i64],
    budget: &QueryBudget<'_>,
) -> Result<BTreeMap<i64, NodeSummary>> {
    if hashes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let j = serde_json::to_string(hashes)?;
    let sql = format!("SELECT {NODE_COLS} FROM json_each(?1) req JOIN nodes n ON n.node_hash=CAST(req.value AS INTEGER) JOIN path_dictionary p ON p.path_id=n.path_id");
    Ok(load_nodes(conn, &sql, &[&j], None, false, budget)?
        .into_iter()
        .map(|n| (n.hash, n))
        .collect())
}

pub(super) fn fetch_frontier_edges(
    conn: &Connection,
    frontier: &[i64],
    inbound: bool,
    prod_only: bool,
    budget: &QueryBudget<'_>,
) -> Result<Vec<GraphEdge>> {
    const MAX_FRONTIER_EDGES: usize = 50_000;
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    budget.check()?;
    let (join_col, next_col) = if inbound {
        ("e.dst_hash", "e.src_hash")
    } else {
        ("e.src_hash", "e.dst_hash")
    };
    let prod_sql = if prod_only {
        " AND next_n.is_test=0 AND next_n.generated=0"
    } else {
        ""
    };
    let sql = format!(
        "SELECT e.src_hash,e.dst_hash,e.kind,ep.path,e.line,e.occurrence_count,next_p.path,next_n.details \
         FROM json_each(?1) f JOIN edges e ON {join_col}=CAST(f.value AS INTEGER) \
         JOIN nodes next_n ON next_n.node_hash={next_col} JOIN path_dictionary next_p ON next_p.path_id=next_n.path_id \
         JOIN path_dictionary ep ON ep.path_id=e.path_id \
         WHERE e.kind NOT IN({NON_DEP_EDGES}){prod_sql} ORDER BY e.src_hash,e.dst_hash,e.kind,e.line"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut edges = Vec::new();
    for row in stmt.query_map([serde_json::to_string(frontier)?], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })? {
        budget.check()?;
        let (src, dst, kind, path, line, occurrence_count, next_path, details) = row?;
        if prod_only && is_non_prod_raw(&next_path, &details) {
            continue;
        }
        edges.push(GraphEdge {
            src,
            dst,
            kind,
            path,
            line,
            occurrence_count,
        });
        if edges.len() > MAX_FRONTIER_EDGES {
            bail!("graph traversal exceeds frontier edge budget; select a narrower symbol");
        }
    }
    budget.check()?;
    Ok(edges)
}
