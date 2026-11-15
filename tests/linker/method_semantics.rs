#![cfg(all(feature = "lang-python", feature = "lang-rust"))]

use crate::common::Workspace;
use contextunity_acdd::{
    core::response::{QueryOptions, ResponsePolicy},
    db::symbols,
    engine::ast,
};
use serde_json::Value;

const RECORD_RUST: &str = r#"
struct Record;
impl Marker for Record {
    fn persist_record(&self) {}
    fn create_record() {}
}
impl Record {
    fn direct_record(&mut self) {}
    fn static_record() {}
    fn outer_record(&self) {
        fn nested_record() {}
    }
}
fn module_record(self) {}
"#;

const RECORD_PYTHON: &str = r#"
class PythonRecord:
    def persist_python(self): pass

    @staticmethod
    def create_python(value): pass

    def implicit_static_python(value): pass

    @classmethod
    def class_python(cls): pass

    def outer_python(self):
        def nested_python(): pass
        return nested_python()

def module_python(self): pass
"#;

const RECORD_TYPESCRIPT: &str = r#"
class TsRecord {
    persist_ts(): void {}
    static create_ts(): void {}
}
function module_ts(): void {}
"#;

const NULLABLE_PYTHON: &str = r#"
from typing import Optional, Union
import typing as t
import typing_extensions as te

class Service:
    def work(self): pass

class Other:
    def other(self): pass

def optional_import(optional: Optional[Service]):
    optional.work()

def union_import(unioned: Union[Service, None]):
    unioned.work()

def union_none_first(none_first: Union[None, Service]):
    none_first.work()

def module_optional(module_optional: t.Optional[Service]):
    module_optional.work()

def extensions_union(extensions_union: te.Union[Service, None]):
    extensions_union.work()

def typed_local(value):
    local_optional: Optional[Service] = value
    local_optional.work()
    local_union: Union[Service, None] = value
    local_union.work()

def multi_nonnull(multi_nonnull: Union[Service, Other]):
    multi_nonnull.work()

def multi_nonnull_with_none(multi_nonnull_with_none: Union[Service, Other, None]):
    multi_nonnull_with_none.work()

def bad_optional_arity(bad_optional_arity: Optional[Service, Other]):
    bad_optional_arity.work()

def pep604(pep604: Service | None):
    pep604.work()

def pep604_reversed(pep604_reversed: None | Service):
    pep604_reversed.work()

def pep604_str(pep604_str: 'Service | None'):
    pep604_str.work()

def pep604_multi(pep604_multi: Service | Other):
    pep604_multi.work()
"#;

const CLEAN_PEP604_PYTHON: &str = r#"
class Service:
    def work(self): pass

class Other:
    def other(self): pass

class NoneType:
    def work(self): pass

def clean_pep604(clean: Service | None):
    clean.work()

def clean_reversed(rev: None | Service):
    rev.work()

def clean_parenthesized(paren: (Service | None)):
    paren.work()

def clean_local(value):
    local: Service | None = value
    local.work()

def clean_nonetype(nt: Service | NoneType):
    nt.work()

def clean_nested_list(lst: list[Service | None]):
    lst.work()

def clean_bad_pipe(bad: 'Service | | None'):
    bad.work()

def clean_local_quoted_paren(value):
    local_qp: '(Service | None)' = value
    local_qp.work()

class CustomUnionScope:
    class Union: pass
    def custom_union(self, cu: Union[Service, None]):
        cu.work()
"#;

fn method_workspace() -> &'static crate::common::sqlite::PersistedSnapshot {
    static SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        let workspace = Workspace::new();
        workspace.write("record.rs", RECORD_RUST);
        workspace.write("record.py", RECORD_PYTHON);
        workspace.write("record.ts", RECORD_TYPESCRIPT);
        workspace.write("nullable.py", NULLABLE_PYTHON);
        workspace.write("clean_pep604.py", CLEAN_PEP604_PYTHON);
        workspace.build();
        crate::common::sqlite::PersistedSnapshot::from_connection(&workspace.open())
    })
}

fn stored_node(conn: &rusqlite::Connection, path: &str, name: &str) -> (String, Value) {
    let (kind, details): (String, String) = conn
        .query_row(
            "SELECT n.kind, n.details FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.name=?2",
            [path, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    (kind, serde_json::from_str(&details).unwrap())
}

#[test]
fn rust_and_python_methods_are_classified_persisted_and_searchable() {
    let rust = RECORD_RUST;
    let python = RECORD_PYTHON;

    let typescript = RECORD_TYPESCRIPT;

    for (path, language, source) in [
        ("record.rs", "rust", rust),
        ("record.py", "python", python),
        ("record.ts", "typescript", typescript),
    ] {
        let facts = ast::extract(path, language, source).unwrap();
        assert!(facts.errors.is_empty(), "{path}: {:?}", facts.errors);
    }

    let workspace = method_workspace();
    let conn = workspace.open();

    for (name, receiver_type, is_static) in [
        ("persist_record", "Record", false),
        ("create_record", "Record", true),
        ("direct_record", "Record", false),
        ("static_record", "Record", true),
        ("persist_python", "PythonRecord", false),
        ("create_python", "PythonRecord", true),
        ("implicit_static_python", "PythonRecord", true),
        ("class_python", "PythonRecord", false),
        ("persist_ts", "TsRecord", false),
        ("create_ts", "TsRecord", true),
    ] {
        let path = if name.contains("_python") {
            "record.py"
        } else if name.contains("_ts") {
            "record.ts"
        } else {
            "record.rs"
        };
        let (kind, details) = stored_node(&conn, path, name);
        assert_eq!(kind, "method", "{path}: {name}");
        assert_eq!(details["receiver_type"], receiver_type, "{path}: {name}");
        assert!(
            details.get("is_method").is_none(),
            "{path}: {name}: method kind already carries this fact"
        );
        assert_eq!(details["is_static"] == true, is_static, "{path}: {name}");
    }

    for (path, name) in [
        ("record.rs", "nested_record"),
        ("record.rs", "module_record"),
        ("record.py", "nested_python"),
        ("record.py", "module_python"),
        ("record.ts", "module_ts"),
    ] {
        let (kind, details) = stored_node(&conn, path, name);
        assert_eq!(kind, "function", "{path}: {name}");
        assert_ne!(details["is_method"], true, "{path}: {name}");
    }

    for pattern in ["persist_record*", "persist_python*", "persist_ts*"] {
        let page =
            QueryOptions::resolve(&ResponsePolicy::default(), Some(10), 0, None, None).unwrap();
        let found = symbols::search_with_options(
            &conn,
            pattern,
            &symbols::SearchOptions {
                kind: Some("method"),
                path: None,
                include_docs: false,
                exact: false,
                page: &page,
            },
        )
        .unwrap();
        assert!(
            found["nodes"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| {
                    node["kind"] == "method"
                        && node["name"]
                            .as_str()
                            .is_some_and(|name| name == pattern.trim_end_matches('*'))
                }),
            "method search missed {pattern}: {found}"
        );
    }
}

#[test]
#[cfg(feature = "lang-typescript")]
fn explicit_payloads_and_bounded_dispatch_survive_index_readback() {
    #[derive(Debug)]
    struct StoredEdge {
        src: String,
        dst: String,
        dst_name: String,
        kind: String,
        confidence: String,
        evidence: String,
    }
    let read_edges = |conn: &rusqlite::Connection| {
        let mut statement = conn
            .prepare(
                "SELECT s.id, d.id, d.name, e.kind, c.evidence, v.evidence \
             FROM edges e JOIN nodes s ON s.node_hash=e.src_hash \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             JOIN coverage_evidence c ON c.evidence_id=e.confidence_id \
             JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
             ORDER BY s.id, e.kind, d.id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(StoredEdge {
                    src: row.get(0)?,
                    dst: row.get(1)?,
                    dst_name: row.get(2)?,
                    kind: row.get(3)?,
                    confidence: row.get(4)?,
                    evidence: row.get(5)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let has_coverage = |conn: &rusqlite::Connection, expression: &str, status: &str| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
             JOIN coverage_expressions x ON x.expression_id=r.expression_id \
             WHERE x.expression=?1 AND r.status=?2)",
            [expression, status],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    let mut failures = Vec::new();

    // Only an explicit operation unwraps a standard wrapper's first payload.
    // Sources are parsed and indexed statically, never compiled or executed.
    let mut wrapper_cases = Vec::new();
    let mut wrapper_source = "struct Service; struct Error;\nimpl Service { fn work(&self) {} }\n".to_owned();
    for wrapper in [
        "Option<Service>",
        "Result<Service, Error>",
        "anyhow::Result<Service>",
    ] {
        for (operation, adapter) in [
            ("?", ""),
            (".unwrap()", ""),
            (".expect(\"present\")", ""),
            (".as_ref().unwrap()", ".as_ref()"),
            (".as_mut().unwrap()", ".as_mut()"),
            (".ok().unwrap()", ".ok()"),
            (".ok()?", ".ok()"),
        ] {
            if wrapper.starts_with("Option") && adapter == ".ok()" {
                continue;
            }
            let idx = wrapper_cases.len();
            wrapper_source.push_str(&format!(
                "fn make_{idx}() -> {wrapper} {{ unknown() }}\n\
                 fn caller_{idx}() {{ let payload_{idx} = make_{idx}(){operation}; payload_{idx}.work(); \
                 let wrapped_{idx} = make_{idx}(); wrapped_{idx}.work(); }}\n\
                 fn direct_caller_{idx}() {{ make_{idx}(){operation}.work(); make_{idx}().work(); }}\n\
                 fn parameter_caller_{idx}(value_{idx}: {wrapper}) {{ \
                 let parameter_payload_{idx} = value_{idx}{operation}; parameter_payload_{idx}.work(); \
                 value_{idx}.work(); }}\n\
                 fn direct_parameter_caller_{idx}(value_{idx}: {wrapper}) {{ \
                 value_{idx}{operation}.work(); value_{idx}.work(); }}\n\
                 fn adapted_caller_{idx}() {{ let adapted_{idx} = make_{idx}(){adapter}; \
                 adapted_{idx}.work(); make_{idx}(){adapter}.work(); }}\n"
            ));
            wrapper_cases.push((idx, wrapper, operation, adapter));
        }
    }
    let wrapper_facts = ast::extract("lib.rs", "rust", &wrapper_source).unwrap();
    assert!(
        wrapper_facts.errors.is_empty(),
        "wrapper/operation matrix parse errors: {:?}",
        wrapper_facts.errors
    );
    let wrapper_matrix_ws = Workspace::new();
    wrapper_matrix_ws.write("lib.rs", &wrapper_source);
    wrapper_matrix_ws.build();
    let wrapper_matrix_conn = wrapper_matrix_ws.open();
    let wrapper_matrix_edges = read_edges(&wrapper_matrix_conn);
    for &(idx, wrapper, operation, adapter) in &wrapper_cases {
        let payload_ev = format!("payload_{idx}.work");
        if !wrapper_matrix_edges.iter().any(|edge| {
            edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == payload_ev
        }) {
            let caller_name = format!("caller_{idx}");
            failures.push(format!(
                "{wrapper}/{operation}: explicit payload lacks work edge; caller flow: {:?}",
                wrapper_facts
                    .nodes
                    .iter()
                    .filter(|node| node.name == caller_name)
                    .map(|node| {
                        crate::common::assertions::projected_node_details(&wrapper_facts, node)
                            ["value_flow"]
                            .clone()
                    })
                    .collect::<Vec<_>>()
            ));
        }
        for expression in [
            format!("make_{idx}(){operation}.work"),
            format!("parameter_payload_{idx}.work"),
            format!("value_{idx}{operation}.work"),
        ] {
            if !wrapper_matrix_edges.iter().any(|edge| {
                edge.kind == "calls"
                    && edge.dst_name == "work"
                    && edge.confidence == "inferred"
                    && edge.evidence == expression
            }) {
                failures.push(format!(
                    "{wrapper}/{operation}: explicit receiver {expression} lacks payload work edge; extracted reference: {:?}; stored outgoing: {:?}",
                    wrapper_facts.references.iter().find(|reference| reference.expression == expression),
                    wrapper_matrix_edges.iter().filter(|edge| edge.evidence.starts_with(&format!("value_{idx}")) || edge.evidence == format!("parameter_payload_{idx}.work")).collect::<Vec<_>>()
                ));
            }
        }
        for expression in [
            format!("make_{idx}().work"),
            format!("value_{idx}.work"),
            format!("adapted_{idx}.work"),
            format!("make_{idx}(){adapter}.work"),
        ] {
            if wrapper_matrix_edges
                .iter()
                .any(|edge| edge.kind == "calls" && edge.evidence == expression)
                || !has_coverage(&wrapper_matrix_conn, &expression, "unresolved")
            {
                failures.push(format!(
                    "{wrapper}/{operation}: ordinary receiver {expression} exposed payload"
                ));
            }
        }
        let wrapped_ev = format!("wrapped_{idx}.work");
        if wrapper_matrix_edges
            .iter()
            .any(|edge| edge.kind == "calls" && edge.evidence == wrapped_ev)
            || !has_coverage(&wrapper_matrix_conn, &wrapped_ev, "unresolved")
        {
            failures.push(format!(
                "{wrapper}/{operation}: wrapper was implicitly unwrapped"
            ));
        }
    }

    let ts_factories = [
        ("typescript", "async function make_0() { return new Service(); }"),
        ("typescript", "async function make_1(): Promise<Service> { return new Service(); }"),
        ("typescript", "function make_2(): Promise<Service> { return unknown(); }"),
        ("typescript", "const make_3 = async () => new Service();"),
    ];
    let mut ts_source = "class Service { work() {} }\n".to_owned();
    for (idx, (_, factory)) in ts_factories.iter().enumerate() {
        ts_source.push_str(&format!(
            "{factory}\n\
             async function caller_{idx}() {{ const payload_{idx} = await make_{idx}(); payload_{idx}.work(); \
             const wrapped_{idx} = make_{idx}(); wrapped_{idx}.work(); }}\n"
        ));
    }
    let ts_facts = ast::extract("factory.ts", "typescript", &ts_source).unwrap();
    assert!(ts_facts.errors.is_empty(), "ts factories: {:?}", ts_facts.errors);
    let ts_ws = Workspace::new();
    ts_ws.write("factory.ts", &ts_source);
    ts_ws.build();
    let ts_conn = ts_ws.open();
    let ts_edges = read_edges(&ts_conn);
    for (idx, (language, factory)) in ts_factories.iter().enumerate() {
        let payload_ev = format!("payload_{idx}.work");
        let wrapped_ev = format!("wrapped_{idx}.work");
        if !ts_edges.iter().any(|edge| {
            edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == payload_ev
        }) {
            failures.push(format!("{language}/{factory}: awaited payload lacks work edge"));
        }
        if ts_edges.iter().any(|edge| edge.kind == "calls" && edge.evidence == wrapped_ev)
            || !has_coverage(&ts_conn, &wrapped_ev, "unresolved")
        {
            failures.push(format!("{language}/{factory}: un-awaited factory exposed payload"));
        }
    }
    {
        let js_source = "class Service { work() {} }\nasync function make() { return new Service(); }\n\
             async function caller() { const payload = await make(); payload.work(); \
             const wrapped = make(); wrapped.work(); }\n";
        let js_facts = ast::extract("factory.js", "javascript", js_source).unwrap();
        assert!(js_facts.errors.is_empty(), "js factory: {:?}", js_facts.errors);
        let js_ws = Workspace::new();
        js_ws.write("factory.js", js_source);
        js_ws.build();
        let js_conn = js_ws.open();
        let js_edges = read_edges(&js_conn);
        if !js_edges.iter().any(|edge| {
            edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == "payload.work"
        }) {
            failures.push("javascript/async function make(): awaited payload lacks work edge".into());
        }
        if js_edges.iter().any(|edge| edge.kind == "calls" && edge.evidence == "wrapped.work")
            || !has_coverage(&js_conn, "wrapped.work", "unresolved")
        {
            failures.push("javascript/async function make(): un-awaited factory exposed payload".into());
        }
    }

    // Custom type names and methods do not receive standard adapter behavior.
    {
        let custom_neg_rust = [
            "mod neg_0 { struct Service; impl Service { fn work(&self) {} } struct Envelope<T>(T); impl<T> Envelope<T> { fn unwrap(&self) -> Unknown { unknown() } } fn caller(value: Envelope<Service>) { let payload_0 = value.unwrap(); payload_0.work(); } }",
            "mod neg_1 { struct Service; impl Service { fn work(&self) {} } struct Envelope<T>(T); impl<T> Envelope<T> { fn as_ref(&self) -> Unknown { unknown() } } fn caller(value: Envelope<Service>) { let payload_1 = value.as_ref().unwrap(); payload_1.work(); } }",
            "mod neg_2 { struct Service; impl Service { fn work(&self) {} } fn caller(value: Option<Service>) { let payload_2 = value.ok().unwrap(); payload_2.work(); } }",
            "mod neg_3 { struct Service; impl Service { fn work(&self) {} } struct Vec<T>(T); fn caller(value: Vec<Service>) { let payload_3 = value.iter().cloned().next().unwrap(); payload_3.work(); } }",
            "mod neg_5 { struct Service; impl Service { fn work(&self) {} } async fn make() -> Service { Service {} } fn caller() { let future_5 = make(); future_5.work(); } }",
        ];
        let mut neg_rust_src = String::new();
        for snippet in custom_neg_rust {
            neg_rust_src.push_str(snippet);
            neg_rust_src.push('\n');
        }
        let neg_ts_src = "class Service { work() {} } async function make() { return unknown(); } async function caller() { const payload_4 = await make(); payload_4.work(); }";
        assert!(ast::extract("lib.rs", "rust", &neg_rust_src).unwrap().errors.is_empty());
        assert!(ast::extract("factory.ts", "typescript", neg_ts_src).unwrap().errors.is_empty());
        let neg_ws = Workspace::new();
        neg_ws.write("lib.rs", &neg_rust_src);
        neg_ws.write("factory.ts", neg_ts_src);
        neg_ws.build();
        let neg_conn = neg_ws.open();
        let neg_edges = read_edges(&neg_conn);
        for (language, expression) in [
            ("rust", "payload_0.work"),
            ("rust", "payload_1.work"),
            ("rust", "payload_2.work"),
            ("rust", "payload_3.work"),
            ("typescript", "payload_4.work"),
            ("rust", "future_5.work"),
        ] {
            if neg_edges.iter().any(|edge| edge.kind == "calls" && edge.evidence == expression)
                || !has_coverage(&neg_conn, expression, "unresolved")
            {
                failures.push(format!("{language}: unknown/custom/future receiver gained exact edge"));
            }
        }
    }

    let adapter_cases = [
        (
            "Option as_deref",
            "Option<Box<Service>>",
            ".as_deref().unwrap()",
            true,
        ),
        (
            "Result as_deref",
            "Result<Box<Service>, Error>",
            ".as_deref().unwrap()",
            true,
        ),
        (
            "Option iter",
            "Option<Service>",
            ".iter().next().unwrap()",
            true,
        ),
        (
            "Result iter",
            "Result<Service, Error>",
            ".iter().next().unwrap()",
            true,
        ),
        (
            "Option into_iter",
            "Option<Service>",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "Option cloned reference",
            "Option<&Service>",
            ".cloned().unwrap()",
            true,
        ),
        (
            "Option copied reference",
            "Option<&Service>",
            ".copied().unwrap()",
            true,
        ),
        (
            "Result into_iter",
            "Result<Service, Error>",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "Vec iter cloned element",
            "Vec<Service>",
            ".iter().cloned().next().unwrap()",
            true,
        ),
        (
            "Vec iter copied element",
            "Vec<Service>",
            ".iter().copied().next().unwrap()",
            true,
        ),
        (
            "Vec into_iter owned element",
            "Vec<Service>",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "borrowed Vec into_iter reference",
            "&Vec<Service>",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "mutably borrowed Vec into_iter reference",
            "&mut Vec<Service>",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "slice iter cloned element",
            "&[Service]",
            ".iter().cloned().next().unwrap()",
            true,
        ),
        (
            "slice iter copied element",
            "&[Service]",
            ".iter().copied().next().unwrap()",
            true,
        ),
        (
            "slice into_iter reference",
            "&[Service]",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "mutable slice into_iter reference",
            "&mut [Service]",
            ".into_iter().next().unwrap()",
            true,
        ),
        (
            "Option Vec as_deref slice",
            "Option<Vec<Service>>",
            ".as_deref().unwrap().iter().cloned().next().unwrap()",
            true,
        ),
        (
            "Option cloned shared reference",
            "Option<&&Service>",
            ".cloned().unwrap()",
            true,
        ),
        (
            "Option copied shared reference",
            "Option<&&Service>",
            ".copied().unwrap()",
            true,
        ),
        (
            "Result cloned shared reference",
            "Result<&&Service, Error>",
            ".cloned().unwrap()",
            true,
        ),
        (
            "Result copied shared reference",
            "Result<&&Service, Error>",
            ".copied().unwrap()",
            true,
        ),
        (
            "Iterator cloned shared reference",
            "Option<&&Service>",
            ".iter().cloned().next().unwrap()",
            true,
        ),
        (
            "Iterator copied shared reference",
            "Option<&&Service>",
            ".iter().copied().next().unwrap()",
            true,
        ),
        (
            "iterator item still needs unwrap",
            "Option<Service>",
            ".iter().next()",
            false,
        ),
    ];
    let mut source =
        "#[derive(Clone, Copy)] struct Service; struct Error; impl Service { fn work(&self) {} }\n"
            .to_owned();
    for (index, (_, wrapper, adapter, _)) in adapter_cases.iter().enumerate() {
        source.push_str(&format!(
            "fn adapter_{index}(value: {wrapper}) {{ let payload_{index} = value{adapter}; payload_{index}.work(); }}\n"
        ));
    }
    assert!(
        ast::extract("lib.rs", "rust", &source)
            .unwrap()
            .errors
            .is_empty(),
        "{source}"
    );
    let workspace = Workspace::new();
    workspace.write("lib.rs", &source);
    workspace.build();
    let conn = workspace.open();
    let edges = read_edges(&conn);
    for (index, (label, _, _, resolves)) in adapter_cases.iter().enumerate() {
        let evidence = format!("payload_{index}.work");
        let resolved = edges.iter().any(|edge| {
            edge.kind == "calls"
                && edge.dst_name == "work"
                && edge.evidence == evidence
                && edge.confidence == "inferred"
        });
        if resolved != *resolves {
            failures.push(format!(
                "{label}: expected resolved={resolves}, stored edges: {edges:#?}"
            ));
        }
        if !*resolves && !has_coverage(&conn, &evidence, "unresolved") {
            failures.push(format!(
                "{label}: unresolved iterator wrapper lost coverage"
            ));
        }
    }

    let wrapper_source = "struct Service; struct Error;\n\
        fn option_member(maybe: Option<Service>) { maybe.is_some(); }\n\
        fn result_member(outcome: Result<Service, Error>) { outcome.is_ok(); }\n\
        mod shadowed { struct Service; struct Option<T>(T);\n\
        fn custom_option(shadow: Option<Service>) { shadow.is_some(); } }";
    assert!(
        ast::extract("lib.rs", "rust", wrapper_source)
            .unwrap()
            .errors
            .is_empty(),
        "{wrapper_source}"
    );
    let wrapper_workspace = Workspace::new();
    wrapper_workspace.write("lib.rs", wrapper_source);
    wrapper_workspace.build();
    let wrapper_conn = wrapper_workspace.open();
    for (label, expression, status) in [
        ("Option standard member", "maybe.is_some", "external"),
        ("Result standard member", "outcome.is_ok", "external"),
        ("shadowed Option", "shadow.is_some", "unresolved"),
    ] {
        if !has_coverage(&wrapper_conn, expression, status) {
            failures.push(format!("{label}: missing {status} coverage"));
        }
        if status == "external" && has_coverage(&wrapper_conn, expression, "unresolved") {
            failures.push(format!(
                "{label}: standard wrapper method remains unresolved"
            ));
        }
    }

    {
        let custom_methods = ["unwrap", "as_ref", "as_mut", "ok"];
        let mut custom_source = "struct Service; impl Service { fn work(&self) {} }\n".to_owned();
        for (idx, method) in custom_methods.iter().enumerate() {
            custom_source.push_str(&format!(
                "struct Envelope_{idx}; impl Envelope_{idx} {{ fn {method}(&self) -> Service {{ unknown() }} }}\n\
                 fn caller_custom_{idx}(value: Envelope_{idx}) {{ let payload_custom_{idx} = value.{method}(); payload_custom_{idx}.work(); }}\n"
            ));
        }
        let custom_ws = Workspace::new();
        custom_ws.write("lib.rs", &custom_source);
        custom_ws.build();
        let custom_conn = custom_ws.open();
        let custom_edges = read_edges(&custom_conn);
        for (idx, _) in custom_methods.iter().enumerate() {
            let evidence = format!("payload_custom_{idx}.work");
            if !custom_edges.iter().any(|edge| {
                edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == evidence
            }) {
                failures.push("custom method lost its explicit nominal return contract".into());
            }
        }
    }

    {
        let dyn_counts = [2usize, 8, 9];
        let mut dyn_source = String::new();
        for (idx, &implementors) in dyn_counts.iter().enumerate() {
            dyn_source.push_str(&format!("trait Work{idx} {{ fn run_{idx}(&self); }}\n"));
            for impl_idx in 0..implementors {
                dyn_source.push_str(&format!(
                    "struct Worker{idx}_{impl_idx}; impl Work{idx} for Worker{idx}_{impl_idx} {{ fn run_{idx}(&self) {{}} }}\n"
                ));
            }
            dyn_source.push_str(&format!(
                "fn dyn_caller_{idx}(value: &dyn Work{idx}) {{ value.run_{idx}(); }}\n"
            ));
        }
        assert!(ast::extract("lib.rs", "rust", &dyn_source)
            .unwrap()
            .errors
            .is_empty());
        let dyn_ws = Workspace::new();
        dyn_ws.write("lib.rs", &dyn_source);
        dyn_ws.build();
        let dyn_conn = dyn_ws.open();
        let dyn_edges = read_edges(&dyn_conn);
        for (idx, &implementors) in dyn_counts.iter().enumerate() {
            let caller_name = format!("dyn_caller_{idx}");
            let (caller_id, caller_line, caller_end_line): (String, i64, i64) = dyn_conn
                .query_row(
                    "SELECT id, line, end_line FROM nodes WHERE name=?1",
                    [&caller_name],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            let run_name = format!("run_{idx}");
            let trait_line = 1 + dyn_counts[..idx].iter().map(|count| count + 2).sum::<usize>();
            let trait_method_id: String = dyn_conn
                .query_row(
                    "SELECT id FROM nodes WHERE name=?1 AND kind='method' AND line=?2",
                    rusqlite::params![run_name, trait_line],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(dyn_edges
                .iter()
                .filter(|edge| edge.kind == "implements" && edge.dst_name == run_name)
                .all(|edge| edge.dst == trait_method_id));
            let implementations: Vec<_> = dyn_edges
                .iter()
                .filter(|edge| edge.kind == "implements" && edge.dst == trait_method_id)
                .map(|edge| edge.src.as_str())
                .collect();
            assert_eq!(
                implementations.len(),
                implementors,
                "fixture must index every implementor: {dyn_edges:#?}"
            );
            let outgoing: Vec<_> = dyn_edges
                .iter()
                .filter(|edge| edge.src == caller_id)
                .collect();
            if !outgoing.iter().any(|edge| {
                edge.kind == "calls"
                    && edge.dst == trait_method_id
                    && edge.confidence == "inferred"
            }) {
                failures.push(format!(
                    "dyn Work/{implementors}: missing inferred trait declaration call"
                ));
            }
            if outgoing.iter().any(|edge| {
                edge.kind == "calls"
                    && edge.confidence == "inferred"
                    && edge.dst != trait_method_id
            }) {
                failures.push(format!(
                    "dyn Work/{implementors}: inferred call targets another declaration"
                ));
            }
            if outgoing
                .iter()
                .any(|edge| edge.kind == "calls" && implementations.contains(&edge.dst.as_str()))
            {
                failures.push(format!(
                    "dyn Work/{implementors}: candidate became exact implementation call"
                ));
            }
            let possible: Vec<_> = outgoing
                .iter()
                .filter(|edge| edge.kind == "possible_calls")
                .collect();
            let expected = if implementors <= 8 { implementors } else { 0 };
            if possible.len() != expected
                || possible.iter().any(|edge| {
                    edge.confidence != "possible" || !implementations.contains(&edge.dst.as_str())
                })
            {
                failures.push(format!(
                    "dyn Work/{implementors}: expected {expected} bounded candidates, got {possible:#?}"
                ));
            }
            let has_dispatch_gap: bool = dyn_conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
                     JOIN coverage_expressions x ON x.expression_id=r.expression_id \
                     WHERE x.expression='trait_object_dispatch' AND r.status='unresolved' \
                     AND r.line BETWEEN ?1 AND ?2)",
                    [caller_line, caller_end_line],
                    |row| row.get(0),
                )
                .unwrap();
            if !has_dispatch_gap {
                failures.push(format!(
                    "dyn Work/{implementors}: dispatch analysis gap disappeared"
                ));
            }
        }
    }

    let macro_source = "fn macro_target() {}\nstruct MacroType;\nmacro_rules! invoke { ($value:expr) => {{ macro_target(); let typed = MacroType {}; $value; }}; }\nfn caller() { invoke!(unknown()); }\n";
    assert!(ast::extract("lib.rs", "rust", macro_source)
        .unwrap()
        .errors
        .is_empty());
    let workspace = Workspace::new();
    workspace.write("lib.rs", macro_source);
    workspace.build();
    let conn = workspace.open();
    let macro_id: String = conn
        .query_row(
            "SELECT id FROM nodes WHERE name='invoke' AND kind='macro'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let edges = read_edges(&conn);
    for target in ["macro_target", "MacroType"] {
        if !edges.iter().any(|edge| {
            edge.src == macro_id && edge.kind == "references" && edge.dst_name == target
        }) {
            failures.push(format!(
                "macro transcription lacks macro-owned reference to {target}"
            ));
        }
        if edges
            .iter()
            .any(|edge| edge.kind == "calls" && edge.dst_name == target)
        {
            failures.push(format!(
                "macro transcription executed {target} as a calls edge"
            ));
        }
    }
    if !edges
        .iter()
        .any(|edge| edge.kind == "calls" && edge.dst == macro_id)
        || !has_coverage(&conn, "macro_expansion_not_analyzed", "unresolved")
    {
        failures
            .push("macro invocation lost declaration navigation or opaque expansion gap".into());
    }
    assert!(
        failures.is_empty(),
        "contract readback failures:\n{}",
        failures.join("\n")
    );
}

#[test]
#[cfg(feature = "lang-typescript")]
fn awaited_promise_parameters_and_aliases_survive_index_readback() {
    let shared_cases = [
        ("parameter", "Promise<Service>", "", "p", true),
        ("parameter alias", "Promise<Service>", "const alias = p;", "alias", true),
        ("alias chain", "Promise<Service>", "const first = p; const alias = first;", "alias", true),
        ("typed local", "Promise<Service>", "const alias: Promise<Service> = unknown();", "alias", true),
        ("factory alias", "Promise<Service>", "const alias = make();", "alias", true),
        ("unknown payload", "Promise<Unknown>", "", "p", false),
        ("unsupported generic", "Envelope<Service>", "", "p", false),
        ("unsupported arity", "Promise<Service, Unknown>", "", "p", false),
        ("reassigned alias", "Promise<Service>", "let alias = p; alias = unknown();", "alias", false),
        ("conditional alias write", "Promise<Service>", "let alias = p; if (flag) { alias = unknown(); }", "alias", false),
    ];

    let mut source = "class Service { work() {} }\nfunction make(): Promise<Service> { return unknown(); }\n".to_owned();
    let mut rendered_cases = Vec::with_capacity(shared_cases.len());
    for (idx, &(label, parameter_type, setup, receiver, resolves)) in shared_cases.iter().enumerate() {
        let p_var = format!("p_{idx}");
        let flag_var = format!("flag_{idx}");
        let alias_var = format!("alias_{idx}");
        let first_var = format!("first_{idx}");
        let payload_var = format!("payload_{idx}");
        let setup_rewritten = setup
            .replace("alias", &alias_var)
            .replace("first", &first_var)
            .replace("= p;", &format!("= {p_var};"))
            .replace("(flag)", &format!("({flag_var})"));
        let receiver_var = if receiver == "alias" { alias_var } else { p_var.clone() };
        source.push_str(&format!(
            "async function caller_{idx}({p_var}: {parameter_type}, {flag_var}: boolean) {{ \
             {setup_rewritten} const {payload_var} = await {receiver_var}; {payload_var}.work(); \
             {p_var}.work(); {receiver_var}.work(); }}\n"
        ));
        rendered_cases.push((label, format!("caller_{idx}"), payload_var, p_var, receiver_var, resolves));
    }
    assert!(ast::extract("awaited.ts", "typescript", &source).unwrap().errors.is_empty());
    let shared_ws = Workspace::new();
    shared_ws.write("awaited.ts", &source);
    shared_ws.build();
    let shared_conn = shared_ws.open();

    for (label, caller_name, payload_var, p_var, receiver_var, resolves) in rendered_cases {
        let payload_ev = format!("{payload_var}.work");
        let resolved: bool = shared_conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM edges e \
                 JOIN nodes d ON d.node_hash=e.dst_hash \
                 JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                 JOIN coverage_evidence c ON c.evidence_id=e.confidence_id \
                 WHERE e.kind='calls' AND d.name='work' AND v.evidence=?1 \
                 AND c.evidence='inferred')",
                [&payload_ev],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            resolved,
            resolves,
            "{label}: awaited payload resolution; stored caller facts: {:#?}",
            stored_node(&shared_conn, "awaited.ts", &caller_name).1
        );
        let mut unresolved = vec![format!("{p_var}.work"), format!("{receiver_var}.work")];
        if !resolves {
            unresolved.push(payload_ev);
        }
        for expression in unresolved {
            let is_unresolved: bool = shared_conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
                     JOIN coverage_expressions x ON x.expression_id=r.expression_id \
                     WHERE x.expression=?1 AND r.status='unresolved')",
                    [&expression],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(is_unresolved, "{label}: {expression} must remain unresolved");
            let has_call: bool = shared_conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM edges e \
                     JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                     WHERE e.kind='calls' AND v.evidence=?1)",
                    [&expression],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!has_call, "{label}: {expression} gained a call edge");
        }
    }

    // Shadowed Promise class cannot be combined into the shared file without shadowing stdlib Promise for sibling tests.
    let shadowed_ws = Workspace::new();
    let shadowed_source = "class Service { work() {} }\n\
         function make(): Promise<Service> { return unknown(); }\n\
         async function caller(p: Promise<Service>, flag: boolean) { \
         class Promise<T> {} const payload = await p; payload.work(); \
         p.work(); p.work(); }\n";
    assert!(ast::extract("awaited.ts", "typescript", shadowed_source)
        .unwrap()
        .errors
        .is_empty());
    shadowed_ws.write("awaited.ts", shadowed_source);
    shadowed_ws.build();
    let shadowed_conn = shadowed_ws.open();
    let shadowed_resolved: bool = shadowed_conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM edges e \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
             JOIN coverage_evidence c ON c.evidence_id=e.confidence_id \
             WHERE e.kind='calls' AND d.name='work' AND v.evidence='payload.work' \
             AND c.evidence='inferred')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !shadowed_resolved,
        "shadowed Promise: awaited payload resolution; stored caller facts: {:#?}",
        stored_node(&shadowed_conn, "awaited.ts", "caller").1
    );
    for expression in ["p.work", "payload.work"] {
        let is_unresolved: bool = shadowed_conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
                 JOIN coverage_expressions x ON x.expression_id=r.expression_id \
                 WHERE x.expression=?1 AND r.status='unresolved')",
                [expression],
                |row| row.get(0),
            )
            .unwrap();
        assert!(is_unresolved, "shadowed Promise: {expression} must remain unresolved");
        let has_call: bool = shadowed_conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM edges e \
                 JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                 WHERE e.kind='calls' AND v.evidence=?1)",
                [expression],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!has_call, "shadowed Promise: {expression} gained a call edge");
    }
}

#[test]
#[cfg(feature = "lang-python")]
fn python_optional_and_single_none_union_project_one_payload_on_index_readback() {
    let source = NULLABLE_PYTHON;
    let facts = ast::extract("nullable.py", "python", source).unwrap();
    assert!(facts.errors.is_empty(), "{:?}", facts.errors);
    let workspace = method_workspace();
    let conn = workspace.open();
    let has_call = |caller: &str, expression: &str, line: usize| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM edge_occurrences e \
             JOIN nodes s ON s.node_hash=e.src_hash \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             JOIN path_dictionary p ON p.path_id=e.owner_id \
             JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
             WHERE p.path='nullable.py' AND s.name=?1 AND d.name='work' \
             AND e.kind='calls' AND v.evidence=?2 AND e.line=?3)",
            rusqlite::params![caller, expression, line],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    let unresolved = |expression: &str, line: usize| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
             JOIN coverage_expressions x ON x.expression_id=r.expression_id \
             JOIN path_dictionary p ON p.path_id=r.path_id \
             WHERE p.path='nullable.py' AND x.expression=?1 AND r.line=?2 \
             AND r.status='unresolved')",
            rusqlite::params![expression, line],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };

    for (caller, expression, line) in [
        ("optional_import", "optional.work", 13),
        ("union_import", "unioned.work", 16),
        ("union_none_first", "none_first.work", 19),
        ("module_optional", "module_optional.work", 22),
        ("extensions_union", "extensions_union.work", 25),
        ("typed_local", "local_optional.work", 29),
        ("typed_local", "local_union.work", 31),
        ("pep604", "pep604.work", 43),
        ("pep604_reversed", "pep604_reversed.work", 46),
        ("pep604_str", "pep604_str.work", 49),
    ] {
        assert!(
            has_call(caller, expression, line),
            "{caller}: expected a persisted payload call for {expression}; node: {:#?}",
            stored_node(&conn, "nullable.py", caller).1
        );
    }

    for (caller, expression, line) in [
        ("multi_nonnull", "multi_nonnull.work", 34),
        (
            "multi_nonnull_with_none",
            "multi_nonnull_with_none.work",
            37,
        ),
        ("bad_optional_arity", "bad_optional_arity.work", 40),
        ("pep604_multi", "pep604_multi.work", 52),
    ] {
        assert!(
            !has_call(caller, expression, line),
            "{caller}: unsupported nullable type exposed {expression}"
        );
        assert!(
            unresolved(expression, line),
            "{caller}: unsupported nullable type lost unresolved coverage for {expression}"
        );
    }
}

#[test]
#[cfg(feature = "lang-python")]
fn python_pep604_without_union_imports_and_nested_invariants_survive_readback() {
    let source = CLEAN_PEP604_PYTHON;
    let facts = ast::extract("clean_pep604.py", "python", source).unwrap();
    assert!(facts.errors.is_empty(), "{:?}", facts.errors);
    let workspace = method_workspace();
    let conn = workspace.open();
    let has_call = |caller: &str, expression: &str, line: usize| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM edge_occurrences e \
             JOIN nodes s ON s.node_hash=e.src_hash \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             JOIN path_dictionary p ON p.path_id=e.owner_id \
             JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
             WHERE p.path='clean_pep604.py' AND s.name=?1 AND d.name='work' \
             AND e.kind='calls' AND v.evidence=?2 AND e.line=?3)",
            rusqlite::params![caller, expression, line],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    let unresolved = |expression: &str, line: usize| {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
             JOIN coverage_expressions x ON x.expression_id=r.expression_id \
             JOIN path_dictionary p ON p.path_id=r.path_id \
             WHERE p.path='clean_pep604.py' AND x.expression=?1 AND r.line=?2 \
             AND r.status='unresolved')",
            rusqlite::params![expression, line],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };

    // Positive cases: should resolve call to Service.work
    for (caller, expression, line) in [
        ("clean_pep604", "clean.work", 12),
        ("clean_reversed", "rev.work", 15),
        ("clean_parenthesized", "paren.work", 18),
        ("clean_local", "local.work", 22),
        ("clean_local_quoted_paren", "local_qp.work", 35),
    ] {
        assert!(
            has_call(caller, expression, line),
            "{caller}: expected a persisted payload call for {expression}; node: {:#?}",
            stored_node(&conn, "clean_pep604.py", caller).1
        );
    }

    // Negative / fail-closed cases: must not resolve and must remain unresolved
    for (caller, expression, line) in [
        ("clean_nonetype", "nt.work", 25),
        ("clean_nested_list", "lst.work", 28),
        ("clean_bad_pipe", "bad.work", 31),
        ("custom_union", "cu.work", 40),
    ] {
        assert!(
            !has_call(caller, expression, line),
            "{caller}: unsupported type exposed call for {expression}"
        );
        assert!(
            unresolved(expression, line),
            "{caller}: unsupported type lost unresolved coverage for {expression}"
        );
    }
}
