use crate::models::compact_graph::{
    CompactCoverage as Coverage, CompactEdge as Edge, CompactGraph as Graph,
};
use hashbrown::HashSet;
use std::sync::Arc;

pub(super) fn compare_edges(a: &Edge, b: &Edge) -> std::cmp::Ordering {
    (&a.path, a.line, &a.src, &a.dst, &a.kind).cmp(&(&b.path, b.line, &b.src, &b.dst, &b.kind))
}

pub(super) fn compare_coverage(a: &Coverage, b: &Coverage) -> std::cmp::Ordering {
    (&a.path, a.line, &a.expression, &a.status, &a.evidence).cmp(&(
        &b.path,
        b.line,
        &b.expression,
        &b.status,
        &b.evidence,
    ))
}

pub(super) fn sort_graph(graph: &mut Graph) {
    graph.edges.sort_by(compare_edges);
    graph.coverage.sort_by(compare_coverage);
}

thread_local! {
    static LOCAL_GRAPH_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static LOCAL_GRAPH_COMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> =
        const { std::cell::RefCell::new(None) };
    static LOCAL_GRAPH_DECODER: std::cell::RefCell<Option<zstd::zstd_safe::DCtx<'static>>> =
        const { std::cell::RefCell::new(None) };
}

fn clear_pack_scratch_current_thread() {
    LOCAL_GRAPH_COMPRESSOR.with(|slot| {
        slot.borrow_mut().take();
    });
}

fn clear_unpack_scratch_current_thread() {
    LOCAL_GRAPH_DECODER.with(|slot| {
        slot.borrow_mut().take();
    });
    LOCAL_GRAPH_BUFFER.with(|buffer| {
        *buffer.borrow_mut() = Vec::new();
    });
}

pub(super) fn release_pack_scratch(parallel: bool) {
    if parallel {
        rayon::broadcast(|_| clear_pack_scratch_current_thread());
    }
    clear_pack_scratch_current_thread();
}

pub(super) fn release_unpack_scratch(parallel: bool) {
    if parallel {
        rayon::broadcast(|_| clear_unpack_scratch_current_thread());
    }
    clear_unpack_scratch_current_thread();
}

pub(super) enum GraphPart {
    Resident(Graph),
    Packed {
        bytes: Vec<u8>,
        edges: usize,
        coverage: usize,
    },
}

impl GraphPart {
    pub(super) fn new(graph: Graph, pack: bool) -> Self {
        if !pack {
            return Self::Resident(graph);
        }
        if graph.edges.is_empty() && graph.coverage.is_empty() {
            return Self::Packed {
                bytes: Vec::new(),
                edges: 0,
                coverage: 0,
            };
        }
        let compressed = LOCAL_GRAPH_BUFFER.with(|buffer| {
            let mut bytes = buffer.borrow_mut();
            bytes.clear();
            for edge in &graph.edges {
                for value in [
                    edge.src.as_ref(),
                    edge.dst.as_ref(),
                    edge.kind.as_str(),
                    edge.path.as_ref(),
                    edge.evidence.as_str(),
                    edge.confidence.as_str(),
                ] {
                    write_string(&mut bytes, value);
                }
                bytes.extend_from_slice(&(edge.line as u64).to_le_bytes());
            }
            for row in &graph.coverage {
                for value in [
                    row.path.as_ref(),
                    row.expression.as_str(),
                    row.status.as_str(),
                    row.evidence.as_ref(),
                ] {
                    write_string(&mut bytes, value);
                }
                bytes.extend_from_slice(&(row.line as u64).to_le_bytes());
            }
            LOCAL_GRAPH_COMPRESSOR.with(|slot| {
                let mut slot = slot.borrow_mut();
                let compressor = slot.get_or_insert_with(|| {
                    zstd::bulk::Compressor::new(1).expect("graph zstd compressor initialization")
                });
                compressor
                    .compress(&bytes)
                    .expect("in-memory graph compression")
            })
        });
        Self::Packed {
            bytes: compressed,
            edges: graph.edges.len(),
            coverage: graph.coverage.len(),
        }
    }

    pub(super) fn counts(&self) -> (usize, usize) {
        match self {
            Self::Resident(graph) => (graph.edges.len(), graph.coverage.len()),
            Self::Packed {
                edges, coverage, ..
            } => (*edges, *coverage),
        }
    }

    pub(super) fn into_graph(self) -> Graph {
        let (bytes, edges, coverage) = match self {
            Self::Packed {
                bytes,
                edges,
                coverage,
            } => (bytes, edges, coverage),
            Self::Resident(graph) => return graph,
        };
        if edges == 0 && coverage == 0 {
            return Graph::default();
        }
        LOCAL_GRAPH_DECODER.with(|slot| {
            let mut slot = slot.borrow_mut();
            let context = slot.get_or_insert_with(|| {
                zstd::zstd_safe::DCtx::try_create()
                    .expect("failed to allocate graph decompression context")
            });
            context.init().expect("graph decompression context reset");
            LOCAL_GRAPH_BUFFER.with(|buffer| {
                let mut raw = buffer.borrow_mut();
                raw.clear();
                {
                    let mut decoder =
                        zstd::stream::read::Decoder::with_context(bytes.as_slice(), context);
                    std::io::Read::read_to_end(&mut decoder, &mut raw)
                        .expect("graph compression is lossless");
                }
                drop(bytes);
                let mut cursor = raw.as_slice();
                let mut path_pool = HashSet::<Arc<str>>::new();
                let mut node_id_pool = HashSet::<Arc<str>>::new();
                let mut evidence_pool = HashSet::<Arc<str>>::new();
                let edges = (0..edges)
                    .map(|_| {
                        let src = intern_arc_str(&mut node_id_pool, read_str(&mut cursor));
                        let dst = intern_arc_str(&mut node_id_pool, read_str(&mut cursor));
                        let kind = read_str(&mut cursor).into();
                        let path = intern_arc_str(&mut path_pool, read_str(&mut cursor));
                        let evidence = read_str(&mut cursor).into();
                        let confidence = read_str(&mut cursor).into();
                        let line = read_number(&mut cursor);
                        Edge {
                            src,
                            dst,
                            kind,
                            path,
                            line,
                            evidence,
                            confidence,
                        }
                    })
                    .collect();
                let coverage = (0..coverage)
                    .map(|_| {
                        let path = intern_arc_str(&mut path_pool, read_str(&mut cursor));
                        let expression = read_string(&mut cursor);
                        let status = read_str(&mut cursor).into();
                        let evidence = intern_arc_str(&mut evidence_pool, read_str(&mut cursor));
                        let line = read_number(&mut cursor);
                        Coverage {
                            path,
                            line,
                            expression,
                            status,
                            evidence,
                        }
                    })
                    .collect();
                debug_assert!(cursor.is_empty());
                Graph {
                    edges,
                    coverage,
                    path_arc: None,
                    path_pool: HashSet::new(),
                    node_id_pool: HashSet::new(),
                    evidence_pool: HashSet::new(),
                }
            })
        })
    }
}

fn intern_arc_str(pool: &mut HashSet<Arc<str>>, value: &str) -> Arc<str> {
    if let Some(existing) = pool.get(value) {
        return Arc::clone(existing);
    }
    let shared = Arc::<str>::from(value);
    pool.insert(Arc::clone(&shared));
    shared
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn read_number(bytes: &mut &[u8]) -> usize {
    let (number, rest) = bytes.split_at(8);
    *bytes = rest;
    u64::from_le_bytes(number.try_into().expect("eight-byte graph field")) as usize
}

fn read_string(bytes: &mut &[u8]) -> String {
    read_str(bytes).to_owned()
}

fn read_str<'a>(bytes: &mut &'a [u8]) -> &'a str {
    let length = read_number(bytes);
    let (value, rest) = bytes.split_at(length);
    *bytes = rest;
    std::str::from_utf8(value).expect("graph strings were encoded internally")
}

#[cfg(test)]
mod tests {
    use super::GraphPart;
    use crate::models::compact_graph::CompactGraph as Graph;
    use std::sync::Arc;

    #[test]
    fn packed_graph_part_roundtrips_and_interns_shared_strings() {
        let empty = GraphPart::new(Graph::default(), true).into_graph();
        assert!(empty.edges.is_empty());
        assert!(empty.coverage.is_empty());

        let mut first = Graph::default();
        first.push_edge("src/a.rs", "fn:a", "fn:b", "calls", 10, "b()", "exact");
        first.push_edge("src/a.rs", "fn:a", "fn:b", "calls", 12, "b()", "exact");
        first.push_coverage(
            "src/a.rs",
            10,
            "b()",
            "resolved",
            "1 lexically justified candidates; implementor resolved=true, heuristic=false",
        );
        first.push_coverage(
            "src/a.rs",
            14,
            "c()",
            "resolved",
            "1 lexically justified candidates; implementor resolved=true, heuristic=false",
        );

        let unpacked_first = GraphPart::new(first, true).into_graph();
        assert_eq!(unpacked_first.edges.len(), 2);
        assert!(Arc::ptr_eq(&unpacked_first.edges[0].src, &unpacked_first.edges[1].src));
        assert!(Arc::ptr_eq(&unpacked_first.edges[0].dst, &unpacked_first.edges[1].dst));
        assert!(Arc::ptr_eq(&unpacked_first.edges[0].path, &unpacked_first.coverage[0].path));
        assert!(Arc::ptr_eq(
            &unpacked_first.coverage[0].evidence,
            &unpacked_first.coverage[1].evidence
        ));

        let mut second = Graph::default();
        second.push_edge("src/a.rs", "fn:b", "fn:a", "calls", 20, "a()", "exact");
        second.push_coverage("src/a.rs", 22, "d()", "unresolved", "custom diagnostic");
        second.push_coverage(
            "src/a.rs",
            24,
            "e()",
            "resolved",
            "1 lexically justified candidates; implementor resolved=true, heuristic=false",
        );
        let unpacked_second = GraphPart::new(second, true).into_graph();

        let mut merged = Graph::default();
        merged.merge(unpacked_first);
        merged.merge(unpacked_second);
        assert!(Arc::ptr_eq(&merged.edges[0].src, &merged.edges[2].dst));
        assert!(Arc::ptr_eq(&merged.edges[0].dst, &merged.edges[2].src));
        assert!(Arc::ptr_eq(&merged.edges[0].path, &merged.edges[2].path));
        assert!(Arc::ptr_eq(&merged.coverage[0].evidence, &merged.coverage[3].evidence));
        assert_eq!(merged.coverage[2].evidence.as_ref(), "custom diagnostic");
        merged.clear_pools();
        assert!(merged.path_pool.is_empty());
        assert!(merged.node_id_pool.is_empty());
        assert!(merged.evidence_pool.is_empty());
    }
}
