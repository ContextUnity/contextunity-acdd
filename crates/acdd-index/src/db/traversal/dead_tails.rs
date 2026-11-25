use super::types::{normalize_scope, page_bounds};
use crate::db::{
    lint::{collect_dead_tails, DeadTailsNode},
    paging,
};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct DeadTailNodeDto<'a> {
    id: &'a str,
    name: &'a str,
    qualname: &'a str,
    kind: &'a str,
    path: &'a str,
    line: i64,
    end_line: i64,
    lines: i64,
}

impl<'a> DeadTailNodeDto<'a> {
    fn from_node(node: &'a DeadTailsNode, lines: i64) -> Self {
        Self {
            id: &node.id,
            name: &node.name,
            qualname: &node.qualname,
            kind: node.kind.as_str(),
            path: &node.path,
            line: node.line,
            end_line: node.end_line,
            lines,
        }
    }
}

#[derive(Serialize)]
struct TestCallerPreviewDto<'a> {
    id: &'a str,
    name: &'a str,
    qualname: &'a str,
    path: &'a str,
    line: i64,
}

#[derive(Serialize)]
struct PhantomTestedItemDto<'a> {
    #[serde(flatten)]
    node: DeadTailNodeDto<'a>,
    test_caller_count: usize,
    test_callers: Vec<TestCallerPreviewDto<'a>>,
}

#[derive(Serialize)]
struct DeadTailItemDto<'a> {
    #[serde(flatten)]
    node: DeadTailNodeDto<'a>,
    dead_callees_count: usize,
}

#[derive(Serialize)]
struct DanglingCallDto<'a> {
    path: &'a str,
    symbol: &'a str,
    kind: &'a str,
    resolution: &'a str,
}

#[derive(Serialize)]
struct DeadTailsSummaryDto {
    phantom_tested_count: usize,
    dead_tails_count: usize,
    dangling_calls_count: usize,
}

#[derive(Serialize)]
struct DeadTailsResponseDto<'a> {
    operation: &'static str,
    scope: Option<&'a str>,
    generation: &'a str,
    summary: DeadTailsSummaryDto,
    phantom_tested: Value,
    dead_tails: Value,
    dangling_calls: Value,
}

/// Identifies dead tails, phantom-tested production callables, and unresolved internal calls.
pub fn dead_tails_paged(
    conn: &Connection,
    target: Option<&str>,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let scope = normalize_scope(target, path, "target")?;
    let report = collect_dead_tails(conn, &scope)?;

    let (phantom_count, phantom_slice) = page_bounds(&report.phantom_nodes, options);
    let phantom_items = phantom_slice
        .iter()
        .map(|&(nh, lines)| {
            let node = &report.nodes[&nh];
            let mut caller_nodes: Vec<&DeadTailsNode> = report
                .test_callers
                .get(&node.hash)
                .map(|hashes| hashes.iter().filter_map(|h| report.nodes.get(h)).collect())
                .unwrap_or_default();
            caller_nodes.sort_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let test_caller_count = caller_nodes.len();
            let test_callers = caller_nodes
                .into_iter()
                .take(5)
                .map(|c| TestCallerPreviewDto {
                    id: &c.id,
                    name: &c.name,
                    qualname: &c.qualname,
                    path: &c.path,
                    line: c.line,
                })
                .collect();
            serde_json::to_value(PhantomTestedItemDto {
                node: DeadTailNodeDto::from_node(node, lines),
                test_caller_count,
                test_callers,
            })
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    let phantom_page = paging::value(phantom_items, phantom_count, options, &generation);

    let (dead_tails_count, dead_tails_slice) = page_bounds(&report.dead_tail_nodes, options);
    let dead_tail_items = dead_tails_slice
        .iter()
        .map(|&(nh, lines)| {
            let node = &report.nodes[&nh];
            let dead_callees_count = report.fwd.get(&node.hash).map_or(0, |callees| {
                callees.iter().filter(|c| report.dead.contains(*c)).count()
            });
            serde_json::to_value(DeadTailItemDto {
                node: DeadTailNodeDto::from_node(node, lines),
                dead_callees_count,
            })
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    let dead_tails_page = paging::value(dead_tail_items, dead_tails_count, options, &generation);

    let (dangling_calls_count, dangling_slice) = page_bounds(&report.dangling, options);
    let dangling_items = dangling_slice
        .iter()
        .map(|d| {
            serde_json::to_value(DanglingCallDto {
                path: &d.path,
                symbol: &d.symbol,
                kind: &d.kind,
                resolution: &d.resolution,
            })
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    let dangling_page = paging::value(dangling_items, dangling_calls_count, options, &generation);

    Ok(serde_json::to_value(DeadTailsResponseDto {
        operation: "dead_tails",
        scope: (!scope.is_empty()).then_some(scope.as_str()),
        generation: &generation,
        summary: DeadTailsSummaryDto {
            phantom_tested_count: phantom_count,
            dead_tails_count,
            dangling_calls_count,
        },
        phantom_tested: phantom_page,
        dead_tails: dead_tails_page,
        dangling_calls: dangling_page,
    })?)
}
