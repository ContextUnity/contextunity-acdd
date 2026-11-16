use crate::languages::harness::ExtractorHarness;

struct Case<'a> {
    case_id: &'a str,
    path: &'a str,
    source: &'a str,
    expected: &'a [(&'a str, usize, &'a str, bool, &'a str, &'a str)],
    iterator_sites: bool,
    companion: bool,
}

#[test]
fn callback_receiver_authority() {
    let mut cases = vec![Case {
        case_id: "iterator_callbacks_inherit_item_receivers",
        path: "src/probe_iter.rs",
        source: r#"
        struct Item; impl Item { fn finish(&self) {} }
        fn f(items: Vec<Item>) {
            items.iter().map(|entry| entry.finish());
            items.iter().filter(|entry| { entry.finish(); true });
        }
    "#,
        expected: &[],
        iterator_sites: true,
        companion: false,
    }];
    let unrelated_callback = r#"
        fn unrelated<F: Fn(i32)>(callback: F) { callback(1); }
        fn f() { unrelated(|row| row.get(0)); }
    "#;
    let parser_homonym = r#"
        use pulldown_cmark::Parser;
        fn f(parser: &mut Parser) {
            let tree = parser.parse("x", None).unwrap();
            let root = tree.root_node(); root.kind();
        }
    "#;
    let local_query_map = r#"
        struct LocalItem; impl LocalItem { fn get(&self, _: i32) {} }
        struct Local; impl Local { fn query_map<F: Fn(LocalItem)>(&self, f: F) { f(LocalItem {}); } }
        fn f(val: Local) { val.query_map(|entry| entry.get(0)); }
    "#;
    let local_connection = r#"
        struct LocalItem; impl LocalItem { fn get(&self, _: i32) {} }
        struct Connection;
        impl Connection { fn query_map<F: Fn(LocalItem)>(&self, f: F) { f(LocalItem {}); } }
        fn f(db: Connection) { db.query_map(|entry| entry.get(0)); }
    "#;
    let negative_rows = [
        (
            "unrelated-callback",
            unrelated_callback,
            "row.get",
            None,
        ),
        (
            "parser-homonym",
            parser_homonym,
            "root.kind",
            None,
        ),
        (
            "local-query-map",
            local_query_map,
            "entry.get",
            None,
        ),
        (
            "local-connection",
            local_connection,
            "entry.get",
            Some("rusqlite::Row"),
        ),
        (
            "nested-local",
            "use rusqlite::Result;\nstruct LocalItem; impl LocalItem { fn get(&self, _: i32) {} }\nfn f() { struct Connection; impl Connection { fn query_map<F: Fn(LocalItem)>(&self, f: F) { f(LocalItem {}); } } fn run(c: Connection) { c.query_map(|entry| entry.get(0)); } }",
            "entry.get",
            Some("rusqlite::Row"),
        ),
        (
            "qualified-local",
            "use rusqlite::Result;\nstruct LocalItem; impl LocalItem { fn get(&self, _: i32) {} }\nmod other { pub struct Connection; impl Connection { pub fn query_map<F: Fn(super::LocalItem)>(&self, f: F) { f(super::LocalItem {}); } } }\nfn f(c: other::Connection) { c.query_map(|entry| entry.get(0)); }",
            "entry.get",
            Some("rusqlite::Row"),
        ),
    ];
    let negative_expectations: Vec<_> = negative_rows
        .iter()
        .map(|(_, _, expression, forbidden)| {
            [(
                *expression,
                0,
                "external",
                false,
                "",
                forbidden.unwrap_or(""),
            )]
        })
        .collect();
    for ((label, source, _, _), expected) in negative_rows.iter().zip(&negative_expectations) {
        cases.push(Case {
            case_id: label,
            path: "probe.rs",
            source,
            expected,
            iterator_sites: false,
            companion: false,
        });
    }
    let external_rows = [
        (
            "rusqlite",
            r#"
                use rusqlite::Connection;
                fn f(conn: &Connection) {
                    let mut statement = conn.prepare("SELECT 1").unwrap();
                    statement.execute([]);
                    statement.query_map([], |entry| entry.get(0));
                }
            "#,
            &[
                ("conn.prepare", "external"),
                ("statement.execute", "external"),
                ("entry.get", "external"),
            ][..],
        ),
        (
            "tree-sitter",
            r#"
                use tree_sitter::Parser;
                fn f(parser: &mut Parser) {
                    let tree = parser.parse("x", None).unwrap();
                    let root = tree.root_node(); root.kind();
                    let child = root.child_by_field_name("body").unwrap(); child.kind();
                }
            "#,
            &[
                ("tree.root_node", "external"),
                ("root.kind", "external"),
                ("child.kind", "external"),
            ][..],
        ),
    ];
    let external_expectations: Vec<Vec<_>> = external_rows
        .iter()
        .map(|(_, _, expected)| {
            expected
                .iter()
                .map(|(expression, status)| (*expression, 0, *status, true, "", ""))
                .collect()
        })
        .collect();
    for ((label, source, _), expected) in external_rows.iter().zip(&external_expectations) {
        cases.push(Case {
            case_id: label,
            path: "probe.rs",
            source,
            expected,
            iterator_sites: false,
            companion: false,
        });
    }
    for case in cases {
        let mut sources = vec![(case.path, case.source)];
        if case.companion {
            sources.push((
                "src/engine/ast.rs",
                "pub fn extract(_path: &str, _lang: &str, _code: &str) {}",
            ));
        }
        let catalog = ExtractorHarness::linked_with_root("rust", &sources, None);
        let facts = &catalog.facts[case.path];
        assert!(
            facts.errors.is_empty(),
            "{}: {:?}",
            case.case_id,
            facts.errors
        );
        let graph = &catalog.graph;
        for &(expression, ordinal, status, equal, required, forbidden) in case.expected {
            let rows: Vec<_> = graph
                .coverage
                .iter()
                .filter(|row| {
                    row.expression == expression
                        || (expression == "row.get" && row.expression.starts_with(expression))
                })
                .collect();
            let row = rows
                .get(ordinal)
                .unwrap_or_else(|| panic!("{}: missing {expression}[{ordinal}]", case.case_id));
            if !status.is_empty() {
                assert_eq!(row.status == status, equal, "{}: {row:#?}", case.case_id);
            }
            assert!(
                row.evidence.contains(required),
                "{}: required {required}: {row:#?}",
                case.case_id
            );
            if !forbidden.is_empty() {
                assert!(
                    !row.evidence.contains(forbidden),
                    "{}: forbidden {forbidden}: {row:#?}",
                    case.case_id
                );
            }
        }
        // Check extraction independently of the linker rejecting callback homonyms.
        let local_receiver = match case.case_id {
            "local-query-map" => Some(("Local", "val")),
            "local-connection" => Some(("Connection", "db")),
            _ => None,
        };
        if let Some((receiver, parameter)) = local_receiver {
            let method = facts
                .nodes
                .iter()
                .find(|node| node.name == "query_map")
                .unwrap();
            let caller = facts.nodes.iter().find(|node| node.name == "f").unwrap();
            assert_eq!(method.kind, "method");
            let method_details = crate::common::assertions::projected_node_details(facts, method);
            assert_eq!(
                method_details["receiver_type"], receiver,
                "{}",
                case.case_id
            );
            let caller_details = crate::common::assertions::projected_node_details(facts, caller);
            assert_eq!(
                caller_details["param_types"][parameter], receiver,
                "{}",
                case.case_id
            );
        }
        if case.iterator_sites {
            let target = &facts
                .nodes
                .iter()
                .find(|node| node.kind == "method" && node.qualname.ends_with(".Item.finish"))
                .unwrap()
                .id;
            let mut references: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.expression == "entry.finish")
                .map(|r| (r.line, r.column, r.kind.as_str()))
                .collect();
            references.sort_unstable();
            assert_eq!(references, [(4, 37, "calls"), (5, 42, "calls")]);
            let mut coverage: Vec<_> = graph
                .coverage
                .iter()
                .filter(|r| r.expression == "entry.finish")
                .map(|r| (r.path.as_str(), r.line, r.status.as_str()))
                .collect();
            coverage.sort_unstable();
            assert_eq!(
                coverage,
                [(case.path, 4, "resolved"), (case.path, 5, "resolved")]
            );
            let mut calls: Vec<_> = graph
                .edges
                .iter()
                .filter(|e| e.kind == "calls" && e.evidence == "entry.finish")
                .map(|e| (e.path.as_str(), e.line, e.dst.as_str()))
                .collect();
            calls.sort_unstable();
            assert_eq!(
                calls,
                [
                    (case.path, 4, target.as_str()),
                    (case.path, 5, target.as_str())
                ]
            );
        }
    }
}

#[test]
fn tuple_and_lexical_receiver_scope() {
    let tuple_rows = [
        (
            "src/probe_tup_ret.rs",
            r#"
                struct A; impl A { fn finish(&self) {} }
                struct B; impl B { fn close(&self) {} }
                fn make() -> (A, B) { (A, B) }
                fn f() { let (a, b) = make(); a.finish(); b.close(); }
            "#,
            &[
                ("a.finish", "resolved", None),
                ("b.close", "resolved", None),
            ][..],
        ),
        (
            "src/probe_tup_rest.rs",
            r#"
                struct A;
                struct B; impl B { fn finish(&self) {} }
                struct C; impl C { fn finish(&self) {} }
                fn f() { let (.., last) = (A {}, B {}, C {}); last.finish(); }
            "#,
            &[(
                "last.finish",
                "resolved",
                Some("src.probe_tup_rest.C.finish"),
            )][..],
        ),
    ];
    let tuple_expectations: Vec<Vec<_>> = tuple_rows
        .iter()
        .map(|(_, _, expected)| {
            expected
                .iter()
                .map(|(expression, status, evidence)| {
                    (*expression, 0, *status, true, evidence.unwrap_or(""), "")
                })
                .collect()
        })
        .collect();
    let mut cases = Vec::new();
    for ((path, source, _), expected) in tuple_rows.iter().zip(&tuple_expectations) {
        cases.push(Case {
            case_id: "tuple_payload_patterns_keep_exact_receivers",
            path,
            source,
            expected,
            iterator_sites: false,
            companion: false,
        });
    }
    cases.push(Case {
        case_id: "scoped_tuple_and_loop_bindings_keep_their_own_receivers/while-let",
        path: "src/probe_while_let.rs",
        source: r#"
        struct Before; impl Before { fn finish(&self) {} }
        struct After; impl After { fn finish(&self) {} }
        fn f(option: Option<After>) {
            let entry = Before {};
            while let Some(entry) = option {
                entry.finish();
                break;
            }
            entry.finish();
        }
    "#,
        expected: &[
            (
                "entry.finish",
                0,
                "",
                true,
                "src.probe_while_let.After.finish",
                "",
            ),
            (
                "entry.finish",
                1,
                "",
                true,
                "src.probe_while_let.Before.finish",
                "",
            ),
        ],
        iterator_sites: false,
        companion: false,
    });
    cases.push(Case {
        case_id: "scoped_tuple_and_loop_bindings_keep_their_own_receivers",
        path: "src/probe_tup_shadow.rs",
        source: r#"
        struct A; impl A { fn finish(&self) {} }
        struct B; impl B { fn finish(&self) {} }
        fn make() -> (A, A) { (A {}, A {}) }
        fn f() {
            let make = || (B {}, B {});
            let (entry, _) = make();
            entry.finish();
        }
    "#,
        expected: &[(
            "entry.finish",
            0,
            "",
            true,
            "",
            "src.probe_tup_shadow.A.finish",
        )],
        iterator_sites: false,
        companion: false,
    });
    cases.push(Case {
        case_id: "scoped_tuple_and_loop_bindings_keep_their_own_receivers",
        path: "src/probe_nested_fn.rs",
        source: r#"
        struct A; impl A { fn finish(&self) {} }
        struct B; impl B { fn finish(&self) {} }
        fn make() -> (A, A) { (A {}, A {}) }
        fn f() {
            fn make() -> (B, B) { (B {}, B {}) }
            let (entry, _) = make();
            entry.finish();
        }
    "#,
        expected: &[(
            "entry.finish",
            0,
            "",
            true,
            "src.probe_nested_fn.B.finish",
            "src.probe_nested_fn.A.finish",
        )],
        iterator_sites: false,
        companion: false,
    });
    cases.push(Case {
        case_id: "tuple_destructuring_carries_real_external_receivers",
        path: "src/seam_destructuring.rs",
        source: r#"
        use tempfile::NamedTempFile;
        use walkdir::WalkDir;
        use rusqlite::Connection;
        fn run_seam(conn: &Connection) {
            let (file, code) = (NamedTempFile::new().unwrap(), 42);
            file.path();
            let walker = WalkDir::new(".").into_iter();
            let mut stmt = conn.prepare("SELECT 1").unwrap();
            let _ = stmt.query_map([], |row| {
                row.get::<_, i64>(0)
            });
        }
        fn run_match_and_if_let() {
            let conn_res = Connection::open_in_memory();
            let conn = match conn_res {
                Ok(c) => c,
                Err(_) => return,
            };
            if let Ok(mut stmt) = conn.prepare("SELECT 1") {
                stmt.execute([]).unwrap();
            }
        }
        fn run_root_crate_import() {
            use contextunity_acdd::engine::ast;
            let _ = ast::extract("test.rs", "rust", "fn main() {}");
        }
    "#,
        expected: &[
            ("file.path", 0, "external", true, "", ""),
            ("row.get", 0, "external", true, "", ""),
            ("stmt.execute", 0, "external", true, "", ""),
            ("ast::extract", 0, "external", true, "", ""),
        ],
        iterator_sites: false,
        companion: true,
    });
    for case in cases {
        let mut sources = vec![(case.path, case.source)];
        if case.companion {
            sources.push((
                "src/engine/ast.rs",
                "pub fn extract(_path: &str, _lang: &str, _code: &str) {}",
            ));
        }
        let catalog = ExtractorHarness::linked_with_root("rust", &sources, None);
        let facts = &catalog.facts[case.path];
        assert!(
            facts.errors.is_empty(),
            "{}: {:?}",
            case.case_id,
            facts.errors
        );
        let graph = &catalog.graph;
        for &(expression, ordinal, status, equal, required, forbidden) in case.expected {
            let rows: Vec<_> = graph
                .coverage
                .iter()
                .filter(|row| {
                    row.expression == expression
                        || (expression == "row.get" && row.expression.starts_with(expression))
                })
                .collect();
            let row = rows
                .get(ordinal)
                .unwrap_or_else(|| panic!("{}: missing {expression}[{ordinal}]", case.case_id));
            if !status.is_empty() {
                assert_eq!(row.status == status, equal, "{}: {row:#?}", case.case_id);
            }
            assert!(
                row.evidence.contains(required),
                "{}: required {required}: {row:#?}",
                case.case_id
            );
            if !forbidden.is_empty() {
                assert!(
                    !row.evidence.contains(forbidden),
                    "{}: forbidden {forbidden}: {row:#?}",
                    case.case_id
                );
            }
        }
        if case.case_id == "scoped_tuple_and_loop_bindings_keep_their_own_receivers/while-let" {
            assert_eq!(
                graph
                    .coverage
                    .iter()
                    .filter(|r| r.expression == "entry.finish")
                    .count(),
                2
            );
        }
    }
}
