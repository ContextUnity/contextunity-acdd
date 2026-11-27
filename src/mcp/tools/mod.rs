mod code_tools;
mod dispatch;
/// Typed MCP tool request schemas.
pub mod inputs;
mod state_tools;

use inputs::{
    Analyze, Ast, BlackboardInput, ClaimInput, Explain, GetDoc, Guide, Impact, Inspect, Lint,
    OverviewInput, Query, SearchDocs, SearchSymbols, Snippet, StateInput, SubmitInput, VerifyInput,
};

use super::server::Server;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_handler, tool_router,
    ServerHandler,
};

#[tool_router]
impl Server {
    #[tool(
        description = "Execute a canonical active-run state lifecycle command (`start`, `extend_scope`, `reopen`, `sync`, `subtask`, `snapshot`, `clear`, `run`) or read-only active-run query (`status`, `list`, `show`, `next`, `context`, `observability`). Use CLI `acdd roadmap` or `acdd draft` for spec planning/authoring. Read-only actions operate over `SQLITE_OPEN_READ_ONLY` with zero database creation."
    )]
    async fn acdd_state(&self, Parameters(p): Parameters<StateInput>) -> CallToolResult {
        state_tools::acdd_state(self, p).await
    }

    #[tool(
        description = "Atomically claim or re-claim a ready/active gate node (or auto-select the next ready node when `node` is omitted). Returns budgeted `ClaimResp` with mandatory contract/guidance, unresolved findings, dependency receipts, blackboard messages, and context lenses."
    )]
    async fn acdd_claim(&self, Parameters(p): Parameters<ClaimInput>) -> CallToolResult {
        state_tools::acdd_claim(self, p).await
    }

    #[tool(
        description = "Execute gate verification (`command`, `call`, `checks`, and admitted scope barrier) for a claimed node, recording an execution-digest-bound trace for `acdd_submit`."
    )]
    async fn acdd_verify(&self, Parameters(p): Parameters<VerifyInput>) -> CallToolResult {
        state_tools::acdd_verify(self, p).await
    }

    #[tool(
        description = "Submit `pass` or `reject` for a claimed gate node. Passing verifies trace freshness (`execution_digest`, `candidate_tree`), `independent_from`, `immutable`, and `need:`; `reject` requires structured `rejection_findings` (`kind`: `defect | architectural_drift`). For large JSON proofs (> 8 KiB) prefer CLI `acdd submit ... --evidence @.acdd/tmp/<name>.json` with exact unquoted `task_ref` keys."
    )]
    async fn acdd_submit(&self, Parameters(p): Parameters<SubmitInput>) -> CallToolResult {
        state_tools::acdd_submit(self, p).await
    }

    #[tool(
        description = "Read (`StateQuery::BlackboardRead` over `SQLITE_OPEN_READ_ONLY`) or post (`StateCommand::BlackboardPost`) run-, node-, or recipient-scoped blackboard entries."
    )]
    async fn acdd_blackboard(&self, Parameters(p): Parameters<BlackboardInput>) -> CallToolResult {
        state_tools::acdd_blackboard(self, p).await
    }

    #[tool(
        description = "Workspace root, counts, languages, and resolution coverage, including paged coverage_by_workspace counts from the indexed primary and linked workspace snapshot. Pass optional path to scope overview counts, languages, and files to a directory or file prefix. Uses server worktree routing; pass worktree for explicit selection."
    )]
    async fn code_map_overview(&self, Parameters(p): Parameters<OverviewInput>) -> CallToolResult {
        code_tools::code_map_overview(self, p).await
    }

    #[tool(
        description = "Compact symbol record: signature, docstring, container, call counts, and at most five direct call previews per direction. Accepts 'symbol' as an alias for 'selector' and 'file_path' as an alias for 'path'. include_coverage adds paged resolution evidence. Source stays off unless show_source is true. Source continuation requires the previous page's generation."
    )]
    async fn code_map_inspect(&self, Parameters(p): Parameters<Inspect>) -> CallToolResult {
        code_tools::code_map_inspect(self, p).await
    }

    #[tool(
        description = "Read code/config slices (`selector`/`symbol`, `outline`, `delta`, `search`, or bounded `lines`). Prefer `selector` (digest-verified complete symbol body) or `outline` first, and pass `detail=\"full\"` (48 KiB) or `output_file` when a larger slice is needed rather than sequential line paging. Read Markdown docs (`docs/**`, ADRs, `AGENTS.md`, specs) via `get_doc` / `search_docs`. Auto selects snippet for symbol/selector, search for pattern, otherwise lines."
    )]
    async fn acdd_read(&self, Parameters(p): Parameters<Snippet>) -> CallToolResult {
        code_tools::acdd_read(self, p).await
    }

    #[tool(
        description = "Search names, qualified names, indexed parameter and return types, decorators, and tokens. Filter optionally by kind, classification ('production', 'test', 'documentation'), or path; use takes:Type or returns:Type for type filters. A trailing * is a name prefix. exact=true matches an indexed name or qualname without full text. Hits include indexed signatures and inspect_selector arguments for code_map_inspect; previews appear automatically for at most three total hits or on request, capped at three hits, 20 lines, and 12 KiB combined."
    )]
    async fn code_map_search(&self, Parameters(p): Parameters<SearchSymbols>) -> CallToolResult {
        code_tools::code_map_search(self, p).await
    }

    #[tool(
        description = "Transitive reachability from one selector or a Git change view (staged, unstaged, or head). Provide exactly one of selector or changes. inbound (default) lists dependents; outbound lists dependencies. Each reached depth-one node includes the persisted edge endpoints, kind, path, line, direction, confidence, and occurrence count; full detail adds edge evidence. Change views bind every page to the graph, selected diff, and verified postimages, and report unmapped changes explicitly."
    )]
    async fn code_map_impact(&self, Parameters(p): Parameters<Impact>) -> CallToolResult {
        code_tools::code_map_impact(self, p).await
    }

    #[tool(
        description = "Symbol summary plus paged incoming and outgoing edges of every kind. The embedded call summary shows counts and at most five direct call previews per direction. direction defaults to both. Source continuation requires the previous page's generation."
    )]
    async fn code_map_explain(&self, Parameters(p): Parameters<Explain>) -> CallToolResult {
        code_tools::code_map_explain(self, p).await
    }

    #[tool(
        description = "Bounded graph operation or one read-only SELECT/WITH. Supports tests (covering test cases or tested dependencies), prove_removal (indexed removal check for a symbol or file), untested, implementations, slice, unwired, duplicates, and dead_tails (phantom_tested, dead_tails, dangling_calls)."
    )]
    async fn code_map_query(&self, Parameters(p): Parameters<Query>) -> CallToolResult {
        code_tools::code_map_query(self, p).await
    }

    #[tool(
        description = "Read-only diagnostics over indexed facts. Default mode='summary' reports workspace/path diagnostics, untested public surface, stored syntax diagnostics (lint=true), cycles (include_cycles=true), or read-only SQL. Optional mode='hotspots'|'boundary'|'path'|'entrypoints'|'diff_impact'|'cochange' runs PageRank hotspots, module boundary, shortest path (with to), entrypoints, Git diff impact, or co-change analysis (with optional base)."
    )]
    async fn code_map_analyze(&self, Parameters(p): Parameters<Analyze>) -> CallToolResult {
        code_tools::code_map_analyze(self, p).await
    }

    #[tool(
        description = "Run unified architectural, graph hygiene, syntax, and documentation lint rules across the workspace or a target path/glob. Default categories: arch, syntax, docs."
    )]
    async fn code_map_lint(&self, Parameters(p): Parameters<Lint>) -> CallToolResult {
        code_tools::code_map_lint(self, p).await
    }

    #[tool(
        description = "Tree-sitter structural search over admitted source (default limit: 15). Language profiles declare complete_syntax, declaration_without_body, fragment_probe, and attribute capabilities. $NAME captures one syntax node; $$$SEQ captures a syntax-node sequence. language must be a compiled profile id (e.g. 'python', 'rust', 'typescript') or omitted to infer from path or search compatible indexed languages in scope. Call acdd_guide with topic=ast for the profile capability matrix and examples."
    )]
    async fn ast_grep_search(&self, Parameters(p): Parameters<Ast>) -> CallToolResult {
        code_tools::ast_grep_search(self, p).await
    }

    #[tool(
        description = "Search indexed Markdown sections with natural-language terms: common English stop words are ignored, any remaining term can match, and section-title matches receive three times the BM25 weight of body text. Returns paths and section anchors for get_doc. include_excerpt adds a short match-centered preview."
    )]
    async fn search_docs(&self, Parameters(p): Parameters<SearchDocs>) -> CallToolResult {
        code_tools::search_docs(self, p).await
    }

    #[tool(
        description = "Canonical reader for indexed Markdown (`docs/**`, ADRs, `AGENTS.md`, specs). Returns full text by default (`detail=full`); pass `detail=compact` for a heading outline or `section` for one section. A missing path or section is an error."
    )]
    async fn get_doc(&self, Parameters(p): Parameters<GetDoc>) -> CallToolResult {
        code_tools::get_doc(self, p).await
    }

    #[tool(description = "Built-in guide. The default topic is query.")]
    async fn acdd_guide(&self, Parameters(p): Parameters<Guide>) -> CallToolResult {
        code_tools::acdd_guide(self, p).await
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        dispatch::get_info(self)
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        dispatch::get_tool(self, name)
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        dispatch::list_tools(self, context).await
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        dispatch::call_tool(self, request, context).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::response;
    use acdd_core::errors::{ErrorCode, AcddError};
    use acdd_core::ports::{
        is_builtin_mcp_tool, DomainActionReq, DomainActionResult, DomainCapabilities,
        DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider, DomainToolDef,
        DomainToolOutput, DomainToolReq,
    };
    use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
    use rmcp::model::{CallToolRequestParams, CallToolResponse, NumberOrString};
    use rmcp::service::{serve_directly, Peer, RequestContext, RoleServer};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct MockHttpProvider;
    impl DomainProvider for MockHttpProvider {
        fn domain_id(&self) -> &str {
            "http"
        }
        fn capabilities(&self) -> DomainCapabilities {
            let mut schema = SchemaDef::object();
            schema.properties.insert("url".into(), SchemaDef::string());
            schema
                .properties
                .insert("oversize".into(), SchemaDef::of_type("boolean"));
            schema.required = vec!["url".into()];
            schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
            DomainCapabilities {
                tools: vec![DomainToolDef {
                    name: "probe".into(),
                    description: "Probe an HTTP endpoint".into(),
                    input_schema: schema,
                    read_only: true,
                }],
                ..DomainCapabilities::default()
            }
        }
        fn execute_action(
            &self,
            _action: &str,
            _params: &Value,
            _idempotency_key: &str,
            _req: &DomainActionReq,
        ) -> anyhow::Result<DomainActionResult> {
            unreachable!()
        }
        fn run_check(
            &self,
            _check: &str,
            _params: &Value,
            _req: &DomainCheckReq,
        ) -> anyhow::Result<DomainCheckOutcome> {
            unreachable!()
        }
        fn provide_context(&self, _lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
            unreachable!()
        }
        fn call_tool(
            &self,
            tool: &str,
            args: &Value,
            _req: &DomainToolReq,
        ) -> anyhow::Result<DomainToolOutput> {
            anyhow::ensure!(tool == "probe");
            let url = args["url"].as_str().unwrap_or("");
            if args["oversize"].as_bool() == Some(true) {
                return Ok(DomainToolOutput::Markdown("x".repeat(200_000)));
            }
            if args["url"] == "json_out" {
                return Ok(DomainToolOutput::Json(
                    serde_json::json!({"status": 200, "url": url}),
                ));
            }
            Ok(DomainToolOutput::Markdown(format!(
                "# Probe\n\nOK: {url}\n"
            )))
        }
    }

    async fn make_peer_ctx(
        id: i64,
    ) -> (
        rmcp::service::RunningService<RoleServer, Server>,
        tokio::io::DuplexStream,
        RequestContext<RoleServer>,
    ) {
        let (client, transport) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(transport);
        let dummy = Server::new(PathBuf::new(), PathBuf::new());
        let running = serve_directly(dummy, (reader, response::BoundedWriter::new(writer)), None);
        let peer: Peer<RoleServer> = running.peer().clone();
        let ctx = RequestContext::new(NumberOrString::Number(id), peer);
        (running, client, ctx)
    }

    async fn call_raw(
        server: &Server,
        name: &str,
        args: Value,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let req = CallToolRequestParams::new(name.to_owned())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let CallToolResponse::Complete(res) = server.call_tool(req, ctx).await.unwrap() else {
            panic!("expected complete CallToolResponse");
        };
        res
    }

    #[tokio::test]
    async fn mcp_18_builtin_tools_and_plugin_tool_contract() {
        let ws = acdd_core::fs::ScopedTempDir::create("acdd-mcp-tools-18tools").unwrap();
        std::fs::write(
            ws.path().join("acdd.yaml"),
            "roots: [.]\nmodules:\n  domains:\n    http: true\n",
        )
        .unwrap();
        let mut server = Server::new(
            ws.path().to_path_buf(),
            ws.path().join(".acdd/code-map.sqlite"),
        );
        server.extra_domain_providers = vec![Arc::new(MockHttpProvider)];

        let info = server.get_info();
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        let instructions = info.instructions.as_deref().unwrap_or("");
        assert!(
            instructions.contains("19 tools"),
            "expected dynamic 18+1=19 tools in instructions: {instructions}"
        );

        let (_running, _client, ctx) = make_peer_ctx(1).await;
        let listed = server.list_tools(None, ctx.clone()).await.unwrap();
        let names: BTreeSet<String> = listed
            .tools
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        assert_eq!(names.len(), 19);
        assert!(names.contains("http_probe"));
        assert_eq!(
            names
                .iter()
                .filter(|name| name.as_str() != "http_probe" && is_builtin_mcp_tool(name))
                .count(),
            18
        );
        for legacy in [
            "code_map_tests",
            "code_map_prove_removal",
            "task_list",
            "task_claim",
            "task_submit",
            "task_manage",
            "task_blackboard",
            "session_checkpoint",
            "forge_guide",
        ] {
            assert!(!names.contains(legacy), "legacy tool {legacy} leaked");
        }

        let plugin_tool = listed
            .tools
            .iter()
            .find(|t| t.name.as_ref() == "http_probe")
            .unwrap();
        assert_eq!(
            plugin_tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            Some(true)
        );
        assert!(plugin_tool.input_schema["properties"].get("url").is_some());
        assert!(server.get_tool("http_probe").is_some());

        // Typo rejected with Levenshtein suggestion
        let (_, _, c1) = make_peer_ctx(2).await;
        let typo = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"ur": "https://example.com"}),
            c1,
        )
        .await;
        assert_eq!(typo.is_error, Some(true));
        let typo_text = typo.content[0].as_text().unwrap().text.as_str();
        assert!(typo_text.contains("rejected_arguments") && typo_text.contains("url"));

        // Invalid schema type fails closed
        let (_, _, c2) = make_peer_ctx(3).await;
        let bad_type = call_raw(&server, "http_probe", serde_json::json!({"url": 123}), c2).await;
        assert_eq!(bad_type.is_error, Some(true));

        // Markdown & JSON output parity
        let (_, _, c3) = make_peer_ctx(4).await;
        let md = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "https://example.com", "format": "markdown"}),
            c3,
        )
        .await;
        assert_ne!(md.is_error, Some(true));
        assert!(md.content[0]
            .as_text()
            .unwrap()
            .text
            .contains("OK: https://example.com"));

        let (_, _, c4) = make_peer_ctx(5).await;
        let js = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "json_out", "format": "json"}),
            c4,
        )
        .await;
        assert_ne!(js.is_error, Some(true));
        let parsed: Value =
            serde_json::from_str(js.content[0].as_text().unwrap().text.as_str()).unwrap();
        assert_eq!(parsed["status"], 200);

        // Oversized plugin output clamped by response::enforce
        let (_, _, c5) = make_peer_ctx(6).await;
        let big = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "https://example.com", "oversize": true}),
            c5,
        )
        .await;
        assert_eq!(big.is_error, Some(true));
        assert!(big.content[0]
            .as_text()
            .unwrap()
            .text
            .contains("exceeds the response byte limit"));
    }

    #[tokio::test]
    async fn mcp_modules_isolation_and_readonly_state_queries() {
        let ws = acdd_core::fs::ScopedTempDir::create("acdd-mcp-tools-modules").unwrap();
        std::fs::write(
            ws.path().join("acdd.yaml"),
            "roots: [.]\nmodules:\n  domains:\n    http: true\n",
        )
        .unwrap();
        let server = Server::new(
            ws.path().to_path_buf(),
            ws.path().join(".acdd/code-map.sqlite"),
        );

        // Read-only state queries & blackboard read must NOT create state.sqlite
        for (idx, (tool, args)) in [
            (
                "acdd_state",
                serde_json::json!({"action": "status", "format": "json"}),
            ),
            (
                "acdd_state",
                serde_json::json!({"action": "list", "format": "json"}),
            ),
            (
                "acdd_state",
                serde_json::json!({"action": "next", "format": "json"}),
            ),
            (
                "acdd_state",
                serde_json::json!({"action": "observability", "format": "json"}),
            ),
            (
                "acdd_state",
                serde_json::json!({"action": "context", "format": "json"}),
            ),
            (
                "acdd_blackboard",
                serde_json::json!({"action": "read", "format": "json"}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let (_, _, ctx) = make_peer_ctx(10 + idx as i64).await;
            let _ = call_raw(&server, tool, args, ctx).await;
            assert!(
                !ws.path().join(".acdd/state.sqlite").exists(),
                "read-only query {tool} must not create state.sqlite"
            );
        }

        // Physical module gating matrix: state=false, code=false, docs=false, domains.http=false
        struct ModuleCase {
            yaml: &'static str,
            hidden: &'static [&'static str],
            disabled_call: (&'static str, Value, &'static str),
        }
        let cases = [
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [state]\n",
                hidden: &[
                    "acdd_state",
                    "acdd_claim",
                    "acdd_verify",
                    "acdd_submit",
                    "acdd_blackboard",
                ],
                disabled_call: (
                    "acdd_state",
                    serde_json::json!({"action": "status"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [code]\n",
                hidden: &["code_map_overview", "code_map_search", "ast_grep_search"],
                disabled_call: (
                    "code_map_search",
                    serde_json::json!({"pattern": "main"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [docs]\n",
                hidden: &["get_doc", "search_docs"],
                disabled_call: (
                    "search_docs",
                    serde_json::json!({"query": "readme"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  domains:\n    http: false\n",
                hidden: &["http_probe"],
                disabled_call: (
                    "http_probe",
                    serde_json::json!({"url": "https://example.com"}),
                    "DOMAIN_CAPABILITY_UNAVAILABLE",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\n",
                hidden: &["http_probe"],
                disabled_call: (
                    "http_probe",
                    serde_json::json!({"url": "https://example.com"}),
                    "DOMAIN_CAPABILITY_UNAVAILABLE",
                ),
            },
        ];

        for (idx, case) in cases.iter().enumerate() {
            let sub_ws =
                acdd_core::fs::ScopedTempDir::create(&format!("acdd-mcp-tools-mod-{idx}")).unwrap();
            std::fs::write(sub_ws.path().join("acdd.yaml"), case.yaml).unwrap();
            let mut s = Server::new(
                sub_ws.path().to_path_buf(),
                sub_ws.path().join(".acdd/code-map.sqlite"),
            );
            s.extra_domain_providers = vec![Arc::new(MockHttpProvider)];
            let (_, _, ctx) = make_peer_ctx(100 + idx as i64).await;
            let listed = s.list_tools(None, ctx).await.unwrap();
            let names: BTreeSet<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
            for tool in case.hidden {
                assert!(
                    !names.contains(tool),
                    "{tool} should be hidden for {}",
                    case.yaml
                );
            }
            let (_, _, c2) = make_peer_ctx(200 + idx as i64).await;
            let res = call_raw(&s, case.disabled_call.0, case.disabled_call.1.clone(), c2).await;
            assert_eq!(res.is_error, Some(true));
            let text = res.content[0].as_text().unwrap().text.as_str();
            assert!(
                text.contains(case.disabled_call.2),
                "expected {} in {text}",
                case.disabled_call.2
            );
            assert!(!sub_ws.path().join(".acdd/state.sqlite").exists());
        }

        // validate_explicit_worktree_git_ancestors accepts normal dirs/names and rejects symlinked explicit paths
        assert!(dispatch::validate_explicit_worktree_git_ancestors(ws.path(), None).is_ok());
        assert!(dispatch::validate_explicit_worktree_git_ancestors(ws.path(), Some(".")).is_ok());
        assert!(
            dispatch::validate_explicit_worktree_git_ancestors(ws.path(), Some("named-only"))
                .is_ok()
        );
        #[cfg(unix)]
        {
            let real_wt = ws.path().join("real-wt");
            std::fs::create_dir_all(&real_wt).unwrap();
            let wt_link = ws.path().join("symlinked-wt");
            std::os::unix::fs::symlink(&real_wt, &wt_link).unwrap();
            let err = dispatch::validate_explicit_worktree_git_ancestors(
                ws.path(),
                Some("./symlinked-wt"),
            )
            .unwrap_err();
            let state_err = AcddError::from_anyhow(&err).expect("AcddError");
            assert_eq!(state_err.code, ErrorCode::ExternalGitDrift);
        }
    }
}
