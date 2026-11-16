use super::*;
#[test]
fn runtime_stub_and_child_precedence() {
    let cases = [
        (
            "paired_runtime_and_stub_choose_runtime_for_each_import_form",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("pkg/child.py", "def value(): return 1\n"),
                ("pkg/child.pyi", "def value() -> int: ...\n"),
            ][..],
            "from pkg import child\ndef consume(): return child.value()\n",
            "child",
            "resolved",
            Some(&["pkg/__init__.py", "pkg/child.py"] as &[&str]),
            Some(&["pkg", "pkg.child"] as &[&str]),
            Some(&["pkg.child.value"] as &[&str]),
            Some(&["pkg/child.py"] as &[&str]),
            true,
        ),
        (
            "paired_runtime_and_stub_choose_runtime_for_each_import_form_aliased",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("pkg/child.py", "def value(): return 1\n"),
                ("pkg/child.pyi", "def value() -> int: ...\n"),
            ][..],
            "import pkg.child as child\ndef consume(): return child.value()\n",
            "pkg.child",
            "resolved",
            Some(&["pkg/child.py"] as &[&str]),
            None,
            Some(&["pkg.child.value"] as &[&str]),
            Some(&["pkg/child.py"] as &[&str]),
            false,
        ),
        (
            "child_module_call_uses_stub_only_member",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("pkg/child.py", "def __getattr__(name): return None\n"),
                ("pkg/child.pyi", "class ContextUnit: ...\n"),
            ][..],
            "from pkg import child\ndef consume(): return child.ContextUnit()\n",
            "child",
            "resolved",
            Some(&["pkg/__init__.py", "pkg/child.py"] as &[&str]),
            None,
            None,
            Some(&["pkg/child.pyi"] as &[&str]),
            false,
        ),
        (
            "stub_only_module_remains_navigable",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("pkg/child.pyi", "def value() -> int: ...\n"),
            ][..],
            "from pkg import child\ndef consume(): return child.value()\n",
            "child",
            "resolved",
            Some(&["pkg/__init__.py", "pkg/child.pyi"] as &[&str]),
            None,
            Some(&["pkg.child.value"] as &[&str]),
            None,
            false,
        ),
        (
            "child_module_candidates_must_be_unique_and_related_multiple",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("pkg/child.py", "def value(): return 1\n"),
                ("src/pkg/child.py", "def value(): return 2\n"),
            ][..],
            "from pkg import child\ndef consume(): return child.value()\n",
            "child",
            "ambiguous",
            Some(&["pkg/__init__.py"] as &[&str]),
            None,
            Some(&[] as &[&str]),
            None,
            false,
        ),
        (
            "child_module_candidates_must_be_unique_and_related_unrelated",
            &[
                ("pkg/__init__.py", "# package\n"),
                ("other/child.py", "def value(): return 1\n"),
            ][..],
            "from pkg import child\ndef consume(): return child.value()\n",
            "child",
            "unresolved",
            None,
            Some(&["pkg"] as &[&str]),
            Some(&[] as &[&str]),
            None,
            false,
        ),
        (
            "package_symbol_takes_precedence_over_same_named_child_module",
            &[
                ("pkg/__init__.py", "def child(): return 1\n"),
                ("pkg/child.py", "def value(): return 2\n"),
            ][..],
            "from pkg import child\ndef consume(): return child()\n",
            "child",
            "resolved",
            Some(&["pkg/__init__.py", "pkg/__init__.py"] as &[&str]),
            Some(&["pkg", "pkg.child"] as &[&str]),
            Some(&["pkg.child"] as &[&str]),
            None,
            false,
        ),
    ];
    let w = fixture_workspace(&[]);
    for (index, (_, files, source, ..)) in cases.iter().enumerate() {
        let package = format!("package_{index}");
        for (path, source) in *files {
            w.write(path.replace("pkg", &package), source);
        }
        let consumer = if index == 0 {
            "consumer.py".to_owned()
        } else {
            format!("consumer_{index}.py")
        };
        w.write(&consumer, &source.replace("pkg", &package));
    }
    w.build();
    let conn = w.open();
    for (
        index,
        (case_id, _, _, expression, expected, imports, import_names, calls, call_paths, check_stub),
    ) in cases.into_iter().enumerate()
    {
        let package = format!("package_{index}");
        let path = if index == 0 {
            "consumer.py".to_owned()
        } else {
            format!("consumer_{index}.py")
        };
        assert_eq!(
            persisted_coverage(&conn, &path, 1, &expression.replace("pkg", &package)).0,
            expected,
            "{case_id}"
        );
        for (kind, field, expected) in [
            ("imports", "p.path", imports),
            ("imports", "dst.qualname", import_names),
            ("calls", "dst.qualname", calls),
            ("calls", "p.path", call_paths),
        ] {
            if let Some(expected) = expected {
                let sql = format!("SELECT {field} FROM edges e JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash JOIN path_dictionary p ON p.path_id=dst.path_id JOIN path_dictionary owner ON owner.path_id=src.path_id WHERE owner.path=?1 AND e.kind=?2 ORDER BY {field}");
                let actual = if index == 0 {
                    match (kind, field) {
                        ("imports", "p.path") => import_target_paths(&conn),
                        ("imports", "dst.qualname") => import_targets(&conn),
                        ("calls", "dst.qualname") => call_targets(&conn),
                        ("calls", "p.path") => call_target_paths(&conn),
                        _ => unreachable!(),
                    }
                } else {
                    query_strings(&conn, &sql, (&path, kind))
                };
                let expected: Vec<_> = expected
                    .iter()
                    .map(|v| v.replace("pkg", &package))
                    .collect();
                assert_eq!(actual, expected, "{case_id}: {kind} {field}");
            }
        }
        if check_stub {
            let stub: i64 = query_scalar(&conn,
 "SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1",
 [format!("{package}/child.pyi")]);
            assert!(stub > 0, "{case_id}");
        }
    }
}

#[test]
fn paired_stub_supplies_declaration_missing_from_generated_runtime() {
    let w = fixture_workspace(&[
        ("pkg/__init__.py", "# package\n"),
        ("pkg/child.py", "def __getattr__(name): return None\n"),
        (
            "pkg/child.pyi",
            "class ContextUnit:\n    def value(self) -> int: ...\n",
        ),
        (
            "consumer.py",
            "from pkg.child import ContextUnit\ndef consume(): return ContextUnit()\n",
        ),
    ]);
    w.build();
    let conn = w.open();
    assert_eq!(status(&conn, "ContextUnit"), "resolved");
    assert_eq!(
        import_target_paths(&conn),
        ["pkg/child.py", "pkg/child.pyi"]
    );
    assert_eq!(call_target_paths(&conn), ["pkg/child.pyi"]);
    let evidence: String = query_scalar(
        &conn,
        "SELECT (SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='consumer.py' AND line=1 AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='ContextUnit'",
        [],
    );
    assert!(evidence.contains("type stub"), "{evidence}");
    drop(conn);
    w.write("pkg/child.py", "def __getattr__(name): return name\n");
    w.delta(&["pkg/child.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    w.write("pkg/child.py", "def __getattr__(name): return None\n");
    w.delta(&["pkg/child.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
}

#[test]
fn child_edit_and_restore_match_cold_graph() {
    let w = fixture_workspace(&[
        ("pkg/__init__.py", "# package\n"),
        ("pkg/child.py", "def value(): return 1\n"),
        ("pkg/child.pyi", "def value() -> int: ...\n"),
        (
            "consumer.py",
            "from pkg import child\ndef consume(): return child.value()\n",
        ),
    ]);
    w.build();
    w.write("pkg/child.py", "def value(arg=1): return arg\n");
    w.delta(&["pkg/child.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    w.write("pkg/child.py", "def value(): return 1\n");
    w.delta(&["pkg/child.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
}
