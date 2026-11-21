use super::*;
#[test]
fn known_external_alias_is_external_without_claiming_a_callable_target() {
    let w = fixture_workspace(&[]);
    w.write(
        "consumer.py",
        "import requests as http\ndef run(): return http.get('https://example.test')\n",
    );
    w.build();
    let conn = w.open();
    assert_eq!(
        persisted_coverage(&conn, "consumer.py", 1, "requests").0,
        "external"
    );
    let (status, evidence) = persisted_coverage(&conn, "consumer.py", 2, "http.get");
    assert_eq!(status, "external");
    assert_eq!(
        evidence,
        "call through external import requests; callable target unverified"
    );
    let edges: i64 = query_scalar(&conn, "SELECT count(*) FROM edges WHERE kind='calls'", []);
    assert_eq!(edges, 0);
    let page = QueryOptions::resolve(&ResponsePolicy::default(), Some(10), 0, None, None).unwrap();
    for overview in [
        reader::overview_with_options(
            &conn,
            &reader::OverviewOptions {
                aspects: None,
                page: &page,
            },
        )
        .unwrap(),
        traversal::query_with_path_options(
            &conn,
            "overview",
            None,
            None,
            &traversal::GraphQueryOptions {
                depth: 1,
                direction: None,
                coverage: CoverageOptions::default(),
                page: &page,
            },
        )
        .unwrap(),
    ] {
        assert_eq!(overview["counts"]["unresolved"], 0);
        assert_eq!(overview["counts"]["external_imports"], 2);
    }
    let removal = traversal::removal_paged_with_path(&conn, "consumer.py", None, &page).unwrap();
    assert_eq!(removal["unresolved_references"], 0);
}

#[test]
fn module_type_aliases_resolve_imports_and_annotation_references() {
    let w = fixture_workspace(&[]);
    w.write("aliases.py", "from typing import TypeAlias, TypeVar, NewType\nfrom typing_extensions import TypeAliasType\nimport typing\ntype JsonValue = str | list[JsonValue] | dict[str, JsonValue]\ntype GenericAlias[T] = list[T]\nJsonPrimitive: TypeAlias = str | int\nJsonDict: typing.TypeAlias = dict[str, JsonValue]\nExplicit: TypeAliasType = TypeAliasType('Explicit', str)\nVariable = TypeVar('Variable')\nUserId = NewType('UserId', int)\nConstructed = TypeAliasType('Constructed', list[str])\nordinary = 1\nannotated: int = 2\ndef local():\n    Local: TypeAlias = str\n    LocalVariable = TypeVar('LocalVariable')\n    type LocalPep = int\nclass Container:\n    Nested: TypeAlias = str\n");
    let names = [
        "JsonValue",
        "GenericAlias",
        "JsonPrimitive",
        "JsonDict",
        "Explicit",
        "Variable",
        "UserId",
        "Constructed",
    ];
    let mut consumer = format!("from aliases import {}\n", names.join(", "));
    for name in names {
        consumer.push_str(&format!(
            "def use_{name}(value: {name}) -> {name}: return value\n"
        ));
    }
    consumer.push_str("from aliases import Missing\ndef missing(value: Missing): return value\n");
    w.write("consumer.py", &consumer);
    w.build();
    let conn = w.open();
    for name in names {
        let (kind, qualname): (String, String) = conn
            .query_row("SELECT kind,qualname FROM nodes WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='aliases.py') AND name=?1", [name], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap_or_else(|error| {
                panic!(
                    "missing type {name}: {error}; nodes: {:?}",
                    query_strings(&conn, "SELECT name || ':' || kind FROM nodes WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='aliases.py') ORDER BY line", [])
                )
            });
        assert_eq!(kind, "type", "{name}");
        assert_eq!(qualname, format!("aliases.{name}"));
        assert!(
            query_strings(&conn, &format!("SELECT id FROM nodes WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='aliases.py') AND name='{name}'"), [])[0].starts_with("type:aliases.py:")
        );
        let statuses = query_strings(
            &conn,
            &format!(
                "SELECT status FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='consumer.py' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='{name}' ORDER BY line"
            ),
            [],
        );
        assert_eq!(statuses, vec!["resolved"; 2], "{name}");
    }
    assert!(query_strings(&conn, "SELECT name FROM nodes WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='aliases.py') AND kind='type' AND name IN ('ordinary','annotated','Local','LocalVariable','LocalPep','Nested')", []).is_empty());
    assert_eq!(
        query_strings(
            &conn,
            "SELECT (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='consumer.py' AND status='unresolved' ORDER BY line",
            []
        ),
        vec!["Missing", "Missing"]
    );
}

#[test]
fn module_alias_conversion_delta_matches_cold_build() {
    let w = fixture_workspace(&[
        ("aliases.py", "Value = 1\n"),
        (
            "consumer.py",
            "from aliases import Value\ndef use(value: Value) -> Value: return value\n",
        ),
    ]);
    w.build();
    for (source, expected_status) in [
        ("Value = 1\n", "resolved"),
        ("type Value = str | list[Value]\n", "resolved"),
        (
            "from typing import TypeAlias, TYPE_CHECKING\nif TYPE_CHECKING:\n    Value: TypeAlias = str\n",
            "resolved",
        ),
        ("Value = 1\nValue = 2\n", "ambiguous"),
        ("", "unresolved"),
        ("Value = 1\n", "resolved"),
    ] {
        w.write("aliases.py", source);
        w.delta(&["aliases.py"]);
        let parity = crate::languages::harness::cold_relations(&w);
        assert_eq!(parity.incremental_verification, Ok(()));
        assert_eq!(parity.cold_verification, Ok(()));
        assert_eq!(parity.incremental, parity.cold);
        let conn = w.open();
        assert_eq!(
            persisted_coverage(&conn, "consumer.py", 1, "Value").0,
            expected_status,
            "{source:?}"
        );
        let import_edges: i64 = query_scalar(
            &conn,
            "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)='consumer.py' AND e.kind='imports' AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)='aliases.py' AND dst.name='Value'",
            [],
        );
        assert_eq!(import_edges, i64::from(expected_status == "resolved"), "{source:?}");
    }
}

#[test]
fn external_import_authority() {
    let cases = [
        ("absolute_external_import_and_shadowed_alias_have_distinct_evidence",
 "import unknown_package as missing\nimport requests as http\ndef unknown(): return missing.get()\ndef shadowed(http): return http.get()\n",
 &[(3,
"missing.get",
"external",
"prefix",
"call through external import unknown_package;",
true),
(4,
"http.get",
"unresolved",
"absent",
"call through external import ",
false)][..]),

        ("duplicate_aliases_and_local_provider_do_not_gain_external_provenance",
 "import requests as client\nimport httpx as client\nimport provider as local\ndef run(): return client.get(), local.get()\n",
 &[(4,
"client.get",
"unresolved",
"absent",
"call through external import ",
false),
(4,
"local.get",
"resolved",
"absent",
"call through external import ",
false)][..]),

        ("local_import_is_scoped_to_its_function",
 "def first():\n    import requests as http\n    return http.get('https://example.test')\ndef second(): return http.get('https://example.test')\n",
 &[(3,
"http.get",
"",
"exact",
"call through external import requests; callable target unverified",
false),
(4,
"http.get",
"",
"absent",
"call through external import ",
false)][..]),

        ("call_before_external_import_has_no_provenance",
 "def run(): return http.get('https://example.test')\nimport requests as http\n",
 &[(1,
"http.get",
"unresolved",
"absent",
"call through external import ",
false)][..]),

        ("assignment_rebind_has_no_external_provenance",
 "import json\njson = object()\ndef run(): return json.loads('{}')\n",
 &[(3,
"json.loads",
"unresolved",
"absent",
"call through external import ",
false)][..]),

        ("imported_cursor_methods_are_finite_and_untyped_logger_names_stay_unknown",
 "import sqlite3\ndef run(cursor: sqlite3.Cursor, logger):\n    cursor.execute('select 1')\n    cursor.unsupported_method()\n    logger.info('unknown')\n",
 &[(3,
"cursor.execute",
"external",
"contains",
"sqlite3.Cursor",
false),
(3,
"cursor.execute",
"external",
"contains",
"line 1",
false),
(4,
"cursor.unsupported_method",
"unresolved",
"contains",
"",
false),
(5,
"logger.info",
"unresolved",
"contains",
"",
false)][..]),

        ("module_qualified_framework_call_requires_an_earlier_import_position",
 "models.Record.model_validate({}); from pkg import models\n",
 &[(1,
"models.Record.model_validate",
"unresolved",
"contains",
"",
false)][..]),

        ("module_qualified_framework_call_requires_an_earlier_import_position_after",
 "from pkg import models; models.Record.model_validate({})\n",
 &[(1,
"models.Record.model_validate",
"external",
"contains",
"pydantic",
false)][..]),

    ];
    let w = fixture_workspace(&[
        ("provider.py", "def get(): return 1\n"),
        ("pkg/__init__.py", ""),
        (
            "pkg/models.py",
            "from pydantic import BaseModel\nclass Record(BaseModel):\n    pass\n",
        ),
    ]);
    for (case_id, source, _) in &cases {
        w.write(format!("{case_id}.py"), source);
    }
    w.build();
    let conn = w.open();
    for (case_id, _, checks) in cases {
        let path = format!("{case_id}.py");
        for &(line, expression, expected, rule, text, no_call) in checks {
            let (status, evidence) = persisted_coverage(&conn, &path, line, expression);
            if !expected.is_empty() {
                assert_eq!(
                    status, expected,
                    "{case_id}:{line} {expression}: {evidence}"
                );
            }
            assert!(
                match rule {
                    "prefix" => evidence.starts_with(text),

                    "absent" => !evidence.starts_with(text),

                    "exact" => evidence == text,

                    "contains" => evidence.contains(text),

                    _ => unreachable!(),
                },
                "{case_id}:{line} {expression}: {evidence}"
            );
            if no_call {
                let calls: i64 = query_scalar(&conn,

                    "SELECT count(*) FROM edge_occurrences e JOIN path_dictionary p ON p.path_id=e.owner_id JOIN coverage_evidence v ON v.evidence_id=e.evidence_id WHERE p.path=?1 AND e.kind='calls' AND v.evidence=?2",

                    (&path,
 expression));
                assert_eq!(calls, 0, "{case_id}: no fabricated target for {expression}");
            }
        }
    }
}

#[test]
fn changing_import_provenance_preserves_cold_delta_equivalence() {
    let w = fixture_workspace(&[]);
    let external =
        "import requests as client\ndef run(): return client.get('https://example.test')\n";
    let unknown =
        "import unknown_package as client\ndef run(): return client.get('https://example.test')\n";
    w.write("consumer.py", external);
    w.build();
    w.write("consumer.py", unknown);
    w.delta(&["consumer.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    assert_eq!(
        persisted_coverage(&w.open(), "consumer.py", 2, "client.get").0,
        "external"
    );
    assert_eq!(
        persisted_coverage(&w.open(), "consumer.py", 2, "client.get").1,
        "call through external import unknown_package; callable target unverified"
    );
    w.write("consumer.py", external);
    w.delta(&["consumer.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    assert_eq!(
        persisted_coverage(&w.open(), "consumer.py", 2, "client.get").1,
        "call through external import requests; callable target unverified"
    );
}

#[test]
fn module_qualified_local_subclass_inherits_only_its_own_imported_framework_members() {
    let first = fixture_workspace(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/models.py",
            "from pydantic import BaseModel\nclass Record(BaseModel):\n    title: str\n",
        ),
        (
            "pkg/consumer.py",
            "from pkg import models\ndef run():\n    models.Record.model_validate({'title': 'ok'})\n",
        ),
    ]);

    let second = fixture_workspace(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/models.py",
            "from pydantic import BaseModel\nclass Record(BaseModel):\n    title: str\nRecord = object\n",
        ),
        (
            "pkg/consumer.py",
            "from pkg import models\ndef run():\n    models.Record.model_validate({'title': 'unknown'})\n",
        ),
    ]);

    let linked = fixture_workspace(&[]);
    linked.write(
        "acdd.yaml",
        &format!(
            "roots: [.]\nlinked_workspaces:\n  - name: first\n    path: '{}'\n    roots: [.]\n  - name: second\n    path: '{}'\n    roots: [.]\n",
            first.root().display(),
            second.root().display()
        ),
    );
    linked.build();
    let conn = linked.open();
    let expression = "models.Record.model_validate";
    let (status, evidence) = persisted_coverage(&conn, "[first]/pkg/consumer.py", 3, expression);
    assert_eq!(status, "external", "{evidence}");
    assert!(evidence.contains("pydantic"), "{evidence}");
    assert_eq!(
        persisted_coverage(&conn, "[second]/pkg/consumer.py", 3, expression).0,
        "unresolved"
    );
}
