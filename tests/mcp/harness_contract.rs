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
    workspace.build();

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
    let workspace =
        crate::harness::session_workspace().expect("real session fixture should initialize");
    let root_rules = "# Rules\n\nKeep shared interfaces stable.\n";
    let local_rules = format!(
        "# Source rules\n\nKeep domain checks in their owner.\n{}",
        "\t\\\"🦀".repeat(2_000)
    );
    let long_root = format!(
        "{root_rules}{}",
        "Keep changes within the task scope. ".repeat(40)
    );
    let long_local = format!(
        "# Source rules\n\nKeep domain checks in their owner.\n{}",
        "Use the owning domain's test harness. ".repeat(40)
    );
    let config = |cap| format!("roots: [src/]\nspecs:\n  milestones:\n    path: docs/milestones\nresponse:\n  max_output_bytes: {cap}\n");
    workspace.write("acdd.yaml", &config(4096));
    for (next, cap, root_source, local_source, descendants) in [
        (false, 4096, root_rules, local_rules.as_str(), 0),
        (true, 2048, long_root.as_str(), long_local.as_str(), 0),
        (true, 2048, long_root.as_str(), long_local.as_str(), 9),
    ] {
        workspace.write("AGENTS.md", root_source);
        workspace.write("src/AGENTS.md", local_source);
        if descendants > 0 {
            let path = "docs/milestones/010-session.md";
            let spec = std::fs::read_to_string(workspace.path(path)).unwrap();
            workspace.write(path, &spec.replace("scope: [src/]", "scope: [src/, tests/]"));
        }
        for index in 0..descendants {
            workspace.write(format!("src/{index}/AGENTS.md"), local_source);
        }
        let mut builder =
            McpTestHarness::with_session(&workspace, "builder-session").expect("MCP initialize");
        builder
            .payload(
                "acdd_state",
                json!({"action":"start","spec_path":"docs/milestones/010-session.md","in_place":true}),
            )
            .expect("MCP start should succeed");
        let claim_args = json!({
            "node":"first.build","worker_id":"builder","worktree":workspace.root(),"format":"json"
        });
        if cap < 4096 {
            // Bind the candidate with the normal config before changing the response budget.
            builder
                .payload("acdd_claim", claim_args.clone())
                .expect("initial MCP claim should succeed");
            workspace.write("acdd.yaml", &config(cap));
        }
        #[cfg(unix)]
        let fifo = (descendants > 0).then(|| {
            use std::os::unix::ffi::OsStrExt;
            let path = workspace.path("TESTS.md");
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: The NUL-terminated path lives through the synchronous syscall.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            path
        });
        let claim = builder.call("acdd_claim", claim_args);
        #[cfg(unix)]
        if let Some(fifo) = fifo {
            if claim.is_err() {
                // Release a blocked reader on request timeout before dropping the harness.
                let _ = std::fs::OpenOptions::new().read(true).write(true).open(&fifo);
            }
            std::fs::remove_file(fifo).unwrap();
        }
        let (bytes, response) = claim.expect("MCP claim should succeed");
        let build_claim: Value = mcp_payload(&response);
        assert_eq!(
            build_claim["node_cursor"]["Task"]["gates"][0], "build",
            "{build_claim}"
        );
        assert!(bytes <= cap, "claim response used {bytes} bytes");
        assert_eq!(build_claim["context_lenses"].as_object().unwrap().len(), 1);
        let instructions = &build_claim["context_lenses"]["docs.instructions"];
        assert_eq!(
            instructions["directive"],
            "MANDATORY: read repository rules for respective scope."
        );
        let items = instructions["items"].as_array().unwrap();
        if descendants > 0 {
            assert_eq!(instructions["total"], 11);
            assert!(items.iter().all(|item| item["path"] != "TESTS.md"));
        }
        for path in ["src/AGENTS.md", "AGENTS.md"] {
            let item = items.iter().find(|item| item["path"] == path).unwrap();
            assert!(item.get("uri").is_none() && item.get("title").is_none() && item.get("content").is_none());
            assert!(item["headings"].as_array().is_some_and(|h| !h.is_empty()));
        }
        workspace.write("acdd.yaml", &config(4096));
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
        reviewer
            .payload("acdd_state", json!({"action":"clear","force":true}))
            .expect("MCP clear should succeed");
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
        assert_eq!(
            schema["type"], "object",
            "{name} root schema must remain type=object: {schema}"
        );
        assert!(
            schema.get("oneOf").is_none() && schema.get("anyOf").is_none(),
            "{name} root schema must not use oneOf/anyOf: {schema}"
        );
        assert!(
            !schema.to_string().contains("session_fingerprint"),
            "{name}: {schema}"
        );
        if name == "acdd_state" {
            let state_schema = schema.to_string();
            assert!(
                state_schema.contains("\"snapshot\"") && !state_schema.contains("\"label\""),
                "acdd_state schema must expose snapshots without legacy labels: {schema}"
            );
        }
        if name == "acdd_submit" {
            let kind_schema = &schema["$defs"]["RejectionKind"];
            let kind_str = if kind_schema.is_null() {
                schema["properties"]["rejection_findings"].to_string()
            } else {
                kind_schema.to_string()
            };
            assert!(
                schema.to_string().contains("\"kind\"")
                    && kind_str.contains("defect")
                    && kind_str.contains("architectural_drift"),
                "acdd_submit schema must expose rejection_findings.kind: {schema}"
            );
        }
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

#[test]
fn mcp_relative_worktrees_custom_dbs_and_extra_providers_contract() {
    use acdd_core::ports::{
        CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities,
        DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider, DomainToolOutput,
        DomainToolReq, FreshnessObservation,
    };
    use contextunity_acdd::{
        mcp::server::Server,
        state_bridge::{
            resolve_worktree_span_context, CodeDomainProvider, DocsDomainProvider,
            InvocationSettings,
        },
    };
    use rmcp::{
        model::{CallToolRequestParams, CallToolResponse, NumberOrString},
        service::{serve_directly, RequestContext, RoleServer},
        ServerHandler,
    };
    use std::{collections::BTreeSet, sync::Arc};

    struct ProbeProvider;
    impl DomainProvider for ProbeProvider {
        fn domain_id(&self) -> &str { "probe" }
        fn capabilities(&self) -> DomainCapabilities {
            DomainCapabilities {
                checks: vec![CheckCapability { name: "ping".into(), input_schema: acdd_core::schema::SchemaDef::object() }],
                context_lenses: BTreeSet::from(["status".to_owned()]),
                ..DomainCapabilities::default()
            }
        }
        fn execute_action(&self, _: &str, _: &Value, _: &str, _: &DomainActionReq) -> anyhow::Result<DomainActionResult> { unreachable!() }
        fn run_check(&self, c: &str, _: &Value, r: &DomainCheckReq) -> anyhow::Result<DomainCheckOutcome> {
            anyhow::ensure!(c == "ping");
            Ok(DomainCheckOutcome::Passed { summary: r.worktree.display().to_string(), execution_digest_salt: Some("v1".into()) })
        }
        fn observe_freshness(&self, c: &str, _: &Value, _: &DomainCheckReq) -> anyhow::Result<FreshnessObservation> {
            anyhow::ensure!(c == "ping");
            Ok(FreshnessObservation::Passed { index_generation: None, execution_digest_salt: Some("v1".into()) })
        }
        fn provide_context(&self, l: &str, r: &DomainContextReq) -> anyhow::Result<Value> {
            anyhow::ensure!(l == "status");
            Ok(json!({ "ok": true, "worktree": r.worktree.display().to_string() }))
        }
        fn call_tool(&self, _: &str, _: &Value, _: &DomainToolReq) -> anyhow::Result<DomainToolOutput> { unreachable!() }
    }

    let workspace = Workspace::with_files(&[
        ("AGENTS.md", "# Rules\n"),
        (".gitignore", ".acdd/\n"),
        ("acdd.yaml", "roots: [src/]\ndocs: [docs/]\nmodules:\n  domains:\n    probe: true\n"),
        ("src/lib.rs", "pub fn target_fn() -> i32 { 42 }\n#[cfg(test)]\nmod tests { #[test] fn covers_target() { assert_eq!(super::target_fn(), 42); } }\n"),
        ("docs/adr/0001-custom-db.md", "---\ntitle: Custom DB ADR\ndoc_type: adr\n---\n# Custom DB ADR\n"),
        (".acdd/profile.yaml", "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ngates:\n  - id: execute\n    role: worker\n    proof: none\n    checks: [probe.ping]\n    context: [probe.status]\n    save:\n      post: none\n      prune: true\n"),
    ]);
    workspace.seed_git().unwrap();
    let rel_wt = workspace.path(".worktrees/wt-rel");
    workspace.run_git(&["worktree", "add", "--detach", rel_wt.to_str().unwrap()]).unwrap();
    let (custom_code_db, wt_code_db, custom_state_db) = (
        workspace.path(".acdd/custom-code.sqlite"),
        rel_wt.join(".acdd/custom-code.sqlite"),
        workspace.path(".acdd/custom-state.sqlite"),
    );
    workspace.build_to(&custom_code_db);
    contextunity_acdd::engine::pipeline::build(&rel_wt, &wt_code_db, None).unwrap();

    let server = Server::new(workspace.root().to_path_buf(), custom_code_db.clone())
        .with_cache_dir(workspace.path(".acdd/extraction-cache"))
        .with_session_fingerprint("rel-wt-session".into())
        .with_invocation_settings(InvocationSettings {
            root: workspace.root().to_path_buf(), config: None, profile: None,
            state_db: Some(custom_state_db.clone()), code_db: custom_code_db.clone(),
        })
        .with_domain_providers(vec![Arc::new(ProbeProvider)]);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let call_json = |id: i64, name: &str, args: Value| -> Value {
        rt.block_on(async {
            let (_c, tr) = tokio::io::duplex(4096);
            let running = serve_directly(Server::new(Default::default(), Default::default()), tokio::io::split(tr), None);
            let ctx = RequestContext::<RoleServer>::new(NumberOrString::Number(id), running.peer().clone());
            let req = CallToolRequestParams::new(name.to_owned()).with_arguments(args.as_object().cloned().unwrap_or_default());
            let CallToolResponse::Complete(res) = server.call_tool(req, ctx).await.unwrap() else { panic!("incomplete"); };
            let text = res.content[0].as_text().unwrap().text.as_str();
            assert_ne!(res.is_error, Some(true), "{name}({args}) failed: {text}");
            serde_json::from_str(text).unwrap()
        })
    };

    let (canon_wt, canon_root) = (rel_wt.canonicalize().unwrap(), workspace.root().canonicalize().unwrap());
    let adapter = acdd_index::scanner::load_adapter(workspace.root(), None).unwrap();
    for (idx, (label, wt_sel, exp_wt)) in [("rel", ".worktrees/wt-rel", &canon_wt), ("bare", "wt-rel", &canon_wt), ("dot", ".", &canon_root)].iter().enumerate() {
        let base = (idx as i64) * 10 + 1;
        let run_id = call_json(base, "acdd_state", json!({"action":"start","goal":label,"run_name":format!("run-{label}"),"scope":["src/lib.rs"],"in_place":true,"worktree":wt_sel,"format":"json"}))["run"]["run_id"].as_str().unwrap().to_owned();
        assert_eq!(call_json(base + 1, "acdd_state", json!({"action":"status","run":&run_id,"worktree":wt_sel,"format":"json"}))["run"]["run_id"], run_id);
        let claim = call_json(base + 2, "acdd_claim", json!({"run":&run_id,"node":"execute","worker_id":"builder","worktree":wt_sel,"format":"json"}));
        assert_eq!(claim["context_lenses"]["probe.status"], json!({"ok": true, "worktree": exp_wt.display().to_string()}));
        assert_eq!(resolve_worktree_span_context(workspace.root(), exp_wt, Some(&custom_state_db), &adapter).unwrap().unwrap().run_id.as_str(), run_id);
        assert_eq!(call_json(base + 3, "acdd_read", json!({"path":"src/lib.rs","worktree":wt_sel,"format":"json"}))["in_write_scope"], "ALLOWED");
        let post = call_json(base + 4, "acdd_blackboard", json!({"action":"post","run":&run_id,"topic":"notes","payload":label,"worktree":wt_sel,"format":"json"}));
        assert_eq!(call_json(base + 5, "acdd_blackboard", json!({"action":"read","run":&run_id,"worktree":wt_sel,"format":"json"}))["items"][0]["id"], post["entry"]["id"]);
        assert_eq!(call_json(base + 6, "acdd_verify", json!({"run":&run_id,"node":"execute","worker_id":"builder","claim_revision":claim["claim_revision"],"worktree":wt_sel,"format":"json"}))["passed"], json!(true));
        let sub = call_json(base + 7, "acdd_submit", json!({"run":&run_id,"node":"execute","action":"pass","worker_id":"builder","claim_revision":claim["claim_revision"],"worktree":wt_sel,"format":"json"}));
        assert!(sub["run_completed"] == json!(true) || sub["node_cursor"].is_object());
    }
    assert!(custom_state_db.is_file() && !workspace.path(".acdd/state.sqlite").exists() && !workspace.path(".acdd/code-map.sqlite").exists() && !rel_wt.join(".acdd/code-map.sqlite").exists());

    let check_req = DomainCheckReq {
        run_id: "custom-db-check".parse().unwrap(), node_cursor: "execute".parse().unwrap(), gate_id: "execute".parse().unwrap(),
        workspace_root: workspace.root().to_path_buf(), worktree: canon_wt.clone(), admitted_scope: vec!["src/lib.rs".into()],
        changed_paths: vec![], test_target: Some("src/lib.rs::covers_target".into()), path_policy: Default::default(), baseline_ref: None,
    };
    let custom_code = CodeDomainProvider::with_code_db(workspace.root().to_path_buf(), adapter.clone(), Some(custom_code_db.clone()));
    let default_code = CodeDomainProvider::new(workspace.root().to_path_buf(), adapter.clone());
    let FreshnessObservation::Passed { execution_digest_salt: Some(custom_salt), .. } = custom_code.observe_freshness("seam", &json!({}), &check_req).unwrap() else { panic!("expected custom salt"); };
    let FreshnessObservation::Passed { execution_digest_salt: Some(default_salt), .. } = default_code.observe_freshness("seam", &json!({}), &check_req).unwrap() else { panic!("expected default salt"); };
    assert!(!custom_salt.contains("index_generation=no-index") && default_salt.contains("index_generation=no-index"));
    let adrs = DocsDomainProvider::with_code_db(workspace.root().to_path_buf(), adapter, Some(custom_code_db))
        .provide_context("adrs", &DomainContextReq {
            run_id: "custom-db-check".parse().unwrap(), node_cursor: "execute".parse().unwrap(), gate_id: "execute".parse().unwrap(),
            workspace_root: workspace.root().to_path_buf(), worktree: canon_wt, admitted_scope: vec!["src/lib.rs".into()],
            target: None, max_bytes: 16_384, path_policy: Default::default(), baseline_ref: None,
        }).unwrap();
    assert!(adrs["items"].as_array().is_some_and(|a| a.iter().any(|i| i["path"] == "docs/adr/0001-custom-db.md")));
}
