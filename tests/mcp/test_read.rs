//! Contract tests for the layered reader.

use crate::common::Workspace;
use crate::harness::McpTestHarness;
use acdd_core::model::ReadMode;
use acdd_core::subsystems::{filter_subsystem_tools, SubsystemPreset, SubsystemRegistry};
use contextunity_acdd::engine::reader::ReadRequest;
use serde_json::{json, Value};

fn read_response(client: &mut McpTestHarness, arguments: Value) -> Value {
    client
        .call("acdd_read", arguments)
        .expect("MCP transport")
        .1
}

fn read_tool(client: &mut McpTestHarness, arguments: Value) -> Value {
    client
        .payload("acdd_read", arguments)
        .expect("MCP tool should succeed")
}

#[test]
fn acdd_read_symbol_alias_matches_selector_with_and_without_path() {
    let (w, harness) = McpTestHarness::indexed_workspace(&[
        ("src/lib.rs", "pub fn unique_symbol() -> u32 {\n    42\n}\npub fn example() {}\npub fn paged_symbol() {\n    let x = 1;\n    let y = 2;\n    println!(\"{} {}\", x, y);\n}\n"),
        ("first.py", "def repeated():\n    return 1\n"),
        ("second.py", "def repeated():\n    return 2\n"),
        ("values.py", "def value():\n    return 42\n"),
    ]);
    let mut client = harness.expect("MCP initialize");
    let list = client
        .request_value("tools/list", json!({}))
        .expect("tools/list");
    let tools = list["result"]["tools"].as_array().unwrap();
    assert!(
        !tools.iter().any(|tool| tool["name"] == "forge_read"),
        "forge_read must not be registered after one-pass rename"
    );
    for preserved in [
        "code_map_search",
        "code_map_inspect",
        "code_map_impact",
        "code_map_query",
        "code_map_overview",
        "ast_grep_search",
        "acdd_guide",
        "acdd_state",
        "acdd_claim",
        "acdd_verify",
        "acdd_submit",
        "acdd_blackboard",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == preserved),
            "expected preserved MCP tool {preserved}"
        );
    }
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "acdd_read")
        .expect("acdd_read tool");
    let properties = &tool["inputSchema"]["properties"];
    assert!(properties["selector"].is_object(), "{tool}");
    assert!(properties["symbol"].is_object(), "{tool}");
    assert!(
        tool["description"].as_str().unwrap().contains("symbol"),
        "{tool}"
    );

    for (name, path, expected_path, expected_source) in [
        ("unique_symbol", None, "src/lib.rs", "42"),
        ("unique_symbol", Some("src/lib.rs"), "src/lib.rs", "42"),
        ("repeated", Some("second.py"), "second.py", "return 2"),
    ] {
        let mut selector = json!({"selector":name});
        let mut symbol = json!({"symbol":name});
        if let Some(path) = path {
            selector["path"] = json!(path);
            symbol["path"] = json!(path);
        }
        let expected = read_tool(&mut client, selector);
        let actual = read_tool(&mut client, symbol);
        assert_eq!(actual["source"], expected["source"]);
        assert!(
            actual["source"].as_str().unwrap().contains(expected_source),
            "{actual}"
        );
        assert_eq!(actual["node"]["path"], expected_path);
    }
    let mut rejected = vec![
        (json!({"selector":"repeated"}), "ambiguous"),
        (json!({"symbol":"repeated"}), "ambiguous"),
    ];
    rejected.extend(
        [
            ("example", "example"),
            ("example", "different"),
            ("example", ""),
            ("", "example"),
        ]
        .map(|(selector, symbol)| {
            (
                json!({"selector":selector,"symbol":symbol}),
                "duplicate field",
            )
        }),
    );
    for (arguments, diagnostic) in rejected {
        let response = read_response(&mut client, arguments);
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response.to_string().to_lowercase().contains(diagnostic),
            "{response}"
        );
    }
    for arguments in [
        json!({"selector":"example","symbol":null}),
        json!({"selector":null,"symbol":"example"}),
    ] {
        let result = read_tool(&mut client, arguments);
        assert!(
            result["source"].as_str().unwrap().contains("example"),
            "{result}"
        );
    }
    let first = read_tool(
        &mut client,
        json!({"symbol":"paged_symbol","max_body_lines":1,"leading_lines":0}),
    );
    assert_eq!(first["source_preview"]["next_source_offset"], 1, "{first}");
    let next = read_tool(
        &mut client,
        json!({"symbol":"paged_symbol","max_body_lines":1,"leading_lines":0,
        "source_offset":first["source_preview"]["next_source_offset"],"generation":first["generation"]}),
    );
    let selector_next = read_tool(
        &mut client,
        json!({"selector":"paged_symbol","max_body_lines":1,"leading_lines":0,
        "source_offset":first["source_preview"]["next_source_offset"],"generation":first["generation"]}),
    );
    assert_eq!(next["source"], selector_next["source"]);
    assert_eq!(next["source_preview"]["body_offset"], 1, "{next}");
    assert_eq!(next["generation"], first["generation"]);
    let response = read_tool(
        &mut client,
        json!({"paths":["values.py"], "selector":"value", "output_file":"exports/snippets.txt"}),
    );
    let source = response["batch"][0]["source"].as_str().unwrap();
    let exported = std::fs::read_to_string(w.path("exports/snippets.txt")).unwrap();
    assert_eq!(exported, format!("### values.py\n\n{source}"));
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_validates_ranges_and_argument_names_at_the_mcp_seam() {
    let mut lines_content = String::new();
    for i in 1..=30 {
        lines_content.push_str(&format!("line {i}\n"));
    }
    let workspace = Workspace::with_files(&[
        ("acdd.yaml", "profile: minimal\n"),
        ("notes.txt", lines_content.as_str()),
        ("src/lib.rs", "pub fn value() {}\n"),
        ("test.txt", "hello\n"),
    ]);
    let disabled_build = contextunity_acdd::engine::pipeline::build(
        workspace.root(),
        &workspace.path(".acdd/code-map.sqlite"),
        None,
    )
    .unwrap_err();
    assert_eq!(
        acdd_core::errors::StateError::from_anyhow(&disabled_build)
            .expect("FeatureDisabled StateError")
            .code,
        acdd_core::errors::ErrorCode::FeatureDisabled
    );
    assert!(!workspace.path(".acdd/code-map.sqlite").exists());
    let mut client = McpTestHarness::new(&workspace).expect("MCP initialize");
    for lines in [
        "",
        "garbage",
        "2-1",
        "0-2",
        "1-x",
        "1-2-3",
        "1..",
        "-2",
        "0",
        "18446744073709551616",
    ] {
        let response = read_response(
            &mut client,
            json!({"path":"notes.txt", "lines":lines, "line_count":1}),
        );
        assert_eq!(response["result"]["isError"], true, "{lines}: {response}");
        assert!(response.to_string().contains("lines"), "{response}");
    }
    for (arguments, expected) in [
        (
            json!({"path":"notes.txt", "start_line":2, "end_line":1}),
            "end_line",
        ),
        (
            json!({"path":"src/lib.rs", "start_line":usize::MAX, "line_count":2}),
            "bounds",
        ),
        (
            json!({"path":"src/lib.rs", "start_line":i64::MAX as usize, "line_count":2}),
            "bounds",
        ),
        (
            json!({"path":"src/lib.rs", "start_line":2, "line_count":usize::MAX}),
            "bounds",
        ),
    ] {
        let details = arguments.to_string();
        let response = read_response(&mut client, arguments);
        assert_eq!(response["result"]["isError"], true, "{details}: {response}");
        assert!(
            response.to_string().contains(expected),
            "{details}: {response}"
        );
    }
    for (arguments, expected) in [
        (
            json!({"path":"notes.txt", "start_line":5, "end_line":10}),
            "5: line 5\n6: line 6\n7: line 7\n8: line 8\n9: line 9\n10: line 10\n",
        ),
        (
            json!({"path":"notes.txt", "lines":"12-15"}),
            "12: line 12\n13: line 13\n14: line 14\n15: line 15\n",
        ),
        (
            json!({"path":"notes.txt", "lines":"20..22"}),
            "20: line 20\n21: line 21\n22: line 22\n",
        ),
        (
            json!({"path":"notes.txt", "start_line":1, "lines":3}),
            "1: line 1\n2: line 2\n3: line 3\n",
        ),
        (
            json!({"path":"notes.txt", "start_line":25, "max_lines":2}),
            "25: line 25\n26: line 26\n",
        ),
    ] {
        let result = read_tool(&mut client, arguments);
        assert_eq!(result["content"], expected, "{result}");
    }

    let rejected = read_response(&mut client, json!({"path":"test.txt", "line_count_max":50}));
    assert_eq!(rejected["result"]["isError"], true);
    let rejected_json: Value =
        serde_json::from_str(rejected["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(rejected_json["error"]["code"], "rejected_arguments");
    assert_eq!(rejected_json["error"]["fields"], json!(["line_count_max"]));
    let msg = rejected_json["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("Valid supported parameters for 'acdd_read'"),
        "expected parameter list in {msg}"
    );
    assert!(
        msg.contains("path") && msg.contains("start_line") && msg.contains("line_count"),
        "expected core params in {msg}"
    );
    assert!(
        msg.contains("Did you mean:"),
        "expected suggestion in {msg}"
    );
    assert!(
        rejected_json["error"]["supported_parameters"].is_array(),
        "expected supported_parameters list"
    );
    let target_rejected = read_response(&mut client, json!({"target": "test.txt"}));
    assert_eq!(target_rejected["result"]["isError"], true);
    assert!(
        target_rejected["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("'path' or 'selector'"),
        "{target_rejected}"
    );
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_core_modes_and_cli_share_bounded_text_and_exports() {
    use crate::cli::run_cli_json;
    let linked = Workspace::with_files(&[("lib.rs", "pub fn linked() { /* needle */ }\n")]);
    let w = Workspace::with_files(&[
        (
            "acdd.yaml",
            &format!(
                "profile: minimal\nroots: [src]\nlinked_workspaces:\n  - name: extra\n    path: {}\n",
                linked.root().display()
            ),
        ),
        ("notes.txt", "first\r\nПривіт 42\r\nthird\r\nfourth\r\n"),
        ("src/alpha.rs", "pub fn alpha() -> &'static str {\n    \"needle \\\"quoted\\\"\"\n}\n"),
        ("src/nested/beta.rs", "pub fn beta() {\n    // needle in nested file\n}\n"),
        ("src/notes.txt", "needle in non-rust file\n"),
        (".env", "SECRET=needle\n"),
    ]);
    std::fs::write(
        w.path("src/m_bad_utf8.bin"),
        [0xff_u8, 0xfe, b'n', b'e', b'e', b'd'],
    )
    .unwrap();
    let mut client = McpTestHarness::new(&w).expect("MCP initialize");
    for (mode, extra, expected) in [
        (
            "lines",
            json!({}),
            "1: first\r\n2: Привіт 42\r\n3: third\r\n4: fourth\r\n",
        ),
        (
            "raw",
            json!({"start_line":2,"line_count":1}),
            "Привіт 42\r\n",
        ),
        (
            "lines",
            json!({"start_line":2,"line_count":1}),
            "2: Привіт 42\r\n",
        ),
        (
            "search",
            json!({"pattern":"Привіт","context":0}),
            "2: Привіт 42\r\n",
        ),
        (
            "search",
            json!({"pattern":"[0-9]+$","regex":true,"context":0}),
            "",
        ),
        (
            "search",
            json!({"pattern":"[0-9]+","regex":true,"context":0}),
            "2: Привіт 42\r\n",
        ),
    ] {
        let mut args = extra;
        args["path"] = json!("notes.txt");
        args["mode"] = json!(mode);
        let result = read_tool(&mut client, args);
        assert_eq!(result["mode"], mode, "{result}");
        assert_eq!(result["content"], expected, "{result}");
        assert!(result.get("Hints").is_none(), "{result}");
    }
    let args = json!({"path":"notes.txt","mode":"raw","start_line":2,"line_count":1,"output_file":"exports/read.txt"});
    let mcp = read_tool(&mut client, args);
    assert_eq!(
        std::fs::read_to_string(w.path("exports/read.txt")).unwrap(),
        "Привіт 42\r\n"
    );
    let output = run_cli_json(
        &w,
        &[
            "read",
            "notes.txt",
            "--mode",
            "raw",
            "--start-line",
            "2",
            "--line-count",
            "1",
            "--output-file",
            "exports/read.txt",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        mcp
    );
    let (_, old) = client
        .call(
            &["get_", "code_snippet"].concat(),
            json!({"selector":"notes.txt"}),
        )
        .expect("MCP transport");
    assert_eq!(old["result"]["isError"], true);
    let (_, md_resp) = client
        .call(
            "acdd_read",
            json!({"path": "src/alpha.rs", "mode": "lines"}),
        )
        .expect("MCP transport");
    let md_text = md_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(md_text.contains("```rust\n1: pub fn alpha()"), "{md_text}");
    assert!(
        !md_text.contains("\\n") && !md_text.contains("Hints:"),
        "{md_text}"
    );

    for (args, expected, forbidden) in [
        (
            json!({"path": "src", "mode": "search", "pattern": "needle", "context": 0}),
            "src/nested/beta.rs:2:",
            ".env",
        ),
        (
            json!({"path": "src/**/*.rs", "mode": "search", "pattern": "needle", "context": 0}),
            "src/alpha.rs:2:",
            "src/notes.txt",
        ),
        (
            json!({"mode": "search", "pattern": "needle", "context": 0}),
            "src/alpha.rs:2:",
            "SECRET=needle",
        ),
    ] {
        let res = read_tool(&mut client, args);
        let content = res["content"].as_str().unwrap();
        assert!(
            content.contains(expected) && !content.contains(forbidden),
            "{content}"
        );
    }

    for export_args in [
        json!({"path": "src", "mode": "search", "pattern": "needle", "line_count": 1, "output_file": "src/notes.txt"}),
        json!({"paths": ["src/**/*.rs"], "mode": "search", "pattern": "needle", "output_file": "src/alpha.rs"}),
    ] {
        let (_, err) = client
            .call("acdd_read", export_args)
            .expect("MCP transport");
        assert_eq!(err["result"]["isError"], true, "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(w.path("src/notes.txt")).unwrap(),
        "needle in non-rust file\n"
    );

    let linked_res = read_tool(
        &mut client,
        json!({"path": "[extra]/lib.rs", "mode": "search", "pattern": "needle", "context": 0}),
    );
    assert!(
        linked_res["content"].as_str().unwrap().starts_with("1: "),
        "{linked_res}"
    );
    assert!(!w.db().exists(), "core reads must not create the index");
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_handles_path_aliases_and_rejects_unsafe_paths() {
    let linked = Workspace::with_files(&[("secrets/token.txt", "linked-secret\n")]);
    let w = Workspace::with_files(&[
        (
            "acdd.yaml",
            &format!("profile: minimal\nsensitive: ['secrets/**', '.env*']\nlinked_workspaces:\n  - name: linked\n    path: {}\n", linked.root().display()),
        ),
        ("notes.txt", "safe\n"),
        ("small.txt", "inside\n"),
        ("data.txt", "alpha\nbeta\ngamma needle\ndelta\n"),
        ("docs/guide.md", "# Guide\n"),
        ("secrets/token.txt", "secret\n"),
        (".env.local", "secret\n"),
    ]);
    w.write_bytes("binary.dat", b"abc\0def");
    #[cfg(unix)]
    let outside = Workspace::new();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(w.path("notes.txt"), outside.path("alias.txt")).unwrap();
        std::os::unix::fs::symlink(w.root(), outside.path("alias-dir")).unwrap();
    }
    let absolute_data_path = w.root().join("data.txt").display().to_string();
    let parent_absolute = format!(
        "/tmp/../{}/notes.txt",
        w.root().strip_prefix("/").unwrap().display()
    );
    let sibling = format!("{}small.txt", w.root().display());
    let mut client = McpTestHarness::new(&w).expect("MCP initialize");

    for (arguments, path, content) in [
        (
            json!({"file_path":"data.txt", "start_line":1, "line_count":2}),
            "data.txt",
            "1: alpha\n2: beta\n",
        ),
        (
            json!({"path":absolute_data_path.clone(), "start_line":1, "line_count":1}),
            "data.txt",
            "1: alpha\n",
        ),
        (
            json!({"file_path":absolute_data_path, "start_line":2, "line_count":1}),
            "data.txt",
            "2: beta\n",
        ),
    ] {
        let result = read_tool(&mut client, arguments);
        assert_eq!(result["path"], path);
        assert_eq!(result["content"], content);
    }
    let search = read_tool(
        &mut client,
        json!({"path":"data.txt", "mode":"search", "query":"needle"}),
    );
    assert!(search["content"].as_str().unwrap().contains("gamma needle"));

    let mut rejected_paths = vec![
        (json!({"path":"../notes.txt"}), Some("relative")),
        (json!({"path":parent_absolute}), Some("relative")),
        (json!({"path":sibling}), None),
        (json!({"path":"/etc/passwd"}), Some("relative")),
        (
            json!({"path":"secrets/token.txt","output_file":"leaked.txt"}),
            Some("SENSITIVE_FILE"),
        ),
        (json!({"path":".env.local"}), Some("SENSITIVE_FILE")),
        (
            json!({"path":"binary.dat","output_file":"leaked.txt"}),
            Some("BINARY_FILE"),
        ),
        (
            json!({"path":"notes.txt","output_file":"../escaped.txt"}),
            Some("relative"),
        ),
        (
            json!({"path":"notes.txt","output_file":".env.export"}),
            Some("SENSITIVE_FILE"),
        ),
        (
            json!({"path":"notes.txt","mode":"snippet"}),
            Some("FEATURE_DISABLED"),
        ),
        (
            json!({"path":"notes.txt","mode":"outline"}),
            Some("FEATURE_DISABLED"),
        ),
        (
            json!({"path":"docs/guide.md","mode":"outline"}),
            Some("FEATURE_DISABLED"),
        ),
        (
            json!({"path":"notes.txt","mode":"search","pattern":"[","regex":true}),
            Some("regex"),
        ),
    ];
    #[cfg(unix)]
    {
        for path in [
            outside.path("alias.txt"),
            outside.path("alias-dir/notes.txt"),
        ] {
            rejected_paths.push((json!({"path":path}), None));
        }
        std::os::unix::fs::symlink(w.path("notes.txt"), w.path("alias.txt")).unwrap();
        rejected_paths.extend([
            (json!({"path":"alias.txt"}), None),
            (json!({"path":"notes.txt","output_file":"alias.txt"}), None),
        ]);
    }
    for (arguments, expected_error) in rejected_paths {
        let response = read_response(&mut client, arguments);
        assert_eq!(response["result"]["isError"], true, "{response}");
        if let Some(expected_error) = expected_error {
            assert!(response.to_string().contains(expected_error), "{response}");
        }
    }
    assert!(!w.path("leaked.txt").exists());
    std::fs::hard_link(w.path("notes.txt"), w.path("notes-hardlink.txt"))
        .expect("create hardlink alias");
    for args in [
        json!({"path": "notes.txt", "mode": "raw", "output_file": "notes-hardlink.txt"}),
        json!({"paths": ["notes.txt"], "mode": "outline", "output_file": "notes-hardlink.txt"}),
    ] {
        let response = read_response(&mut client, args);
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    assert_eq!(
        std::fs::read_to_string(w.path("notes.txt")).expect("read notes.txt"),
        "safe\n"
    );
    if acdd_core::fs::is_case_insensitive_volume(w.root()) {
        for args in [
            json!({"path": "notes.txt", "mode": "raw", "output_file": "NOTES.TXT"}),
            json!({"paths": ["notes.txt"], "mode": "outline", "output_file": "NOTES.TXT"}),
            json!({"path": ".ENV.local", "mode": "raw"}),
            json!({"paths": [".ENV.local"], "mode": "outline"}),
            json!({"path": "[linked]/SECRETS/token.txt", "mode": "raw"}),
        ] {
            let response = read_response(&mut client, args);
            assert_eq!(response["result"]["isError"], true, "{response}");
        }
        assert_eq!(
            std::fs::read_to_string(w.path("notes.txt")).expect("notes intact"),
            "safe\n"
        );
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_delta_uses_the_admitted_linked_git_context() {
    use crate::cli::run_cli_json;
    let linked = Workspace::with_files(&[]);
    let primary = Workspace::with_files(&[(
        "acdd.yaml",
        &format!(
            "profile: minimal\nsensitive: ['**/secrets/**']\nlinked_workspaces:\n  - name: linked\n    path: {}\n",
            linked.root().display()
        ),
    )]);
    let primary_paths = [
        ":(glob)**",
        "*",
        "?",
        "[ab]",
        ":(literal)secrets/token.txt",
        "--option.txt",
    ];
    for (index, path) in primary_paths.iter().enumerate() {
        primary.write(path, &format!("safe-before-{index}\n"));
    }
    primary.write("secrets/token.txt", "old-secret\n");
    let paths = ["nested/notes.txt", "nested/:(glob)**"];
    for path in paths {
        linked.write(path, "linked-before\n");
        primary.write(format!("[linked]/{path}"), "decoy-before\n");
    }
    linked.write("nested/secrets/token.txt", "secret-before\n");
    primary.seed_git().expect("seed primary Git baseline");
    linked.seed_git().expect("seed linked Git baseline");
    primary
        .create_git_tags(&["primary-only"])
        .expect("tag primary baseline");
    linked
        .create_git_tags(&["linked-only"])
        .expect("tag linked baseline");
    for path in paths {
        linked.write(path, "linked-after\n");
        primary.write(format!("[linked]/{path}"), "decoy-after\n");
    }
    linked.write("nested/secrets/token.txt", "secret-after\n");
    for (index, path) in primary_paths.iter().enumerate() {
        primary.write(path, &format!("safe-after-{index}\n"));
    }
    primary.write("secrets/token.txt", "new-secret\n");
    let mut client = McpTestHarness::new(&primary).expect("MCP initialize");
    let mut failures = Vec::new();
    let mut cases: Vec<_> = primary_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                (*path).to_owned(),
                Some("HEAD"),
                format!("safe-before-{index}"),
                format!("safe-after-{index}"),
            )
        })
        .collect();
    for path in paths {
        for baseline in [None, Some("linked-only")] {
            cases.push((
                format!("[linked]/{path}"),
                baseline,
                "linked-before".to_owned(),
                "linked-after".to_owned(),
            ));
        }
    }
    for (index, (path, baseline, before, after)) in cases.into_iter().enumerate() {
        let raw = read_tool(&mut client, json!({"path":path,"mode":"raw"}));
        assert_eq!(raw["content"], format!("{after}\n"));
        let export = format!("exports/delta-{index}.txt");
        let mut request = json!({"path":path,"mode":"delta","output_file":export});
        let mut args = vec!["read", "--mode", "delta"];
        if let Some(baseline) = baseline {
            request["baseline"] = json!(baseline);
            args.extend(["--baseline", baseline]);
        }
        args.extend(["--", &path]);
        let result = read_tool(&mut client, request);
        let content = result["content"].as_str().unwrap();
        assert!(
            content.contains(&format!("-{before}")) && content.contains(&format!("+{after}")),
            "{result}"
        );
        for forbidden in ["decoy-", "secret-", "old-secret", "new-secret"] {
            assert!(
                !content.contains(forbidden),
                "sensitive sibling or decoy leaked: {result}"
            );
        }
        assert_eq!(content.matches("diff --git ").count(), 1, "{result}");
        assert_eq!(
            std::fs::read_to_string(primary.path(export)).unwrap(),
            content
        );
        let output = run_cli_json(&primary, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            cli["content"], result["content"],
            "CLI/MCP delta diverged for {path}"
        );
    }
    for baseline in ["primary-only", "--output=escaped", "missing-revision"] {
        let (_, response) = client
            .call(
                "acdd_read",
                json!({"path":"[linked]/nested/notes.txt","mode":"delta","baseline":baseline}),
            )
            .expect("MCP transport");
        let output = run_cli_json(
            &primary,
            &[
                "read",
                "[linked]/nested/notes.txt",
                "--mode",
                "delta",
                &format!("--baseline={baseline}"),
            ],
        );
        if response["result"]["isError"] != true || output.status.success() {
            failures.push(format!(
                "unavailable/unsafe baseline accepted: {baseline}: {response}"
            ));
        }
    }
    let (_, response) = client
        .call(
            "acdd_read",
            json!({"path":"[linked]/nested/secrets/token.txt","mode":"delta"}),
        )
        .expect("MCP transport");
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(response.to_string().contains("SENSITIVE_FILE"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_codemap_hints_and_outline_use_verified_graph_signals() {
    use crate::cli::run_cli_json;
    let cases = [("distinct.py", "Second"), ("repeated.py", "First")];
    let mut files: Vec<_> = cases
        .iter()
        .map(|(path, second)| {
            (
                *path,
                format!("class First:\n    def value(self):\n        return 1\n\nclass {second}:\n    def value(self):\n        return 2\n"),
            )
        })
        .collect();
    files.push((
        "calculator.py",
        "def compute_total(a, b):\n    x = a * 2\n    y = b * 3\n    return x + y\n\nclass TaxCalculator:\n    def calculate_tax(self, amount):\n        return amount * 0.2\n".to_owned(),
    ));
    files.extend([
        ("exact.py", "def exact():\n    return 1\n".to_owned()),
        (
            "nested/exact.py",
            "def shadow():\n    x = 1\n    y = 2\n    return x + y\n".to_owned(),
        ),
    ]);
    files.extend([
        (
            "acdd.yaml",
            "profile: minimal\nmodules: {codemap: true}\nroots: [.]\n",
        ),
        (
            "model.py",
            "class Base:\n    pass\n\nclass Child(Base):\n    pass\n\ndef standalone():\n    pass\n\nclass InvalidBase(standalone):\n    pass\n",
        ),
        (
            "service.py",
            "def compute():\n    return 42\n\ndef invoke():\n    return compute()\n",
        ),
        (
            "tests/test_service.py",
            "from service import compute\ndef test_compute():\n    assert compute() == 42\n",
        ),
    ].map(|(path, source)| (path, source.to_owned())));
    let file_refs: Vec<_> = files
        .iter()
        .map(|(path, source)| (*path, source.as_str()))
        .collect();
    let (w, harness) = McpTestHarness::indexed_workspace(&file_refs);
    let mut client = harness.expect("MCP initialize");
    for (arguments, expected) in [
        (
            json!({"path":"model.py","mode":"outline","include_hints":true}),
            false,
        ),
        (
            json!({"path":"model.py","mode":"lines","include_hints":true}),
            false,
        ),
        (
            json!({"path":"model.py","mode":"raw","include_hints":true}),
            false,
        ),
        (
            json!({"path":"model.py","mode":"search","pattern":"class","include_hints":true}),
            false,
        ),
        (json!({"selector":"model.py","include_hints":true}), false),
        (json!({"selector":"Base","include_hints":true}), true),
        (json!({"selector":"Child","include_hints":true}), false),
        (json!({"selector":"standalone","include_hints":true}), false),
    ] {
        let read = read_tool(&mut client, arguments);
        let hints = read["Hints"].as_str().unwrap_or("");
        let implementation = hints
            .split(';')
            .find(|call| call.contains("operation: \"implementations\""));
        assert_eq!(implementation.is_some(), expected, "{read}");
        if let Some(call) = implementation {
            let selector = call
                .split("selector: ")
                .nth(1)
                .unwrap()
                .split(", worktree:")
                .next()
                .unwrap();
            let selector: String = serde_json::from_str(selector).unwrap();
            let followed = client
                .payload(
                    "code_map_query",
                    json!({"operation":"implementations","selector":selector,"worktree":w.root()}),
                )
                .expect("MCP tool should succeed");
            assert_eq!(followed["nodes"]["total"], 1, "{followed}");
        }
    }
    for args in [
        vec!["read", "model.py", "--mode", "outline", "--include-hints"],
        vec!["read", "model.py", "--selector", "Base", "--include-hints"],
    ] {
        let output = run_cli_json(&w, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let read: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            read["Hints"]
                .as_str()
                .unwrap_or("")
                .contains("operation: \"implementations\""),
            args.contains(&"Base"),
            "{read}"
        );
    }

    let outline = read_tool(&mut client, json!({"path":"service.py","mode":"outline"}));
    assert!(
        outline["content"].as_str().unwrap().contains("compute"),
        "{outline}"
    );
    assert!(
        !outline["content"].as_str().unwrap().contains("return 42"),
        "{outline}"
    );
    // Default acdd_read suppresses Hints; include_hints=true opts in.
    let no_hints = read_tool(&mut client, json!({"selector":"compute"}));
    assert!(no_hints.get("Hints").is_none(), "{no_hints}");
    let cli_read_out = run_cli_json(&w, &["read", "service.py", "--selector", "compute"]);
    assert!(
        cli_read_out.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_read_out.stderr)
    );
    let cli_read: serde_json::Value = serde_json::from_slice(&cli_read_out.stdout).unwrap();
    assert_eq!(cli_read["node"], no_hints["node"], "{cli_read}");
    let cli_inspect_out = run_cli_json(&w, &["query", "inspect", "compute"]);
    assert!(
        cli_inspect_out.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_inspect_out.stderr)
    );
    let cli_inspect: serde_json::Value = serde_json::from_slice(&cli_inspect_out.stdout).unwrap();
    assert!(cli_inspect.get("coverage").is_none(), "{cli_inspect}");
    let snippet = read_tool(
        &mut client,
        json!({"selector":"compute","include_hints":true}),
    );
    assert_eq!(snippet["source"], "def compute():\n    return 42\n");
    let hints = snippet["Hints"].as_str().unwrap();
    assert!(
        hints.contains("code_map_explain(") && hints.contains("code_map_tests("),
        "{snippet}"
    );
    assert!(
        !hints.contains("get_doc") && !hints.contains("task_"),
        "{snippet}"
    );
    assert!(hints.lines().count() <= 3);
    assert!(snippet["graph_pulse"]["callers"].as_u64().unwrap() >= 2);
    assert_eq!(snippet["graph_pulse"]["covering_tests"], 1);
    for (path, second) in cases {
        for (line, expected) in [(3, "return 1"), (7, "return 2")] {
            let response = read_tool(
                &mut client,
                json!({"path":path, "start_line":line, "line_count":1}),
            );
            assert_eq!(response["path"], path, "{response}");
            assert_eq!(response["content"], format!("{line}:         {expected}\n"));
            let footer = response["footer"].as_str().unwrap();
            let selector = footer
                .split("selector=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            let snippet = read_tool(&mut client, json!({"path":path, "selector":selector}));
            assert_eq!(snippet["node"]["path"], path, "{snippet}");
            let (leading, class) = if line == 3 {
                ("", "First")
            } else {
                ("\n", second)
            };
            assert_eq!(
                snippet["source"],
                format!("{leading}class {class}:\n    def value(self):\n        {expected}\n"),
                "{snippet}"
            );
        }
    }

    let response = read_tool(
        &mut client,
        json!({"path":"calculator.py", "start_line":2, "line_count":2}),
    );
    assert_eq!(response["mode"], "lines");
    let footer = response["footer"]
        .as_str()
        .expect("footer should be present");
    assert!(
        footer.contains("💡 Enclosing symbol: 'fn compute_total'"),
        "unexpected footer: {footer}"
    );
    assert!(
        footer.contains("acdd_read(path=\"calculator.py\", selector=\"compute_total\")"),
        "unexpected footer: {footer}"
    );

    let (_, markdown_response) = client
        .call(
            "acdd_read",
            json!({"path":"calculator.py", "start_line":2, "line_count":2}),
        )
        .expect("MCP transport");
    let markdown = markdown_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        markdown.contains("💡 Enclosing symbol: 'fn compute_total'"),
        "unexpected markdown: {markdown}"
    );

    let response = read_tool(
        &mut client,
        json!({"path":"calculator.py", "start_line":8, "line_count":1}),
    );
    let footer = response["footer"]
        .as_str()
        .expect("footer should be present for method");
    assert!(
        footer.contains("💡 Enclosing symbol: 'fn calculate_tax'"),
        "unexpected footer: {footer}"
    );
    assert!(
        footer.contains("selector=\"calculate_tax\""),
        "unexpected footer: {footer}"
    );
    for line_count in [1, 3] {
        let response = read_tool(
            &mut client,
            json!({"path":"exact.py", "start_line":2, "line_count":line_count}),
        );
        let footer = response["footer"].as_str().expect("enclosing symbol");
        assert!(footer.contains("fn exact"), "{response}");
    }
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_docs_outline_keeps_hierarchy_and_filters_disabled_navigation() {
    let mut client = McpTestHarness::indexed(&[
        (
            "acdd.yaml",
            "profile: minimal\nmodules: {docs: true}\nroots: [.]\ndocs: [docs]\n",
        ),
        (
            "docs/guide.md",
            "# Guide\n\nText\n\n## Setup\n\n### Install\n\n```md\n## Code example\n```\n",
        ),
        ("service.py", "def compute():\n    return 42\n"),
    ])
    .expect("MCP initialize");
    let result = read_tool(
        &mut client,
        json!({"path":"docs/guide.md","mode":"outline","include_hints":true}),
    );
    let outline = result["content"].as_str().unwrap();
    assert!(
        outline.contains("# Guide")
            && outline.contains("## Setup")
            && outline.contains("### Install"),
        "{result}"
    );
    assert!(!outline.contains("Code example"), "{result}");
    let hints = result["Hints"].as_str().unwrap();
    assert!(
        hints.contains("get_doc(") && hints.contains("search_docs("),
        "{result}"
    );
    assert!(
        !hints.contains("code_map_") && !hints.contains("task_"),
        "{result}"
    );
    let (_, rejected) = client
        .call("acdd_read", json!({"path":"service.py","mode":"outline"}))
        .expect("MCP transport");
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    assert!(
        rejected.to_string().contains("FEATURE_DISABLED"),
        "{rejected}"
    );
    client.finish().expect("server must finish successfully");
}

#[test]
fn acdd_read_acdd_badges_and_hints_follow_the_unique_claimed_worktree() {
    for with_command in [false, true] {
        let gate_def = if with_command {
            "          - id: build\n            role: builder\n            proof: command\n            command: reader-check\n"
        } else {
            "          - id: build\n            role: builder\n            proof: none\n"
        };
        let profile_yaml = format!(
            "version: 2\n\
             git:\n  isolation: in_place\n  worktrees: manual\n\
             roles:\n  builder:\n    mode: subagent\n\
             commands:\n  reader-check:\n    command: \"true\"\n\
             gates:\n  - tasks:\n      gates:\n{gate_def}"
        );
        let w = Workspace::with_files(&[
            (
                "acdd.yaml",
                "profile: full\nmodules: {code: true, docs: true, state: true, blackboard: true}\nroots: [.]\ndocs: [docs]\nspecs:\n  milestones:\n    path: docs/milestones\n",
            ),
            (".acdd/profile.yaml", &profile_yaml),
            ("allowed.txt", "allowed\n"),
            ("other.txt", "other\n"),
            (
                "docs/milestones/010-reader.md",
                "---\nid: m-reader\ntitle: Reader\ndoc_type: milestone\nstatus: active\n---\n\n### task: read\n```yaml\ntask_ref: read\ntarget: Read scoped text\nscope: [allowed.txt]\n```\n",
            ),
        ]);
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["add", "-A"],
            vec!["commit", "-q", "-m", "init"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(&args)
                    .current_dir(w.root())
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        }
        let start = crate::cli::run_cli_json(&w, &["start", "docs/milestones/010-reader.md", "--in-place"]);
        assert!(
            start.status.success(),
            "start failed: {}\n{}",
            String::from_utf8_lossy(&start.stdout),
            String::from_utf8_lossy(&start.stderr)
        );
        let claim = crate::cli::run_cli_json(
            &w,
            &["claim", "read.build", "--worker-id", "reader-build"],
        );
        assert!(
            claim.status.success(),
            "claim failed: {}\n{}",
            String::from_utf8_lossy(&claim.stdout),
            String::from_utf8_lossy(&claim.stderr)
        );
        let mut client = McpTestHarness::new(&w).expect("MCP initialize");
        let allowed = read_tool(&mut client, json!({"path":"allowed.txt"}));
        assert_eq!(allowed["in_write_scope"], "ALLOWED", "{allowed}");
        let outside_default = read_tool(&mut client, json!({"path":"other.txt"}));
        assert!(outside_default.get("Hints").is_none(), "{outside_default}");
        let outside = read_tool(
            &mut client,
            json!({"path":"other.txt","include_hints":true}),
        );
        assert_eq!(outside["in_write_scope"], "OUT_OF_SCOPE", "{outside}");
        let hints = outside["Hints"].as_str().unwrap();
        assert!(
            hints.contains("extend_scope") && hints.contains("acdd_blackboard"),
            "{outside}"
        );
        assert!(
            !hints.contains("code_map_") && !hints.contains("get_doc"),
            "{outside}"
        );
        assert_eq!(
            hints.contains("acdd_verify("),
            with_command,
            "{outside}"
        );
        client.finish().expect("server must finish successfully");
    }
}

#[test]
fn minimal_subsystems_filter_tools_and_check_batch_outline_admission() {
    let mut registry = SubsystemRegistry {
        profile: SubsystemPreset::Minimal,
        ..Default::default()
    };
    assert_eq!(
        filter_subsystem_tools(["acdd_read", "get_doc", "code_map_explain"], &registry),
        ["acdd_read"]
    );
    // Minimal has no docs, no codemap
    let req = ReadRequest {
        mode: ReadMode::Outline,
        paths: Some(vec!["README.md".into(), "src/lib.rs".into()]),
        ..Default::default()
    };
    // Should fail because neither subsystem is enabled
    assert!(req
        .requires_index(&acdd_core::config::Adapter {
            modules: registry.clone(),
            ..Default::default()
        })
        .is_err());

    // Enable docs only
    registry.enabled.insert("docs".into());
    let adapter_docs = acdd_core::config::Adapter {
        modules: registry.clone(),
        ..Default::default()
    };
    // Batch with only markdown paths succeeds with docs enabled
    let req_md = ReadRequest {
        mode: ReadMode::Outline,
        paths: Some(vec!["README.md".into(), "GUIDE.markdown".into()]),
        ..Default::default()
    };
    assert!(req_md.requires_index(&adapter_docs).is_ok());

    // Batch with mixed markdown and code paths must fail because code requires codemap
    let req_mixed = ReadRequest {
        mode: ReadMode::Outline,
        paths: Some(vec!["README.md".into(), "src/lib.rs".into()]),
        ..Default::default()
    };
    assert!(req_mixed.requires_index(&adapter_docs).is_err());

    // Enable codemap as well -> now both docs and codemap are enabled, mixed paths succeed
    registry.enabled.insert("codemap".into());
    let adapter_both = acdd_core::config::Adapter {
        modules: registry,
        ..Default::default()
    };
    assert!(req_mixed.requires_index(&adapter_both).is_ok());
}
