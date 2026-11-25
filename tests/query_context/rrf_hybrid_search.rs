use super::*;
use serde_json::json;

fn rrf_corpus() -> &'static crate::common::sqlite::PersistedSnapshot {
    static SNAPSHOT: std::sync::OnceLock<crate::common::sqlite::PersistedSnapshot> =
        std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        let workspace = Workspace::new();
        workspace.write(
            "pkg/hub.py",
            "def dispatch_hub(item: int) -> str:\n    return str(item)\n",
        );
        workspace.write(
            "pkg/leaf.py",
            "def dispatch_leaf(item: int) -> str:\n    return str(item)\n",
        );
        workspace.write(
            "pkg/isolated.py",
            "def dispatch_isolated(item: int) -> str:\n    return str(item)\n",
        );
        workspace.write(
            "pkg/callers.py",
            concat!(
                "from pkg.hub import dispatch_hub\n",
                "from pkg.leaf import dispatch_leaf\n",
                "def c1(): return dispatch_hub(1)\n",
                "def c2(): return dispatch_hub(2)\n",
                "def c3(): return dispatch_hub(3)\n",
                "def c4(): return dispatch_hub(4)\n",
                "def c5(): return dispatch_leaf(5)\n",
            ),
        );
        workspace.write(
            "pkg/noise.py",
            concat!(
                "def hot_unrelated():\n",
                "    \"\"\"dispatch helper for queue worker\"\"\"\n",
                "    return 1\n",
                "def n1(): return hot_unrelated()\n",
                "def n2(): return hot_unrelated()\n",
                "def n3(): return hot_unrelated()\n",
                "def n4(): return hot_unrelated()\n",
                "def n5(): return hot_unrelated()\n",
            ),
        );
        workspace.write(
            "pkg/natural.py",
            concat!(
                "def queue_worker_all():\n",
                "    \"\"\"dispatch queue worker pipeline\"\"\"\n",
                "    return 1\n",
                "def queue_only():\n",
                "    \"\"\"queue pipeline\"\"\"\n",
                "    return 2\n",
            ),
        );
        workspace.write(
            "pkg/dispatch_mod_only.py",
            "def quiet_fn():\n    return 0\n",
        );
        workspace.build();
        crate::common::sqlite::PersistedSnapshot::from_connection(&workspace.open())
    })
}

#[test]
fn rrf_multi_signal_ranking_paging_and_pattern_contract() {
    let conn = rrf_corpus().open();

    // 1. Continuous non-containment graph-degree RRF breaks ties among same-tier prefix matches:
    //    dispatch_hub (4 callers) > dispatch_leaf (1 caller) > dispatch_isolated (0 callers) > indexed_text noise.
    let mut full = options(10);
    full.detail = Detail::Full;
    let ranked = symbols::search_with_options(
        &conn,
        "dispatch",
        &symbols::SearchOptions {
            kind: Some("function"),
            path: Some("pkg"),
            include_docs: false,
            exact: false,
            classification: None,
            page: &full,
        },
    )
    .unwrap();
    assert_eq!(ranked["pattern"], "dispatch");
    let items = ranked["nodes"]["items"].as_array().unwrap();
    let names: Vec<&str> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert!(
        names.len() >= 4,
        "expected prefix and indexed_text hits: {names:?}"
    );
    assert_eq!(
        &names[..3],
        &["dispatch_hub", "dispatch_leaf", "dispatch_isolated"],
        "higher non-containment degree must outrank lower degree and isolated nodes: {names:?}"
    );
    for item in items {
        assert!(
            item.get("rrf_score").is_none(),
            "internal rrf_score must not leak into JSON items: {item}"
        );
    }
    assert_eq!(items[0]["match_reason"], "name_prefix");
    let hot_pos = names
        .iter()
        .position(|&n| n == "hot_unrelated")
        .expect("indexed_text hit must be present");
    assert!(
        hot_pos >= 3,
        "structural prefix hits must outrank high-degree indexed_text hit: {names:?}"
    );
    assert_eq!(items[hot_pos]["match_reason"], "indexed_text");

    // 2. Generation-bound pagination matches single-page RRF order deterministically.
    let mut paged = options(1);
    paged.detail = Detail::Full;
    let mut paged_names = Vec::new();
    let mut seen_generation = None;
    loop {
        let res = symbols::search_with_options(
            &conn,
            "dispatch",
            &symbols::SearchOptions {
                kind: Some("function"),
                path: Some("pkg"),
                include_docs: false,
                exact: false,
                classification: None,
                page: &paged,
            },
        )
        .unwrap();
        assert_eq!(res["pattern"], "dispatch");
        let page = &res["nodes"];
        assert_eq!(page["total"], ranked["nodes"]["total"]);
        let gen = page["generation"].as_str().unwrap().to_owned();
        if let Some(prev) = &seen_generation {
            assert_eq!(prev, &gen);
        } else {
            seen_generation = Some(gen);
        }
        paged_names.push(page["items"][0]["name"].as_str().unwrap().to_owned());
        if !continue_page(&mut paged, page) {
            break;
        }
    }
    let expected_names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(paged_names, expected_names);

    // 3. Natural multi-term query ranks all-terms AND matches ahead of partial OR matches.
    let natural = symbols::search_with_options(
        &conn,
        "dispatch queue worker",
        &symbols::SearchOptions {
            kind: Some("function"),
            path: Some("pkg/natural.py"),
            include_docs: false,
            exact: false,
            classification: None,
            page: &options(10),
        },
    )
    .unwrap();
    assert_eq!(natural["pattern"], "dispatch queue worker");
    assert_eq!(natural["nodes"]["items"][0]["name"], "queue_worker_all");

    // 4. Pattern metadata is populated consistently across every search branch.
    let branch_cases = [
        ("  dispatch_hub  ", "dispatch_hub", false),
        ("  dispatch_hub  ", "dispatch_hub", true),
        ("  dispatch*  ", "dispatch*", false),
        ("  *dispatch*  ", "*dispatch*", false),
        ("  returns:str  ", "returns:str", false),
    ];
    for (raw_pattern, expected_pattern, exact) in branch_cases {
        let res = symbols::search_with_options(
            &conn,
            raw_pattern,
            &symbols::SearchOptions {
                kind: None,
                path: Some("pkg"),
                include_docs: false,
                exact,
                classification: None,
                page: &options(10),
            },
        )
        .unwrap();
        assert_eq!(
            res["pattern"], expected_pattern,
            "pattern metadata mismatch for raw={raw_pattern:?} exact={exact}: {res}"
        );
        assert!(
            res["nodes"]["total"].as_u64().unwrap_or(0) >= 1,
            "expected hits for raw={raw_pattern:?}: {res}"
        );
    }
}

#[test]
fn module_indexed_text_and_snippetless_hits_are_deprioritized() {
    use crate::common::mcp_harness::McpTestHarness;

    let ws = Workspace::new();
    let pattern = "quartztoken azuretoken";
    // The module qualifies for the AND bonus and graph degree, but its source
    // contains neither token. The concrete hit matches only one term.
    ws.write(
        "pkg/quartztoken_azuretoken_dir/holder.py",
        "# module without token\n",
    );
    ws.write(
        "pkg/consumer.py",
        "from pkg.quartztoken_azuretoken_dir import holder\n",
    );
    ws.write(
        "pkg/concrete.py",
        "def concrete_consumer():\n    \"\"\"quartztoken processor\"\"\"\n    return 2\n",
    );
    ws.build();
    let mut client = McpTestHarness::new(&ws).unwrap();
    let mut search = |limit, offset, generation| {
        client
            .payload(
                "code_map_search",
                json!({"pattern":pattern,"path":"pkg","detail":"full",
                       "limit":limit,"offset":offset,"generation":generation,"include_pulse":false}),
            )
            .unwrap()
    };
    let full = search(10, 0, Value::Null);
    assert_eq!(full["pattern"], pattern);
    let nav_items = full["nodes"]["items"].as_array().unwrap();
    let concrete = nav_items
        .iter()
        .position(|item| item["name"] == "concrete_consumer")
        .unwrap();
    assert!(
        nav_items[concrete]["match_snippet"].as_str().is_some(),
        "concrete hit must carry resolved match_snippet: {full}"
    );
    let module = nav_items
        .iter()
        .position(|item| {
            item["kind"] == "module" && item["path"] == "pkg/quartztoken_azuretoken_dir/holder.py"
        })
        .unwrap_or_else(|| panic!("expected the indexed-text module hit: {full}"));
    assert!(
        concrete < module,
        "concrete hit must outrank the module: {full}"
    );
    assert!(
        nav_items[module].get("match_snippet").is_none(),
        "module hit has no matching source line snippet: {full}"
    );
    let mut offset = 0;
    let mut generation = Value::Null;
    for expected in nav_items {
        let page = search(1, offset, generation);
        assert_eq!(page["nodes"]["items"][0]["id"], expected["id"], "{page}");
        assert_eq!(page["nodes"]["total"], full["nodes"]["total"]);
        offset = page["nodes"]["next_offset"].as_u64().unwrap_or_default();
        generation = page["nodes"]["generation"].clone();
    }
    assert_eq!(offset, 0, "last page must terminate");
    client.finish().unwrap();
}

#[test]
fn exact_search_retains_the_matching_symbol_without_reading_graph_tables() {
    use crate::common::mcp_harness::McpTestHarness;
    use contextunity_acdd::mcp::server::Server;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let ws = Workspace::with_files(&[(
        "pkg/hub.py",
        "def dispatch_hub(item: int) -> str:\n    return str(item)\n",
    )]);
    ws.build();
    let server = Server::new(ws.root().to_owned(), ws.db());
    let attempts = Arc::new(AtomicUsize::new(0));
    let authorizer_attempts = Arc::clone(&attempts);
    server
        .read(|conn| {
            conn.authorizer(Some(move |context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "edges" | "shared_owners",
                    ..
                } => {
                    authorizer_attempts.fetch_add(1, Ordering::Relaxed);
                    Authorization::Deny
                }
                _ => Authorization::Allow,
            }));
            Ok(Value::Null)
        })
        .unwrap();
    let mut client = McpTestHarness::from_server(server);
    for (pattern, exact, include_pulse, expect_pulse) in [
        ("dispatch_hub", true, None, false),
        ("dispatch_hub", true, Some(true), false),
        ("*dispatch_hub*", false, Some(false), false),
        ("returns:str", false, Some(false), false),
        ("*dispatch_hub*", false, None, true),
    ] {
        let result = client
            .payload(
                "code_map_search",
                json!({"pattern":pattern,"kind":"function",
                "path":"pkg","exact":exact,"include_pulse":include_pulse}),
            )
            .unwrap();
        assert_eq!(result["pattern"], pattern);
        let first = &result["nodes"]["items"][0];
        assert_eq!(first["name"], "dispatch_hub");
        assert_eq!(first.get("graph_pulse").is_some(), expect_pulse);
        assert_eq!(
            attempts.swap(0, Ordering::Relaxed) > 0,
            expect_pulse,
            "{pattern}, exact={exact}, include_pulse={include_pulse:?}"
        );
    }
    client.finish().unwrap();
}
