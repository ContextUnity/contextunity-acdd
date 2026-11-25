use super::*;

#[test]
fn cli_and_mcp_test_mapping_preserve_verified_call_paths() {
    let ws = &document_workspace();
    let mut client = McpTestHarness::new(ws).expect("MCP initialize");
    let mcp_text = response_text(
        &mut client,
        "code_map_tests",
        json!({"selector":"main.py:target","direction":"inbound","format":"markdown"}),
    );
    let cli_text = ws
        .cli_text(&["query", "tests", "main.py:target"])
        .expect("CLI should succeed");
    for (caller, text) in [("mcp", mcp_text), ("cli", cli_text)] {
        let breadcrumb = text
            .split("Verified call path:")
            .nth(1)
            .expect("verified test-to-target path");
        let test_case = breadcrumb.find("test_target").expect("case node");
        let helper = breadcrumb.find("helper_one").expect("helper node");
        let target = breadcrumb.rfind("target").expect("target node");
        assert!(
            test_case < helper && helper < target,
            "{caller}: {breadcrumb}"
        );
        for token in ["test_case", "test_helper", "(target)", "file://", "#L"] {
            assert!(breadcrumb.contains(token), "{caller} {token}: {breadcrumb}");
        }
    }
    let cli = ws
        .query(&["query", "tests", "main.py:target"])
        .expect("CLI should succeed");
    let mcp = client
        .payload(
            "code_map_tests",
            json!({"selector":"main.py:target","direction":"inbound"}),
        )
        .expect("MCP tool should succeed");
    for pointer in [
        "/nodes/items/0/connection/call_path",
        "/nodes/items/0/connection/verified_call_path_nodes",
        "/nodes/items/0/test_role",
    ] {
        assert_eq!(cli.pointer(pointer), mcp.pointer(pointer), "{pointer}");
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn document_detail_preserves_full_content_and_compact_outline_across_formats() {
    let ws = &document_workspace();
    let mut client = McpTestHarness::new(ws).expect("MCP initialize");
    let document = client
        .document("README.md", json!({}))
        .expect("MCP tool should succeed");
    assert!(document["sections"]["items"][0]["content"]
        .as_str()
        .unwrap()
        .contains("documented contract"));
    let outline = client
        .document("README.md", json!({"detail":"compact"}))
        .expect("MCP tool should succeed");
    assert!(outline["sections"]["items"][0].get("content").is_none());
    assert_eq!(document["sections"]["total"], outline["sections"]["total"]);
    let full_markdown = response_text(
        &mut client,
        "get_doc",
        json!({"path_or_id":"README.md","format":"markdown"}),
    );
    assert!(full_markdown.contains("Document sections"));
    assert!(full_markdown.contains("Source Markdown"));
    assert!(full_markdown.contains("documented contract"));
    assert!(full_markdown.contains("[README.md](file://"));
    let sections_slice = full_markdown
        .split("## Verified referenced symbols")
        .next()
        .unwrap_or(&full_markdown);
    assert!(
        !full_markdown.contains("## Ambiguous symbol candidates")
            && !sections_slice.contains("**doc_id**")
            && !sections_slice.contains("**is_invariant**"),
        "default get_doc markdown strips candidate stubs and raw DB metadata: {full_markdown}"
    );
    assert_eq!(document["referenced_symbol_candidates"]["omitted"], true);
    let outline_markdown = response_text(
        &mut client,
        "get_doc",
        json!({"path_or_id":"README.md","detail":"compact","format":"markdown"}),
    );
    assert!(outline_markdown.contains("Document sections"));
    assert!(!outline_markdown.contains("Source Markdown"));
    assert!(
        !outline_markdown.contains("## Ambiguous symbol candidates")
            && !outline_markdown.contains("Request detail=full"),
        "compact output strips candidate stubs: {outline_markdown}"
    );
    client.finish().expect("server must finish successfully");
}

#[test]
fn document_search_markdown_preserves_page_metadata_excerpts_and_section_identity() {
    let ws = &document_workspace();
    let mut client = McpTestHarness::new(ws).expect("MCP initialize");
    let markdown = response_text(
        &mut client,
        "search_docs",
        json!({"query":"documented contract","include_excerpt":true,"format":"markdown"}),
    );
    let json = client
        .payload(
            "search_docs",
            json!({"query":"documented contract","include_excerpt":true}),
        )
        .expect("MCP tool should succeed");
    let section = &json["sections"]["items"][0];
    assert_eq!(markdown.lines().next(), Some("# search_docs"));
    for token in [
        "Search results",
        "## Sections",
        "**total**: `1`",
        "**limit**: `30`",
        "**offset**: `0`",
        "**has_more**: `false`",
        "Match excerpt",
        "# [Contract]\n\nThe `target` function preserves the [documented] [contract].",
        "— `README.md`",
        section["doc_type"].as_str().unwrap(),
    ] {
        assert!(markdown.contains(token), "{token}: {markdown}");
    }
    assert!(
        !markdown.contains(section["doc_id"].as_str().unwrap()),
        "search_docs markdown must not duplicate path via doc_id: {markdown}"
    );
    assert!(
        !markdown.contains("file://"),
        "search_docs markdown must use repo-relative paths: {markdown}"
    );
    assert!(markdown.contains(&format!(
        "- **{}**",
        section["section_title"].as_str().unwrap()
    )));
    client.finish().expect("server must finish successfully");
}

#[test]
fn document_markdown_admits_edited_sources_and_keeps_freshness_evidence() {
    let (ws, harness) = McpTestHarness::with_files(DOCUMENT_FILES);
    let mut client = harness.expect("MCP initialize");
    client
        .document("README.md", json!({}))
        .expect("MCP tool should succeed");
    ws.write(
        "README.md",
        "# Contract\n\nThe `target` function preserves the documented contract.\nFreshly refreshed documentation evidence.\n",
    );
    invalidate_mcp_connection_cache(&ws.db()).unwrap();
    let refreshed_markdown = response_text(
        &mut client,
        "get_doc",
        json!({"path_or_id":"README.md","format":"markdown"}),
    );
    assert!(refreshed_markdown.contains("Freshly refreshed documentation evidence."));
    for token in [
        "- **freshness**\n",
        "  - **corpus_hash**:",
        "  - **files_checked**:",
        "  - **output_root**:",
        "  - **refresh**: `\"delta\"`",
        "  - **status**: `\"source_inventory_matched\"`",
    ] {
        assert!(
            refreshed_markdown.contains(token),
            "missing source-admitted freshness token {token}: {refreshed_markdown}"
        );
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn explain_document_controls_and_direction_aliases_preserve_the_graph() {
    let ws = &document_workspace();
    let mut client = McpTestHarness::new(ws).expect("MCP initialize");
    let mut explain = |arguments: Value| {
        client
            .payload("code_map_explain", arguments)
            .expect("MCP tool should succeed")
    };
    let with_docs = explain(json!({"selector":"target"}));
    assert!(with_docs["documents"]["total"].as_u64().unwrap() > 0);
    let without_docs = explain(json!({"selector":"target","show_doc":false}));
    assert!(without_docs.get("documents").is_none() || without_docs["documents"]["total"] == 0);
    assert_eq!(with_docs["coverage"], without_docs["coverage"]);
    assert_eq!(with_docs["incoming"], without_docs["incoming"]);
    for (alias, canonical, omitted) in [
        ("inbound", "incoming", "outgoing"),
        ("outbound", "outgoing", "incoming"),
    ] {
        let alias_result = explain(json!({"selector":"target","direction":alias}));
        let canonical_result = explain(json!({"selector":"target","direction":canonical}));
        assert_eq!(alias_result["direction"], canonical);
        assert_eq!(alias_result[canonical], canonical_result[canonical]);
        assert_eq!(alias_result[omitted]["omitted"], true);
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn get_doc_large_doc_decouples_candidates_and_read_engine_defaults() {
    let mut big_doc = String::from("---\ntitle: Large Doc\ndoc_type: reference\n---\n# Large Doc\n");
    for i in 0..80 {
        big_doc.push_str(&format!("\n## Section {i}\nReferences `shared` and `helper`.\n"));
    }
    let long_file: String = (1..=125).map(|i| format!("line_{i}\n")).collect();
    let (ws, harness) = McpTestHarness::with_files(&[
        ("src/a.py", "def shared():\n    return 'a'\n\ndef helper():\n    return 'needle_hit'\n"),
        ("src/b.py", "def shared():\n    return 'b'\n"),
        ("docs/archive/old.md", "archived needle_hit\n"),
        ("docs/large.md", &big_doc),
        ("src/long.txt", &long_file),
    ]);
    let mut client = harness.expect("MCP initialize");
    // Prime index before measuring the 2s SQLite query boundary on get_doc
    ws.build();

    // 1. Large-doc get_doc default (include_candidates: false) stays well below 2s SQLite boundary and omits candidates
    let started = std::time::Instant::now();
    let default_doc = client
        .document("docs/large.md", json!({}))
        .expect("default get_doc should succeed");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "default get_doc exceeded 2s boundary: {:?}",
        started.elapsed()
    );
    assert_eq!(default_doc["referenced_symbol_candidates"]["omitted"], true);
    assert!(default_doc["referenced_symbol_candidates"]["hint"]
        .as_str()
        .is_some_and(|h| h.contains("include_candidates=true")));

    // Opt-in include_candidates returns ambiguous candidates
    let with_candidates = client
        .document(
            "docs/large.md",
            json!({"section":"Section 0","include_candidates":true}),
        )
        .expect("opt-in get_doc should succeed");
    assert_eq!(with_candidates["referenced_symbol_candidates"]["total"], 2);

    // 2. acdd_read mode="search" defaults: context=0, ## <path> grouping, **/archive/** exclusion, explicit overrides
    let search_default = client
        .payload("acdd_read", json!({"mode":"search","pattern":"needle_hit"}))
        .expect("search should succeed");
    let content = search_default["content"].as_str().unwrap();
    assert!(content.contains("## src/a.py\n5: "), "{content}");
    assert!(!content.contains("4: "), "default context must be 0: {content}");
    assert!(!content.contains("docs/archive/old.md"), "archive must be excluded by default: {content}");

    let search_ctx = client
        .payload("acdd_read", json!({"path":"src/a.py","mode":"search","pattern":"needle_hit","context":1}))
        .expect("explicit context override");
    assert!(search_ctx["content"].as_str().unwrap().contains("4: def helper():"));

    let search_archive = client
        .payload("acdd_read", json!({"path":"docs/archive","mode":"search","pattern":"needle_hit"}))
        .expect("explicit archive search");
    assert!(search_archive["content"].as_str().unwrap().contains("## docs/archive/old.md\n1: archived needle_hit"));

    // 3. acdd_read default line_count=100 vs explicit override
    let default_lines = client
        .payload("acdd_read", json!({"path":"src/long.txt"}))
        .expect("default lines read");
    assert_eq!(default_lines["truncated"], true);
    assert_eq!(default_lines["content"].as_str().unwrap().lines().count(), 100);

    let full_lines = client
        .payload("acdd_read", json!({"path":"src/long.txt","line_count":130}))
        .expect("explicit line_count override");
    assert_eq!(full_lines["truncated"], false);
    assert_eq!(full_lines["content"].as_str().unwrap().lines().count(), 125);

    // 4. Concise enclosing footer + trimmed source_preview defaults
    let snippet = client
        .payload("acdd_read", json!({"path":"src/a.py","selector":"helper"}))
        .expect("snippet read");
    let preview = snippet["source_preview"].as_object().unwrap();
    assert!(!preview.contains_key("next_source_offset") && !preview.contains_key("continuation_hint"));
    assert!(!preview.contains_key("omitted_leading_lines"));
    let _ = ws;
    client.finish().expect("server must finish successfully");
}
