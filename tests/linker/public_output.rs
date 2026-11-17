use acdd_index::models::{Edge, Facts};
use acdd_index::linker;
use std::collections::BTreeMap;

#[test]
fn public_linker_preserves_arbitrary_edge_tags_and_orders_edges_stably() {
    let unusual = Edge {
        src: "source-α".into(),
        dst: "target-β".into(),
        kind: "custom-kind-雪".into(),
        path: "z/文件.rs".into(),
        line: 17,
        evidence: "evidence-дані".into(),
        confidence: "confidence-🧭".into(),
    };
    let earlier = Edge {
        path: "a/файл.rs".into(),
        ..unusual.clone()
    };
    let facts = BTreeMap::from([(
        "input.rs".to_owned(),
        Facts {
            edges: vec![unusual.clone(), earlier.clone()],
            ..Facts::default()
        },
    )]);

    let linked = linker::link(&facts);
    assert_eq!(linked.edges.len(), 2);
    assert_eq!(linked.edges[0].path, earlier.path);
    assert_eq!(linked.edges[1].path, unusual.path);
    assert_eq!(
        serde_json::to_vec(&linked.edges[0]).unwrap(),
        serde_json::to_vec(&earlier).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&linked.edges[1]).unwrap(),
        serde_json::to_vec(&unusual).unwrap()
    );

    // Deterministic SymbolId and linked edge assignment across repeated runs.
    use acdd_index::interner::LinkerInterner;
    use acdd_index::models::Node;
    let node_a = Node {
        native_semantic: None,
        id: "func:a.rs:alpha".into(),
        path: "a.rs".into(),
        language: "rust".into(),
        kind: "function".into(),
        name: "alpha".into(),
        qualname: "alpha".into(),
        line: 1,
        end_line: 3,
        is_test: false,
        generated: false,
        details: serde_json::json!({}),
    };
    let node_b = Node {
        native_semantic: None,
        id: "func:b.rs:beta".into(),
        path: "b.rs".into(),
        language: "rust".into(),
        kind: "function".into(),
        name: "beta".into(),
        qualname: "beta".into(),
        line: 1,
        end_line: 3,
        is_test: false,
        generated: false,
        details: serde_json::json!({}),
    };
    let call_edge = Edge {
        src: node_a.id.clone(),
        dst: node_b.id.clone(),
        kind: "calls".into(),
        path: "a.rs".into(),
        line: 2,
        evidence: "direct".into(),
        confidence: "exact".into(),
    };
    let catalog_one = BTreeMap::from([
        (
            "a.rs".to_owned(),
            Facts {
                nodes: vec![node_a.clone()],
                edges: vec![call_edge.clone()],
                ..Facts::default()
            },
        ),
        (
            "b.rs".to_owned(),
            Facts {
                nodes: vec![node_b.clone()],
                ..Facts::default()
            },
        ),
    ]);
    let catalog_two = BTreeMap::from([
        (
            "b.rs".to_owned(),
            Facts {
                nodes: vec![node_b],
                ..Facts::default()
            },
        ),
        (
            "a.rs".to_owned(),
            Facts {
                nodes: vec![node_a],
                edges: vec![call_edge],
                ..Facts::default()
            },
        ),
    ]);
    let interner_one = LinkerInterner::from_catalog(&catalog_one);
    let interner_two = LinkerInterner::from_catalog(&catalog_two);
    for sym in ["func:a.rs:alpha", "func:b.rs:beta"] {
        assert_eq!(
            interner_one.lookup_symbol(sym),
            interner_two.lookup_symbol(sym),
            "SymbolId for {sym} must remain deterministic across repeated runs"
        );
    }
    let run_one = linker::link(&catalog_one);
    let run_two = linker::link(&catalog_two);
    assert_eq!(
        serde_json::to_vec(&run_one.edges).unwrap(),
        serde_json::to_vec(&run_two.edges).unwrap()
    );
}
