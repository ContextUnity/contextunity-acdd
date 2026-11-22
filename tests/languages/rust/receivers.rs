use super::{coverage, linked};

const PAYLOAD_CASES: &[(&str, &str, bool, &str)] = &[
    ("filter", "filter(|_| true)", true, "resolved"),
    ("take", "take(1)", true, "resolved"),
    ("skip", "skip(1)", true, "resolved"),
    ("step_by", "step_by(1)", true, "resolved"),
    ("collect_into_iter", "collect::<Vec<_>>().into_iter()", true, "resolved"),
    ("map_constant", "map(|_| B)", false, "unresolved"),
    ("map_conversion", "map(convert)", false, "resolved"),
    ("enumerate", "enumerate()", false, "unresolved"),
    ("map_identity", "map(|item| item)", false, "unresolved"),
];

const ITERATOR_MEMBERS: &[(&str, &str)] = &[
    ("filter", "|_| true"),
    ("map", "|item| item"),
    ("filter_map", "|item| Some(item)"),
    ("flat_map", "|item| Some(item)"),
    ("inspect", "|_| {}"),
    ("fuse", ""),
    ("collect", ""),
    ("enumerate", ""),
    ("take", "1"),
    ("skip", "1"),
    ("for_each", "|_| {}"),
];

#[test]
fn computed_iterator_receiver_identity() {
    let computed = linked(
        "src/probe_task6_chain.rs",
        r#"
            use std::process::Command;
            use serde_json::Value;
            use rusqlite::{Connection, Statement, Row};
            type Syntax<'a> = tree_sitter::Node<'a>;
            fn probe_chained(opt: Option<&str>) {
                let s = opt.unwrap();
                let is_emp = s.trim().is_empty();
                opt.and_then(|s| Some(s)).unwrap().trim().is_empty();
            }
            fn probe_external(cmd: &mut Command, json: &Value) {
                cmd.args(["x"]).output();
                cmd.arg("x").output();
                json.get("key").unwrap().as_str().unwrap().trim().is_empty();
                json.as_array().unwrap().is_empty();
            }
            fn probe_references(conn: &Connection, stmt: &Statement, row: &Row, node: &Syntax<'_>) {
                conn.execute("", []); stmt.column_count(); row.get(0); node.kind();
            }
        "#,
    );
    let mut source = String::from(
        "struct Item; impl Item { fn finish(&self) {} }\n\
         struct A; impl A { fn finish(&self) {} }\n\
         struct B; impl B { fn finish(&self) {} }\n\
         fn convert(_: A) -> B { B }\n\
         trait Finish { fn finish(&self); }\n\
         impl Finish for (usize, A) { fn finish(&self) {} }\n",
    );
    for (index, (member, arguments)) in ITERATOR_MEMBERS.iter().enumerate() {
        source.push_str(&format!("fn probe_{index}(items: Vec<Item>) {{ let stream = items.into_iter(); stream.{member}({arguments}); }}\n"));
    }
    for (index, (_, adapter, _, _)) in PAYLOAD_CASES.iter().enumerate() {
        source.push_str(&format!(
            "fn payload_{index}(items: Vec<A>) {{ let mut adapted = items.into_iter().{adapter}; let item_{index} = adapted.next().unwrap(); item_{index}.finish(); }}\n"
        ));
    }
    source.push_str(
        "fn iterator_results(items: Vec<Item>) {\
         items.iter().flat_map(|item| [item]).count();\
         items.iter().inspect(|_| {}).count();\
         items.iter().fuse().count();\
         let changed = items.iter().flat_map(|_| [1]).collect();\
         for item in changed { item.finish(); }\
         }\n",
    );
    source.push_str("fn guards(option: Option<Item>, result: Result<Item, ()>, items: Vec<Item>) { option.is_some_and(|_| true); option.is_none_or(|_| true); result.is_ok_and(|_| true); result.is_err_and(|_| true); let slice = items.as_slice(); slice.len(); slice.is_empty(); let stream = items.into_iter(); stream.made_up(); }\n");
    source.push_str("struct Inner { nodes: Vec<Item> } struct Facts { inner: Inner } struct Root(std::path::PathBuf); fn fields(facts: &Facts, root: &Root) { for node in facts.inner.nodes.as_slice().iter() { node.finish(); } let nodes = facts.inner.nodes.iter().filter(|_| true).collect(); for collected in nodes { collected.finish(); } root.0.join(\"child\"); }\n");
    let owned = crate::languages::harness::ExtractorHarness::for_lang("rust")
        .at_path("owned_iterators.rs")
        .linked(&source);

    let mut cases = Vec::new();
    for expression in [
        "opt.and_then(|s| Some(s)).unwrap().trim().is_empty",
        "cmd.args([\"x\"]).output",
        "cmd.arg(\"x\").output",
        "json.get(\"key\").unwrap().as_str().unwrap().trim().is_empty",
        "json.as_array().unwrap().is_empty",
        "conn.execute",
        "stmt.column_count",
        "row.get",
        "node.kind",
    ] {
        cases.push((&computed, expression.to_owned(), "external", ""));
    }
    let stdlib = computed
        .coverage
        .iter()
        .find(|row| row.expression.ends_with("is_empty"))
        .unwrap();
    cases.push((
        &computed,
        stdlib.expression.clone(),
        "external",
        "Rust standard library",
    ));
    for expression in ITERATOR_MEMBERS
        .iter()
        .map(|(member, _)| format!("stream.{member}"))
        .chain(
            [
                "option.is_some_and",
                "option.is_none_or",
                "result.is_ok_and",
                "result.is_err_and",
                "slice.len",
                "slice.is_empty",
            ]
            .map(str::to_owned),
        )
    {
        cases.push((&owned, expression, "external", ""));
    }
    cases.push((&owned, "stream.made_up".to_owned(), "unresolved", ""));
    for (expression, status) in [
        ("item.finish", "unresolved"),
        ("node.finish", "resolved"),
        ("collected.finish", "resolved"),
        ("root.0.join", "external"),
    ] {
        cases.push((&owned, expression.to_owned(), status, ""));
    }
    for (graph, expression, status, evidence) in cases {
        let row = coverage(graph, &expression);
        assert_eq!(row.status, status, "{expression}: {row:#?}");
        assert!(
            row.evidence.contains(evidence),
            "{expression}: expected {evidence}: {row:#?}"
        );
    }
    let target = owned
        .facts
        .nodes
        .iter()
        .find(|node| node.name == "finish" && node.details["receiver_type"] == "A")
        .unwrap();
    for (index, (label, adapter, preserves_item, expected_status)) in PAYLOAD_CASES.iter().enumerate() {
        let source = format!(
            "struct A; impl A {{ fn finish(&self) {{}} }}\n\
             struct B; impl B {{ fn finish(&self) {{}} }}\n\
             fn convert(_: A) -> B {{ B }}\n\
             trait Finish {{ fn finish(&self); }}\n\
             impl Finish for (usize, A) {{ fn finish(&self) {{}} }}\n\
             fn run(items: Vec<A>) {{ let mut adapted = items.into_iter().{adapter}; let item = adapted.next().unwrap(); item.finish(); }}"
        );
        let facts = crate::languages::harness::ExtractorHarness::for_lang("rust")
            .at_path("iterator.rs")
            .extract(&source);
        assert!(facts.errors.is_empty(), "{adapter}: {facts:#?}");

        let expression = format!("item_{index}.finish");
        let observed = coverage(&owned, &expression);
        assert_eq!(
            observed.status,
            *expected_status,
            "{label}: {observed:#?}"
        );
        if *label == "map_conversion" {
            assert!(
                observed.evidence.contains("B.finish"),
                "{label}: expected B.finish evidence: {observed:#?}"
            );
        }
        let calls_a_target = owned.edges.iter().any(|edge| {
            edge.kind == "calls" && edge.evidence == expression && edge.dst == target.id
        });
        assert_eq!(calls_a_target, *preserves_item, "{label}: {owned:#?}");
    }

    let counts: Vec<_> = owned
        .coverage
        .iter()
        .filter(|row| row.expression.ends_with(".count"))
        .collect();
    assert_eq!(counts.len(), 3, "{owned:#?}");
    for row in counts {
        assert_eq!(row.status, "external", "{row:#?}");
    }
}

#[test]
fn static_variants_associated_type_derives_slices_and_field_chains_matrix() {
    let source = r#"
        struct Reference { line: usize }
        struct Facts { references: Vec<Reference> }
        struct Config { port: u16 }
        impl Config { fn port(&self) -> u16 { self.port } }
        static GLOBAL_CFG: Config = Config { port: 8080 };
        trait Container { type Item; }
        #[derive(Default, Clone, Debug, Serialize, Deserialize)]
        struct DerivedModel { label: String }
        impl DerivedModel { fn ping(&self) {} }
        struct NoClone;
        impl NoClone { fn ping(&self) {} }
        struct Target;
        impl Target { fn target_only(&self) {} }
        struct CustomClone;
        impl CustomClone { fn clone(&self) -> Target { Target } }
        enum Format { Json, JSON, A }
        impl Format { fn tag(&self) {} }
        fn probe(
            facts: &mut Facts,
            slice: &[&str],
            k: usize,
            nc: &NoClone,
            cc: &CustomClone,
        ) {
            GLOBAL_CFG.port();
            facts.references.last_mut();
            let joined = slice[..k].join(",");
            joined.is_empty();
            let d = DerivedModel::default();
            d.clone().ping();
            Format::Json.tag();
            Format::JSON.tag();
            Format::A.tag();
            let bad = Format::NonExistentVariant();
            bad.tag();
            nc.clone().ping();
            cc.clone().target_only();
        }
    "#;
    let facts = acdd_index::ast::extract("src/ac1_ac2.rs", "rust", source).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "constant" && n.name == "GLOBAL_CFG"),
        "{facts:#?}"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "type" && n.name == "Item"),
        "expected associated_type indexed as NodeKind::Type: {facts:#?}"
    );
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.expression.ends_with(".join") && !r.dynamic),
        "expected slice[..k].join with dynamic: false: {facts:#?}"
    );
    let graph = acdd_index::linker::link(&std::collections::BTreeMap::from([(
        "src/ac1_ac2.rs".to_owned(),
        facts,
    )]));
    for (expr, expected) in [
        ("GLOBAL_CFG.port", "resolved"),
        ("facts.references.last_mut", "external"),
        ("joined.is_empty", "external"),
        ("DerivedModel::default", "resolved"),
        ("d.clone().ping", "resolved"),
        ("Format::Json.tag", "resolved"),
        ("Format::JSON.tag", "resolved"),
        ("Format::A.tag", "resolved"),
        ("cc.clone().target_only", "resolved"),
        ("bad.tag", "unresolved"),
        ("nc.clone().ping", "unresolved"),
    ] {
        let cov = coverage(&graph, expr);
        assert_eq!(
            cov.status, expected,
            "expression {expr} expected status {expected}: {cov:#?}"
        );
    }
}

#[test]
fn callback_guard_payload_identity() {
    enum Expected {
        CallsToA(bool),
        Coverage(&'static str, &'static str),
    }
    let mut cases = Vec::new();
    for (label, expression, resolved) in [
        (
            "identity",
            "opt.and_then(|value| Some(value)).unwrap()",
            true,
        ),
        ("changed", "opt.and_then(|_| Some(B)).unwrap()", false),
        ("unknown", "opt.and_then(|_| opaque()).unwrap()", false),
        ("callback", "opt.and_then(callback).unwrap()", false),
        (
            "reassigned",
            "{ let mut value = opt.and_then(|v| Some(v)).unwrap(); value = opaque(); value }",
            false,
        ),
    ] {
        let source = format!("struct A; impl A {{ fn finish(&self) {{}} }} struct B; impl B {{ fn finish(&self) {{}} }} fn probe(opt: Option<A>) {{ let result = {expression}; result.finish(); }}");
        cases.push((
            label,
            "callbacks.rs",
            source,
            vec![Expected::CallsToA(resolved)],
        ));
    }
    cases.push(("query_callbacks_require_proven_rusqlite_receivers", "query_callbacks.rs", "use rusqlite::{Connection, Statement}; struct Fake; impl Fake { fn query_row<F>(&self, callback: F) {} } fn probe(conn: &Connection, statement: &mut Statement, fake: Fake) { conn.query_row(\"sql\", [], |row| row.get(0)); conn.query_row::<String, _, _>(\"sql\", [], |typed| typed.get(0)); statement.query_map([], |r| r.get(0)); fake.query_row(|unproven| unproven.get(0)); }".to_owned(), vec![Expected::Coverage("row.get", "external"), Expected::Coverage("typed.get", "external"), Expected::Coverage("r.get", "external"), Expected::Coverage("unproven.get", "unresolved")]));
    for (case_id, path, source, expected) in cases {
        let graph = linked(path, &source);
        for expectation in expected {
            match expectation {
                Expected::CallsToA(resolved) => {
                    let caller = graph
                        .facts
                        .nodes
                        .iter()
                        .find(|node| node.name == "probe")
                        .unwrap();
                    let target = graph
                        .facts
                        .nodes
                        .iter()
                        .find(|node| node.qualname.ends_with(".A.finish"))
                        .unwrap();
                    let count = graph
                        .edges
                        .iter()
                        .filter(|edge| {
                            edge.kind == "calls" && edge.src == caller.id && edge.dst == target.id
                        })
                        .count();
                    assert_eq!(
                        count,
                        usize::from(resolved),
                        "{case_id}: {:#?}",
                        graph.coverage
                    );
                }
                Expected::Coverage(expression, status) => {
                    let row = coverage(&graph, expression);
                    assert_eq!(row.status, status, "{case_id}: {row:#?}");
                }
            }
        }
    }
}
