use super::*;

#[test]
fn search_preview_failures_preserve_hits_and_actionable_source_errors() {
    // Warm the real server's read snapshot before changing source so these calls
    // exercise per-hit preview verification rather than index refresh.
    let original_source = "def resilient_hit(): return 1\n# no declaration\n";
    let w = Workspace::with_files(&[
        ("a.py", original_source),
        ("z.py", "def resilient_hit(): return 2\n"),
    ]);
    w.build();
    for failure in ["digest", "missing"] {
        let mut m = McpTestHarness::new(&w).expect("MCP initialize");
        m.payload("code_map_overview", json!({}))
            .expect("MCP tool should succeed");
        match failure {
            "digest" => w.write("a.py", "def resilient_hit(): return 9\n"),
            "missing" => w.remove("a.py"),
            _ => {}
        }
        for include_preview in [false, true] {
            let result = m
                .search(
                    "resilient_hit",
                    json!({"exact":true,"include_preview":include_preview}),
                )
                .expect("MCP tool should succeed");
            assert_eq!(result["nodes"]["total"], 2, "{failure}: {result}");
            let hits = crate::json_array(&result["nodes"]["items"]);
            assert_eq!(hits.len(), 2);
            for (hit, path) in hits.iter().zip(["a.py", "z.py"]) {
                assert_eq!(hit["path"], path, "{failure}: {result}");
                assert_eq!(hit["line"], 1);
                assert_eq!(hit["inspect_selector"]["selector"], hit["id"]);
                assert_eq!(hit["inspect_selector"]["show_source"], true);
            }
            assert!(
                hits[0].get("source_preview").is_none(),
                "{failure}: {result}"
            );
            assert!(hits[1]["source_preview"].is_object(), "{failure}: {result}");
        }
        let (_, response) = m
            .call(
                "acdd_read",
                json!({"selector":"a.py:resilient_hit","format":"json"}),
            )
            .expect("MCP transport");
        assert_eq!(response["result"]["isError"], true, "{failure}: {response}");
        m.finish().expect("server must finish successfully");
        w.write("a.py", original_source);
    }
}

#[test]
fn mcp_search_ast_boundaries() {
    let mut m = McpTestHarness::indexed(&[
        (
            "src/app.py",
            "from typing import Literal\nModelPoolStrategy = Literal[\"direct\", \"fallback\"]\ndef item_name(): pass\ndef itemXname(): pass\nprint(item_name())\n",
        ),
        (
            "docs/intro.md",
            "---\ndoc_type: guide\ntitle: Intro\n---\n# Intro\n\nA searchable contract.\n",
        ),
    ])
    .expect("MCP initialize");
    let overview = m
        .payload("code_map_overview", json!({"limit":1}))
        .expect("MCP tool should succeed");
    assert_eq!(overview["languages"]["has_more"], true, "{overview}");
    assert_eq!(overview["languages"]["next_offset"], 1, "{overview}");
    assert_eq!(overview["generation"], overview["languages"]["generation"]);
    assert!(overview.get("counts").is_some());
    let prefix = m
        .search("item*", json!({"kind":"function","limit":1}))
        .expect("MCP tool should succeed");
    assert_eq!(crate::json_array(&prefix["nodes"]["items"]).len(), 1);
    assert_eq!(prefix["nodes"]["total"], 2);
    let scoped = m
        .search("item*", json!({"kind":"function","path":"src/app.py"}))
        .expect("MCP tool should succeed");
    assert_eq!(scoped["nodes"]["total"], 2);
    let outside = m
        .search("item*", json!({"path":"docs"}))
        .expect("MCP tool should succeed");
    assert_eq!(outside["nodes"]["total"], 0);
    let literal_underscore = m
        .search("item_*", json!({}))
        .expect("MCP tool should succeed");
    assert_eq!(literal_underscore["nodes"]["total"], 1);
    for pattern in ["item%*", "item\\*", "item'*", "item\"*"] {
        let result = m
            .payload("code_map_search", json!({"pattern":pattern}))
            .expect("MCP tool should succeed");
        assert_eq!(result["nodes"]["total"], 0, "{pattern}: {result}");
    }

    let type_search = m
        .search("ModelPoolStrategy", json!({}))
        .expect("MCP tool should succeed");
    assert_eq!(type_search["nodes"]["total"], 1);
    assert!(type_search["nodes"]["items"][0]["source_preview"].is_object());

    let ast = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"print($VALUE)","language":"python","path":"src/app.py"}),
        )
        .expect("MCP tool should succeed");
    assert!(!crate::json_array(&ast["matches"]["items"]).is_empty());

    let ranked = m
        .search("item*", json!({"limit":1}))
        .expect("MCP tool should succeed");
    let ranked_total = ranked["nodes"]["total"].clone();
    assert_eq!(ranked_total, 2);
    assert_eq!(ranked["nodes"]["has_more"], true);
    let empty_ranked_page = m
        .search(
            "item*",
            json!({"limit":1,"offset":100,"generation":ranked["nodes"]["generation"]}),
        )
        .expect("MCP tool should succeed");
    assert!(crate::json_array(&empty_ranked_page["nodes"]["items"]).is_empty());
    assert_eq!(empty_ranked_page["nodes"]["total"], ranked_total);
    let rejection = m
        .text(
            "ast_grep_search",
            json!({"pattern":"print($VALUE)","language":"unknown"}),
        )
        .expect_err("tool should reject input");
    let expected: &str = "unknown";
    assert!(
        rejection
            .tool_message()
            .expect("expected MCP tool rejection")
            .contains(expected),
        "{rejection}"
    );
    let rejection = m
        .text(
            "ast_grep_search",
            json!({"pattern":"print($VALUE)","language":"python","path":""}),
        )
        .expect_err("tool should reject input");
    let expected: &str = "path";
    assert!(
        rejection
            .tool_message()
            .expect("expected MCP tool rejection")
            .contains(expected),
        "{rejection}"
    );

    m.finish().expect("server must finish successfully");

    let mut cls = McpTestHarness::indexed(&[
        ("src/app.py", "def item_target(): pass\n"),
        ("tests/test_app.py", "def test_item_target(): pass\n"),
        (
            "docs/intro.md",
            "---\ntitle: Item Target Guide\n---\n# Item Target Guide\n",
        ),
    ])
    .expect("MCP initialize");
    for (pattern, classification, expected_names) in [
        ("*item*", "production", &["item_target"][..]),
        ("*item*", "test", &["test_item_target"][..]),
        ("Item*", "documentation", &["Item Target Guide"][..]),
        ("*", "test", &["test_app.py", "test_item_target"][..]),
    ] {
        let found = cls
            .payload(
                "code_map_search",
                json!({"pattern": pattern, "classification": classification}),
            )
            .expect("MCP classification filter should succeed");
        let items = crate::json_array(&found["nodes"]["items"]);
        assert_eq!(
            found["nodes"]["total"],
            expected_names.len(),
            "{classification}: {found}"
        );
        let actual_names: Vec<&str> = items
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert_eq!(actual_names, expected_names, "{classification}: {found}");
        for item in items {
            assert_eq!(item["classification"], classification, "{found}");
        }
    }
    let bad_cls = cls
        .text(
            "code_map_search",
            json!({"pattern": "*item*", "classification": "bogus"}),
        )
        .expect_err("invalid classification must reject");
    assert!(bad_cls.tool_message().is_some(), "{bad_cls}");
    let cli_out = run_cli(
        cls.workspace(),
        &[
            "--format",
            "json",
            "query",
            "search",
            "*item*",
            "--classification",
            "test",
        ],
    );
    assert!(
        cli_out.status.success(),
        "CLI query search --classification failed: {}",
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let cli_json: Value = serde_json::from_slice(&cli_out.stdout).expect("CLI JSON");
    assert_eq!(cli_json["nodes"]["total"], 1, "{cli_json}");
    assert_eq!(cli_json["nodes"]["items"][0]["name"], "test_item_target");
    cls.finish().expect("server must finish successfully");
}

#[test]
fn document_search_and_section_validation_boundaries() {
    let mut m = McpTestHarness::indexed(&[
        (
            "src/app.py",
            "from typing import Literal\nModelPoolStrategy = Literal[\"direct\", \"fallback\"]\ndef item_name(): pass\ndef itemXname(): pass\nprint(item_name())\n",
        ),
        (
            "docs/intro.md",
            "---\ndoc_type: guide\ntitle: Intro\n---\n# Intro\n\nA searchable contract.\n",
        ),
    ])
    .expect("MCP initialize");
    let docs = m
        .payload("search_docs", json!({"query":"searchable"}))
        .expect("MCP tool should succeed");
    assert!(docs["sections"]["total"].as_u64().unwrap() >= 1);
    assert!(docs["sections"]["items"][0].get("excerpt").is_none());
    let excerpt = m
        .payload(
            "search_docs",
            json!({"query":"searchable","include_excerpt":true}),
        )
        .expect("MCP tool should succeed");
    assert!(excerpt["sections"]["items"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("[searchable]"));
    let rejection = m
        .text("search_docs", json!({"query":""}))
        .expect_err("tool should reject input");
    let expected: &str = "search query is empty";
    assert!(
        rejection
            .tool_message()
            .expect("expected MCP tool rejection")
            .contains(expected),
        "{rejection}"
    );
    let quoted = m
        .payload("search_docs", json!({"query":"\""}))
        .expect("MCP tool should succeed");
    assert_eq!(quoted["sections"]["total"], 0);
    let filtered = m
        .payload(
            "search_docs",
            json!({"query":"searchable","doc_type":"no-such-type"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(filtered["sections"]["total"], 0);
    let rejection = m
        .text(
            "get_doc",
            json!({"path_or_id":"docs/intro.md","section":"NonExistentSection"}),
        )
        .expect_err("tool should reject input");
    let expected: &str = "document or section not found";
    assert!(
        rejection
            .tool_message()
            .expect("expected MCP tool rejection")
            .contains(expected),
        "{rejection}"
    );
    m.finish().expect("server must finish successfully");
}

#[test]
fn ast_language_inference_and_actionable_diagnostics() {
    use acdd_index::languages;
    let many_calls: String = (0..16).map(|i| format!("many_call({i})\n")).collect();
    let fixture = AnalysisFixture::indexed_files(&[
        (
            "app.py",
            "class Widget:\n    pass\ndef target(value):\n    return value\n",
        ),
        ("more.py", "def target(other):\n    return other\n"),
        ("many.py", &many_calls),
        ("pkg/only.py", "def pkg_only():\n    return 7\n"),
        ("lib.rs", "fn target() {}\n"),
    ]);
    let mut m = fixture.start_mcp().expect("MCP initialize");
    for (path, pattern) in [("app.py", "class Widget"), ("app.py", "def target($$$)")]
        .into_iter()
        .chain(cfg!(feature = "lang-rust").then_some(("lib.rs", "fn target")))
    {
        let result = m
            .payload("ast_grep_search", json!({"pattern":pattern,"path":path}))
            .expect("MCP tool should succeed");
        assert_eq!(result["matches"]["total"], 1, "{pattern}: {result}");
        assert_eq!(result["matches"]["limit"], 12, "{pattern}: {result}");
        if pattern.contains("$$$") {
            assert!(
                !result["matches"]["items"][0]["captures"]
                    .as_object()
                    .unwrap()
                    .contains_key(""),
                "{result}"
            );
        }
    }
    let mixed = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"def $NAME($$$)","path":"app.py"}),
        )
        .expect("MCP");
    let mixed_caps = mixed["matches"]["items"][0]["captures"]
        .as_object()
        .unwrap();
    assert_eq!(mixed_caps.get("NAME"), Some(&json!("target")));
    assert!(!mixed_caps.contains_key(""), "{mixed}");
    let paged = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"many_call($$$)","path":"many.py"}),
        )
        .expect("MCP");
    assert_eq!(paged["matches"]["limit"], 12);
    assert_eq!(crate::json_array(&paged["matches"]["items"]).len(), 12);
    assert_eq!(paged["matches"]["has_more"], true);
    // Explicit language supports a workspace scope without a file extension.
    let explicit = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"class Widget","language":"python"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(explicit["matches"]["total"], 1);
    let captured = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"def target($ARG)","language":"python","limit":1}),
        )
        .expect("MCP tool should succeed");
    let captured_page = &captured["matches"];
    assert_eq!(captured_page["total"], serde_json::Value::Null);
    assert_eq!(captured_page["total_status"], "unavailable");
    assert_eq!(captured_page["has_more"], true);
    assert_eq!(captured_page["next_offset"], 1);
    assert_eq!(captured_page["computation_truncated"], false);
    let first_match = &captured_page["items"][0];
    assert_eq!(first_match["path"], "app.py");
    assert_eq!(first_match["line"], 3);
    assert_eq!(first_match["end_line"], 4);
    assert!(first_match["text"]
        .as_str()
        .unwrap()
        .contains("def target(value):"));
    assert_eq!(first_match["captures"]["ARG"], "value");
    let (_, captured_response) = m
        .call(
            "ast_grep_search",
            json!({"pattern":"def target($ARG)","language":"python","limit":1,"format":"markdown"}),
        )
        .expect("MCP transport");
    assert_ne!(
        captured_response["result"]["isError"], true,
        "{captured_response}"
    );
    let captured_markdown = captured_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    for expected in [
        "app.py#L3-L4",
        "def target(value):",
        "captures:",
        "ARG",
        "value",
    ] {
        assert!(captured_markdown.contains(expected), "{captured_markdown}");
    }
    assert!(
        captured_markdown.lines().any(|line| {
            line.starts_with("Continue AST matches for ")
                && line.contains("pattern=def target($ARG)")
                && line.contains("language=python")
                && line.contains("at offset 1 with generation `")
        }),
        "AST continuation must retain its query, language, and accepted token: {captured_markdown}"
    );
    assert!(
        !captured_markdown.contains("enclosing"),
        "{captured_markdown}"
    );
    let next_ast_page = m
        .payload(
            "ast_grep_search",
            json!({
                "pattern":"def target($ARG)",
                "language":"python",
                "limit":1,
                "offset":1,
                "generation":captured_page["generation"],
            }),
        )
        .expect("MCP tool should succeed");
    assert_eq!(next_ast_page["matches"]["items"][0]["path"], "more.py");
    assert_eq!(next_ast_page["matches"]["offset"], 1);
    assert_eq!(next_ast_page["matches"]["has_more"], false);
    assert_eq!(next_ast_page["matches"]["total"], 2);
    for language in [json!(null), json!("")] {
        let inferred = m
            .payload(
                "ast_grep_search",
                json!({"pattern":"class Widget","language":language,"path":"app.py"}),
            )
            .expect("MCP tool should succeed");
        assert_eq!(inferred["matches"]["total"], 1);
    }
    let dir_inferred = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"def pkg_only():","path":"pkg"}),
        )
        .expect("single-language directory should infer AST language");
    assert_eq!(dir_inferred["language"], "python");
    assert_eq!(dir_inferred["matches"]["total"], 1);
    for path in [None, Some("."), Some("./.")] {
        let narrowed = m
            .payload(
                "ast_grep_search",
                json!({"pattern":"class Widget","path":path}),
            )
            .expect("multi-language workspace should select compatible AST language");
        assert_eq!(narrowed["language"], "python", "{narrowed}");
        assert_eq!(narrowed["matches"]["total"], 1, "{narrowed}");
    }
    for (path, pattern, expected_fragments) in [
        (
            Some("src"),
            "class Widget",
            &["language", "path", "acdd_guide", "Examples:"][..],
        ),
        (
            Some("unknown.xyz"),
            "class Widget",
            &["language", "path", "acdd_guide", "Examples:"][..],
        ),
        (
            Some("../outside"),
            "class Widget",
            &["relative plain path"][..],
        ),
        (
            None,
            "def (((",
            &[
                "does not parse in any detected language",
                "language",
                "path",
                "acdd_guide",
                "Examples:",
            ][..],
        ),
    ] {
        let (_, response) = m
            .call("ast_grep_search", json!({"pattern":pattern,"path":path}))
            .expect("MCP transport");
        assert_eq!(response["result"]["isError"], true, "{response}");
        let message = response["result"]["content"][0]["text"].as_str().unwrap();
        if expected_fragments.contains(&"acdd_guide") {
            for profile in languages::profiles() {
                assert!(message.contains(profile.id()), "{message}");
            }
            if cfg!(feature = "lang-rust") && path.is_none() {
                assert!(message.contains("python, rust"), "{message}");
            }
        }
        for fragment in expected_fragments {
            assert!(message.contains(fragment), "{message}");
        }
    }
    if cfg!(feature = "lang-rust") {
        let mut multi_files = vec![
            ("a.py", "helper(alpha)\n"),
            (
                "b.rs",
                "enum Command { ExtendScope }\nfn main() { let _ = Command::ExtendScope; let _ = helper(beta); }\n",
            ),
        ];
        if languages::by_id("yaml").is_some() {
            multi_files.push((
                "config.yaml",
                "command: Command::ExtendScope\nitems:\n  - name: alpha\n",
            ));
        }
        if languages::by_id("json").is_some() {
            multi_files.push(("config.json", "{\"port\": 8080}\n"));
        }
        if languages::by_id("toml").is_some() {
            multi_files.push(("config.toml", "port = 8080\n"));
        }
        let multi = AnalysisFixture::indexed_files(&multi_files);
        let mut mm = multi.start_mcp().expect("MCP initialize");
        let page1 = mm
            .payload(
                "ast_grep_search",
                json!({"pattern":"helper($ARG)","limit":1}),
            )
            .expect("compatible multi-language AST search should succeed");
        assert_eq!(page1["language"], "python, rust", "{page1}");
        assert_eq!(page1["matches"]["has_more"], true, "{page1}");
        assert_eq!(page1["matches"]["items"][0]["path"], "a.py", "{page1}");
        assert_eq!(page1["matches"]["items"][0]["captures"]["ARG"], "alpha");
        let page2 = mm
            .payload(
                "ast_grep_search",
                json!({"pattern":"helper($ARG)","limit":1,"offset":1,"generation":page1["generation"]}),
            )
            .expect("multi-language AST continuation should succeed");
        assert_eq!(page2["matches"]["total"], 2, "{page2}");
        assert_eq!(page2["matches"]["has_more"], false, "{page2}");
        assert_eq!(page2["matches"]["items"][0]["path"], "b.rs", "{page2}");
        assert_eq!(page2["matches"]["items"][0]["captures"]["ARG"], "beta");
        let mut infer_cases = vec![("Command::ExtendScope", None, "rust", "b.rs")];
        if languages::by_id("yaml").is_some() {
            infer_cases.extend([
                ("- name: $NAME", None, "yaml", "config.yaml"),
                ("Command::ExtendScope", Some("yaml"), "yaml", "config.yaml"),
            ]);
        }
        for (pattern, lang, expected_lang, expected_path) in infer_cases {
            let args = match lang {
                Some(l) => json!({"pattern":pattern,"language":l}),
                None => json!({"pattern":pattern}),
            };
            let res = mm
                .payload("ast_grep_search", args)
                .expect("MCP tool should succeed");
            assert_eq!(res["language"], expected_lang, "{pattern}: {res}");
            assert_eq!(
                res["matches"]["items"][0]["path"], expected_path,
                "{pattern}: {res}"
            );
        }
        mm.finish().expect("server must finish successfully");

        let cli_out = run_cli(
            multi.workspace(),
            &[
                "--format",
                "json",
                "ast",
                "grep",
                "helper($ARG)",
                "--limit",
                "150",
            ],
        );
        assert!(
            cli_out.status.success(),
            "CLI ast grep without --lang failed: {}",
            String::from_utf8_lossy(&cli_out.stderr)
        );
        let cli_json: Value = serde_json::from_slice(&cli_out.stdout).expect("CLI JSON");
        assert_eq!(cli_json["language"], "python, rust", "{cli_json}");
        assert_eq!(
            cli_json["matches"].as_array().unwrap().len(),
            2,
            "{cli_json}"
        );
    }
    for language in ["python"]
        .into_iter()
        .chain(cfg!(feature = "lang-rust").then_some("rust"))
    {
        let (_, response) = m
            .call(
                "ast_grep_search",
                json!({"pattern":"def (","language":language}),
            )
            .expect("MCP transport");
        assert_eq!(response["result"]["isError"], true, "{response}");
        let message = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("unsupported_ast_pattern"), "{message}");
        assert!(message.contains("> [!CAUTION]"), "{message}");
        assert!(
            message.contains("Pattern span:") && message.contains("start_line=1"),
            "{message}"
        );
        assert!(
            message.contains("Hint:") && message.contains("Valid patterns for this profile"),
            "{message}"
        );
        for example in languages::require(language)
            .unwrap()
            .ast_search_capabilities()
            .valid_examples
        {
            assert!(message.contains(&example.replace('\n', " ")), "{message}");
        }
        assert!(!message.contains("\"diagnostic\":"), "{message}");
        let (_, markdown_response) = m
            .call(
                "ast_grep_search",
                json!({"pattern":"def (","language":language,"format":"markdown"}),
            )
            .expect("MCP transport");
        assert_eq!(
            markdown_response["result"]["isError"], true,
            "{markdown_response}"
        );
        let markdown = markdown_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            markdown.contains("> [!CAUTION]"),
            "profile diagnostics need a caution callout: {markdown}"
        );
        assert_eq!(markdown, message, "default and explicit Markdown agree");
        assert!(markdown.contains("unsupported_ast_pattern") && markdown.contains(language));
        for example in languages::require(language)
            .unwrap()
            .ast_search_capabilities()
            .valid_examples
        {
            assert!(
                example.lines().all(|line| markdown.contains(line)),
                "selected profile examples must survive: {markdown}"
            );
        }
        let (_, json_response) = m
            .call(
                "ast_grep_search",
                json!({"pattern":"def (","language":language,"format":"json"}),
            )
            .expect("MCP transport");
        assert_eq!(json_response["result"]["isError"], true, "{json_response}");
        let json_error = json_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        for expected in [
            "unsupported_ast_pattern: Unsupported AST pattern",
            " at 1:1 to 1:6.",
            "Hint:",
            "Examples of valid patterns",
        ] {
            assert!(json_error.contains(expected), "{json_error}");
        }
        assert!(!json_error.contains("> [!CAUTION]"), "{json_error}");
    }
    m.finish().expect("server must finish successfully");

    // Invalid patterns still need their actionable diagnostic when the workspace
    // has no language candidates for the selected profile.
    let fixture = AnalysisFixture::indexed_files(&[("README.md", "# No Python sources\n")]);
    let mut m = fixture.start_mcp().expect("MCP initialize");
    let (_, response) = m
        .call(
            "ast_grep_search",
            json!({"pattern":"def (","language":"python"}),
        )
        .expect("MCP transport");
    assert_eq!(response["result"]["isError"], true, "{response}");
    let markdown = response["result"]["content"][0]["text"].as_str().unwrap();
    for expected in [
        "> [!CAUTION]",
        "unsupported_ast_pattern",
        "python",
        "Pattern span:",
        "Hint:",
        "Valid patterns for this profile",
    ] {
        assert!(markdown.contains(expected), "{markdown}");
    }
    let (_, json_response) = m
        .call(
            "ast_grep_search",
            json!({"pattern":"def (","language":"python","format":"json"}),
        )
        .expect("MCP transport");
    assert_eq!(json_response["result"]["isError"], true, "{json_response}");
    let json_error = json_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    for expected in [
        "unsupported_ast_pattern: Unsupported AST pattern",
        " at 1:1 to 1:6.",
        "Hint:",
        "Examples of valid patterns",
    ] {
        assert!(json_error.contains(expected), "{json_error}");
    }
    assert!(!json_error.contains("> [!CAUTION]"), "{json_error}");
    m.finish().expect("server must finish successfully");
}

#[test]
fn ast_grep_prefilter_preserves_empty_index_and_body_only_matches() {
    let fixture = AnalysisFixture::indexed_files(&[
        (
            "src/a.py",
            "def compute():\n    local_value = 1 + 2\n    return local_value\n",
        ),
        (
            "src/b.py",
            "def compute():\n    local_value = 1 + 2\n    return local_value\n",
        ),
        (
            "src_other.py",
            "def compute():\n    local_value = 1 + 2\n    return local_value\n",
        ),
        ("src/a_candidate.py", "def marker():\n    return 1\n"),
        ("src/z_body.py", "def other():\n    return \"marker\"\n"),
    ]);
    let indexed = fixture
        .workspace()
        .open()
        .query_row(
            "SELECT count(*) FROM nodes WHERE name=?1",
            ["local_value"],
            |row| row.get::<_, i64>(0),
        )
        .expect("query indexed symbol count");
    assert_eq!(indexed, 0);
    let mut m = fixture.start_mcp().expect("MCP initialize");
    for (pattern, path, expected) in [
        (
            "local_value = $EXPR",
            None,
            vec!["src/a.py", "src/b.py", "src_other.py"],
        ),
        (
            "local_value = $EXPR",
            Some("src"),
            vec!["src/a.py", "src/b.py"],
        ),
        ("local_value = $EXPR", Some("src/a.py"), vec!["src/a.py"]),
        ("return \"marker\"", None, vec!["src/z_body.py"]),
    ] {
        let result = m
            .payload(
                "ast_grep_search",
                json!({"pattern":pattern,"language":"python","path":path}),
            )
            .expect("MCP tool should succeed");
        assert_eq!(result["matches"]["total"], expected.len(), "{result}");
        let items = crate::json_array(&result["matches"]["items"]);
        let paths: Vec<_> = items
            .iter()
            .map(|item| item["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, expected, "{result}");
        assert!(items.iter().all(|item| item["line"] == 2), "{result}");
    }
    let first = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"local_value = $EXPR","language":"python","path":"src","limit":1}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(first["matches"]["items"][0]["path"], "src/a.py");
    assert_eq!(first["matches"]["has_more"], true);
    let second = m.payload(
        "ast_grep_search",
        json!({"pattern":"local_value = $EXPR","language":"python","path":"src","limit":1,"offset":first["matches"]["next_offset"],"generation":first["matches"]["generation"]}),
    ).expect("MCP tool should succeed");
    assert_eq!(second["matches"]["items"][0]["path"], "src/b.py");
    let absent = m
        .payload(
            "ast_grep_search",
            json!({"pattern":"absent_value = $EXPR","language":"python"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(absent["matches"]["total"], 0);
    m.finish().expect("server must finish successfully");
}
