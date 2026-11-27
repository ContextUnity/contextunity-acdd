use super::*;

#[test]
fn mcp_server_keeps_its_linked_worktree_and_resolves_registered_sibling_names() {
    let primary = Workspace::with_files(&[
        (
            "acdd.yaml",
            "roots: [src]\ndocs: [docs]\nstate_db: .acdd/state.sqlite\n",
        ),
        (
            ".acdd/workflows/default.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: worker\n          proof: none\n          pass: review\n        - id: review\n          role: worker\n          proof: none\n          independent_from: [build]\n          need: [build]\n          pass: deliver\n        - id: deliver\n          role: worker\n          receipt: true\n          need: [build, review]\n          save:\n            post: none\n",
        ),
        ("src/lib.rs", "pub fn primary_only() {}\n"),
    ]);
    primary.seed_git().expect("seed real Git baseline");
    let linked = primary.path(".worktrees/049-current");
    let sibling_container = Workspace::with_files(&[]);
    let sibling = sibling_container.path("051-external");
    let mirror = sibling_container.path("052-mirror");
    for worktree in [&linked, &sibling, &mirror] {
        let path = worktree
            .to_str()
            .expect("Git worktree path should be valid UTF-8");
        primary
            .run_git(&["worktree", "add", "--detach", path])
            .expect("create real Git worktree");
    }
    fs::write(
        linked.join("src/lib.rs"),
        "pub fn linked_only() {}\npub fn shared_source() {\n    linked_only();\n    let b = 2;\n    let c = 3;\n}\n",
    )
    .unwrap();
    fs::write(
        sibling.join("src/lib.rs"),
        "pub fn sibling_only() {}\npub fn shared_source() {\n    sibling_only();\n    let y = 2;\n    let z = 3;\n}\n#[cfg(test)]\nmod tests {\n    use super::sibling_only;\n    #[test]\n    fn exercises_sibling_symbol() {\n        sibling_only();\n    }\n}\n",
    )
    .unwrap();
    fs::create_dir_all(sibling.join("src/space dir")).unwrap();
    fs::write(
        sibling.join("src/space dir/#unicode-λ.rs"),
        "pub fn escaped_path_symbol() {}\n",
    )
    .unwrap();
    fs::create_dir_all(linked.join("docs")).unwrap();
    fs::write(
        linked.join("docs/linked symbols.md"),
        "# Linked symbol reference\n\nThe `linked_only` function is declared in the source.\n",
    )
    .unwrap();
    fs::write(
        mirror.join("src/lib.rs"),
        "pub fn linked_only() {}\npub fn shared_source() {\n let a = 1;\n let b = 2;\n let c = 3;\n}\n",
    )
    .unwrap();
    let claims = TaskClaimsFixture::worktrees_only(
        &primary,
        ".acdd/tasks.sqlite",
        &[&linked, &sibling, &mirror],
    );
    let custom_db = linked.join(".acdd/custom.sqlite");
    let mut client =
        McpTestHarness::with_root_and_db(&linked, &custom_db, "linked-routing-session")
            .expect("MCP initialize");
    for worktree in [None, Some(".")] {
        let mut args = json!({"pattern":"linked_only", "exact":true});
        if let Some(worktree) = worktree {
            args["worktree"] = json!(worktree);
        }
        assert_eq!(
            client
                .payload("code_map_search", args)
                .expect("MCP tool should succeed")["nodes"]["total"],
            1
        );
    }
    let branch_manifest_content = "---\nid: m-branch-only\ntitle: Branch only\ndoc_type: milestone\nstatus: active\n---\n# Branch only\n\n### task: branch-task\n\n```yaml\ntask_ref: branch-task\ntarget: branch task\nscope: [src/lib.rs]\n```\n";
    primary.write(
        "docs/milestones/060-branch-only.md",
        branch_manifest_content,
    );
    let branch_manifest = sibling.join("docs/milestones/060-branch-only.md");
    fs::create_dir_all(branch_manifest.parent().unwrap()).unwrap();
    fs::write(&branch_manifest, branch_manifest_content).unwrap();
    for alias in ["051", "51", "051-ex", "external", "1-exter"] {
        assert_eq!(
            client
                .search("sibling_only", json!({ "exact":true, "worktree":alias}))
                .expect("MCP tool should succeed")["nodes"]["total"],
            1
        );
    }
    drop(claims);
    primary.remove(".acdd/tasks.sqlite");
    fs::write(sibling.join("AGENTS.md"), "Load the acdd skill.\n").unwrap();
    let started = client
        .payload(
            "acdd_state",
            json!({"action":"start", "spec_path":"docs/milestones/060-branch-only.md", "in_place":true, "worktree":"51"}),
        )
        .expect("MCP start should succeed");
    assert!(started.to_string().contains("branch-task"), "{started}");
    let run_id = started["run"]["run_id"]
        .as_str()
        .or_else(|| started["run_id"].as_str())
        .unwrap()
        .to_owned();
    let list = client
        .payload(
            "acdd_state",
            json!({"action":"list", "status":"active", "worktree":"51", "format":"json"}),
        )
        .expect("MCP list should succeed");
    assert!(
        !list["page"]["runs"].as_array().unwrap().is_empty(),
        "{list}"
    );
    let posted = client.payload(
        "acdd_blackboard",
        json!({"action":"post", "run":&run_id, "topic":"notes", "author":"routing-author", "payload":"branch-only message", "worktree":"51"}),
    ).expect("MCP blackboard post should succeed");
    let board = client
        .payload(
            "acdd_blackboard",
            json!({"action":"read", "run":&run_id, "worktree":"51", "format":"json"}),
        )
        .expect("MCP blackboard read should succeed");
    assert_eq!(board["items"][0]["id"], posted["entry"]["id"], "{board}");
    assert_eq!(
        board["items"][0]["payload"], "branch-only message",
        "{board}"
    );
    let node_id = "branch-task.build";
    let claim = client
        .payload(
            "acdd_claim",
            json!({"node":node_id, "worker_id":"routing-author", "worktree":"51"}),
        )
        .expect("MCP claim should succeed");
    let traces = primary.path(format!(".acdd/run/{run_id}/traces.jsonl"));
    let before = fs::read(&traces).unwrap();
    let sentinel = primary.path(".acdd/run/retention-sentinel");
    fs::create_dir_all(&sentinel).unwrap();
    fs::write(sentinel.join("traces.jsonl"), "sentinel\n").unwrap();
    primary.write("acdd.yaml", "state_db: [invalid\n");
    let _ = client.payload(
        "acdd_state",
        json!({"action":"status", "run":&run_id, "worktree":"51"}),
    );
    assert_eq!(fs::read(&traces).unwrap(), before);
    assert!(sentinel.exists());
    primary.write(
        "acdd.yaml",
        "roots: [src]\ndocs: [docs]\nstate_db: .acdd/state.sqlite\n",
    );
    let canonical_sibling = sibling.canonicalize().unwrap();
    assert_eq!(
        claim["worktree"],
        canonical_sibling.to_str().unwrap(),
        "{claim}"
    );
    let session = crate::sqlite_fixture::claim_session_fingerprint(
        &primary,
        "task:branch-task:build",
        claim["claim_revision"].as_u64().unwrap(),
    )
    .unwrap();
    assert_eq!(session, "linked-routing-session");
    let rejected = client
        .payload(
            "acdd_submit",
            json!({
                "node": node_id,
                "action": "reject",
                "worker_id": "routing-author",
                "claim_revision": claim["claim_revision"],
                "worktree": "51",
                "rejection_findings": {
                    "summary": "Release the fixture claim after proving selected-root submission",
                    "items": [{
                        "severity": "medium",
                        "message": "Contract fixture rejection test",
                        "remediation": "Re-claim and pass"
                    }]
                }
            }),
        )
        .expect("MCP reject should succeed");
    assert_eq!(
        rejected["node_cursor"]["Task"]["gates"][0], "build",
        "{rejected}"
    );
    client
        .payload(
            "acdd_state",
            json!({"action":"reopen", "node":node_id, "reason":"reopen for next check", "worktree":"51"}),
        )
        .expect("MCP reopen should succeed");
    let shown = client
        .payload(
            "acdd_state",
            json!({"action":"show", "node":node_id, "worktree":"051", "format":"json"}),
        )
        .expect("MCP show should succeed");
    assert_eq!(shown["node"]["status"], "ready", "{shown}");
    let next = client
        .payload(
            "acdd_state",
            json!({"action":"next", "worktree":"051", "format":"json"}),
        )
        .expect("MCP next should succeed");
    assert_eq!(next["waves"], json!([["branch-task"]]), "{next}");

    assert!(custom_db.is_file());
    assert!(!linked.join(".acdd/code-map.sqlite").exists());
    let sibling_result = client
        .search(
            "sibling_only",
            json!({ "exact":true, "worktree":"external"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(sibling_result["nodes"]["total"], 1);
    assert!(sibling_result["nodes"]["items"][0].get("uri").is_none());
    let linked_page = client
        .source(
            "shared_source",
            json!({"max_body_lines":1,"leading_lines":0,"worktree":"."}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(linked_page["source_preview"]["next_source_offset"], 1);
    let (_, mirror_continuation) = client.call(
        "acdd_read",
        json!({"selector":"shared_source","max_body_lines":1,"leading_lines":0,"source_offset":1,"worktree":"mirror","format":"json"}),
    ).expect("MCP transport");
    assert_eq!(mirror_continuation["result"]["isError"], true);
    assert!(mirror_continuation["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("generation"));
    let (_, sibling_continuation) = client.call(
        "acdd_read",
        json!({"selector":"shared_source","max_body_lines":1,"leading_lines":0,"source_offset":1,"worktree":"external","format":"json"}),
    ).expect("MCP transport");
    assert_eq!(sibling_continuation["result"]["isError"], true);

    let expected_sibling_source =
        crate::common::workspace::canonical_file_uri(&sibling, "src/lib.rs", Some(1));
    let expected_special_source = crate::common::workspace::canonical_file_uri(
        &sibling,
        "src/space dir/#unicode-λ.rs",
        Some(1),
    );
    let expected_linked_source =
        crate::common::workspace::canonical_file_uri(&linked, "src/lib.rs", Some(1));
    let uri_cases = [
        (
            "relative root symbol",
            "code_map_search",
            json!({"pattern":"linked_only","exact":true,"worktree":".","file_uris":true,"format":"markdown"}),
            expected_linked_source,
        ),
        (
            "registered linked-workspace symbol",
            "code_map_search",
            json!({"pattern":"sibling_only","exact":true,"worktree":"external","file_uris":true,"format":"markdown"}),
            expected_sibling_source.clone(),
        ),
        (
            "encoded physical path",
            "code_map_search",
            json!({"pattern":"escaped_path_symbol","exact":true,"worktree":"external","file_uris":true,"format":"markdown"}),
            expected_special_source.clone(),
        ),
        (
            "AST match",
            "ast_grep_search",
            json!({"language":"rust","pattern":"pub fn escaped_path_symbol() {}","worktree":"external","file_uris":true,"format":"markdown"}),
            expected_special_source,
        ),
        (
            "inspect symbol",
            "code_map_inspect",
            json!({"selector":"shared_source","worktree":"external","file_uris":true,"format":"markdown"}),
            crate::common::workspace::canonical_file_uri(&sibling, "src/lib.rs", Some(2)),
        ),
        (
            "impact traversal node",
            "code_map_impact",
            json!({"selector":"sibling_only","direction":"inbound","worktree":"external","file_uris":true,"format":"markdown"}),
            crate::common::workspace::canonical_file_uri(&sibling, "src/lib.rs", Some(2)),
        ),
        (
            "traversal call edge",
            "code_map_explain",
            json!({"selector":"shared_source","direction":"outbound","worktree":"external","file_uris":true,"format":"markdown"}),
            crate::common::workspace::canonical_file_uri(&sibling, "src/lib.rs", Some(3)),
        ),
        (
            "test edge",
            "code_map_query",
            json!({"operation":"tests","selector":"sibling_only","direction":"inbound","worktree":"external","file_uris":true,"format":"markdown"}),
            crate::common::workspace::canonical_file_uri(&sibling, "src/lib.rs", Some(11)),
        ),
        (
            "documentation section",
            "search_docs",
            json!({"query":"linked symbol reference","format":"markdown"}),
            "— `docs/linked symbols.md`".to_owned(),
        ),
    ];
    let mut uri_failures = Vec::new();
    for (label, tool, arguments, expected_uri) in uri_cases {
        let (_, response) = client.call(tool, arguments).expect("MCP transport");
        assertions::assert_rpc_succeeded(&response);
        let markdown = response["result"]["content"][0]["text"]
            .as_str()
            .expect("Markdown tool call should return text");
        if !markdown.contains(&expected_uri) {
            uri_failures.push(format!(
                "{label} did not link to {expected_uri}: {markdown}"
            ));
        }
        if markdown.contains("(link unavailable)") {
            uri_failures.push(format!(
                "{label} retained a link-unavailable marker: {markdown}"
            ));
        }
    }

    let doc_path = "docs/linked symbols.md";
    let doc_json = client
        .payload("get_doc", json!({"path_or_id":doc_path}))
        .expect("MCP tool should succeed");
    let referenced_symbol = doc_json["referenced_symbols"]["items"]
        .as_array()
        .and_then(|items| items.first());
    if let Some(referenced_symbol) = referenced_symbol {
        assert_eq!(referenced_symbol["path"], "src/lib.rs");
        assert_eq!(referenced_symbol["line"], 1);
        assert_eq!(referenced_symbol["citation_line"], 3);
        assert!(referenced_symbol["doc_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        assert!(referenced_symbol.get("uri").is_none());
    } else {
        uri_failures.push(format!(
            "get_doc did not retain the referenced symbol row: {doc_json}"
        ));
    }
    let doc_line = doc_json["sections"]["items"][0]["line"]
        .as_u64()
        .unwrap_or(1);
    let expected_doc_uri = crate::common::workspace::canonical_file_uri(
        &linked,
        "docs/linked symbols.md",
        Some(doc_line.try_into().unwrap()),
    );
    let (_, doc_markdown_response) = client
        .call(
            "get_doc",
            json!({"path_or_id":doc_path,"format":"markdown"}),
        )
        .expect("MCP transport");
    assertions::assert_rpc_succeeded(&doc_markdown_response);
    let doc_markdown = doc_markdown_response["result"]["content"][0]["text"]
        .as_str()
        .expect("get_doc should return Markdown text");
    for expected_uri in [
        expected_doc_uri,
        crate::common::workspace::canonical_file_uri(&linked, "src/lib.rs", Some(1)),
    ] {
        if !doc_markdown.contains(&expected_uri) {
            uri_failures.push(format!(
                "get_doc did not link to {expected_uri}: {doc_markdown}"
            ));
        }
    }
    assert!(
        uri_failures.is_empty(),
        "coordinate URI failures: {uri_failures:#?}"
    );
    client.finish().expect("server must finish successfully");
}
