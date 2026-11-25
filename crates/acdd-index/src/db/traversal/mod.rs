//! Graph traversal, directory slice, Git change impact, duplicate detection, and dead-tail queries.

mod change_impact;
mod dead_tails;
mod duplicates;
/// Graph impact and bounded traversal queries.
pub mod impact;
mod slice;
mod types;

use super::{paging, reader, symbols};
use crate::models::{
    details::NodeDetailKey,
    tags::{EdgeKind, NodeKind},
};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

pub(crate) use dead_tails::dead_tails_paged;
pub use duplicates::duplicates_paged;
use impact::traverse_with_path_options;
pub use impact::{impact_with_path_options, removal_paged_with_path};
use types::GraphQueryOp;
pub use types::{GraphQueryOptions, ImpactDirection, TraversalMode, TraversalOptions};

pub(super) fn predecessor_endpoints(
    direction: ImpactDirection,
) -> (&'static str, &'static str, &'static str, &'static str) {
    if direction.is_inbound() {
        ("src_hash", "dst_hash", "dst_hash", "src_hash")
    } else {
        ("dst_hash", "src_hash", "src_hash", "dst_hash")
    }
}

pub(super) fn reverse_predecessor_union(
    reverse: &str,
    reverse_current: &str,
    reverse_predecessor: &str,
    distance_expr: &str,
    current_expr: &str,
) -> String {
    if reverse.is_empty() {
        String::new()
    } else {
        format!(
            " UNION ALL SELECT prior_node.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{reverse_predecessor} AND prior.distance={distance_expr}-1 JOIN nodes prior_node ON prior_node.node_hash=prior.id WHERE e.{reverse_current}={current_expr} AND e.kind IN({reverse})"
        )
    }
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
                direction,
                mode: TraversalMode::Calls,
                edge_types: None,
                page: options,
            },
        ),
        GraphQueryOp::Slice => {
            slice::slice_paged(conn, selector.unwrap_or(""), path, depth, options)
        }
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
                classification: None,
                path,
                include_docs: false,
                exact: false,
                page: options,
            },
        ),
        GraphQueryOp::Duplicates => {
            let (target, level) = match selector {
                Some("file") | Some("symbol") => (None, selector),
                _ => (selector, None),
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
    use super::types::{GraphQueryOp, TraversalMode};
    use super::*;
    use crate::db::schema::SCHEMA_DDL;
    use acdd_core::response::{QueryOptions, ResponsePolicy};
    use rusqlite::Connection;

    #[test]
    fn boundary_enums_parse_accepted_and_rejected_values() {
        assert_eq!(
            ImpactDirection::parse_optional(None).unwrap(),
            ImpactDirection::Inbound
        );
        assert_eq!(
            ImpactDirection::parse_optional(Some("outbound")).unwrap(),
            ImpactDirection::Outbound
        );
        assert!(ImpactDirection::parse_optional(Some("sideways")).is_err());

        assert_eq!(TraversalMode::parse_optional(None), TraversalMode::Calls);
        assert_eq!(
            TraversalMode::parse_optional(Some("dataflow")),
            TraversalMode::DataFlow
        );
        assert_eq!(
            TraversalMode::parse_optional(Some("all")),
            TraversalMode::All
        );
        assert!(matches!(
            "dead_tails".parse::<GraphQueryOp>(),
            Ok(GraphQueryOp::DeadTails)
        ));
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

    #[test]
    fn slice_edge_ordering_uses_native_ids_and_confidence() {
        use crate::db::commitments;
        use acdd_core::response::{CoverageOptions, Detail};

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "{SCHEMA_DDL}INSERT INTO metadata(key,value) VALUES ('output_root','test-gen'),('workspace_root','/tmp');"
        ))
        .unwrap();
        conn.execute_batch(
            "INSERT INTO path_dictionary(path_id, path) VALUES (1, 'contextunity/commerce/ingestion/a.py'), (2, 'contextunity/commerce/ingestion/b.py');
             INSERT INTO files(path, status, digest, size, language, is_test, generated, path_hash)
             VALUES ('contextunity/commerce/ingestion/a.py', 'indexed', 'aa', 0, 'python', 0, 0, 11),
                    ('contextunity/commerce/ingestion/b.py', 'indexed', 'bb', 0, 'python', 0, 0, 22);
             INSERT INTO coverage_evidence(evidence_id, evidence) VALUES (1, 'aaa'), (2, 'zzz');
             INSERT INTO nodes(node_hash, id, kind, name, qualname, path_id, owner_path_id, line, end_line, language, generated, details, is_test)
             VALUES (100, 'src', 'function', 'src', 'a.src', 1, 1, 1, 2, 'python', 0, '{}', 0),
                    (200, 'test-a', 'function', 'test_a', 'b.test_a', 2, 2, 1, 2, 'python', 0, '{}', 0),
                    (300, 'test-a!', 'function', 'test_a_bang', 'b.test_a_bang', 2, 2, 3, 4, 'python', 0, '{}', 0);
             INSERT INTO edges(src_hash, dst_hash, kind, path_id, line, evidence_id, confidence_id, occurrence_count)
             VALUES (100, 300, 'calls', 1, 1, 1, 1, 1),
                    (100, 200, 'calls', 1, 2, 1, 2, 1);",
        )
        .unwrap();
        commitments::seal(&conn).unwrap();
        for detail in [Detail::Compact, Detail::Full] {
            let page =
                QueryOptions::resolve(&ResponsePolicy::default(), Some(100), 0, Some(detail), None)
                    .unwrap();
            let result = query_with_path_options(
                &conn,
                "slice",
                Some("contextunity/commerce/ingestion"),
                None,
                &GraphQueryOptions {
                    depth: 1,
                    direction: ImpactDirection::Inbound,
                    coverage: CoverageOptions::default(),
                    page: &page,
                },
            )
            .unwrap();
            let edges = result["edges"]["items"].as_array().unwrap();
            assert!(edges.iter().any(|edge| edge["confidence"] == "aaa"));
            assert!(edges.iter().any(|edge| edge["confidence"] == "zzz"));
            let mut expected = edges.clone();
            expected.sort_by(|a, b| {
                a["src_public_id"]
                    .as_str()
                    .cmp(&b["src_public_id"].as_str())
                    .then_with(|| {
                        a["dst_public_id"]
                            .as_str()
                            .cmp(&b["dst_public_id"].as_str())
                    })
                    .then_with(|| a["kind"].as_str().cmp(&b["kind"].as_str()))
                    .then_with(|| a["line"].as_i64().cmp(&b["line"].as_i64()))
                    .then_with(|| {
                        a["occurrence_count"]
                            .as_i64()
                            .cmp(&b["occurrence_count"].as_i64())
                    })
                    .then_with(|| a["path"].as_str().cmp(&b["path"].as_str()))
                    .then_with(|| a["confidence"].as_str().cmp(&b["confidence"].as_str()))
                    .then_with(|| {
                        if detail == Detail::Full {
                            a["evidence"].as_str().cmp(&b["evidence"].as_str())
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
            });
            assert_eq!(edges, &expected, "internal edge order for {detail:?}");
            let punctuated = edges
                .iter()
                .position(|edge| edge["dst_public_id"] == "test-a!")
                .unwrap();
            let prefix = edges
                .iter()
                .position(|edge| edge["dst_public_id"] == "test-a")
                .unwrap();
            assert!(prefix < punctuated);
            let generation = result["edges"]["generation"].as_str().unwrap();
            for (offset, edge) in expected.iter().enumerate() {
                let page = QueryOptions::resolve(
                    &ResponsePolicy::default(),
                    Some(1),
                    offset,
                    Some(detail),
                    Some(generation.to_owned()),
                )
                .unwrap();
                let part = query_with_path_options(
                    &conn,
                    "slice",
                    Some("contextunity/commerce/ingestion"),
                    None,
                    &GraphQueryOptions {
                        depth: 1,
                        direction: ImpactDirection::Inbound,
                        coverage: CoverageOptions::default(),
                        page: &page,
                    },
                )
                .unwrap();
                assert_eq!(&part["edges"]["items"][0], edge);
            }
        }
    }
}
