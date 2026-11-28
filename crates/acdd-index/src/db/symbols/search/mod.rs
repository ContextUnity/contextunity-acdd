mod navigation;
mod projection;
mod query;

use crate::db::symbols::types::SearchOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

pub use navigation::search_results_with_navigation;
pub use projection::SearchSignatureHit;

/// Search indexed symbols using one bounded options contract.
pub fn search_with_options(
    conn: &Connection,
    pattern: &str,
    options: &SearchOptions<'_>,
) -> Result<Value> {
    let (pattern, page) = query::search_typed(conn, pattern, options)?;
    let items: Vec<Value> = page
        .items
        .iter()
        .map(projection::TypedSearchHit::to_wire_json)
        .collect::<Result<_>>()?;
    Ok(json!({"pattern": pattern, "nodes": page.into_page_json(items)}))
}

/// Runs the canonical RRF/FTS5 search ranking and projects hits with bounded indexed signatures.
pub fn search_signature_hits(
    conn: &Connection,
    pattern: &str,
    options: &SearchOptions<'_>,
    signature_char_limit: usize,
) -> Result<Vec<SearchSignatureHit>> {
    let (_, page) = query::search_typed(conn, pattern, options)?;
    if page.items.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<&str> = page.items.iter().map(|hit| hit.id.as_str()).collect();
    let metadata = projection::indexed_node_metadata(conn, &ids)?;
    Ok(page
        .items
        .into_iter()
        .filter(|hit| !hit.qualname.is_empty())
        .map(|hit| {
            let signature = metadata
                .get(&hit.id)
                .and_then(|meta| meta.signature.as_deref())
                .map(|sig| projection::bounded_signature(sig, signature_char_limit))
                .unwrap_or_default();
            SearchSignatureHit {
                qualname: hit.qualname,
                path: hit.path,
                line: hit.line,
                end_line: hit.end_line,
                signature,
            }
        })
        .collect())
}
