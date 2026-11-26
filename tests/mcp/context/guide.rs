use crate::{
    common::cli::{run_cli, run_cli_json},
    harness::McpTestHarness,
};
use acdd_core::response::MAX_OUTPUT_BYTES;
use contextunity_acdd::engine::guide;
use serde_json::{json, Value};

const GUIDANCE_TOPICS: [&str; 6] = ["lifecycle", "acdd", "gates", "subtasks", "code", "docs"];

#[test]
fn guide_configuration_matrix() {
    let (workspace, harness) = McpTestHarness::indexed_workspace(&[]);
    let mut harness = harness.unwrap();
    workspace.write("acdd.yaml", "roots: [\n");
    assert!(guide::run(workspace.root(), "init", false).is_err());
    assert_eq!(
        harness
            .call("acdd_guide", json!({"topic":"init"}))
            .unwrap()
            .1["result"]["isError"],
        true
    );
    assert!(!run_cli_json(&workspace, &["guide", "init"])
        .status
        .success());
    assert_eq!(
        std::fs::read_to_string(workspace.path("acdd.yaml")).unwrap(),
        "roots: [\n"
    );
    let response = harness
        .call("acdd_guide", json!({"topic":"init","force":true}))
        .unwrap()
        .1;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let mcp_written = std::fs::read_to_string(workspace.path("acdd.yaml")).unwrap();
    assert!(mcp_written.contains("roots:"), "{mcp_written}");
    assert!(mcp_written.contains("specs:"), "{mcp_written}");
    workspace.write("acdd.yaml", "roots: [\n");
    let cli = run_cli_json(&workspace, &["guide", "init", "--force"]);
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path("acdd.yaml")).unwrap(),
        mcp_written
    );
    harness.finish().unwrap();
    workspace.write(
        "configs/guide.yaml",
        "modules:\n  codemap: false\n  docs: false\n  acdd: true\n  blackboard: false\n",
    );
    let output = run_cli_json(
        &workspace,
        &["--config", "configs/guide.yaml", "guide", "query"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let query: Value = serde_json::from_slice(&output.stdout).unwrap();
    let topics = query["topics"].as_array().unwrap();
    assert!(!topics.contains(&json!("code")));
    assert!(!topics.contains(&json!("docs")));
    assert!(topics.contains(&json!("lifecycle")));
    let output = run_cli_json(
        &workspace,
        &["--config", "configs/guide.yaml", "guide", "lifecycle"],
    );
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("task_blackboard"));
    let default = guide::run(workspace.root(), "query", false).unwrap();
    assert!(default["topics"]
        .as_array()
        .unwrap()
        .contains(&json!("code")));
    for bits in 0u8..16 {
        let codemap = bits & 1 != 0;
        let docs = bits & 2 != 0;
        let acdd = bits & 4 != 0;
        let blackboard = bits & 8 != 0;
        workspace.write("acdd.yaml", &format!("roots: [src/]\nmodules:\n  codemap: {codemap}\n  docs: {docs}\n  acdd: {acdd}\n  blackboard: {blackboard}\n"));
        let mut harness = McpTestHarness::new(&workspace).expect("module configuration");
        let query = harness
            .payload("acdd_guide", json!({}))
            .expect("default query");
        assert_eq!(
            query,
            harness
                .payload("acdd_guide", json!({"topic":"query"}))
                .unwrap()
        );
        assert_eq!(
            query,
            harness.payload("acdd_guide", json!({"topic":" "})).unwrap()
        );
        let cli = run_cli_json(&workspace, &["guide", "query"]);
        assert!(cli.status.success());
        assert_eq!(serde_json::from_slice::<Value>(&cli.stdout).unwrap(), query);
        let topics = query["topics"].as_array();
        assert!(topics.is_some());
        let topics = topics.unwrap();
        for (topic, enabled) in [
            ("query", true),
            ("init", true),
            ("adapter", true),
            ("validate", true),
            ("code", codemap),
            ("ast", codemap),
            ("docs", docs),
            ("lifecycle", acdd),
            ("acdd", acdd),
            ("gates", acdd),
            ("subtasks", acdd),
        ] {
            assert_eq!(topics.contains(&json!(topic)), enabled, "{bits}: {topic}");
            if matches!(topic, "init" | "adapter" | "validate" | "query") {
                continue;
            }
            let value = harness
                .payload("acdd_guide", json!({"topic":topic}))
                .expect("known topic");
            let cli = run_cli_json(&workspace, &["guide", topic]);
            assert!(cli.status.success());
            assert_eq!(serde_json::from_slice::<Value>(&cli.stdout).unwrap(), value);
            assert_eq!(value == json!({}), !enabled, "{bits}: {topic}");
            if topic == "acdd" && acdd {
                assert_eq!(
                    value.get("blackboard_topics").is_some(),
                    blackboard,
                    "{bits}: blackboard_topics"
                );
            }
            if topic == "lifecycle" && acdd {
                assert_eq!(value.to_string().contains("acdd_blackboard"), blackboard);
            }
        }
        for key in [
            "start",
            "sql_schema",
            "symbols",
            "snippets",
            "tests",
            "queries",
            "diagnostics",
            "recovery",
        ] {
            assert_eq!(query.get(key).is_some(), codemap, "{bits}: {key}");
        }
        assert_eq!(query.get("documents").is_some(), docs);
        assert_eq!(query.get("pages").is_some(), codemap || docs);
        harness.finish().expect("guide cleanup");
    }
}

#[test]
fn guide_content_and_surfaces_matrix() {
    let (workspace, harness) = McpTestHarness::indexed_workspace(&[
        ("src/lib.rs", "pub fn guide_contract() {}\n"),
        ("docs/guide.md", "---\ntitle: Guide contract\ndoc_type: guide\n---\n# Guide contract\nNavigation example.\n"),
    ]);
    let mut harness = harness.expect("indexed guide workspace");

    assert!(guide::run(workspace.root(), "unknown-topic", false)
        .unwrap_err()
        .to_string()
        .contains("unknown guide topic"));
    let mcp = harness
        .call("acdd_guide", json!({"topic":"unknown-topic"}))
        .unwrap()
        .1;
    assert_eq!(mcp["result"]["isError"], true);
    assert!(mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("unknown guide topic"));
    let cli = run_cli_json(&workspace, &["guide", "unknown-topic"]);
    assert!(!cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stderr).contains("unknown guide topic"));
    let query = guide::run(workspace.root(), "query", false).unwrap();
    assert_eq!(guide::run(workspace.root(), " ", false).unwrap(), query);
    assert_eq!(
        serde_json::from_slice::<Value>(&run_cli_json(&workspace, &["guide", " "]).stdout).unwrap(),
        query
    );

    let list = harness
        .request_value("tools/list", json!({}))
        .expect("tool discovery");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let schema = tools
        .iter()
        .find(|tool| tool["name"] == "acdd_guide")
        .expect("guide tool");
    let description = schema["inputSchema"]["properties"]["topic"]["description"]
        .as_str()
        .expect("topic discovery");
    for topic in GUIDANCE_TOPICS {
        assert!(description.contains(topic), "schema must advertise {topic}");
        let value = harness
            .payload("acdd_guide", json!({"topic":topic}))
            .expect("supported guide");
        let shared = guide::run(workspace.root(), topic, false).unwrap();
        assert_eq!(value, shared);
        let cli = run_cli_json(&workspace, &["guide", topic]);
        assert!(
            cli.status.success(),
            "{}",
            String::from_utf8_lossy(&cli.stderr)
        );
        assert_eq!(serde_json::from_slice::<Value>(&cli.stdout).unwrap(), value);
        match topic {
            "lifecycle" => {
                let text = value.to_string().to_lowercase();
                for operation in [
                    "acdd next",
                    "acdd_state",
                    "acdd claim",
                    "acdd_claim",
                    "acdd verify",
                    "acdd_verify",
                    "acdd submit",
                    "acdd_submit",
                    "acdd run",
                    "acdd_blackboard",
                ] {
                    assert!(
                        text.contains(operation),
                        "missing lifecycle operation {operation}"
                    );
                }
                assert!(
                    !text.contains("acdd_state(action: \"roadmap\")")
                        && !text.contains("acdd_state(action: \"draft\")"),
                    "lifecycle guide must not instruct retired MCP acdd_state actions: {text}"
                );
                let authority = value["authority"].to_string().to_lowercase();
                for source in ["gateprofile", "acdd_claim", "authoritative"] {
                    assert!(
                        authority.contains(source),
                        "missing profile authority distinction {source}"
                    );
                }
                let runners = value["runners"].to_string().to_lowercase();
                assert!(runners.contains("acdd_run_id"));
                assert!(runners.contains("acdd_agent_session_id"));
                assert!(runners.contains("claimresp"));
                for rule in [
                    "runner",
                    "acdd_node_cursor",
                    "acdd_claim_revision",
                    "acdd_worker_id",
                    "acdd_worktree",
                    "sanitized",
                ] {
                    assert!(
                        runners.contains(rule),
                        "missing live runner boundary {rule}"
                    );
                }
                for code in [
                    "TASK_ALREADY_CLAIMED",
                    "STALE_CLAIM_REVISION",
                    "TASK_PROOF_UNOBSERVED",
                    "TASK_CANDIDATE_MISMATCH",
                    "TASK_NOT_INTEGRATED",
                ] {
                    assert!(
                        value["recovery"][code]
                            .as_str()
                            .is_some_and(|next| !next.trim().is_empty()),
                        "missing next action for {code}"
                    );
                }
            }
            "acdd" => {
                let text = value.to_string().to_lowercase();
                for term in ["gateprofile", "18 static", "domainprovider", "acdd start"] {
                    assert!(
                        text.contains(term),
                        "acdd guide must explain {term}: {text}"
                    );
                }
                assert!(value["mcp_tools"].is_object());
                assert!(
                    !text.contains("code_map_tests") && !text.contains("code_map_prove_removal")
                );
                assert!(value["blackboard_topics"].is_object());
                assert!(value["review_contours"].is_array());
            }
            "gates" => {
                let when = value["when"].to_string().to_lowercase();
                for term in ["boolean", "tag", "tasks", "task_gate", "root_gate"] {
                    assert!(when.contains(term), "when expression must explain {term}");
                }
                let transitions = value["transitions"].to_string().to_lowercase();
                for term in [
                    "pass",
                    "fail",
                    "fail_drift",
                    "max_attempts",
                    "escalated",
                    "independent_from",
                ] {
                    assert!(transitions.contains(term), "missing routing rule {term}");
                }
                let persistence = value["persistence"].to_string().to_lowercase();
                for term in [
                    "git.isolation",
                    "worktree",
                    "in_place",
                    "none",
                    "commit",
                    "immutable",
                ] {
                    assert!(
                        persistence.contains(term),
                        "missing persistence boundary {term}"
                    );
                }
                assert!(
                    persistence.contains("snapshot")
                        && persistence.contains("leaf task gates")
                        && !persistence.contains("save.pre"),
                    "persistence guide must document leaf-task snapshot save and omit save.pre: {persistence}"
                );
                let text = value["verification"].to_string().to_lowercase();
                for term in [
                    "verify",
                    "gate_traces",
                    "execution_digest",
                    "contract_revision",
                    "test_target",
                    "candidate_tree_before",
                ] {
                    assert!(
                        text.contains(term),
                        "missing observed-proof rule {term}: {text}"
                    );
                }
            }
            "subtasks" => {
                let text = value.to_string().to_lowercase();
                for term in [
                    "acdd subtask",
                    "in_progress",
                    "completed",
                    "cancelled",
                    "evidence",
                    "receipt: true",
                ] {
                    assert!(text.contains(term), "missing subtask guidance {term}");
                }
                assert_eq!(
                    value["delivery"]["terminal_statuses"],
                    json!(["completed", "cancelled"])
                );
                assert_eq!(
                    value["delivery"]["evidence_required_for"],
                    json!(["completed", "cancelled"])
                );
                let evidence = value["delivery"]["evidence_boundary"]
                    .as_str()
                    .expect("evidence explanation")
                    .to_lowercase();
                for term in ["verif", "not", "independent"] {
                    assert!(
                        evidence.contains(term) || evidence.contains("proof"),
                        "subtask evidence is stored text, not independent proof: {evidence}"
                    );
                }
            }
            _ => {}
        }
        let name = value["example"]["mcp"]["name"]
            .as_str()
            .expect("MCP example");
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .expect("real MCP tool");
        let args = value["example"]["mcp"]["arguments"]
            .as_object()
            .expect("example arguments");
        let properties = tool["inputSchema"]["properties"]
            .as_object()
            .expect("tool arguments");
        for key in args.keys() {
            assert!(properties.contains_key(key), "{name} has no {key} argument");
        }
        if let Some(required) = tool["inputSchema"]["required"].as_array() {
            for key in required {
                assert!(
                    args.contains_key(key.as_str().unwrap()),
                    "{name} example misses {key}"
                );
            }
        }
        let command = value["example"]["cli"].as_str().expect("CLI example");
        assert!(
            command.starts_with("acdd "),
            "invalid command example: {command}"
        );
    }
    for (topic, operations) in [
        (
            "code",
            &[
                "code_map_overview",
                "code_map_search",
                "code_map_inspect",
                "acdd_read",
                "code_map_explain",
                "code_map_impact",
                "code_map_query",
                "ast_grep_search",
                "candidate",
                "lexical",
                "verified",
            ][..],
        ),
        (
            "docs",
            &[
                "search_docs",
                "get_doc",
                "acdd_read",
                "docs search",
                "docs get",
            ][..],
        ),
    ] {
        let value = harness.payload("acdd_guide", json!({"topic":topic}));
        assert!(value.is_ok());
        let value = value.unwrap();
        let text = value.to_string();
        for operation in operations {
            assert!(text.contains(operation), "{topic} must cover {operation}");
        }
        if topic == "docs" {
            assert!(value["frontmatter"].is_string());
            assert!(value["invariant"].is_string());
            assert!(value["types"].is_array());
        }
        let example = &value["example"];
        let name = example["mcp"]["name"].as_str().expect("MCP example name");
        assert!(
            matches!(
                name,
                "code_map_overview" | "code_map_search" | "search_docs" | "get_doc" | "acdd_read"
            ),
            "navigation example must use a read operation"
        );
        let observed = harness
            .call(name, example["mcp"]["arguments"].clone())
            .expect("execute MCP example")
            .1;
        assert!(observed.get("error").is_none(), "{observed}");
        assert_ne!(observed["result"]["isError"], true, "{observed}");
        let words = example["cli"]
            .as_str()
            .expect("CLI example")
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(words.first().copied(), Some("acdd"));
        let output = run_cli_json(&workspace, &words[1..]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(serde_json::from_slice::<Value>(&output.stdout).is_ok());
    }

    for topic in GUIDANCE_TOPICS.into_iter().chain(["query"]) {
        let mut markdown = None;
        for format in [None, Some("markdown"), Some("json")] {
            let mut args = json!({"topic":topic});
            let mut cli_args = vec!["guide", topic];
            if let Some(format) = format {
                args["format"] = json!(format);
                cli_args.extend(["--format", format]);
            }
            let (bytes, envelope) = harness.call("acdd_guide", args).expect("guide request");
            assert!(bytes <= MAX_OUTPUT_BYTES);
            assert_ne!(envelope["result"]["isError"], true, "{topic}: {envelope}");
            let mcp = envelope["result"]["content"][0]["text"]
                .as_str()
                .expect("guide text");
            let cli = run_cli(&workspace, &cli_args);
            assert!(
                cli.status.success(),
                "{}",
                String::from_utf8_lossy(&cli.stderr)
            );
            let cli = String::from_utf8(cli.stdout).unwrap();
            if format == Some("json") {
                let mcp: Value = serde_json::from_str(mcp).expect("structured JSON");
                assert_eq!(serde_json::from_str::<Value>(&cli).unwrap(), mcp);
                if topic != "query" {
                    assert!(mcp["purpose"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()));
                    assert!(mcp["steps"].as_array().is_some_and(|s| !s.is_empty()));
                    assert!(mcp["references"].as_array().is_some_and(|s| !s.is_empty()));
                }
            } else {
                assert_eq!(cli.trim_end_matches('\n'), mcp.trim_end_matches('\n'));
                assert!(mcp.starts_with('#'), "readable Markdown, not JSON");
                if let Some(default) = &markdown {
                    assert_eq!(default, mcp);
                } else {
                    markdown = Some(mcp.to_owned());
                }
            }
        }
    }

    harness.finish().expect("guide cleanup");
}
