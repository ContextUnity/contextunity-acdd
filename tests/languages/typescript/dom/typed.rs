use super::*;

const TYPED: &str = "function run(root: Element, event: Event, doc: Document) { root.querySelectorAll('.item'); event.stopPropagation(); doc.querySelectorAll('.item'); root.unknownMember(); doc.closest('.item'); }";

const UNTYPED: &str =
    "function run(root, event) { root.querySelectorAll('.item'); event.stopPropagation(); }";

const LOCAL: &str = "class Element { querySelectorAll() {} } function run(root: Element) { root.querySelectorAll(); }";

const NESTED: &str = r#"
    function root(): Element { return document.getElementById('root')!; }
    function run(element: Element, doc: Document, win: Window) {
        element.parentElement?.querySelector('.item');
        element.ownerDocument?.querySelector('.item');
        doc.documentElement?.getAttribute('id');
        win.document?.getElementById('root');
        const fromReturn = root();
        fromReturn.parentElement?.matches('.item');
        element.parentElement?.invented();
        unknown.parentElement?.querySelector('.item');
    }
"#;

type CoverageExpectation = (&'static str, &'static str);
type ReceiverCase = (&'static str, &'static str, &'static [CoverageExpectation]);

const CASES: &[ReceiverCase] = &[
    (
        "dom.ts",
        TYPED,
        &[
            ("root.querySelectorAll", "external"),
            ("event.stopPropagation", "external"),
            ("doc.querySelectorAll", "external"),
            ("root.unknownMember", "unresolved"),
            ("doc.closest", "unresolved"),
        ],
    ),
    (
        "untyped.ts",
        UNTYPED,
        &[
            ("root.querySelectorAll", "unresolved"),
            ("event.stopPropagation", "unresolved"),
        ],
    ),
    ("local.ts", LOCAL, &[("root.querySelectorAll", "resolved")]),
    (
        "nested.ts",
        NESTED,
        &[
            ("element.parentElement?.querySelector", "external"),
            ("element.ownerDocument?.querySelector", "external"),
            ("doc.documentElement?.getAttribute", "external"),
            ("win.document?.getElementById", "external"),
            ("fromReturn.parentElement?.matches", "external"),
            ("element.parentElement?.invented", "unresolved"),
            ("unknown.parentElement?.querySelector", "unresolved"),
        ],
    ),
];

#[test]
fn typed_receiver_linker_contract() {
    for &(path, source, expectations) in CASES {
        let graph = linked(path, "typescript", source);
        assert!(
            graph.facts.errors.is_empty(),
            "{path}: {:?}",
            graph.facts.errors
        );
        for &(expression, expected) in expectations {
            let row = graph
                .coverage
                .iter()
                .find(|row| row.expression == expression)
                .unwrap_or_else(|| panic!("{path}:{expression}: {graph:#?}"));
            assert_eq!(row.status, expected, "{path}:{expression}: {row:#?}");
            if expected == "external" {
                assert!(row.evidence.contains("builtin:web_api"), "{row:#?}");
            }
            if expected == "resolved" {
                assert!(
                    graph.edges.iter().any(|edge| edge.kind == "calls"
                        && edge.evidence == expression
                        && graph.facts.nodes.iter().any(|node| node.id == edge.dst
                            && node.kind == "method"
                            && node.name == "querySelectorAll")),
                    "{graph:#?}"
                );
            }
        }
    }
}

#[test]
fn persisted_typed_receiver_contract() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    for &(path, source, _) in CASES {
        workspace.write(path, source);
    }
    workspace.build();
    let conn = workspace.open();
    for &(path, _, expectations) in CASES {
        for &(expression, expected) in expectations {
            let row = persisted_coverage(&conn, path, None, expression);
            assert_eq!(row.0, expected, "{path}:{expression}: {row:?}");
            if expected == "external" {
                assert!(row.1.contains("builtin:web_api"), "{row:?}");
            }
        }
    }
    acdd_index::db::commitments::verify(&conn).unwrap();
}
