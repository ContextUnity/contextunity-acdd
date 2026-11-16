use super::{coverage, linked};

#[test]
fn macro_identity_and_lexical_precedence() {
    struct Case {
        case_id: String,
        path: &'static str,
        source: String,
        // expression, expected status, equality, evidence, select last occurrence
        expected: Vec<(String, &'static str, bool, &'static str, bool)>,
        clean_parse: bool,
    }
    let mut cases = Vec::new();
    for (label, source, names, external) in [
        (
            "imported",
            r#"
use anyhow::bail;
use anyhow::ensure;
use serde_json::json;

fn run_macros() {
    let _f = format!("hello {}", 42);
    let _v = vec![1, 2, 3];
    let _w = write!(f, "data");
    let _j = json!({ "key": "value" });
    let _b = bail!("operation failed");
    let _e = ensure!(true);
    let _qb = anyhow::bail!("qualified bail");
    let _qe = anyhow::ensure!(true);
    let _qj = serde_json::json!({ "status": 200 });
    tracing::info!("hello");
    log::info!("hello");
}
"#,
            &[
                "format",
                "vec",
                "write",
                "json",
                "bail",
                "ensure",
                "anyhow::bail",
                "anyhow::ensure",
                "serde_json::json",
                "tracing::info",
                "log::info",
            ][..],
            true,
        ),
        (
            "unimported",
            r#"
fn run_unimported_macros() {
    let _j = json!({ "key": "value" });
    let _b = bail!("operation failed");
    let _e = ensure!(true);
}
"#,
            &["json", "bail", "ensure"][..],
            false,
        ),
    ] {
        let mut expected: Vec<_> = names
            .iter()
            .map(|name| {
                (
                    (*name).to_owned(),
                    if external { "external" } else { "unresolved" },
                    external,
                    "",
                    false,
                )
            })
            .collect();
        if external {
            expected.extend([
                (
                    "tracing::info".to_owned(),
                    "external",
                    true,
                    "macro tracing::",
                    false,
                ),
                (
                    "log::info".to_owned(),
                    "external",
                    true,
                    "macro log::",
                    false,
                ),
            ]);
        }
        cases.push(Case {
            case_id: format!("declarative_macros_keep_their_declared_external_identity/{label}"),
            path: "macros.rs",
            source: source.to_owned(),
            expected,
            clean_parse: false,
        });
    }
    cases.push(Case {
        case_id: "local_macro_definitions_precede_external_identity_and_type_inference".to_owned(),
        path: "local_macro.rs",
        source: r#"
struct A;
macro_rules! json { () => { 1 }; }
macro_rules! vec { () => { A }; }
macro_rules! format { () => { A }; }
fn run() {
    let _value = json!();
    let vector = vec!();
    vector.len();
    let formatted = format!();
    formatted.len();
}
"#
        .to_owned(),
        expected: ["json", "vec", "format", "vector.len", "formatted.len"]
            .map(|name| (name.to_owned(), "external", false, "", false))
            .to_vec(),
        clean_parse: true,
    });
    for name in ["vec", "format"] {
        for (label, import, definition) in [
            (
                "named",
                format!("use local::{name};"),
                format!("macro_rules! {name} {{ () => {{ A }}; }} pub(crate) use {name};"),
            ),
            (
                "renamed",
                format!("use local::custom as {name};"),
                "macro_rules! custom { () => { A }; } pub(crate) use custom;".to_owned(),
            ),
            (
                "crate",
                format!("use crate::local::{name};"),
                format!("macro_rules! {name} {{ () => {{ A }}; }} pub(crate) use {name};"),
            ),
        ] {
            cases.push(Case { case_id: format!("local_macro_definitions_precede_external_identity_and_type_inference/{name}/{label}"), path: "probe.rs", source: format!("struct A; mod local {{ {definition} }} {import} fn run() {{ let item = {name}!(); item.len(); }}"), expected: vec![("item.len".to_owned(), "external", false, "", false)], clean_parse: true });
        }
    }
    for (label, source, expression) in [
        ("qualified", "mod serde_json { macro_rules! json { () => { 1 }; } pub(crate) use json; } fn run() { serde_json::json!(); }", "serde_json::json"),
        ("import", "mod local { macro_rules! json { () => { 1 }; } pub(crate) use json; } use local::json; fn run() { json!(); }", "json"),
        ("renamed-import", "mod local { macro_rules! custom { () => { 1 }; } pub(crate) use custom; } use local::custom as json; fn run() { json!(); }", "json"),
        ("crate-import", "mod local { macro_rules! json { () => { 1 }; } pub(crate) use json; } use crate::local::json; fn run() { json!(); }", "json"),
        ("module-alias", "mod local { macro_rules! custom { () => { 1 }; } pub(crate) use custom as json; } use local as serde_json; fn run() { serde_json::json!(); }", "serde_json::json"),
        ("parent-module", "mod outer { macro_rules! json { () => { 1 }; } mod inner { fn run() { json!(); } } }", "json"),
        ("parent-control", "mod outer { macro_rules! custom { () => { 1 }; } mod inner { fn run() { custom!(); } } }", "custom"),

    ] {
        cases.push(Case { case_id: format!("local_macro_imports_resolve_only_in_their_scope/{label}"), path: "probe.rs", source: source.to_owned(), expected: vec![(expression.to_owned(), "external", false, "", false), (expression.to_owned(), "resolved", true, "", false)], clean_parse: true });
    }
    for (label, source, expression) in [
        ("direct-import", "mod local { pub fn available() {} } use local::missing; fn run() { missing(); }", "missing"),
        ("module-alias", "mod local { pub fn available() {} } use local as alias; fn run() { alias::missing(); }", "alias::missing"),
        ("macro-member", "mod local { macro_rules! custom { () => { 1 }; } pub(crate) use custom; } use local as serde_json; fn run() { serde_json::json!(); }", "serde_json::json"),

    ] {
        cases.push(Case { case_id: format!("local_macro_imports_resolve_only_in_their_scope/{label}"), path: "probe.rs", source: source.to_owned(), expected: vec![(expression.to_owned(), "resolved", false, "", false)], clean_parse: true });
    }
    cases.push(Case { case_id: "local_macro_imports_resolve_only_in_their_scope/outer".to_owned(), path: "probe.rs", source: "mod local { macro_rules! custom { () => { 1 }; } pub(crate) use custom; } mod child { use crate::local::custom as json; fn scoped() { json!(); } }\nfn run() { json!(); }".to_owned(), expected: vec![("json".to_owned(), "resolved", false, "", true)], clean_parse: true });
    for (label, source) in [
        ("sibling-macro", "mod other { macro_rules! vec { () => { 1 }; } } fn run() { let item = vec![1]; item.len(); }"),
        ("value-namespace", "use std::fmt::format; fn run() { let item = format!(\"hello\"); item.len(); }"),
        ("later-macro", "fn run() { let item = vec![1]; item.len(); } macro_rules! vec { () => { 1 }; }"),
        ("foreign-function", "mod local { extern \"C\" { pub fn custom(); } } use local::custom as vec; fn run() { let item = vec![1]; item.len(); }"),
        ("foreign-static", "mod local { extern \"C\" { pub static custom: u8; } } use local::custom as format; fn run() { let item = format!(\"hello\"); item.len(); }"),
        ("extern-crate", "mod local { pub extern crate core as custom; } use local::custom as vec; fn run() { let item = vec![1]; item.len(); }"),

    ] {
        cases.push(Case { case_id: format!("unrelated_namespaces_and_later_macros_do_not_suppress_stdlib_results/{label}"), path: "probe.rs", source: source.to_owned(), expected: vec![("item.len".to_owned(), "external", true, "", false)], clean_parse: true });
    }
    cases.push(Case { case_id: "unrelated_namespaces_and_later_macros_do_not_suppress_stdlib_results/ordinary-functions".to_owned(), path: "ordinary_functions.rs", source: r#"
fn run() {
    let _json = json();
    let _format = format();
    let _vec = vec();
    let _write = write();
    let _println = println();
}
"#.to_owned(), expected: ["json", "format", "vec", "write", "println"].map(|name| (name.to_owned(), "unresolved", true, "", false)).to_vec(), clean_parse: false });
    for case in cases {
        let graph = linked(case.path, &case.source);
        if case.clean_parse {
            assert!(
                graph.facts.errors.is_empty(),
                "{}: {:?}",
                case.case_id,
                graph.facts.errors
            );
        }
        for (expression, status, equal, evidence, last) in case.expected {
            let row = if last {
                graph
                    .coverage
                    .iter()
                    .filter(|r| r.expression == expression)
                    .max_by_key(|r| r.line)
                    .unwrap()
            } else {
                coverage(&graph, &expression)
            };
            assert_eq!(
                row.status == status,
                equal,
                "{}: {expression}: {row:#?}",
                case.case_id
            );
            assert!(
                row.evidence.contains(evidence),
                "{}: expected {evidence}: {row:#?}",
                case.case_id
            );
        }
    }
}

#[test]
fn iterator_chains_and_local_adapters_preserve_only_proven_receivers() {
    let chain_source = r#"
fn run_iterator_chain() {
    let numbers = Vec::new();
    let _collected = numbers.into_iter().filter(|x| true).map(|x| x).collect();
    let _deep = numbers.into_iter().filter(|_| true).map(|x| x).take(5).enumerate().skip(1).step_by(2).collect();
    let _from_macro = vec![1, 2, 3].into_iter().filter(|x| true).map(|x| x).collect();
}
"#;
    let chain_graph = linked("chain.rs", chain_source);
    for expr in [
        "numbers.into_iter().filter(|x| true).map(|x| x).collect",
        "numbers.into_iter().filter(|_| true).map(|x| x).take(5).enumerate().skip(1).step_by(2).collect",
        "vec![1, 2, 3].into_iter().filter(|x| true).map(|x| x).collect",
    ] {
        let collect_cov = coverage(&chain_graph, expr);
        assert_ne!(
            collect_cov.status, "unresolved",
            "collect() on chained iterator receiver must not be unresolved: {collect_cov:#?}"
        );
    }

    let over_eight_source = r#"
fn run_over_eight_chain() {
    let numbers = Vec::new();
    let _call7 = numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).collect();
    let _call8 = numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).collect();
    let _over9 = numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).collect();
    let _over10 = numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).take(1).collect();
    let _over19 = numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).collect();
}
"#;
    let over_eight_graph = linked("over_eight.rs", over_eight_source);
    for (expr, expected) in [
        ("numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).collect", "external"),
        ("numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).collect", "external"),
        ("numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).collect", "unresolved"),
        ("numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).take(1).collect", "unresolved"),
        ("numbers.into_iter().map(|x| x).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).take(1).collect", "unresolved"),
    ] {
        let cov = coverage(&over_eight_graph, expr);
        assert_eq!(
            cov.status, expected,
            "chain call {expr} expected status {expected}: {cov:#?}"
        );
    }
    for iter_name in ["iter", "into_iter", "iter_mut"] {
        for adapter in ["take(1)", "skip(1)", "step_by(1)", "filter(|_| true)"] {
            let r4_source = format!(
                "struct A; struct B;
                 impl A {{
                     fn {iter_name}(&self) -> A {{ A }}
                     fn take(&self, _: usize) -> B {{ B }}
                     fn skip(&self, _: usize) -> B {{ B }}
                     fn step_by(&self, _: usize) -> B {{ B }}
                     fn filter(&self, _: fn(&A) -> bool) -> B {{ B }}
                     fn finish(&self) {{}}
                 }}
                 fn run(a: A) {{
                     let item = a.{iter_name}().{adapter};
                     item.finish();
                 }}"
            );
            let r4_graph = linked("r4.rs", &r4_source);
            let r4_cov = coverage(&r4_graph, "item.finish");
            assert_ne!(
                r4_cov.status, "resolved",
                "item.finish on non-iterator B from {iter_name}.{adapter} must not resolve to A.finish: {r4_cov:#?}"
            );
        }
    }
}
