use crate::cli::run_cli;
use crate::common::{
    assertions::{assert_command_succeeded, assert_rpc_succeeded, mcp_payload},
    mcp_client::in_process_server,
    Workspace,
};
use crate::harness::McpTestHarness;

#[test]
fn malformed_handshake_preserves_the_real_server_failure_through_finish() {
    use crate::harness::HarnessError;
    let workspace = Workspace::with_files(&[]);
    let mut client = McpTestHarness::spawn(workspace.root(), &workspace.db(), "handshake-failure")
        .expect("real server fixture");
    let error = client
        .request("tools/list", json!({}))
        .expect_err("server must reject missing initialize");
    assert!(
        matches!(error, HarnessError::Server { phase: "serve", ref cause } if cause.contains("expect initialized request")),
        "{error}"
    );
    let finished = client
        .finish()
        .expect_err("finish must retain the same server failure");
    assert!(
        matches!(finished, HarnessError::Server { phase: "serve", ref cause } if cause.contains("expect initialized request")),
        "{finished}"
    );
}

#[test]
fn cancellation_after_the_last_response_is_reported_by_finish() {
    use crate::harness::HarnessError;
    let workspace = Workspace::with_files(&[]);
    let mut client =
        McpTestHarness::spawn(workspace.root(), &workspace.db(), "cancellation-failure")
            .expect("real server fixture");
    let initialized = client
        .request_value(
            "initialize",
            json!({
                "protocolVersion":"2025-03-26", "capabilities":{},
                "clientInfo":{"name":"in-process-test","version":"1"}
            }),
        )
        .expect("initialize real server");
    assert!(initialized.get("error").is_none(), "{initialized}");
    client
        .notify("notifications/initialized", json!({}))
        .expect("notify initialized");
    let response = client
        .request_value("tools/list", json!({}))
        .expect("real tools response");
    assert!(response["result"]["tools"].is_array(), "{response}");
    let receiver = client
        .server_control
        .take()
        .expect("server exposes its cancellation token");
    let cancellation = client.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .expect("server cancellation token should arrive")
            .expect("server cancellation channel should remain open")
    });
    cancellation.cancel();
    client.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while client
                .server_task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled service should finish");
    });
    let error = client
        .finish()
        .expect_err("late cancellation must not be green");
    assert!(
        matches!(error, HarnessError::Server { phase: "waiting", ref cause } if cause == "Cancelled"),
        "{error}"
    );
}
use serde_json::{json, Value};

#[test]
fn shared_mcp_harness_supports_server_rpc_and_cli_seams() {
    let workspace = Workspace::with_files(&[("service.py", "def entry(): return 1\n")]);
    let database = workspace.path(".acdd/index.sqlite");
    workspace.build_to(&database);

    let server =
        in_process_server(&workspace).with_cache_dir(workspace.path(".acdd/extraction-cache"));
    let stored_nodes = server
        .read(|conn| Ok(json!(crate::sqlite_fixture::stored_node_count(conn)?)))
        .unwrap();
    assert!(stored_nodes.as_i64().unwrap() > 0);

    let mut client = McpTestHarness::new(&workspace).expect("MCP initialize");
    let response = client
        .request_value("tools/list", json!({}))
        .expect("MCP transport");
    assert_rpc_succeeded(&response);
    assert!(!response["result"]["tools"].as_array().unwrap().is_empty());
    let (_, response) = client
        .call(
            "code_map_search",
            json!({"pattern":"entry","exact":true,"format":"json"}),
        )
        .expect("MCP transport");
    let payload: Value = mcp_payload(&response);
    assert_eq!(payload["nodes"]["total"], 1);
    client.finish().expect("server must finish successfully");

    let output = run_cli(&workspace, &["build"]);
    assert_command_succeeded(&output);
}

#[test]
fn mcp_claim_and_next_preserve_worker_independence_and_session_metadata() {
    for next in [false, true] {
        let workspace =
            crate::harness::session_workspace().expect("real session fixture should initialize");
        let mut builder =
            McpTestHarness::with_session(&workspace, "builder-session").expect("MCP initialize");
        builder
            .payload(
                "acdd_state",
                json!({"action":"start","spec_path":"docs/milestones/010-session.md","in_place":true}),
            )
            .expect("MCP start should succeed");
        let build_claim = builder
            .payload(
                "acdd_claim",
                json!({"node":"first.build","worker_id":"builder","worktree":workspace.root()}),
            )
            .expect("MCP claim should succeed");
        assert_eq!(
            build_claim["node_cursor"]["Task"]["gates"][0], "build",
            "{build_claim}"
        );
        builder
            .payload(
                "acdd_submit",
                json!({
                    "node":"first.build",
                    "action":"pass",
                    "worker_id":"builder",
                    "claim_revision":build_claim["claim_revision"],
                }),
            )
            .expect("MCP submit should succeed");
        let mut reviewer =
            McpTestHarness::with_session(&workspace, "review-session").expect("MCP initialize");
        if next {
            let next_out = reviewer
                .payload(
                    "acdd_state",
                    json!({"action":"next","worktree":workspace.root(),"format":"json"}),
                )
                .expect("MCP next should succeed");
            assert_eq!(next_out["waves"], json!([["first"]]), "{next_out}");
        } else {
            let same_claim = reviewer
                .payload(
                    "acdd_claim",
                    json!({"node":"first.review","worker_id":"builder","worktree":workspace.root(),"format":"json"}),
                )
                .expect("MCP claim as builder");
            let (_, same) = reviewer
                .call(
                    "acdd_submit",
                    json!({
                        "node":"first.review",
                        "action":"pass",
                        "worker_id":"builder",
                        "claim_revision":same_claim["claim_revision"],
                        "worktree":workspace.root(),
                        "format":"json",
                    }),
                )
                .expect("MCP transport");
            assert!(
                same.to_string().contains("TASK_REVIEW_NOT_INDEPENDENT"),
                "{same}"
            );
        }
        let claimed = reviewer
            .payload(
                "acdd_claim",
                json!({"node":"first.review","worker_id":"reviewer","force":!next,"steal_remote":!next,"worktree":workspace.root()}),
            )
            .expect("MCP reviewer claim should succeed");
        assert_eq!(
            claimed["node_cursor"]["Task"]["gates"][0], "review",
            "{claimed}"
        );
        let persisted = crate::sqlite_fixture::claim_session_fingerprint(
            &workspace,
            "task:first:review",
            claimed["claim_revision"].as_u64().unwrap(),
        )
        .unwrap();
        assert_eq!(persisted, "review-session");
        builder.finish().expect("server must finish successfully");
        reviewer.finish().expect("server must finish successfully");
    }
}

#[test]
fn mcp_submission_uses_claimed_worker_proof_across_transports() {
    for automatic in [false, true] {
        let workspace =
            crate::harness::session_workspace().expect("real session fixture should initialize");
        let mut builder =
            McpTestHarness::with_session(&workspace, "builder-session").expect("MCP initialize");
        builder
            .payload(
                "acdd_state",
                json!({"action":"start","spec_path":"docs/milestones/010-session.md","in_place":true}),
            )
            .expect("MCP start should succeed");
        let build_claim = builder
            .payload(
                "acdd_claim",
                json!({"node":"first.build","worker_id":"builder","worktree":workspace.root()}),
            )
            .expect("MCP claim should succeed");
        builder
            .payload(
                "acdd_submit",
                json!({
                    "node":"first.build",
                    "action":"pass",
                    "worker_id":"builder",
                    "claim_revision":build_claim["claim_revision"],
                }),
            )
            .expect("MCP submit should succeed");
        let mut reviewer =
            McpTestHarness::with_session(&workspace, "review-session").expect("MCP initialize");
        let review_claim = reviewer
            .payload(
                "acdd_claim",
                json!({"node":"first.review","worker_id":"reviewer","worktree":workspace.root()}),
            )
            .expect("MCP reviewer claim should succeed");
        if !automatic {
            reviewer
                .payload(
                    "acdd_verify",
                    json!({"node":"first.review","worker_id":"reviewer","worktree":workspace.root()}),
                )
                .expect("MCP verify should succeed");
        }
        assert_eq!(
            reviewer
                .payload(
                    "acdd_state",
                    json!({"action":"show","node":"first.review","worktree":workspace.root(),"format":"json"})
                )
                .expect("MCP show should succeed")["node"]["node_id"],
            "task:first:review"
        );
        let mut second_transport =
            McpTestHarness::with_session(&workspace, "review-session").expect("MCP initialize");
        let submitted = second_transport
            .payload(
                "acdd_submit",
                json!({
                    "node":"first.review",
                    "action":"pass",
                    "worker_id":"reviewer",
                    "claim_revision":review_claim["claim_revision"],
                    "worktree":workspace.root(),
                }),
            )
            .expect("MCP submit across transport should succeed");
        assert_eq!(
            submitted["node_cursor"]["Task"]["gates"][0], "review",
            "{submitted}"
        );
        builder.finish().expect("server must finish successfully");
        reviewer.finish().expect("server must finish successfully");
        second_transport
            .finish()
            .expect("server must finish successfully");
    }
}

#[test]
fn mcp_session_context_is_absent_from_schemas_and_rejected_in_json() {
    let workspace = Workspace::with_files(&[]);
    workspace.build();
    let mut client =
        McpTestHarness::with_session(&workspace, "trusted-session").expect("MCP initialize");
    let catalog = client
        .request_value("tools/list", json!({}))
        .expect("MCP transport");
    for name in [
        "acdd_claim",
        "acdd_submit",
        "acdd_state",
        "acdd_verify",
        "acdd_blackboard",
    ] {
        let schema = catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["inputSchema"]
            .clone();
        assert!(
            !schema.to_string().contains("session_fingerprint"),
            "{name}: {schema}"
        );
        let mut arguments = match name {
            "acdd_claim" => {
                json!({"node":"first.build","worker_id":"builder","worktree":workspace.root()})
            }
            "acdd_submit" => {
                json!({"node":"first.build","action":"pass","worker_id":"builder"})
            }
            "acdd_verify" => {
                json!({"node":"first.build","worktree":workspace.root()})
            }
            "acdd_blackboard" => {
                json!({"action":"read","worktree":workspace.root()})
            }
            _ => json!({"action":"next","worktree":workspace.root()}),
        };
        arguments["session_fingerprint"] = json!("spoof");
        let (_, response) = client.call(name, arguments).expect("MCP transport");
        assert!(
            response["error"].is_object() || response["result"]["isError"] == true,
            "{response}"
        );
        assert!(
            response.to_string().contains("rejected_arguments")
                && response.to_string().contains("session_fingerprint"),
            "{response}"
        );
    }
    client.finish().expect("server must finish successfully");
}
