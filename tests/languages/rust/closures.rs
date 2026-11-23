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
            use acdd_index::ast;
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
    cases.push(Case {
        case_id: "trait_method_boxed_iterator_propagates_item_fields",
        path: "src/trait_iter.rs",
        source: r#"
            pub struct Node { pub id: String }
            pub struct Facts { pub nodes: Vec<Node> }
            pub trait FactsLookup {
                fn values_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Facts> + 'a>;
            }
            fn scan(all: &dyn FactsLookup) {
                for node in all.values_facts().flat_map(|facts| &facts.nodes) {
                    let _ = node.id.as_str();
                }
                for facts in all.values_facts() {
                    let _ = facts.nodes.iter();
                }
            }
        "#,
        expected: &[
            ("facts.nodes.iter", 0, "external", true, "", ""),
            ("node.id.as_str", 0, "external", true, "", ""),
        ],
        iterator_sites: false,
        companion: false,
    });
    cases.push(Case {
        case_id: "wave_coverage_stdlib_serializer_display_const_and_chains",
        path: "src/wave_contract.rs",
        source: r#"
            use std::cell::RefCell;
            use std::process::Output;
            use serde::Serializer;
            use serde_json::Value;
            use toml::Value as TomlValue;
            use walkdir::WalkDir;

            const CONST_STR: &str = "a,b";
            thread_local! {
                static TLS_CELL: RefCell<String> = RefCell::new(String::new());
            }

            #[derive(Clone)]
            struct LocalDisplay(String);
            impl std::fmt::Display for LocalDisplay {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str(&self.0)
                }
            }

            fn ser_fn<S: Serializer>(serializer: S) {
                let mut state = serializer.serialize_struct("S", 1).unwrap();
                state.serialize_field("k", &1);
            }

            fn exercise(
                cond: bool,
                id: LocalDisplay,
                out: Output,
                json_opt: Option<Value>,
                toml_src: &str,
            ) {
                let _ = CONST_STR.replace(",", ".");
                let _ = id.to_string();
                let _ = id.clone();
                let _ = out.stdout.split(|&b| b == 0);
                let joined = if cond { "a" } else { "b" };
                let _ = joined.trim();
                let pairs = [("x", 1usize)];
                for &(k, _) in &pairs {
                    let _ = k.trim();
                }
                let _ = ("raw" as &str).trim();
                for n in 0..3usize {
                    let _ = n.checked_add(1);
                }
                let _ = json_opt.and_then(Value::as_str);
                TLS_CELL.with(|cell| {
                    let _ = cell.borrow().len();
                });
                let parsed_toml = toml::from_str::<TomlValue>(toml_src).ok();
                let _ = parsed_toml.and_then(|v| v.as_table()).and_then(|t| t.get("k"));
                let items = vec!["a"];
                let _ = items.iter().map(|s| (*s).to_owned());
                for entry in WalkDir::new(".").into_iter().filter_entry(|e| e.depth() == 0) {
                    if let Ok(e) = entry {
                        let _ = e.into_path().exists();
                    }
                }
            }
        "#,
        expected: &[
            ("CONST_STR.replace", 0, "external", true, "", ""),
            ("id.to_string", 0, "external", true, "", ""),
            ("id.clone", 0, "resolved", true, "", ""),
            ("out.stdout.split", 0, "external", true, "", ""),
            ("joined.trim", 0, "external", true, "", ""),
            ("k.trim", 0, "external", true, "", ""),
            ("n.checked_add", 0, "external", true, "", ""),
            ("json_opt.and_then", 0, "external", true, "", ""),
            ("TLS_CELL.with", 0, "external", true, "", ""),
            ("cell.borrow", 0, "external", true, "", ""),
            ("(*s).to_owned", 0, "external", true, "", ""),
            ("e.depth", 0, "external", true, "", ""),
            ("e.into_path", 0, "external", true, "", ""),
            ("state.serialize_field", 0, "external", true, "", ""),
        ],
        iterator_sites: false,
        companion: false,
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

#[test]
fn range_and_closure_map_fail_closed_boundaries() {
    let source = r#"
        use std::path::PathBuf;
        struct Foo;
        impl Foo { fn new() -> Self { Self } }
        struct Bar;
        impl Bar {
            fn new(_: Foo) -> Self { Self }
            fn bar_only(&self) {}
        }
        fn unknown_fn(_: Foo) -> UnknownType { panic!() }
        fn probe(opt: Option<Foo>, opt_s: Option<&str>, start: usize, end: usize, n: usize) {
            for c in 'a'..='z' {
                c.is_ascii_lowercase();
                c.checked_add(1);
            }
            for i in 0..n {
                i.checked_add(1);
            }
            for x in start..end {
                x.checked_add(1);
            }
            opt.map(Bar::new).unwrap().bar_only();
            opt_s.map(PathBuf::from).unwrap().as_path();
            opt_s.map(str::trim).unwrap().is_empty();
            opt.map(unknown_fn).unwrap().bar_only();
        }
    "#;
    let facts = acdd_index::ast::extract("src/range_closure_bounds.rs", "rust", source).unwrap();
    let graph = crate::common::link_facts(&std::collections::BTreeMap::from([(
        "src/range_closure_bounds.rs".to_owned(),
        facts,
    )]));
    let cov = |expr: &str| {
        graph
            .coverage
            .iter()
            .find(|c| c.expression == expr)
            .unwrap_or_else(|| panic!("missing coverage for {expr}: {graph:#?}"))
    };
    for (expr, expected) in [
        ("c.is_ascii_lowercase", "external"),
        ("c.checked_add", "unresolved"),
        ("i.checked_add", "external"),
        ("x.checked_add", "unresolved"),
        ("opt.map(Bar::new).unwrap().bar_only", "resolved"),
        ("opt_s.map(PathBuf::from).unwrap().as_path", "external"),
        ("opt_s.map(str::trim).unwrap().is_empty", "external"),
        ("opt.map(unknown_fn).unwrap().bar_only", "unresolved"),
    ] {
        let entry = cov(expr);
        assert_eq!(
            entry.status, expected,
            "expression {expr} expected status {expected}: {entry:#?}"
        );
    }
}
