use super::*;

#[test]
fn ergonomic_task_aliases_reach_existing_operations() {
    let w = Workspace::with_files(&[
        (
            "acdd.yaml",
            "roots: [.]\ndocs: [docs]\nspecs:\n  milestones:\n    path: docs/milestones\n",
        ),
        (".gitignore", ".acdd/\n"),
        (
            ".acdd/workflows/default.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: { mode: subagent }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: worker\n          proof: none\n",
        ),
        ("app.py", "def first(): pass\n"),
        (
            "docs/milestones/010-aliases.md",
            "---\nid: m-aliases\ntitle: Parameter aliases\ndoc_type: milestone\nstatus: active\n---\n# Parameter aliases\n\n### task: aliases\n```yaml\ntask_ref: aliases\ntarget: Accept parameter aliases\nscope: [app.py]\nsubtasks:\n  - subtask_ref: parameters\n    title: Accept alternate parameter names\n```\n",
        ),
    ]);

    w.seed_git().unwrap();
    w.build();
    w.query(&["start", "docs/milestones/010-aliases.md", "--in-place"])
        .expect("CLI start should succeed");
    let shown = w
        .query(&["show", "aliases.build"])
        .expect("CLI show should succeed");
    assert_eq!(
        shown["Show"]["node"]["node_id"], "task:aliases:build",
        "{shown}"
    );

    let listed = w
        .query(&["list", "--status", "active"])
        .expect("CLI list should succeed");
    assert_eq!(crate::json_array(&listed["List"]["page"]["runs"]).len(), 1);

    let mut m = McpTestHarness::new(&w).expect("MCP initialize");
    let mcp_show = m
        .payload(
            "acdd_state",
            json!({"action":"show","node":"aliases.build","format":"json"}),
        )
        .expect("MCP acdd_state show");
    assert_eq!(
        mcp_show["node"]["node_id"],
        shown["Show"]["node"]["node_id"]
    );

    let first_page = m
        .payload(
            "acdd_state",
            json!({"action":"list","status":"active","limit":1,"offset":0,"format":"json"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(first_page["page"]["total_runs"], 1);
    assert_eq!(crate::json_array(&first_page["page"]["runs"]).len(), 1);
    let cli_page = w
        .query(&[
            "list", "--status", "active", "--limit", "1", "--offset", "0",
        ])
        .expect("CLI should succeed");
    assert_eq!(cli_page["List"]["page"]["runs"], first_page["page"]["runs"]);

    for args in [
        vec!["show", "no-such-node"],
        vec!["start"],
        vec!["submit", "aliases.build", "--action", "invalid"],
    ] {
        assert!(!run_cli(&w, &args).status.success(), "{args:?}");
    }
    m.finish().expect("server must finish successfully");
}

#[test]
fn guide_topics_preserve_query_and_acdd_shapes_and_reject_unknown_topics() {
    let mut m = McpTestHarness::indexed(&[]).expect("MCP initialize");
    let guide = m
        .payload("acdd_guide", json!({"topic":"query"}))
        .expect("MCP tool should succeed");
    assert!(guide["start"].is_string());
    let acdd = m
        .payload("acdd_guide", json!({"topic":"acdd"}))
        .expect("MCP tool should succeed");
    assert!(acdd["mcp_tools"].is_object());
    assert!(acdd["blackboard_topics"].is_object());
    assert!(acdd["review_contours"].is_array());
    let rejection = m
        .text("acdd_guide", json!({"topic":"no-such-topic"}))
        .expect_err("tool should reject input");
    let expected: &str = "unknown guide topic";
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
fn granular_diagnostics_preserve_resolution_evidence_and_crlf_utf8_source() {
    let diagnostics = DIAGNOSTICS_SOURCE;
    let fixture = AnalysisFixture::from_workspace(Workspace::with_files(&[
        ("acdd.yaml", "state_db: .acdd/state.sqlite\n"),
        ("cycle.py", "def a(): b()\ndef b(): a()\n"),
        ("other.py", "def idle(): pass\n"),
        ("requirements.txt", "pydantic\nrequests\n"),
        ("diagnostics.py", diagnostics),
    ]));
    let w = fixture.workspace();
    w.build();
    let mut m = fixture.start_mcp().expect("MCP initialize");
    let mut diagnostic_failures = Vec::new();
    let mut collect_pages = |mut arguments: serde_json::Value, field: &str| {
        arguments["offset"] = json!(0);
        arguments["generation"] = serde_json::Value::Null;
        let mut items = Vec::new();
        loop {
            let result = m
                .payload("code_map_analyze", arguments.clone())
                .expect("MCP tool should succeed");
            let page = &result[field];
            let page_items = page["items"]
                .as_array()
                .expect("diagnostic page should include items");
            items.extend(page_items.iter().cloned());
            if page["has_more"] != true {
                break;
            }
            arguments["offset"] = json!(page["next_offset"]
                .as_u64()
                .expect("continued diagnostic page should include next offset"));
            arguments["generation"] = page["generation"].clone();
        }
        items
    };
    for detail in ["compact", "full"] {
        let arguments = json!({"target":"diagnostics.py","detail":detail,"limit":1});
        let items = collect_pages(arguments.clone(), "resolution");
        let external = collect_pages(arguments, "external_imports");
        for (expression, reason, status, line) in [
            ("callback", "dynamic_callback", "unresolved", 7),
            ("value.run", "unresolved_receiver_type", "unresolved", 9),
            ("record.model_dump", "framework_magic", "external", 11),
            ("requests.get", "external_unindexed", "external", 13),
            ("Record.model_validate", "framework_magic", "external", 15),
        ] {
            let item = items
                .iter()
                .chain(&external)
                .find(|item| item["expression"] == expression && item["line"] == line)
                .unwrap_or_else(|| {
                    panic!("{expression}: resolution={items:?}, external={external:?}")
                });
            assert_eq!(item["status"], status);
            if item["reason"] != reason {
                diagnostic_failures.push(format!(
                    "{detail}/{expression}: expected {reason}, actual={item}"
                ));
            }
            let expected_snippet = diagnostics
                .split_inclusive('\n')
                .nth(line - 1)
                .unwrap()
                .trim_end_matches(['\r', '\n']);
            if item["snippet"]
                .as_str()
                .map(|text| text.trim_end_matches(['\r', '\n']))
                != Some(expected_snippet)
            {
                diagnostic_failures.push(format!(
                    "{detail}/{expression}: expected source {expected_snippet:?}, actual={item}"
                ));
            }
            if status == "unresolved" {
                assert!(item["cause"].is_string());
            }
            if detail == "full" {
                assert!(item["evidence"].is_string());
            }
        }
        let generic = items
            .iter()
            .find(|item| item["expression"] == "missing_target")
            .unwrap();
        assert!(
            generic["reason"].is_null(),
            "generic absence is not framework evidence: {generic}"
        );
        let stdlib = external
            .iter()
            .find(|item| item["line"] == 12)
            .expect("finite pathlib member remains recorded as external");
        assert_ne!(stdlib["reason"], "framework_magic", "{stdlib}");
        for (expression, line) in [("value.missing", 18), ("make()", 20), ("make().run", 21)] {
            let item = items
                .iter()
                .chain(&external)
                .find(|item| item["expression"] == expression && item["line"] == line)
                .unwrap_or_else(|| {
                    panic!("{expression}: resolution={items:?}, external={external:?}")
                });
            assert_eq!(
                item["status"], "unresolved",
                "{detail}/{expression}: {item}"
            );
            assert!(item["cause"].is_string(), "{detail}/{expression}: {item}");
            if detail == "full" {
                assert!(
                    item["evidence"].is_string(),
                    "{detail}/{expression}: {item}"
                );
            }
            if !item["reason"].is_null() {
                diagnostic_failures.push(format!(
                    "{detail}/{expression}: unproven category must stay null, actual={item}"
                ));
            }
        }
    }
    let conn = w.open();
    let phantom_edges: i64 = conn
        .query_row(
            "SELECT count(*) FROM edges e JOIN path_dictionary p ON p.path_id=e.path_id WHERE p.path='diagnostics.py' AND e.line IN(7,9,11,13,15) AND e.kind IN('calls','possible_calls')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        phantom_edges, 0,
        "diagnostic labels cannot create graph targets"
    );
    drop(conn);

    assert!(
        diagnostic_failures.is_empty(),
        "{}",
        diagnostic_failures.join("\n")
    );
    m.finish().expect("server must finish successfully");
}

#[test]
fn workspace_coverage_keeps_published_scope_identity_across_restart_and_rebuild() {
    let workspace = Workspace::with_files(&[
        ("acdd.yaml", "state_db: .acdd/state.sqlite\n"),
        ("cycle.py", "def a(): b()\ndef b(): a()\n"),
        ("other.py", "def idle(): pass\n"),
        ("requirements.txt", "pydantic\nrequests\n"),
        ("diagnostics.py", DIAGNOSTICS_SOURCE),
    ]);
    let fixture = AnalysisFixture::from_workspace(workspace);
    let w = fixture.workspace();
    let mut m = fixture.start_mcp().expect("MCP initialize");
    let linked = Workspace::new();
    linked.write(
        "src/linked.py",
        "def linked_entry(value): value.missing()\n",
    );
    w.write(
        "acdd.yaml",
        &format!(
            "roots: [.]\nlinked_workspaces:\n  - name: linked\n    path: '{}'\n    roots: [src]\n",
            linked.root().display()
        ),
    );
    w.build();
    // Scope identity and counts come from this published snapshot, not from
    // component labels or the current adapter after it has changed.
    let full = m
        .payload("code_map_overview", json!({"limit":100}))
        .expect("MCP tool should succeed");
    let scopes = full["coverage_by_workspace"]["items"]
        .as_array()
        .expect("persisted workspace breakdown");
    assert_eq!(full["coverage_by_workspace"]["total"], 2);
    for (name, root, prefix) in [
        ("primary", w.root(), ""),
        ("linked", linked.root(), "[linked]/"),
    ] {
        let scope = scopes.iter().find(|scope| scope["name"] == name).unwrap();
        assert_eq!(
            scope["root"],
            root.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(scope["path_prefix"], prefix);
        let prefix_parameter: &dyn rusqlite::ToSql = &prefix;
        let expected = fixture
            .rows(
                "SELECT c.status,count(*) total FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id WHERE (?1='' AND p.path NOT LIKE '[linked]/%') OR (?1<>'' AND substr(p.path,1,length(?1))=?1) GROUP BY c.status",
                &[prefix_parameter],
                10,
            )
            .expect("read persisted coverage status counts");
        for (status, field) in [
            ("resolved", "resolved"),
            ("ambiguous", "ambiguous"),
            ("unresolved", "unresolved"),
            ("external", "external_imports"),
        ] {
            let count = expected
                .iter()
                .find(|row| row["status"] == status)
                .and_then(|row| row["total"].as_u64())
                .unwrap_or(0);
            assert_eq!(scope[field], count, "{name}/{field}: {scope}");
        }
    }
    let first = m
        .payload(
            "code_map_overview",
            json!({"aspects":["coverage_by_workspace"],"limit":1}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(first["coverage_by_workspace"]["has_more"], true);
    let next = m.payload("code_map_overview", json!({"aspects":["coverage_by_workspace"],"limit":1,"offset":first["coverage_by_workspace"]["next_offset"],"generation":first["coverage_by_workspace"]["generation"]})).expect("MCP tool should succeed");
    let mut paged = vec![
        first["coverage_by_workspace"]["items"][0].clone(),
        next["coverage_by_workspace"]["items"][0].clone(),
    ];
    paged.sort_by_key(|scope| scope["name"].as_str().unwrap().to_owned());
    let mut expected = scopes.clone();
    expected.sort_by_key(|scope| scope["name"].as_str().unwrap().to_owned());
    assert_eq!(paged, expected);
    m.finish().expect("server must finish before restart");
    let mut restarted = fixture.start_mcp().expect("MCP initialize");
    assert_eq!(
        restarted
            .payload("code_map_overview", json!({"limit":100}))
            .expect("MCP tool should succeed")["coverage_by_workspace"],
        full["coverage_by_workspace"]
    );
    restarted
        .finish()
        .expect("server must finish before restart");
    w.write("acdd.yaml", "roots: [.]\n");
    let connection = w.open();
    let page = acdd_core::response::QueryOptions::resolve(
        &acdd_core::response::ResponsePolicy::default(),
        Some(100),
        0,
        None,
        None,
    )
    .expect("resolve persisted workspace coverage page");
    let persisted = reader::overview_with_options(
        &connection,
        &reader::OverviewOptions {
            aspects: None,
            page: &page,
        },
    )
    .expect("read published workspace coverage before rebuild");
    drop(connection);
    assert_eq!(
        persisted["coverage_by_workspace"]["items"],
        full["coverage_by_workspace"]["items"]
    );
    assert_eq!(
        persisted["coverage_by_workspace"]["total"],
        full["coverage_by_workspace"]["total"]
    );
    w.build();
    let mut rebuilt = fixture.start_mcp().expect("MCP initialize");
    let changed = rebuilt
        .payload("code_map_overview", json!({"limit":100}))
        .expect("MCP tool should succeed");
    assert_eq!(changed["coverage_by_workspace"]["total"], 1);
    assert_eq!(
        changed["coverage_by_workspace"]["items"][0]["name"],
        "primary"
    );
    assert!(full.get("generation").is_none());
    assert!(changed.get("generation").is_none());
    rebuilt.finish().expect("server must finish successfully");
}
