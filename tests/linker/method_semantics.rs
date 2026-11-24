#![cfg(all(feature = "lang-python", feature = "lang-rust"))]

use crate::common::Workspace;
use acdd_core::response::{QueryOptions, ResponsePolicy};
use acdd_index::{ast, db::symbols};
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

fn cached_snapshot(
    cell: &'static std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot>,
    files: &[(&str, &str)],
) -> &'static crate::common::sqlite::PersistedSnapshot {
    cell.get_or_init(|| {
        let workspace = Workspace::new();
        for (path, content) in files {
            workspace.write(path, content);
        }
        workspace.build();
        crate::common::sqlite::PersistedSnapshot::from_connection(&workspace.open())
    })
}

fn method_workspace() -> &'static crate::common::sqlite::PersistedSnapshot {
    static SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    cached_snapshot(
        &SNAPSHOT,
        &[
            ("record.rs", RECORD_RUST),
            ("record.py", RECORD_PYTHON),
            ("record.ts", RECORD_TYPESCRIPT),
            ("nullable.py", NULLABLE_PYTHON),
            ("clean_pep604.py", CLEAN_PEP604_PYTHON),
        ],
    )
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
    let mut wrapper_source =
        "struct Service; struct Error;\nimpl Service { fn work(&self) {} }\n".to_owned();
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
    static WRAPPER_MATRIX_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let wrapper_matrix_conn =
        cached_snapshot(&WRAPPER_MATRIX_SNAPSHOT, &[("lib.rs", &wrapper_source)]).open();
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
        (
            "typescript",
            "async function make_0() { return new Service(); }",
        ),
        (
            "typescript",
            "async function make_1(): Promise<Service> { return new Service(); }",
        ),
        (
            "typescript",
            "function make_2(): Promise<Service> { return unknown(); }",
        ),
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
    assert!(
        ts_facts.errors.is_empty(),
        "ts factories: {:?}",
        ts_facts.errors
    );
    static TS_FACTORY_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let ts_conn = cached_snapshot(&TS_FACTORY_SNAPSHOT, &[("factory.ts", &ts_source)]).open();
    let ts_edges = read_edges(&ts_conn);
    for (idx, (language, factory)) in ts_factories.iter().enumerate() {
        let payload_ev = format!("payload_{idx}.work");
        let wrapped_ev = format!("wrapped_{idx}.work");
        if !ts_edges.iter().any(|edge| {
            edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == payload_ev
        }) {
            failures.push(format!(
                "{language}/{factory}: awaited payload lacks work edge"
            ));
        }
        if ts_edges
            .iter()
            .any(|edge| edge.kind == "calls" && edge.evidence == wrapped_ev)
            || !has_coverage(&ts_conn, &wrapped_ev, "unresolved")
        {
            failures.push(format!(
                "{language}/{factory}: un-awaited factory exposed payload"
            ));
        }
    }
    {
        let js_source =
            "class Service { work() {} }\nasync function make() { return new Service(); }\n\
             async function caller() { const payload = await make(); payload.work(); \
             const wrapped = make(); wrapped.work(); }\n";
        let js_facts = ast::extract("factory.js", "javascript", js_source).unwrap();
        assert!(
            js_facts.errors.is_empty(),
            "js factory: {:?}",
            js_facts.errors
        );
        static JS_FACTORY_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
            std::sync::OnceLock::new();
        let js_conn = cached_snapshot(&JS_FACTORY_SNAPSHOT, &[("factory.js", js_source)]).open();
        let js_edges = read_edges(&js_conn);
        if !js_edges.iter().any(|edge| {
            edge.kind == "calls" && edge.dst_name == "work" && edge.evidence == "payload.work"
        }) {
            failures
                .push("javascript/async function make(): awaited payload lacks work edge".into());
        }
        if js_edges
            .iter()
            .any(|edge| edge.kind == "calls" && edge.evidence == "wrapped.work")
            || !has_coverage(&js_conn, "wrapped.work", "unresolved")
        {
            failures.push(
                "javascript/async function make(): un-awaited factory exposed payload".into(),
            );
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
        assert!(ast::extract("lib.rs", "rust", &neg_rust_src)
            .unwrap()
            .errors
            .is_empty());
        assert!(ast::extract("factory.ts", "typescript", neg_ts_src)
            .unwrap()
            .errors
            .is_empty());
        static CUSTOM_NEG_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
            std::sync::OnceLock::new();
        let neg_conn = cached_snapshot(
            &CUSTOM_NEG_SNAPSHOT,
            &[("lib.rs", &neg_rust_src), ("factory.ts", neg_ts_src)],
        )
        .open();
        let neg_edges = read_edges(&neg_conn);
        for (language, expression) in [
            ("rust", "payload_0.work"),
            ("rust", "payload_1.work"),
            ("rust", "payload_2.work"),
            ("rust", "payload_3.work"),
            ("typescript", "payload_4.work"),
            ("rust", "future_5.work"),
        ] {
            if neg_edges
                .iter()
                .any(|edge| edge.kind == "calls" && edge.evidence == expression)
                || !has_coverage(&neg_conn, expression, "unresolved")
            {
                failures.push(format!(
                    "{language}: unknown/custom/future receiver gained exact edge"
                ));
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
    static ADAPTER_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let conn = cached_snapshot(&ADAPTER_SNAPSHOT, &[("lib.rs", &source)]).open();
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
    static WRAPPER_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let wrapper_conn = cached_snapshot(&WRAPPER_SNAPSHOT, &[("lib.rs", wrapper_source)]).open();
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
        static CUSTOM_METHODS_SNAPSHOT: std::sync::OnceLock<
            crate::common::sqlite::PersistedSnapshot,
        > = std::sync::OnceLock::new();
        let custom_conn =
            cached_snapshot(&CUSTOM_METHODS_SNAPSHOT, &[("lib.rs", &custom_source)]).open();
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
        static DYN_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
            std::sync::OnceLock::new();
        let dyn_conn = cached_snapshot(&DYN_SNAPSHOT, &[("lib.rs", &dyn_source)]).open();
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
            let trait_line = 1 + dyn_counts[..idx]
                .iter()
                .map(|count| count + 2)
                .sum::<usize>();
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
                edge.kind == "calls" && edge.dst == trait_method_id && edge.confidence == "inferred"
            }) {
                failures.push(format!(
                    "dyn Work/{implementors}: missing inferred trait declaration call"
                ));
            }
            if outgoing.iter().any(|edge| {
                edge.kind == "calls" && edge.confidence == "inferred" && edge.dst != trait_method_id
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
            let expected_status = if implementors <= 8 {
                "resolved"
            } else {
                "unresolved"
            };
            let has_dispatch_gap: bool = dyn_conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
                     JOIN coverage_expressions x ON x.expression_id=r.expression_id \
                     JOIN coverage_evidence v ON v.evidence_id=r.evidence_id \
                     WHERE x.expression='trait_object_dispatch' AND r.status=?3 \
                     AND v.evidence LIKE 'analysis gap:%' \
                     AND r.line BETWEEN ?1 AND ?2)",
                    rusqlite::params![caller_line, caller_end_line, expected_status],
                    |row| row.get(0),
                )
                .unwrap();
            if !has_dispatch_gap {
                failures.push(format!(
                    "dyn Work/{implementors}: dispatch analysis gap with status={expected_status} disappeared"
                ));
            }
        }
    }

    let macro_source = "fn macro_target() {}\nstruct MacroType;\nmacro_rules! invoke { ($value:expr) => {{ macro_target(); let typed = MacroType {}; $value; }}; }\nfn caller() { invoke!(unknown()); }\n";
    assert!(ast::extract("lib.rs", "rust", macro_source)
        .unwrap()
        .errors
        .is_empty());
    static MACRO_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let conn = cached_snapshot(&MACRO_SNAPSHOT, &[("lib.rs", macro_source)]).open();
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
        (
            "parameter alias",
            "Promise<Service>",
            "const alias = p;",
            "alias",
            true,
        ),
        (
            "alias chain",
            "Promise<Service>",
            "const first = p; const alias = first;",
            "alias",
            true,
        ),
        (
            "typed local",
            "Promise<Service>",
            "const alias: Promise<Service> = unknown();",
            "alias",
            true,
        ),
        (
            "factory alias",
            "Promise<Service>",
            "const alias = make();",
            "alias",
            true,
        ),
        ("unknown payload", "Promise<Unknown>", "", "p", false),
        ("unsupported generic", "Envelope<Service>", "", "p", false),
        (
            "unsupported arity",
            "Promise<Service, Unknown>",
            "",
            "p",
            false,
        ),
        (
            "reassigned alias",
            "Promise<Service>",
            "let alias = p; alias = unknown();",
            "alias",
            false,
        ),
        (
            "conditional alias write",
            "Promise<Service>",
            "let alias = p; if (flag) { alias = unknown(); }",
            "alias",
            false,
        ),
    ];

    let mut source =
        "class Service { work() {} }\nfunction make(): Promise<Service> { return unknown(); }\n"
            .to_owned();
    let mut rendered_cases = Vec::with_capacity(shared_cases.len());
    for (idx, &(label, parameter_type, setup, receiver, resolves)) in
        shared_cases.iter().enumerate()
    {
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
        let receiver_var = if receiver == "alias" {
            alias_var
        } else {
            p_var.clone()
        };
        source.push_str(&format!(
            "async function caller_{idx}({p_var}: {parameter_type}, {flag_var}: boolean) {{ \
             {setup_rewritten} const {payload_var} = await {receiver_var}; {payload_var}.work(); \
             {p_var}.work(); {receiver_var}.work(); }}\n"
        ));
        rendered_cases.push((
            label,
            format!("caller_{idx}"),
            payload_var,
            p_var,
            receiver_var,
            resolves,
        ));
    }
    assert!(ast::extract("awaited.ts", "typescript", &source)
        .unwrap()
        .errors
        .is_empty());
    static AWAITED_SHARED_SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    let shared_conn =
        cached_snapshot(&AWAITED_SHARED_SNAPSHOT, &[("awaited.ts", &source)]).open();

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
            assert!(
                is_unresolved,
                "{label}: {expression} must remain unresolved"
            );
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
    let shadowed_source = "class Service { work() {} }\n\
         function make(): Promise<Service> { return unknown(); }\n\
         async function caller(p: Promise<Service>, flag: boolean) { \
         class Promise<T> {} const payload = await p; payload.work(); \
         p.work(); p.work(); }\n";
    assert!(ast::extract("awaited.ts", "typescript", shadowed_source)
        .unwrap()
        .errors
        .is_empty());
    static SHADOWED_PROMISE_SNAPSHOT: std::sync::OnceLock<
        crate::common::sqlite::PersistedSnapshot,
    > = std::sync::OnceLock::new();
    let shadowed_conn =
        cached_snapshot(&SHADOWED_PROMISE_SNAPSHOT, &[("awaited.ts", shadowed_source)]).open();
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
        assert!(
            is_unresolved,
            "shadowed Promise: {expression} must remain unresolved"
        );
        let has_call: bool = shadowed_conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM edges e \
                 JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                 WHERE e.kind='calls' AND v.evidence=?1)",
                [expression],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !has_call,
            "shadowed Promise: {expression} gained a call edge"
        );
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
        if caller == "bad_optional_arity" {
            let possible_calls: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM edge_occurrences e \
                     JOIN nodes s ON s.node_hash=e.src_hash \
                     JOIN path_dictionary p ON p.path_id=e.owner_id \
                     JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                     WHERE p.path='nullable.py' AND s.name=?1 AND e.kind='possible_calls' \
                     AND v.evidence=?2 AND e.line=?3",
                    rusqlite::params![caller, expression, line],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                possible_calls, 0,
                "{caller}: invalid Optional arity unexpectedly emitted possible_calls for {expression}"
            );
        }
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

    // Complete 2-member PEP 604 union (Service | NoneType) projects bounded possible_calls
    assert!(
        !has_call("clean_nonetype", "nt.work", 25),
        "clean_nonetype: 2-member union must not fabricate an exact calls edge"
    );
    let nt_ambiguous: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM resolution_coverage r \
             JOIN coverage_expressions x ON x.expression_id=r.expression_id \
             JOIN path_dictionary p ON p.path_id=r.path_id \
             WHERE p.path='clean_pep604.py' AND x.expression='nt.work' AND r.line=25 \
             AND r.status='ambiguous')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        nt_ambiguous,
        "clean_nonetype: Service | NoneType must record ambiguous coverage"
    );
    let nt_possible: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge_occurrences e \
             JOIN nodes s ON s.node_hash=e.src_hash \
             JOIN nodes d ON d.node_hash=e.dst_hash \
             JOIN path_dictionary p ON p.path_id=e.owner_id \
             JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
             WHERE p.path='clean_pep604.py' AND s.name='clean_nonetype' AND d.name='work' \
             AND e.kind='possible_calls' AND v.evidence='nt.work' AND e.line=25",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        nt_possible, 2,
        "clean_nonetype: Service | NoneType must emit 2 possible_calls targets"
    );

    // Negative / fail-closed cases: must not resolve and must remain unresolved
    for (caller, expression, line) in [
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

#[test]
fn rust_linker_core_contract_matrix() {
    use std::collections::BTreeMap;

    // 1. External/stdlib trait impls & unproven_bounds preserve receivers_by_method
    //    without leaking external trait methods into inherent members.
    // 2. Generic/lifetime impl qualnames normalize `<...>` for Type::method / self.method / .method.
    // 3. receiver_type_name strips balanced `<...>` generic/lifetime arguments.
    // 4. Value bindings do not shadow Rust type/module `::` namespaces.
    // 5. &dyn Trait resolves parameter trait objects & emits single trait_object_dispatch row.
    // 6. Mutually exclusive #[cfg(...)] siblings disambiguate; non-exclusive stay ambiguous.
    let files: BTreeMap<String, _> = [
        (
            "src/interner.rs",
            r#"
pub struct LinkerInterner<'a> { pub label: &'a str }
impl<'a> LinkerInterner<'a> {
    pub fn from_catalog(label: &'a str) -> Self { Self { label } }
    pub fn intern(&mut self) -> &'a str { self.peek() }
    fn peek(&self) -> &'a str { self.label }
}
"#,
        ),
        (
            "src/lib.rs",
            r#"
mod interner;

pub struct Worker;
impl Worker {
    pub fn build() -> Self { Self }
    pub fn finish(&self) {}
    pub fn bounded<T: Clone>(&self, _value: T) {
        self.finish();
        let _ = Self::build();
    }
}
impl Default for Worker {
    fn default() -> Self { Self::build() }
}
impl std::fmt::Display for Worker {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.finish();
        Ok(())
    }
}

pub trait Engine {
    fn execute(&self);
}
pub struct FastEngine;
impl Engine for FastEngine {
    fn execute(&self) {}
}

pub struct Platform;
impl Platform {
    #[cfg(feature = "lang-rust")]
    /// Enabled variant.
    pub fn active(&self) {}

    #[cfg(not(feature = "lang-rust"))]
    /// Disabled variant.
    pub fn active(&self) {}
}

#[cfg(unix)]
/// Unix implementation.
fn platform_call() {}
#[cfg(not(unix))]
/// Non-unix implementation.
fn platform_call() {}

#[cfg(target_os = "linux")]
fn os_call() {}
#[cfg(target_os = "macos")]
fn os_call() {}

#[cfg(feature = "alpha")]
fn non_exclusive_cfg() {}
#[cfg(feature = "beta")]
fn non_exclusive_cfg() {}

fn exercise<'a>(
    interner: &mut interner::LinkerInterner<'a>,
    opt: Option<Worker>,
    items: Vec<Worker>,
    map: std::collections::BTreeMap<String, Vec<Worker>>,
    engine: &'a (dyn Engine + Sync),
    writer: &mut dyn std::io::Write,
    maybe_engine: Option<&'a dyn Engine>,
    platform: &Platform,
    worker: &Worker,
) {
    let _ = interner::LinkerInterner::from_catalog("x");
    let _ = interner.intern();
    let _ = opt.is_some();
    let _ = items.len();
    let _ = map.is_empty();
    engine.execute();
    let _ = writer.flush();
    let _ = maybe_engine.is_some();
    platform.active();
    platform_call();
    os_call();
    non_exclusive_cfg();
    worker.fmt();
}
"#,
        ),
    ]
    .into_iter()
    .map(|(path, src)| {
        let facts = ast::extract(path, "rust", src).unwrap();
        assert!(facts.errors.is_empty(), "{path}: {:?}", facts.errors);
        (path.to_owned(), facts)
    })
    .collect();

    // Generic/lifetime impl qualnames must be normalized without `<'a>`.
    let interner_nodes = &files["src/interner.rs"].nodes;
    assert!(
        interner_nodes
            .iter()
            .any(|n| n.kind == "method" && n.qualname.ends_with(".LinkerInterner.from_catalog")),
        "expected normalized qualname without <'a>: {interner_nodes:#?}"
    );
    assert!(
        !interner_nodes.iter().any(|n| n.qualname.contains("<'a>")),
        "unexpected generic qualname leak: {interner_nodes:#?}"
    );

    let graph = crate::common::link_facts(&files);

    // Table-driven coverage status contract rows
    let coverage_cases = [
        ("src/lib.rs", "Self::build", "resolved"),
        ("src/lib.rs", "self.finish", "resolved"),
        ("src/interner.rs", "self.peek", "resolved"),
        ("src/lib.rs", "interner::LinkerInterner::from_catalog", "resolved"),
        ("src/lib.rs", "interner.intern", "resolved"),
        ("src/lib.rs", "opt.is_some", "external"),
        ("src/lib.rs", "items.len", "external"),
        ("src/lib.rs", "map.is_empty", "external"),
        ("src/lib.rs", "maybe_engine.is_some", "external"),
        ("src/lib.rs", "platform.active", "resolved"),
        ("src/lib.rs", "platform_call", "resolved"),
        ("src/lib.rs", "os_call", "resolved"),
        ("src/lib.rs", "non_exclusive_cfg", "ambiguous"),
        // External trait `Display::fmt` must NOT leak into inherent lookup
        ("src/lib.rs", "worker.fmt", "unresolved"),
    ];
    for (path, expression, expected_status) in coverage_cases {
        assert!(
            graph
                .coverage
                .iter()
                .any(|row| row.path == path
                    && row.expression == expression
                    && row.status == expected_status),
            "{path} `{expression}` expected status `{expected_status}`, got: {:#?}",
            graph
                .coverage
                .iter()
                .filter(|row| row.path == path && row.expression == expression)
                .collect::<Vec<_>>()
        );
    }

    // No "Self has no indexed impl receiver" failures in external trait impls or unproven_bounds.
    assert!(
        !graph
            .coverage
            .iter()
            .any(|row| row.evidence.contains("Self has no indexed impl receiver")),
        "unexpected Self receiver failure: {:#?}",
        graph.coverage
    );

    // &dyn (Engine + Sync) emits inferred call to trait decl + possible_calls to FastEngine.
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.kind == "calls" && e.evidence == "engine.execute" && e.confidence == "inferred"),
        "missing inferred dyn trait call: {:#?}",
        graph.edges
    );
    assert!(
        graph.edges.iter().any(
            |e| e.kind == "possible_calls" && e.evidence == "engine.execute" && e.confidence == "possible"
        ),
        "missing possible_calls implementor edge: {:#?}",
        graph.edges
    );

    // External &mut dyn std::io::Write emits exactly ONE trait_object_dispatch row (no duplicate unresolved row).
    let writer_rows: Vec<_> = graph
        .coverage
        .iter()
        .filter(|row| {
            row.path == "src/lib.rs"
                && (row.expression == "writer.flush"
                    || (row.expression == "trait_object_dispatch"
                        && row.evidence.contains("writer")))
        })
        .collect();
    assert_eq!(
        writer_rows.len(),
        1,
        "external dyn Trait call must not double-emit unresolved coverage: {writer_rows:#?}"
    );
    assert_eq!(writer_rows[0].expression, "trait_object_dispatch");
}

#[test]
fn rust_zero_heuristic_resolution_extensions_matrix() {
    let workspace = Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nrusqlite = \"0.32\"\ntree-sitter = \"0.24\"\n",
    );
    workspace.write(
        "src/enums.rs",
        r#"
macro_rules! closed_enum {
    ($vis:vis enum $name:ident { $($variant:ident),+ $(,)? }) => {
        $vis enum $name { $($variant),+ }
        impl $name { pub fn tag(&self) -> u8 { 1 } }
    };
}
closed_enum! { pub enum PublicKind { Alpha, Beta } }
closed_enum! { enum HiddenKind { Gamma } }
"#,
    );
    workspace.write(
        "src/codec.rs",
        r#"
pub trait WireEncode { fn encode(&self, out: &mut Vec<u8>); }
impl WireEncode for &str { fn encode(&self, _out: &mut Vec<u8>) {} }
impl WireEncode for u64 { fn encode(&self, _out: &mut Vec<u8>) {} }
pub struct Store;
impl Store { pub fn ping(&self) {} }
pub fn with_store(f: impl FnOnce(&Store)) { let s = Store; f(&s); }
pub fn visit_tree<'a>(root: tree_sitter::Node<'a>, mut cb: impl FnMut(tree_sitter::Node<'a>, bool)) {
    cb(root, true);
}
"#,
    );
    workspace.write(
        "src/lib.rs",
        r#"
mod codec;
mod enums;
use std::time::Instant;
use crate::enums::PublicKind;
use crate::enums::HiddenKind;
use crate::codec::{with_store, visit_tree, Store, WireEncode};

fn check(
    a: &str,
    b: &str,
    n: u64,
    out: &mut Vec<u8>,
    root: tree_sitter::Node<'_>,
    conn: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    let _k = PublicKind::Alpha;
    let _h = HiddenKind::Gamma;
    "lit".encode(out);
    n.encode(out);
    let _in_range = (1..=16).contains(&n);
    let _ord = (a, n).cmp(&(b, n));
    let started = Instant::now();
    let _secs = started.elapsed().as_secs_f64();
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM t", [])?;
    tx.commit()?;
    with_store(|s| s.ping());
    visit_tree(root, |node, _| { let _ = node.kind(); });
    Ok(())
}
"#,
    );
    workspace.build();
    let conn = workspace.open();

    let row_status = |expr: &str| -> (String, String) {
        conn.query_row(
            "SELECT r.status, e.evidence FROM resolution_coverage r
             JOIN path_dictionary p ON p.path_id = r.path_id
             JOIN coverage_expressions x ON x.expression_id = r.expression_id
             JOIN coverage_evidence e ON e.evidence_id = r.evidence_id
             WHERE p.path = 'src/lib.rs' AND x.expression = ?1
             LIMIT 1",
            [expr],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or_else(|e| panic!("missing coverage row for {expr}: {e}"))
    };

    for (expr, expected_status) in [
        ("crate::enums::PublicKind", "resolved"),
        ("crate::enums::HiddenKind", "unresolved"),
        ("\"lit\".encode", "resolved"),
        ("n.encode", "resolved"),
        ("(1..=16).contains", "external"),
        ("(a, n).cmp", "external"),
        ("started.elapsed", "external"),
        ("started.elapsed().as_secs_f64", "external"),
        ("tx.execute", "external"),
        ("tx.commit", "external"),
        ("s.ping", "resolved"),
        ("node.kind", "external"),
    ] {
        let (status, ev) = row_status(expr);
        assert_eq!(status, expected_status, "expr={expr} evidence={ev}");
    }
}

#[test]
fn rust_precision_destructuring_macro_reexport_and_homonym_matrix() {
    let workspace = Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"precision_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nclap = { version = \"4\", features = [\"derive\"] }\n",
    );
    workspace.write(
        "src/models.rs",
        r#"
pub struct Store;
impl Store {
    pub fn ping(&self) -> bool { true }
}

pub struct Pair {
    pub left: Store,
    pub right: Store,
}

#[derive(Clone, Copy)]
pub enum ReceiverHint {
    Direct,
    Deref,
}

pub trait EnumCodec {
    fn tag_str(&self) -> &'static str;
}

#[macro_export]
macro_rules! enum_codec {
    ($ty:ty, $($var:ident => $lit:literal),+ $(,)?) => {
        impl $crate::models::EnumCodec for $ty {
            fn tag_str(&self) -> &'static str { "ok" }
        }
    };
}
"#,
    );
    workspace.write(
        "src/facade.rs",
        r#"
pub(crate) mod inner {
    pub struct AliasTarget;
    impl AliasTarget {
        pub fn ready(&self) -> bool { true }
    }
}

pub(crate) use self::inner::AliasTarget as PublicAliasTarget;
"#,
    );
    workspace.write(
        "src/lib.rs",
        r#"
mod models;
mod facade;

use clap::Parser;
use crate::enum_codec;
use crate::models::{EnumCodec, Pair, ReceiverHint, Store};
use crate::facade::PublicAliasTarget;

macro_rules! target_decoder {
    ($name:ident) => {
        pub fn $name() -> bool { true }
    };
}

#[derive(clap::Parser)]
struct Cli {
    flag: bool,
}

impl Cli {
    fn boot() -> bool {
        let parsed = Self::parse();
        let _ = Self::try_parse();
        parsed.flag
    }
}

enum_codec!(crate::models::ReceiverHint, Direct => "direct", Deref => "deref");
target_decoder!(decode_target);

fn exercise(
    pair: Pair,
    tup: (Store, Store),
    opt_store: Option<Store>,
    match_opt: Option<Store>,
    res_store: Result<Store, ()>,
    items: Vec<Store>,
    hint: ReceiverHint,
    alias: PublicAliasTarget,
) -> bool {
    let Pair { left, right } = pair;
    let (first, second) = tup;
    let mut ok = left.ping() && right.ping() && first.ping() && second.ping();
    if let Some(inner) = opt_store {
        ok &= inner.ping();
    }
    let Ok(unwrapped) = res_store else {
        return false;
    };
    ok &= unwrapped.ping();
    match match_opt {
        Some(matched) => ok &= matched.ping(),
        None => {}
    }
    for elem in items {
        ok &= elem.ping();
    }
    ok &= hint.tag_str().is_empty();
    ok &= alias.ready();
    ok &= decode_target();
    let _ = Cli::boot();
    let _ = fake_unregistered::time::Instant::now();
    let _ = fake_unregistered::models::Store::ping(&unwrapped);
    ok
}
"#,
    );
    workspace.build();
    let conn = workspace.open();

    let row_status = |expr: &str| -> (String, String) {
        conn.query_row(
            "SELECT r.status, e.evidence FROM resolution_coverage r
             JOIN path_dictionary p ON p.path_id = r.path_id
             JOIN coverage_expressions x ON x.expression_id = r.expression_id
             JOIN coverage_evidence e ON e.evidence_id = r.evidence_id
             WHERE p.path = 'src/lib.rs' AND x.expression = ?1
             LIMIT 1",
            [expr],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or_else(|e| panic!("missing coverage row for {expr}: {e}"))
    };

    for (expr, expected_status) in [
        ("Self::parse", "resolved"),
        ("Self::try_parse", "resolved"),
        ("left.ping", "resolved"),
        ("right.ping", "resolved"),
        ("first.ping", "resolved"),
        ("second.ping", "resolved"),
        ("inner.ping", "resolved"),
        ("unwrapped.ping", "resolved"),
        ("matched.ping", "resolved"),
        ("elem.ping", "resolved"),
        ("hint.tag_str", "resolved"),
        ("alias.ready", "resolved"),
        ("decode_target", "resolved"),
        ("Cli::boot", "resolved"),
        ("fake_unregistered::time::Instant::now", "unresolved"),
        ("fake_unregistered::models::Store::ping", "unresolved"),
    ] {
        let (status, ev) = row_status(expr);
        assert_eq!(status, expected_status, "expr={expr} evidence={ev}");
    }
}

#[test]
fn universal_value_flow_branch_joins_external_aliases_and_call_returns_matrix() {
    let workspace = Workspace::new();
    workspace.write(
        ".acdd/frameworks/probe.toml",
        r#"
enabled = true

[[syntax_rules]]
id = "probe-py-client"
language = "python"
operation = { kind = "receiver_type", receiver = { name = "Probe.PyClient", members = ["ping"], properties = {}, call_returns = { py_connect = "Probe.PyClient" }, callback_callees = [], generic = false } }

[[syntax_rules]]
id = "probe-ts-client"
language = "typescript"
operation = { kind = "receiver_type", receiver = { name = "Probe.TsClient", members = ["ping"], properties = {}, call_returns = { ts_connect = "Promise<Probe.TsClient>" }, callback_callees = [], generic = false } }

[[class_hierarchy_receivers]]
bases = ["tokio::probe::Factory"]
provider = "tokio"
members = ["connect"]
[class_hierarchy_receivers.returns]
connect = { kind = "receiver", base = "std::result::Result<Arc<tokio::probe::Client>, std::io::Error>" }

[[class_hierarchy_receivers]]
bases = ["tokio::probe::Client"]
provider = "tokio"
members = ["ping"]
"#,
    );
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ntokio = \"1\"\n",
    );
    workspace.write(
        "py_reexport.py",
        "from ext_pkg import ExtHelper\n",
    );
    workspace.write(
        "py_consumer.py",
        "from py_reexport import ExtHelper\n\ndef run_py():\n    ExtHelper.run()\n    client = py_connect()\n    client.ping()\n",
    );
    if cfg!(feature = "lang-typescript") {
        workspace.write(
            "ts_reexport.ts",
            "import { ExtWidget } from 'ext-ui';\nexport { ExtWidget };\n",
        );
        workspace.write(
            "ts_consumer.ts",
            "import { ExtWidget } from './ts_reexport';\nexport async function runTs() {\n    ExtWidget.render();\n    const tsClient = await ts_connect();\n    tsClient.ping();\n}\n",
        );
    }
    workspace.write(
        "src/reexport.rs",
        "pub use ext_crate::ExtService;\npub use tokio::probe::Factory;\n",
    );
    workspace.write(
        "src/lib.rs",
        r#"
mod reexport;
use reexport::{ExtService, Factory};

pub struct DefaultItem;
impl DefaultItem {
    pub fn default_work(&self) {}
    pub fn shared_work(&self) {}
}

pub struct ValItem;
impl ValItem {
    pub fn shared_work(&self) {}
}

pub struct PayloadItem;
impl PayloadItem {
    pub fn payload_work(&self) {}
}

pub fn tuple_option_join_and_monotonic_loop(
    input: Option<PayloadItem>,
    flag: bool,
    raw: &str,
) {
    let (same_item, opt_item) = match input {
        None => (DefaultItem {}, None),
        Some(_) => (DefaultItem {}, Some(PayloadItem {})),
    };
    same_item.default_work();
    if let Some(p) = opt_item {
        p.payload_work();
    }

    let (mixed_item, opt_item2) = match flag {
        false => (DefaultItem {}, None),
        true => (ValItem {}, Some(PayloadItem {})),
    };
    mixed_item.shared_work();
    opt_item2.unwrap().payload_work();

    let mut scope = raw;
    while let Some((parent, _)) = scope.rsplit_once('.') {
        scope = parent;
    }
    let _ = scope.len();
    if flag {
        scope = unknown_str();
    }
    let _ = scope.rsplit_once('.');

    let client = Factory::connect().unwrap();
    client.ping();
    ExtService::boot();
}
"#,
    );
    workspace.build();
    let conn = workspace.open();

    let status_for = |path: &str, expr: &str| -> (String, String) {
        conn.query_row(
            "SELECT r.status, e.evidence FROM resolution_coverage r
             JOIN path_dictionary p ON p.path_id = r.path_id
             JOIN coverage_expressions x ON x.expression_id = r.expression_id
             JOIN coverage_evidence e ON e.evidence_id = r.evidence_id
             WHERE p.path = ?1 AND x.expression = ?2
             LIMIT 1",
            [path, expr],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or_else(|err| panic!("missing coverage for {path} `{expr}`: {err}"))
    };

    for (path, expr, expected) in [
        ("py_consumer.py", "ExtHelper.run", "external"),
        ("py_consumer.py", "client.ping", "external"),
        ("src/lib.rs", "same_item.default_work", "resolved"),
        ("src/lib.rs", "p.payload_work", "resolved"),
        ("src/lib.rs", "mixed_item.shared_work", "ambiguous"),
        ("src/lib.rs", "opt_item2.unwrap().payload_work", "resolved"),
        ("src/lib.rs", "scope.len", "external"),
        ("src/lib.rs", "scope.rsplit_once", "external"),
        ("src/lib.rs", "client.ping", "external"),
        ("src/lib.rs", "ExtService::boot", "external"),
    ] {
        let (status, evidence) = status_for(path, expr);
        assert_eq!(status, expected, "{path} `{expr}` ({evidence})");
    }
    if cfg!(feature = "lang-typescript") {
        for (path, expr, expected) in [
            ("ts_consumer.ts", "ExtWidget.render", "external"),
            ("ts_consumer.ts", "tsClient.ping", "external"),
        ] {
            let (status, evidence) = status_for(path, expr);
            assert_eq!(status, expected, "{path} `{expr}` ({evidence})");
        }
    }

    let mixed_possible: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge_occurrences e
             JOIN nodes d ON d.node_hash = e.dst_hash
             JOIN coverage_evidence v ON v.evidence_id = e.evidence_id
             WHERE e.kind = 'possible_calls' AND d.name = 'shared_work' AND v.evidence = 'mixed_item.shared_work'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mixed_possible, 2, "expected 2 possible_calls for mixed_item.shared_work");

    // Also verify element-wise TypeTarget::Tuple branch join directly in ValueFlowIndex.
    let tuple_owner = acdd_index::models::Node {
        native_semantic: None,
        id: "fn:tuple_join.py:1:run".into(),
        kind: "function".into(),
        name: "run".into(),
        qualname: "tuple_join.run".into(),
        path: "tuple_join.py".into(),
        line: 1,
        end_line: 4,
        is_test: true,
        language: "python".into(),
        generated: false,
        details: serde_json::json!({
            "value_flow": acdd_index::semantic::ValueFlowFacts {
                bindings: vec![acdd_index::semantic::BindingFact {
                    name: "pair".into(),
                    position: acdd_index::semantic::SourcePosition { line: 2, column: 4 },
                    value: acdd_index::semantic::ValueExpr::BranchJoin {
                        alternatives: vec![
                            acdd_index::semantic::ValueExpr::Alias { name: "a".into() },
                            acdd_index::semantic::ValueExpr::Alias { name: "b".into() },
                        ],
                    },
                    conditional: false,
                }],
                ..Default::default()
            }
        }),
    };
    struct TupleResolver<'a>(&'a acdd_index::models::Node);
    impl<'a> acdd_index::linker::value_flow::SemanticResolver<'a> for TupleResolver<'a> {
        fn resolve_symbol(
            &self,
            _scope: &'a acdd_index::models::Node,
            name: &str,
            _at: acdd_index::semantic::SourcePosition,
            _role: acdd_index::linker::value_flow::SymbolRole,
        ) -> acdd_index::linker::value_flow::Symbol<'a> {
            use acdd_index::linker::value_flow::{Symbol, TypeTarget};
            match name {
                "a" => Symbol::Type(TypeTarget::Tuple(vec![
                    TypeTarget::Local(self.0),
                    TypeTarget::Builtin("str".into()),
                ])),
                "b" => Symbol::Type(TypeTarget::Tuple(vec![
                    TypeTarget::Local(self.0),
                    TypeTarget::Builtin("str".into()),
                ])),
                _ => Symbol::Unknown,
            }
        }
    }
    let facts_map = std::collections::BTreeMap::from([(
        "tuple_join.py".to_owned(),
        acdd_index::models::Facts {
            nodes: vec![tuple_owner],
            ..Default::default()
        },
    )]);
    let owner_ref = &facts_map["tuple_join.py"].nodes[0];
    let vf_index = acdd_index::linker::value_flow::ValueFlowIndex::build(
        &facts_map,
        &TupleResolver(owner_ref),
    );
    let joined = vf_index.resolve_value(
        owner_ref,
        &acdd_index::semantic::ValueExpr::BranchJoin {
            alternatives: vec![
                acdd_index::semantic::ValueExpr::Annotated {
                    type_expr: acdd_index::semantic::TypeExpr::Named { name: "a".into() },
                },
                acdd_index::semantic::ValueExpr::Annotated {
                    type_expr: acdd_index::semantic::TypeExpr::Named { name: "b".into() },
                },
            ],
        },
        acdd_index::semantic::SourcePosition { line: 3, column: 0 },
        &TupleResolver(owner_ref),
    );
    assert!(
        matches!(
            joined,
            acdd_index::linker::value_flow::TypeTarget::Tuple(ref items)
                if items.len() == 2
                    && matches!(items[0], acdd_index::linker::value_flow::TypeTarget::Local(_))
                    && matches!(items[1], acdd_index::linker::value_flow::TypeTarget::Builtin(ref b) if b == "str")
        ),
        "expected element-wise Tuple BranchJoin, got {joined:?}"
    );
}
