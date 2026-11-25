use super::*;

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
    let ws = Workspace::new();
    // Module directory contains "quartztoken" so the module node matches via indexed_text
    // (not name_prefix) without any source line containing "quartztoken" (snippetless module hit).
    ws.write("pkg/quartztoken_dir/holder.py", "# module without token\n");
    ws.write(
        "pkg/concrete.py",
        "def concrete_consumer():\n    \"\"\"quartztoken processor\"\"\"\n    return 2\n",
    );
    ws.build();
    let conn = ws.open();

    let raw = symbols::search_with_options(
        &conn,
        "quartztoken",
        &symbols::SearchOptions {
            kind: None,
            path: Some("pkg"),
            include_docs: false,
            exact: false,
            classification: None,
            page: &options(10),
        },
    )
    .unwrap();
    assert_eq!(raw["pattern"], "quartztoken");
    let raw_items = raw["nodes"]["items"].as_array().unwrap();
    assert!(raw_items.len() >= 2, "expected concrete + module hits: {raw}");
    assert_eq!(
        raw_items[0]["name"], "concrete_consumer",
        "concrete symbol indexed_text hit must outrank module indexed_text hit: {raw}"
    );

    // Also verify navigation-layer deprioritization when a snippetless module indexed_text
    // hit is forced ahead of a snippet-bearing hit.
    let mut synthetic = raw.clone();
    synthetic["nodes"]["items"].as_array_mut().unwrap().reverse();
    let nav = symbols::search_results_with_navigation(&conn, ws.root(), synthetic, false).unwrap();
    let nav_items = nav["nodes"]["items"].as_array().unwrap();
    assert_eq!(
        nav_items[0]["name"], "concrete_consumer",
        "snippetless module indexed_text hit must be deprioritized behind concrete hit: {nav}"
    );
    assert!(
        nav_items[0]["match_snippet"].as_str().is_some(),
        "top hit must carry resolved match_snippet: {nav}"
    );
    let mod_item = nav_items
        .iter()
        .find(|item| item["kind"] == "module")
        .expect("module hit retained at tail");
    assert!(
        mod_item.get("match_snippet").is_none(),
        "module hit has no matching source line snippet: {mod_item}"
    );
}

#[test]
fn exact_search_retains_the_matching_symbol_without_reading_graph_tables() {
    let conn = rrf_corpus().open();
    conn.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "edges" | "shared_owners",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }));
    for (pattern, exact, expected_first) in [
        ("dispatch_hub", true, "dispatch_hub"),
        ("*dispatch_hub*", false, "dispatch_hub"),
        ("returns:str", false, "dispatch_hub"),
    ] {
        let result = symbols::search_with_options(
            &conn,
            pattern,
            &symbols::SearchOptions {
                kind: Some("function"),
                path: Some("pkg"),
                include_docs: false,
                exact,
                classification: None,
                page: &options(10),
            },
        )
        .unwrap();
        assert_eq!(result["pattern"], pattern);
        assert_eq!(result["nodes"]["items"][0]["name"], expected_first);
    }
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
}
