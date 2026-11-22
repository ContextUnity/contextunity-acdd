use super::*;

type CoverageExpectation<'a> = (&'a str, usize, &'a str, &'a str, &'a str, usize);
type EdgeExpectation<'a> = (&'a str, &'a str, &'a str, usize);
type LinkCase<'a> = (
    &'a str,
    &'a [(&'a str, &'a str)],
    &'a [CoverageExpectation<'a>],
    &'a [EdgeExpectation<'a>],
);

fn python_extracted(path: &str, source: &str) -> acdd_index::models::Facts {
    ExtractorHarness::for_lang("python")
        .at_path(path)
        .extract(source)
}

#[test]
fn python_scope_metadata() {
    for (case_id,
 path,
 source,
 expectations) in [
        ("typed_parameters_preserve_forward_qualified_and_default_types_in_their_scope",
 "parameters.py",
"class Service:\n    def execute(self): pass\ndef run(service: Service, forward: 'pkg.Service', default: pkg.Service = None, *items: Service, **options: 'pkg.Service'):\n    def nested(service: Other): pass\n    return service.execute()\nclass Client:\n    def call(self, service: Service): return service.execute()\ndef untyped(service): return service.execute()\n",
 vec![
            ("run",
 "param_types",
 Some(json!({"service":"Service",
 "forward":"pkg.Service",
 "default":"pkg.Service"}))),

            ("nested",
 "param_types",
 Some(json!({"service":"Other"}))),

            ("call",
 "param_types",
 Some(json!({"service":"Service"}))),

            ("untyped",
 "param_types",
 None),

        ]),

        ("class_assignments_are_bindings_without_leaking_nested_callable_assignments",
 "class_bindings.py",
 "class Model:\n    objects = factory()\n    execute: object = custom\n    def method(self):\n        local = 1\n    class Nested:\n        nested = 1\n",
 vec![("Model",
 "bindings",
 Some(json!(["execute",
 "objects"])))]),

    ] {
        let facts = python_extracted(path,
 source);
        assert!(facts.errors.is_empty(),
 "{case_id}: {:?}",
 facts.errors);
        for (name,
 field,
 expected) in expectations {
            let node = facts.nodes.iter().find(|n| n.name == name).unwrap();
            let details = crate::common::assertions::projected_node_details(&facts,
 node);
            assert_eq!(details.get(field),
 expected.as_ref(),
 "{case_id}: {name}.{field}");
        }
    }
}

#[test]
fn computed_receiver_hints_are_structural_and_preserve_original_expression() {
    use acdd_index::semantic::{SourcePosition, ValueExpr};
    let source = "def run():\n    'value'.upper()\n    r'value'.strip()\n    b'value'.upper()\n    f'{value}'.upper()\n    Widget().execute()\n    package.Widget().execute()\n    super().execute()\n    super(Widget, self).execute()\n    response.request().consume()\n    factory().request().consume()\n";
    let facts = python_extracted("computed.py", source);
    assert!(facts.errors.is_empty(), "{:?}", facts.errors);
    let hint = |expression| {
        &facts
            .references
            .iter()
            .find(|reference| reference.expression == expression)
            .unwrap()
            .receiver_hint
    };
    assert!(
        matches!(hint("'value'.upper"), Some(ReceiverHint::StringLiteral { member }) if member == "upper")
    );
    assert!(
        matches!(hint("r'value'.strip"), Some(ReceiverHint::StringLiteral { member }) if member == "strip")
    );
    assert!(hint("b'value'.upper").is_none());
    assert!(hint("f'{value}'.upper").is_none());
    assert!(
        matches!(hint("Widget().execute"), Some(ReceiverHint::TypedCallResult { value: ValueExpr::Call { callee, site: None }, member }) if callee == "Widget" && member == "execute")
    );
    assert!(
        matches!(hint("package.Widget().execute"), Some(ReceiverHint::TypedCallResult { value: ValueExpr::Call { callee, site: None }, member }) if callee == "package.Widget" && member == "execute")
    );
    assert!(
        matches!(hint("super().execute"), Some(ReceiverHint::Super { member }) if member == "execute")
    );
    assert!(hint("super(Widget, self).execute").is_none());
    assert!(
        matches!(hint("response.request().consume"), Some(ReceiverHint::TypedCallResult { value: ValueExpr::Call { callee, site: None }, member }) if callee == "response.request" && member == "consume")
    );
    let Some(ReceiverHint::TypedCallResult {
        value: ValueExpr::Call {
            callee,
            site: Some(site),
        },
        member,
    }) = hint("factory().request().consume")
    else {
        panic!("nested receiver must retain its canonical call evidence");
    };
    assert_eq!(callee, "request");
    assert_eq!(member, "consume");
    assert_eq!(
        site.position,
        SourcePosition {
            line: 11,
            column: 4
        }
    );
    assert!(site.arguments.is_empty());
    assert!(
        matches!(site.receiver.as_deref(), Some(ValueExpr::Call { callee, site: None }) if callee == "factory")
    );
}

#[test]
fn python_local_receiver_identity() {
    let cases: &[LinkCase<'_>] = &[
        ("verified_constructor_super_and_known_string_methods_resolve_at_the_linker",
            &[("resolved.py",
            "class Base:\n    def execute(self): pass\nclass Child(Base):\n    def execute(self): pass\n    def run(self): return super().execute()\ndef direct(): return Base().execute()\ndef literal(): return 'value'.upper()\ndef invalid(): return 'value'.invented_method()\n")],
            &[("resolved.py",
                    0,
                    "super().execute",
                    "resolved",
                    "",
                    1),
                ("resolved.py",
                    0,
                    "Base().execute",
                    "resolved",
                    "",
                    1),
                ("resolved.py",
                    0,
                    "'value'.upper",
                    "resolved",
                    "",
                    1),
                ("resolved.py",
                    0,
                    "'value'.invented_method",
                    "unresolved",
                    "",
                    1)],
            &[("",
                    "resolved.Base.execute",
                    "",
                    2)]),

        ("local_receiver_dictionary_and_logger_methods_resolve_in_function_scope",
            &[("decode.py",
            "\nfrom typing import Mapping\nimport logging\n\ndef decode(raw: Mapping[str, object], logger: logging.LoggerAdapter):\n    val = raw.get(\"field\")\n    items = raw.items()\n    keys = raw.keys()\n    values = raw.values()\n    logger.info(\"decoded\")\n    logger.warning(\"retry\")\n    logger.debug(\"details\")\n    logger.error(\"failed\")\n\ndef helper():\n    row = {\"id\": 1}\n    row.get(\"id\")\n    row.keys()\n    row.values()\n    row.update({\"next\": 2})\n    return row.pop(\"id\")\n\nclass LocalMapping:\n    def get(self, key): return key\n\ndef local(mapping: LocalMapping):\n    return mapping.get(\"field\")\n\ndef conventional(logger, log):\n    logger.exception(\"failed\")\n    log.debug(\"details\")\n")],
            &[("decode.py",
                    0,
                    "mapping.get",
                    "resolved",
                    "",
                    1),
                ("decode.py",
                    0,
                    "raw.get",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "raw.items",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "raw.keys",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "raw.values",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "row.get",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "row.keys",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "row.values",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "row.update",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "row.pop",
                    "external",
                    "Python standard library",
                    1),
                ("decode.py",
                    0,
                    "logger.info",
                    "external",
                    "logging.LoggerAdapter",
                    1),
                ("decode.py",
                    0,
                    "logger.warning",
                    "external",
                    "logging.LoggerAdapter",
                    1),
                ("decode.py",
                    0,
                    "logger.debug",
                    "external",
                    "logging.LoggerAdapter",
                    1),
                ("decode.py",
                    0,
                    "logger.error",
                    "external",
                    "logging.LoggerAdapter",
                    1),
                ("decode.py",
                    0,
                    "logger.info",
                    "external",
                    "line 3",
                    1),
                ("decode.py",
                    0,
                    "logger.warning",
                    "external",
                    "line 3",
                    1),
                ("decode.py",
                    0,
                    "logger.debug",
                    "external",
                    "line 3",
                    1),
                ("decode.py",
                    0,
                    "logger.error",
                    "external",
                    "line 3",
                    1),
                ("decode.py",
                    0,
                    "logger.exception",
                    "unresolved",
                    "",
                    1),
                ("decode.py",
                    0,
                    "log.debug",
                    "unresolved",
                    "",
                    1)],
            &[("",
                    "decode.LocalMapping.get",
                    "mapping.get",
                    1)]),

        ("inherited_self_member_resolves_cross_file_mixin_methods",
            &[("pkg/__init__.py",
                ""),
                ("pkg/connection.py",
                "class SqliteConnectionMixin:\n    def _get_connection(self):\n        return self\n"),
                ("pkg/projection.py",
                "from .connection import SqliteConnectionMixin\nclass SqliteCellEdgeProjectionLayer(SqliteConnectionMixin):\n    pass\n"),
                ("pkg/validation.py",
                "from .projection import SqliteCellEdgeProjectionLayer\nclass SqliteCellEdgeValidationLayer(SqliteCellEdgeProjectionLayer):\n    pass\n"),
                ("pkg/mutations.py",
            "from .validation import SqliteCellEdgeValidationLayer\nclass SqliteCellEdgeMutationLayer(SqliteCellEdgeValidationLayer, UnindexedBase):\n    def execute(self):\n        self._get_connection()\n        self._missing_method()\n")],
            &[("pkg/mutations.py",
                    4,
                    "self._get_connection",
                    "resolved",
                    "",
                    1),
                ("pkg/mutations.py",
                    5,
                    "self._missing_method",
                    "unresolved",
                    "",
                    1)],
            &[]),

        ("nested_self_and_unbound_cls_do_not_take_the_enclosing_class",
            &[("src/repo.py",
            "import sqlite3\n\nclass Repo:\n    def connect(cls) -> sqlite3.Connection:\n        return sqlite3.connect(':memory:')\n\n    def save(self) -> sqlite3.Connection:\n        return sqlite3.connect(':memory:')\n\n    def run(self):\n        def helper(self):\n            nested = self.save()\n            nested.execute('SELECT 1')\n        borrowed = cls.connect()\n        borrowed.execute('SELECT 2')\n\n    def make(cls):\n        owned = cls.connect()\n        owned.execute('SELECT 3')\n")],
            &[("src/repo.py",
                    0,
                    "nested.execute",
                    "unresolved",
                    "",
                    1),
                ("src/repo.py",
                    0,
                    "borrowed.execute",
                    "unresolved",
                    "",
                    1),
                ("src/repo.py",
                    0,
                    "owned.execute",
                    "external",
                    "builtin:sqlite3",
                    1)],
            &[]),

        ("global_name_declaration_does_not_mask_same_named_instance_field",
            &[("fields.py",
                "class Client:\n    def run(self):\n        pass\n\nclass Box:\n    def __init__(self):\n        global client\n        self.client = Client()\n\ndef invoke(box: Box):\n    box.client.run()\n"),
                ("nonlocal_fields.py",
            "class LocalClient:\n    def run(self):\n        pass\n\ndef outer():\n    client = None\n    class Box:\n        def __init__(self):\n            nonlocal client\n            self.client = LocalClient()\n        def invoke(self):\n            self.client.run()\n")],
            &[("fields.py",
                    11,
                    "box.client.run",
                    "resolved",
                    "",
                    1),
                ("nonlocal_fields.py",
                    12,
                    "self.client.run",
                    "resolved",
                    "",
                    1)],
            &[("fields.invoke",
                    "fields.Client.run",
                    "",
                    1)])
    ];
    for &(case_id, files, expectations, edge_expectations) in cases {
        let catalog = ExtractorHarness::linked_with_root("python", files, None);
        for facts in catalog.facts.values() {
            assert!(facts.errors.is_empty(), "{case_id}: {:?}", facts.errors);
        }
        for &(path, line, expression, status, evidence, count) in expectations {
            let rows: Vec<_> = catalog
                .graph
                .coverage
                .iter()
                .filter(|c| {
                    c.path == path && (line == 0 || c.line == line) && c.expression == expression
                })
                .collect();
            assert_eq!(rows.len(), count, "{case_id}: {path}:{line} {expression}");
            for actual in rows {
                let observed = if line == 0 {
                    actual.status.as_str()
                } else {
                    catalog.coverage_line(path, line, expression)
                };
                assert_eq!(observed, status, "{case_id}: {actual:#?}");
                assert!(actual.evidence.contains(evidence), "{case_id}: {actual:#?}");
            }
        }
        for &(source, target, expression, count) in edge_expectations {
            let target_id = &catalog
                .facts
                .values()
                .flat_map(|f| &f.nodes)
                .find(|n| n.qualname == target)
                .unwrap()
                .id;
            let actual = if source.is_empty() {
                catalog
                    .graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "calls"
                            && &edge.dst == target_id
                            && (expression.is_empty() || edge.evidence == expression)
                    })
                    .count()
            } else {
                catalog
                    .edge_qualnames(source, "calls")
                    .iter()
                    .filter(|name| name.as_str() == target)
                    .count()
            };
            assert_eq!(
                actual, count,
                "{case_id}: {source} -> {target} ({expression})"
            );
        }
    }
}

#[test]
fn logging_factory_methods_have_verified_builtin_origin_after_persistence() {
    let w = fixture_workspace(&[]);
    let source = "import logging\nlogging.basicConfig(level=logging.INFO)\nlogger = logging.getLogger(__name__)\nlogger.info('i')\nlogger.warning('w')\nlogger.error('e')\nlogger.debug('d')\nlogger.exception('x')\nlogger.process('x')\ndef unknown(logger):\n    logger.info('x')\n";
    let service_source = "import sqlite3\nimport logging\n\ndef run(conn: sqlite3.Connection, adapter: logging.LoggerAdapter):\n    conn.execute('SELECT 1')\n    conn.commit()\n    conn.rollback()\n    conn.cursor()\n    conn.close()\n    conn.unknown_method()\n    adapter.info('i')\n    adapter.warning('w')\n    adapter.error('e')\n    adapter.debug('d')\n    adapter.critical('c')\n    adapter.exception('x')\n    adapter.unknown_call()\n";
    w.write("src/factory.py", source);
    w.write(
        "src/rebound.py",
        "import logging\nlogging = object()\nlogger = logging.getLogger(__name__)\nlogger.info('x')\n",
    );
    w.write("src/service.py", service_source);
    let check = |conn: &rusqlite::Connection| {
        for (line, method) in [
            (4, "info"),
            (5, "warning"),
            (6, "error"),
            (7, "debug"),
            (8, "exception"),
        ] {
            let expression = format!("logger.{method}");
            let (status, evidence) = persisted_coverage(conn, "src/factory.py", line, &expression);
            assert_eq!(status, "external", "{expression}: {evidence}");
            assert_eq!(
                evidence,
                format!("builtin:logging.Logger built-in: {expression}")
            );
        }
        assert_eq!(
            persisted_status(conn, "src/factory.py", 9, "logger.process"),
            "unresolved"
        );
        assert_eq!(
            persisted_status(conn, "src/factory.py", 11, "logger.info"),
            "unresolved"
        );
        assert_eq!(
            persisted_status(conn, "src/rebound.py", 4, "logger.info"),
            "unresolved"
        );

        let (status, evidence) = persisted_coverage(conn, "src/service.py", 1, "sqlite3");
        assert_eq!(status, "external");
        assert!(evidence.contains("standard library"), "{evidence}");

        let (status, evidence) =
            persisted_coverage(conn, "src/service.py", 4, "sqlite3.Connection");
        assert_eq!(status, "external");
        assert!(evidence.contains("sqlite3"), "{evidence}");

        for (line, method) in [
            (5, "execute"),
            (6, "commit"),
            (7, "rollback"),
            (8, "cursor"),
            (9, "close"),
        ] {
            let expression = format!("conn.{method}");
            let (status, evidence) = persisted_coverage(conn, "src/service.py", line, &expression);
            assert_eq!(status, "external", "{expression}: {evidence}");
            assert!(
                evidence.contains("builtin:sqlite3"),
                "{expression}: {evidence}"
            );
        }
        assert_eq!(
            persisted_status(conn, "src/service.py", 10, "conn.unknown_method"),
            "unresolved"
        );

        for (line, method) in [
            (11, "info"),
            (12, "warning"),
            (13, "error"),
            (14, "debug"),
            (15, "critical"),
            (16, "exception"),
        ] {
            let expression = format!("adapter.{method}");
            let (status, evidence) = persisted_coverage(conn, "src/service.py", line, &expression);
            assert_eq!(status, "external", "{expression}: {evidence}");
            assert!(
                evidence.contains("builtin:logging.LoggerAdapter"),
                "{expression}: {evidence}"
            );
        }
        assert_eq!(
            persisted_status(conn, "src/service.py", 17, "adapter.unknown_call"),
            "unresolved"
        );

        let contains_edges: i64 = query_scalar(
            conn,
            "SELECT count(*) FROM edge_occurrences o JOIN path_dictionary p ON p.path_id=o.owner_id WHERE p.path='src/service.py' AND o.kind='contains'",
            [],
        );
        assert_eq!(contains_edges, 2);

        let calls_edges: i64 = query_scalar(
            conn,
            "SELECT count(*) FROM edge_occurrences o JOIN path_dictionary p ON p.path_id=o.owner_id WHERE p.path='src/service.py' AND o.kind='calls'",
            [],
        );
        assert_eq!(
            calls_edges, 0,
            "no fabricated calls edges should be emitted for unknown calls"
        );
    };
    w.build();
    let cold = w.open();
    check(&cold);
    drop(cold);
    w.write("src/factory.py", &format!("{source}# index delta\n"));
    w.write(
        "src/service.py",
        &format!("{service_source}# index delta\n"),
    );
    w.delta(&["src/factory.py", "src/service.py"]);
    let delta = w.open();
    check(&delta);
}

#[test]
fn python_context_value_returns() {
    let cases: &[LinkCase<'_>] = &[
        ("with_statement_as_target_uses_the_context_value",
            &[("ctx.py",
            "class Conn:\n    def execute(self):\n        return 1\n\ndef use():\n    with Conn() as db:\n        db.execute()\n    db.execute()\n")],
            &[("ctx.py",
                    0,
                    "db.execute",
                    "resolved",
                    "",
                    2)],
            &[("",
                    "ctx.Conn.execute",
                    "db.execute",
                    2)]),

        ("call_return_type_and_context_manager_propagation_resolves_receiver_types",
            &[("db_factory.py",
                "import sqlite3\n\ndef get_db() -> sqlite3.Connection:\n    return sqlite3.connect(':memory:')\n\ndef unannotated_db():\n    return sqlite3.connect(':memory:')\n"),
                ("consumer.py",
                "import sqlite3\nfrom db_factory import get_db, unannotated_db\n\ndef run():\n    with get_db() as db:\n        db.execute('SELECT 1')\n    with unannotated_db() as raw:\n        raw.execute('SELECT 2')\n    conn = get_db()\n    conn.execute('SELECT 3')\n"),
                ("mixin_consumer.py",
            "import sqlite3\nclass BaseRepo:\n    def get_conn(self) -> sqlite3.Connection:\n        return sqlite3.connect(':memory:')\n\nclass ChildRepo(BaseRepo):\n    def perform(self):\n        with self.get_conn() as db:\n            db.execute('SELECT 4')\n")],
            &[("consumer.py",
                    8,
                    "raw.execute",
                    "unresolved",
                    "",
                    1),
                ("consumer.py",
                    6,
                    "db.execute",
                    "external",
                    "builtin:sqlite3",
                    1),
                ("consumer.py",
                    10,
                    "conn.execute",
                    "external",
                    "builtin:sqlite3",
                    1),
                ("mixin_consumer.py",
                    9,
                    "db.execute",
                    "external",
                    "builtin:sqlite3",
                    1)],
            &[])
    ];
    for &(case_id, files, expectations, edge_expectations) in cases {
        let catalog = ExtractorHarness::linked_with_root("python", files, None);
        for facts in catalog.facts.values() {
            assert!(facts.errors.is_empty(), "{case_id}: {:?}", facts.errors);
        }
        for &(path, line, expression, status, evidence, count) in expectations {
            let rows: Vec<_> = catalog
                .graph
                .coverage
                .iter()
                .filter(|c| {
                    c.path == path && (line == 0 || c.line == line) && c.expression == expression
                })
                .collect();
            assert_eq!(rows.len(), count, "{case_id}: {path}:{line} {expression}");
            for actual in rows {
                let observed = if line == 0 {
                    actual.status.as_str()
                } else {
                    catalog.coverage_line(path, line, expression)
                };
                assert_eq!(observed, status, "{case_id}: {actual:#?}");
                assert!(actual.evidence.contains(evidence), "{case_id}: {actual:#?}");
            }
        }
        for &(source, target, expression, count) in edge_expectations {
            let target_id = &catalog
                .facts
                .values()
                .flat_map(|f| &f.nodes)
                .find(|n| n.qualname == target)
                .unwrap()
                .id;
            let actual = if source.is_empty() {
                catalog
                    .graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "calls"
                            && &edge.dst == target_id
                            && (expression.is_empty() || edge.evidence == expression)
                    })
                    .count()
            } else {
                catalog
                    .edge_qualnames(source, "calls")
                    .iter()
                    .filter(|name| name.as_str() == target)
                    .count()
            };
            assert_eq!(
                actual, count,
                "{case_id}: {source} -> {target} ({expression})"
            );
        }
    }
}

#[test]
fn python_iterable_and_async_return_authority() {
    let cases: &[LinkCase<'_>] = &[
        ("loop_element_types_require_proven_unshadowed_iterables::resolved_source",
            &[("loop_service.py",
            "from typing import Sequence, Mapping, Iterable\n\ndef process_list(items: list[dict]):\n    for row in items:\n        row.get('key')\n\ndef process_seq(items: Sequence[Mapping]):\n    for row in items:\n        row.get('key')\n\ndef process_iter(items: Iterable[dict]):\n    for row in items:\n        row.get('key')\n\ndef process_untyped(items):\n    for row in items:\n        row.get('key')\n")],
            &[("loop_service.py",
                    5,
                    "row.get",
                    "external",
                    "Python standard library",
                    1),
                ("loop_service.py",
                    9,
                    "row.get",
                    "external",
                    "Python standard library",
                    1),
                ("loop_service.py",
                    13,
                    "row.get",
                    "external",
                    "Python standard library",
                    1),
                ("loop_service.py",
                    17,
                    "row.get",
                    "unresolved",
                    "",
                    1)],
            &[]),

        ("loop_element_types_require_proven_unshadowed_iterables::guarded_source",
            &[("loop_guard.py",
            "class Sequence:\n    pass\n\ndef rebound(items: list[dict]):\n    items = None\n    for row in items:\n        row.get('key')\n\ndef shadowed(items: Sequence[dict]):\n    for row in items:\n        row.get('key')\n")],
            &[("loop_guard.py",
                    7,
                    "row.get",
                    "unresolved",
                    "",
                    1),
                ("loop_guard.py",
                    11,
                    "row.get",
                    "unresolved",
                    "",
                    1)],
            &[]),

        ("awaited_generic_returns_provide_only_proven_loop_element_types",
            &[("providers.py",
                "def passthrough(fn):\n    return fn\n\nasync def typed_rows() -> list[dict[str, object]]:\n    return []\n\nasync def untyped_rows():\n    return []\n\n@passthrough\nasync def decorated_rows() -> list[dict[str, object]]:\n    return []\n"),
                ("consumer.py",
            "from providers import typed_rows, untyped_rows, decorated_rows\n\nasync def run():\n    typed_items = await typed_rows()\n    for item in typed_items:\n        item.get('typed')\n    untyped_items = await untyped_rows()\n    for unknown in untyped_items:\n        unknown.get('unknown')\n    decorated_items = await decorated_rows()\n    for decorated in decorated_items:\n        decorated.get('decorated')\n")],
            &[("consumer.py",
                    6,
                    "item.get",
                    "external",
                    "Python standard library",
                    1),
                ("consumer.py",
                    9,
                    "unknown.get",
                    "unresolved",
                    "",
                    1),
                ("consumer.py",
                    12,
                    "decorated.get",
                    "unresolved",
                    "",
                    1)],
            &[]),

        ("awaited_typed_receiver_methods_preserve_async_return_types",
            &[("models.py",
                "class Worker:\n    def work(self) -> None:\n        pass\n\nclass Client:\n    async def fetch(self) -> Worker:\n        return Worker()\n\n    async def fetch_all(self) -> list[Worker]:\n        return []\n"),
                ("consumer.py",
            "from models import Client\n\nasync def typed(client: Client):\n    result = await client.fetch()\n    result.work()\n\nasync def untyped(client):\n    result = await client.fetch()\n    result.work()\n\nasync def shadowed(client: Client):\n    client = object()\n    result = await client.fetch()\n    result.work()\n\nasync def spaced(client: Client):\n    result = await client . fetch()\n    result.work()\n\nasync def typed_collection(client: Client):\n    results = await client.fetch_all()\n    for worker in results:\n        worker.work()\n")],
            &[("consumer.py",
                    5,
                    "result.work",
                    "resolved",
                    "",
                    1),
                ("consumer.py",
                    18,
                    "result.work",
                    "resolved",
                    "",
                    1),
                ("consumer.py",
                    23,
                    "worker.work",
                    "resolved",
                    "",
                    1),
                ("consumer.py",
                    9,
                    "result.work",
                    "unresolved",
                    "",
                    1),
                ("consumer.py",
                    14,
                    "result.work",
                    "unresolved",
                    "",
                    1)],
            &[("consumer.typed",
                    "models.Worker.work",
                    "",
                    1),
                ("consumer.spaced",
                    "models.Worker.work",
                    "",
                    1),
                ("consumer.typed_collection",
                    "models.Worker.work",
                    "",
                    1)])
    ];
    assert_link_cases(cases);
}

fn assert_link_cases(cases: &[LinkCase<'_>]) {
    for &(case_id, files, expectations, edge_expectations) in cases {
        let catalog = ExtractorHarness::linked_with_root("python", files, None);
        for facts in catalog.facts.values() {
            assert!(facts.errors.is_empty(), "{case_id}: {:?}", facts.errors);
        }
        for &(path, line, expression, status, evidence, count) in expectations {
            let rows: Vec<_> = catalog
                .graph
                .coverage
                .iter()
                .filter(|c| {
                    c.path == path && (line == 0 || c.line == line) && c.expression == expression
                })
                .collect();
            assert_eq!(rows.len(), count, "{case_id}: {path}:{line} {expression}");
            for actual in rows {
                let observed = if line == 0 {
                    actual.status.as_str()
                } else {
                    catalog.coverage_line(path, line, expression)
                };
                assert_eq!(observed, status, "{case_id}: {actual:#?}");
                assert!(actual.evidence.contains(evidence), "{case_id}: {actual:#?}");
            }
        }
        for &(source, target, expression, count) in edge_expectations {
            let target_id = &catalog
                .facts
                .values()
                .flat_map(|f| &f.nodes)
                .find(|n| n.qualname == target)
                .unwrap()
                .id;
            let actual = if source.is_empty() {
                catalog
                    .graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "calls"
                            && &edge.dst == target_id
                            && (expression.is_empty() || edge.evidence == expression)
                    })
                    .count()
            } else {
                catalog
                    .edge_qualnames(source, "calls")
                    .iter()
                    .filter(|name| name.as_str() == target)
                    .count()
            };
            assert_eq!(
                actual, count,
                "{case_id}: {source} -> {target} ({expression})"
            );
        }
    }
}

#[test]
fn python_typing_chains_context_and_super_matrix() {
    let cases: &[LinkCase<'_>] = &[
        (
            "callable_protocol_and_type_subscripts",
            &[(
                "app.py",
                concat!(
                    "from typing import Callable, Generic, Protocol, Type, TypeVar\n",
                    "T = TypeVar('T')\n",
                    "class Client:\n",
                    "    def __init__(self): pass\n",
                    "    def execute(self): pass\n",
                    "    @classmethod\n",
                    "    def from_env(cls): pass\n",
                    "class Box:\n",
                    "    def fetch(self): pass\n",
                    "type AliasBox = Box[Client]\n",
                    "class Repo(Generic[T]):\n",
                    "    def fetch(self): pass\n",
                    "class Runner(Protocol):\n",
                    "    def __call__(self, x: int) -> Client: ...\n",
                    "def run(cb: Callable[[int], Client], cb_none: Callable[[int], None], runner: Runner, cls: type[Client], tcls: Type[Client], repo: Repo[Client], box: AliasBox):\n",
                    "    c1 = cb(1)\n",
                    "    c1.execute()\n",
                    "    cb(1).execute()\n",
                    "    cb_none(1)\n",
                    "    cb_none(1).anything()\n",
                    "    c2 = runner(1)\n",
                    "    c2.execute()\n",
                    "    runner(1).execute()\n",
                    "    inst = cls()\n",
                    "    inst.execute()\n",
                    "    cls().execute()\n",
                    "    cls.from_env()\n",
                    "    tcls().execute()\n",
                    "    repo.fetch()\n",
                    "    box.fetch()\n",
                ),
            )],
            &[
                ("app.py", 16, "cb", "resolved", "", 1),
                ("app.py", 17, "c1.execute", "resolved", "", 1),
                ("app.py", 18, "cb(1).execute", "resolved", "", 1),
                ("app.py", 19, "cb_none", "resolved", "", 1),
                ("app.py", 20, "cb_none(1).anything", "unresolved", "", 1),
                ("app.py", 21, "runner", "resolved", "", 1),
                ("app.py", 22, "c2.execute", "resolved", "", 1),
                ("app.py", 23, "runner(1).execute", "resolved", "", 1),
                ("app.py", 24, "cls", "resolved", "", 1),
                ("app.py", 25, "inst.execute", "resolved", "", 1),
                ("app.py", 26, "cls().execute", "resolved", "", 1),
                ("app.py", 27, "cls.from_env", "resolved", "", 1),
                ("app.py", 28, "tcls().execute", "resolved", "", 1),
                ("app.py", 29, "repo.fetch", "resolved", "", 1),
                ("app.py", 30, "box.fetch", "unresolved", "", 1),
            ],
            &[
                ("", "app.Client.execute", "", 7),
                ("", "app.Runner.__call__", "", 2),
                ("", "app.Repo.fetch", "", 1),
            ],
        ),
        (
            "bytearray_pathlib_fstring_slice_context_super_and_chains",
            &[(
                "ops.py",
                concat!(
                    "from pathlib import Path\n",
                    "class Session:\n",
                    "    def query(self): pass\n",
                    "class Pool:\n",
                    "    def __enter__(self) -> Session:\n",
                    "        return Session()\n",
                    "class AsyncPool:\n",
                    "    async def __aenter__(self) -> Session:\n",
                    "        return Session()\n",
                    "class Base:\n",
                    "    def __init__(self): pass\n",
                    "    def make_session(self) -> Session:\n",
                    "        return Session()\n",
                    "class Child(Base):\n",
                    "    def __init__(self):\n",
                    "        super().__init__()\n",
                    "    def run(self, name: str):\n",
                    "        buf = bytearray(b'abc')\n",
                    "        buf.append(1)\n",
                    "        buf.decode()\n",
                    "        buf.encode()\n",
                    "        p = Path('a') / 'b'\n",
                    "        p.read_text()\n",
                    "        (Path('a') / 'b').exists()\n",
                    "        p.bogus()\n",
                    "        greeting = f'hi {name}'\n",
                    "        greeting.upper()\n",
                    "        part = name[1:3]\n",
                    "        part.lower()\n",
                    "        name[1:3].strip()\n",
                    "        with Pool() as s:\n",
                    "            s.query()\n",
                    "            s.missing()\n",
                    "        super().make_session().query()\n",
                    "class UnannotatedAsyncPool:\n",
                    "    async def __aenter__(self):\n",
                    "        return unknown()\n",
                    "async def run_async():\n",
                    "    async with AsyncPool() as s2:\n",
                    "        s2.query()\n",
                    "    async with UnannotatedAsyncPool() as s_bad:\n",
                    "        s_bad.query()\n",
                    "def step() -> Pool:\n",
                    "    return Pool()\n",
                    "def chained():\n",
                    "    step().__enter__().query()\n",
                    "    p2 = Path('a').parent\n",
                    "    p2.exists()\n",
                    "    fn_ref = step().make_session\n",
                    "    fn_ref.query()\n",
                ),
            )],
            &[
                ("ops.py", 16, "super().__init__", "resolved", "", 1),
                ("ops.py", 19, "buf.append", "external", "Python standard library", 1),
                ("ops.py", 20, "buf.decode", "external", "Python standard library", 1),
                ("ops.py", 21, "buf.encode", "unresolved", "", 1),
                ("ops.py", 23, "p.read_text", "external", "Python standard library", 1),
                ("ops.py", 24, "(Path('a') / 'b').exists", "external", "Python standard library", 1),
                ("ops.py", 25, "p.bogus", "unresolved", "", 1),
                ("ops.py", 27, "greeting.upper", "external", "Python standard library", 1),
                ("ops.py", 29, "part.lower", "external", "Python standard library", 1),
                ("ops.py", 30, "name[1:3].strip", "external", "Python standard library", 1),
                ("ops.py", 32, "s.query", "resolved", "", 1),
                ("ops.py", 33, "s.missing", "unresolved", "", 1),
                ("ops.py", 34, "super().make_session().query", "resolved", "", 1),
                ("ops.py", 40, "s2.query", "resolved", "", 1),
                ("ops.py", 42, "s_bad.query", "unresolved", "", 1),
                ("ops.py", 46, "step().__enter__().query", "resolved", "", 1),
                ("ops.py", 48, "p2.exists", "external", "Python standard library", 1),
                ("ops.py", 50, "fn_ref.query", "unresolved", "", 1),
            ],
            &[
                ("", "ops.Base.__init__", "", 1),
                ("", "ops.Session.query", "", 4),
            ],
        ),
    ];
    assert_link_cases(cases);
}
