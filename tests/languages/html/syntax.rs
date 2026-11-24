use super::*;
use crate::languages::harness::ExtractorHarness;

#[test]
fn htmx_request_identity_and_bounds() {
    type RequestExpectation = (
        &'static str,
        Option<usize>,
        Vec<(&'static str, serde_json::Value)>,
    );

    struct Case {
        case_id: &'static str,
        source: String,
        count: usize,
        references: usize,
        expected: Vec<RequestExpectation>,
        absent: &'static [&'static str],
        contexts: Option<usize>,
        max_details: Option<usize>,
        same_line: bool,
        persisted: bool,
    }
    let declarations = concat!(
        "<div hx-target='#result' hx-swap='outerHTML'>\n",
        "<button hx-get='/orders?state=open' hx-trigger='click' hx-include='#filter' hx-vals='{\"token\":\"[REDACTED:API key param]\"}'>Load</button>\n",
        "<button data-hx-post='/orders' data-hx-target='#result'>Save</button>\n",
        "<button hx-delete='https://example.invalid/orders'>Remote</button>\n",
        "<button hx-patch='{{ route }}' hx-vals='js:{token: currentToken()}'>Dynamic</button>\n",
        "<button hx-put=''>Current page</button>\n",
        "</div>\n",
    );
    let value = "x".repeat(240);
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write("page.html", declarations);
    workspace.build();
    let cases = [
        Case {
            case_id: "htmx_declarations_are_searchable_without_inventing_handler_edges",
            source: declarations.into(), count: 5, references: 1, contexts: Some(1),
            expected: vec![
                ("GET /orders?state=open", Some(2), vec![("url_status", serde_json::json!("local_unverified")), ("trigger", serde_json::json!("click")), ("include", serde_json::json!("#filter")), ("vals_key_count", serde_json::json!(1))]),
                ("POST /orders", None, vec![]),
                ("DELETE https://example.invalid/orders", None, vec![("url_status", serde_json::json!("remote"))]),
                ("PATCH {{ route }}", None, vec![("url_status", serde_json::json!("dynamic_or_nonlocal")), ("vals_status", serde_json::json!("dynamic"))]),
                ("PUT ", None, vec![("url_status", serde_json::json!("current_page"))]),
            ], absent: &[], max_details: None, same_line: false, persisted: true,
        },
        Case {
            case_id: "htmx_large_attribute_sets_use_bounded_values_per_request",
            source: (0..64).map(|index| format!("<button hx-get='/route-{index}' hx-trigger='{value}' hx-target='{value}' hx-swap='{value}' hx-include='{value}'>Go</button>\n")).collect(),
            count: 64, references: 0, expected: vec![("GET /route-63", None, vec![])],
            absent: &[], contexts: None, max_details: Some(2048), same_line: false, persisted: false,
        },
        Case {
            case_id: "htmx_same_line_requests_have_distinct_stable_ids",
            source: "<button hx-get='/same'></button><button hx-get='/same'></button><button data-hx-get='/old' hx-get='/right' hx-post='/save'></button>".into(),
            count: 4, references: 0, expected: vec![("GET /right", None, vec![]), ("POST /save", None, vec![])],
            absent: &["GET /old"], contexts: None, max_details: None, same_line: true, persisted: false,
        },
    ];
    for case in cases {
        let facts = ast::extract("page.html", "html", &case.source).unwrap();
        let repeated = ast::extract("page.html", "html", &case.source).unwrap();
        let requests: Vec<_> = facts
            .nodes
            .iter()
            .filter(|node| node.kind == "htmx_request")
            .collect();
        assert_eq!(requests.len(), case.count, "{}", case.case_id);
        assert_eq!(facts.references.len(), case.references, "{}", case.case_id);
        assert!(!facts.edges.iter().any(|edge| edge.kind == "handles"));
        assert!(!serde_json::to_string(&facts).unwrap().contains("secret"));
        for reference in &facts.references {
            assert_eq!(reference.kind, "references");
            assert_eq!(reference.expression, "template.variable.route");
            assert_eq!(reference.source, "module:page.html");
        }
        for (name, line, details) in case.expected {
            let node = requests
                .iter()
                .find(|node| node.name == name)
                .unwrap_or_else(|| panic!("{}: {name}", case.case_id));
            if let Some(line) = line {
                assert_eq!(node.line, line);
            }
            for (key, value) in details {
                assert_eq!(node.details[key], value, "{}: {name}: {key}", case.case_id);
            }
        }
        for name in case.absent {
            assert!(!requests.iter().any(|node| node.name == *name));
        }
        if let Some(count) = case.contexts {
            assert_eq!(
                facts
                    .nodes
                    .iter()
                    .filter(|node| node.kind == "htmx_context")
                    .count(),
                count
            );
        }
        if let Some(bound) = case.max_details {
            assert!(requests
                .iter()
                .all(|node| node.details.to_string().len() < bound));
        }
        let ids: std::collections::HashSet<_> = requests.iter().map(|node| &node.id).collect();
        assert_eq!(ids.len(), case.count);
        assert_eq!(
            facts.nodes.iter().map(|node| &node.id).collect::<Vec<_>>(),
            repeated
                .nodes
                .iter()
                .map(|node| &node.id)
                .collect::<Vec<_>>()
        );
        if case.same_line {
            assert!(requests.iter().all(|node| node.line == 1));
        }
        if case.persisted {
            let get = requests
                .iter()
                .find(|node| node.name == "GET /orders?state=open")
                .unwrap();
            let conn = reader::open(&workspace.db(), workspace.root()).unwrap();
            let options = QueryOptions::resolve(
                &ResponsePolicy::default(),
                Some(10),
                0,
                Some(Detail::Full),
                None,
            )
            .unwrap();
            let inspected =
                reader::inspect_paged_with_path(&conn, &get.id, None, false, &options).unwrap();
            assert_eq!(inspected["node"]["name"], "GET /orders?state=open");
            assert_eq!(inspected["node"]["details"]["vals_key_count"], 1);
            let inspect_bytes = serde_json::to_vec(&inspected).unwrap().len();
            assert!(inspect_bytes < 8192, "{inspect_bytes} bytes");
            println!("HTMX request full inspect={inspect_bytes} bytes");
            let searched = symbols::search_with_options(
                &conn,
                "orders",
                &symbols::SearchOptions {
                    kind: Some("htmx_request"),
                    path: None,
                    include_docs: false,
                    exact: false,
                    classification: None,
                    page: &options,
                },
            )
            .unwrap();
            assert_eq!(searched["nodes"]["total"], 3);
            assert_eq!(searched["nodes"]["items"][0]["path"], "page.html");
            let edges: i64 = conn
        .query_row(
            "SELECT count(*) FROM edges WHERE (SELECT id FROM nodes WHERE node_hash=src_hash)='module:page.html' AND kind='imports'",
            [],
            |row| row.get(0),
        )
        .unwrap();
            assert_eq!(edges, 0);
        }
    }
}

#[test]
fn htmx_requests_remain_complete_after_delta_and_cold_build() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write("page.html", "<button hx-get='/before'>Before</button>");
    writer::build(workspace.root(), &workspace.db(), None).unwrap();
    let mut updated = String::new();
    for index in 0..70 {
        updated.push_str(&format!("<button hx-get='/route-{index}'>Go</button>\n"));
    }
    updated.push_str(&format!(
        "<button hx-post='{}'>Large</button>",
        "x".repeat(300)
    ));
    workspace.write("page.html", &updated);
    writer::delta_with_selection(
        workspace.root(),
        &workspace.db(),
        &[std::path::PathBuf::from("page.html")],
        None,
        None,
    )
    .unwrap();
    let delta = reader::open(&workspace.db(), workspace.root()).unwrap();
    let cold = workspace.root().join("cold.sqlite");
    writer::build(workspace.root(), &cold, None).unwrap();
    let rebuilt = reader::open(&cold, workspace.root()).unwrap();
    let rows = |conn: &rusqlite::Connection| {
        conn.prepare(
            "SELECT id,name,line,details FROM nodes WHERE kind='htmx_request' ORDER BY line,id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    };
    let requests = rows(&delta);
    assert_eq!(requests, rows(&rebuilt));
    assert_eq!(requests.len(), 71);
    assert_eq!(requests.first().unwrap().1, "GET /route-0");
    assert!(requests
        .iter()
        .any(|(_, name, line, _)| name == "GET /route-69" && *line == 70));
    let options = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(10),
        0,
        Some(Detail::Compact),
        None,
    )
    .unwrap();
    let first = symbols::search_with_options(
        &delta,
        "route",
        &symbols::SearchOptions {
            kind: Some("htmx_request"),
            path: None,
            include_docs: false,
            exact: false,
            classification: None,
            page: &options,
        },
    )
    .unwrap();
    assert_eq!(first["nodes"]["total"], 70);
    assert_eq!(first["nodes"]["items"].as_array().unwrap().len(), 10);
    assert_eq!(first["nodes"]["next_offset"], 10);
    let generation = first["nodes"]["generation"].as_str().unwrap().to_owned();
    let second_options = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(10),
        10,
        Some(Detail::Compact),
        Some(generation),
    )
    .unwrap();
    let second = symbols::search_with_options(
        &delta,
        "route",
        &symbols::SearchOptions {
            kind: Some("htmx_request"),
            path: None,
            include_docs: false,
            exact: false,
            classification: None,
            page: &second_options,
        },
    )
    .unwrap();
    assert_eq!(second["nodes"]["total"], 70);
    assert_eq!(second["nodes"]["items"].as_array().unwrap().len(), 10);
    assert_ne!(
        first["nodes"]["items"][0]["id"],
        second["nodes"]["items"][0]["id"]
    );
    let full_options = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(10),
        0,
        Some(Detail::Full),
        None,
    )
    .unwrap();
    let inspected =
        reader::inspect_paged_with_path(&delta, &requests[69].0, None, false, &full_options)
            .unwrap();
    let inspect_bytes = serde_json::to_vec(&inspected).unwrap().len();
    assert!(inspect_bytes < 64 * 1024, "{inspect_bytes} bytes");
    println!("HTMX 71 requests retained; page 10/70; full inspect={inspect_bytes} bytes");
}

#[test]
fn html_diagnostics_and_whole_document_ast_search_use_html_grammar() {
    let facts = ExtractorHarness::for_lang("html")
        .at_path("index.html")
        .extract("<div attr=\"");
    assert!(!facts.errors.is_empty());
    assert_eq!(facts.nodes.len(), 1);
    let ast_search = |src: &str, pat: &str| {
        ast::search::PreparedAstMatcher::new("html", pat).and_then(|mut m| {
            m.search_page(
                src,
                "index.html",
                0,
                10,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            )
            .map(|p| p.items)
        })
    };
    let matches = ast_search("<p>Hello</p>", "<p>Hello</p>").unwrap();
    assert_eq!(matches.len(), 1);
    let nested = ast_search(
        "<html><body><p>Hello</p><p>Hello</p></body></html>",
        "<p>Hello</p>",
    )
    .unwrap();
    assert_eq!(nested.len(), 2);
    assert!(ast_search("<p>Hello</p>", "<div attr=\"").is_err());
}

#[test]
fn html_script_paths_are_preserved_without_requiring_javascript_grammar() {
    let facts = ExtractorHarness::for_lang("html")
        .at_path("index.html")
        .extract("<script src='app.js'></script><link rel='modulepreload' href='./chunk.mjs'>");
    assert_eq!(facts.references.len(), 2);
    assert!(facts
        .references
        .iter()
        .all(|reference| reference.kind == "imports"));
    assert!(facts
        .references
        .iter()
        .any(|reference| reference.module.as_deref() == Some("./app.js")));
}
