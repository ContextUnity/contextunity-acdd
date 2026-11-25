use super::*;

#[test]
fn overview_markdown_reports_workspace_inventory() {
    let mut client = McpTestHarness::indexed(&[("src/lib.rs", "pub fn sample() {}\n")]).unwrap();
    let response = client.text("code_map_overview", json!({})).unwrap();
    assert!(response.contains("Workspace"));
    client.finish().unwrap();
}

#[test]
fn compact_markdown_envelopes_stay_under_4kib_with_json_parity_and_opt_ins() {
    let mut service = String::from("pub fn core_target(x: i32) -> i32 { x + 1 }\n");
    for i in 0..16 {
        service.push_str(&format!(
            "/// Helper {i} delegates to core_target.\npub fn service_op_{i:02}(v: i32) -> i32 {{ core_target(v + {i}) }}\n"
        ));
    }
    let tests_src = "use crate::service::{core_target, service_op_00};\nfn helper_bridge(v: i32) -> i32 { core_target(v) }\n#[test]\nfn test_core_target_direct() { assert_eq!(core_target(1), 2); }\n#[test]\nfn test_core_target_via_helper() { assert_eq!(helper_bridge(1) + service_op_00(1), 4); }\n";
    let mut client = McpTestHarness::indexed(&[
        ("src/lib.rs", "pub mod service;\n#[cfg(test)]\nmod tests;\n"),
        ("src/service.rs", &service),
        ("src/tests.rs", tests_src),
        ("src/other/util.rs", "pub fn other_util() -> usize { 7 }\n"),
        (
            "docs/guide.md",
            "---\ntitle: Guide\ndoc_type: guide\n---\n# Guide\nCalls `core_target`.\n",
        ),
    ])
    .unwrap();

    // 1. Representative compact Markdown payloads must stay under 4096 bytes,
    //    omit no-op matched/none freshness, and omit unrequested file:// URIs.
    let cases = [
        ("code_map_overview", json!({})),
        ("code_map_overview", json!({"path": "src/other"})),
        ("code_map_search", json!({"pattern": "service_op_*"})),
        (
            "code_map_inspect",
            json!({"selector": "src/service.rs:core_target"}),
        ),
        (
            "acdd_read",
            json!({"path": "src/service.rs", "start_line": 1, "line_count": 20}),
        ),
        (
            "code_map_tests",
            json!({"selector": "src/service.rs:core_target", "direction": "inbound"}),
        ),
        (
            "code_map_impact",
            json!({"selector": "src/service.rs:core_target", "direction": "inbound", "depth": 1}),
        ),
        (
            "code_map_explain",
            json!({"selector": "src/service.rs:core_target"}),
        ),
    ];
    for (tool, args) in &cases {
        let md = client.text(tool, args.clone()).expect(tool);
        assert!(
            md.len() < 4096,
            "{tool}({args}) exceeded 4096 bytes ({} bytes):\n{md}",
            md.len()
        );
        assert!(
            !md.contains("file://"),
            "{tool}({args}) must omit file:// URIs in compact mode:\n{md}"
        );
        assert!(
            !md.contains("- **freshness**"),
            "{tool}({args}) must suppress matched/none freshness in Markdown:\n{md}"
        );
    }

    // 2. Overview compaction: inline compiled profiles, omitted 0-count diagnostic rows, and path-scoped overview.
    let overview_md = client.text("code_map_overview", json!({})).unwrap();
    assert!(
        overview_md.contains("**Compiled profiles:**"),
        "{overview_md}"
    );
    assert!(
        !overview_md.contains("status=unresolved): 0"),
        "{overview_md}"
    );
    let path_overview_md = client
        .text("code_map_overview", json!({"path": "src/other"}))
        .unwrap();
    let path_overview_json = client
        .payload("code_map_overview", json!({"path": "src/other"}))
        .unwrap();
    assert!(
        path_overview_md.contains("Path: `src/other`")
            && path_overview_md.contains("src/other/util.rs"),
        "{path_overview_md}"
    );
    assert_eq!(path_overview_json["path"], "src/other");
    assert_eq!(path_overview_json["counts"]["files"], 1);
    assert_eq!(path_overview_json["freshness"]["status"], "matched");

    // 3. Search compaction: default limit=12, 8-char continuing generation replay, include_pulse=false, and opt-in file_uris/detail=full.
    let search_json = client
        .payload("code_map_search", json!({"pattern": "service_op_*"}))
        .unwrap();
    assert_eq!(search_json["nodes"]["limit"], 12);
    assert_eq!(search_json["nodes"]["has_more"], true);
    let short_gen = search_json["nodes"]["generation"].as_str().unwrap();
    assert_eq!(
        short_gen.len(),
        8,
        "continuing generation must be shortened to 8 chars: {short_gen}"
    );
    let page2 = client
        .payload(
            "code_map_search",
            json!({"pattern": "service_op_*", "offset": 12, "generation": short_gen}),
        )
        .unwrap();
    assert_eq!(page2["nodes"]["offset"], 12);
    assert_eq!(page2["nodes"]["has_more"], false);

    let no_pulse = client
        .payload(
            "code_map_search",
            json!({"pattern": "core_target", "include_pulse": false}),
        )
        .unwrap();
    assert!(no_pulse["nodes"]["items"][0].get("graph_pulse").is_none());

    let uri_md = client
        .text(
            "code_map_search",
            json!({"pattern": "core_target", "file_uris": true}),
        )
        .unwrap();
    assert!(
        uri_md.contains("file://"),
        "file_uris=true must emit file:// URIs: {uri_md}"
    );
    let full_md = client
        .text(
            "code_map_search",
            json!({"pattern": "core_target", "detail": "full"}),
        )
        .unwrap();
    assert!(
        full_md.contains("file://"),
        "detail=full must emit file:// URIs: {full_md}"
    );

    // 4. code_map_tests compact breadcrumbs omit per-hop rs: IDs while preserving JSON parity.
    let tests_md = client
        .text(
            "code_map_tests",
            json!({"selector": "src/service.rs:core_target", "direction": "inbound"}),
        )
        .unwrap();
    let tests_json = client
        .payload(
            "code_map_tests",
            json!({"selector": "src/service.rs:core_target", "direction": "inbound"}),
        )
        .unwrap();
    assert!(
        !tests_md.contains("rs:src/"),
        "compact code_map_tests must omit per-hop rs: IDs: {tests_md}"
    );
    assert!(tests_json["nodes"]["items"][0]["connection"]["verified_call_path_nodes"].is_array());

    // 5. 1000+ search total badge and trim_items_page_tail no double-quoting.
    let capped_md = contextunity_acdd::format::render_for_operation(
        &json!({
            "pattern": "fn",
            "nodes": {
                "total": null,
                "offset": 0,
                "limit": 12,
                "has_more": true,
                "next_offset": 12,
                "generation": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "items": [{"name": "foo", "qualname": "foo", "kind": "function", "path": "src/lib.rs", "line": 1}]
            }
        }),
        contextunity_acdd::format::OutputFormat::Markdown,
        "code_map_search",
        Some(contextunity_acdd::format::DiscoveryKind::Search),
        contextunity_acdd::format::OperationKind::Navigation,
    );
    assert!(capped_md.contains("1000+ indexed matches"), "{capped_md}");

    client.finish().unwrap();
}
