use acdd_index::models::compact_graph::{CompactEdge, CompactGraph};
use acdd_index::models::{EdgeConfidence, EdgeKind, Facts, Node};
use acdd_index::semantic::{
    CallableParameter, CallableSignature, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use acdd_index::typed_facts::{decode_facts, encode_facts, TypedFacts};
use std::collections::BTreeSet;

#[test]
fn callback_param_callable_signature_and_compact_graph_contracts() {
    let sig = CallableSignature {
        has_receiver: true,
        params: vec![CallableParameter {
            name: Some("cb".into()),
            type_expr: TypeExpr::Named { name: "FnMut".into() },
            callback_params: vec![TypeExpr::Named { name: "Item".into() }],
        }],
        return_type: Some(TypeExpr::Named { name: "bool".into() }),
    };
    assert!(!sig.is_empty());
    assert!(sig.valid());

    let callback_expr = ValueExpr::CallbackParam {
        callee: Box::new(ValueExpr::Call {
            callee: "items.iter().map".into(),
            site: None,
        }),
        arg_index: 0,
        param_index: 0,
    };
    assert!(callback_expr.valid_call_sites(0));

    let mut flow = ValueFlowFacts::default();
    flow.bindings.push(acdd_index::semantic::BindingFact {
        name: "item".into(),
        position: SourcePosition { line: 4, column: 8 },
        value: callback_expr,
        conditional: false,
    });
    flow.callable_signature = Some(sig.clone());

    let mut typed_keys = BTreeSet::new();
    flow.reference_keys(|k| {
        typed_keys.insert(k.to_owned());
    });
    let serialized = serde_json::to_value(&flow).unwrap();
    let mut raw_keys = BTreeSet::new();
    ValueFlowFacts::reference_keys_from_value(&serialized, |k| {
        raw_keys.insert(k.to_owned());
    });
    assert_eq!(typed_keys, raw_keys);
    assert!(typed_keys.contains("Item"));
    assert!(typed_keys.contains("items.iter().map"));

    flow.offset_lines(10);
    assert_eq!(flow.bindings[0].position.line, 14);

    let node = Node {
        id: "fn:src/lib.rs:1:apply".into(),
        kind: "function".into(),
        name: "apply".into(),
        qualname: "lib.apply".into(),
        path: "src/lib.rs".into(),
        line: 1,
        end_line: 3,
        is_test: false,
        language: "rust".into(),
        generated: false,
        details: serde_json::Value::Null,
        native_semantic: None,
    };
    let mut typed = TypedFacts::from_public(Facts {
        nodes: vec![node],
        ..Default::default()
    });
    typed.flows.insert("fn:src/lib.rs:1:apply".into(), flow);

    let blob = encode_facts(&typed).unwrap();
    let reopened = decode_facts(&blob).unwrap();
    let reopened_flow = reopened.flows.get(&reopened.nodes[0]).unwrap();
    assert_eq!(reopened_flow.callable_signature.as_ref(), Some(&sig));

    let mut graph = CompactGraph::default();
    graph.push_edge(
        "src/lib.rs",
        "fn:a",
        "fn:b",
        EdgeKind::Calls,
        2,
        "b()",
        EdgeConfidence::Inferred,
    );
    let edges: Vec<CompactEdge> = graph.edges.clone();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].dst.as_ref(), "fn:b");
}
