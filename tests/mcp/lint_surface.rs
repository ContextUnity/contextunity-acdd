use crate::cli::{run_cli, run_cli_json};
use crate::common::Workspace;
use crate::harness::McpTestHarness;
use acdd_core::ports::is_builtin_mcp_tool;
use acdd_core::subsystems::{SubsystemId, SubsystemRegistry};
use serde_json::{json, Value};

#[test]
fn mcp_lint_surface_registration_parity_and_budgeting() {
    assert!(is_builtin_mcp_tool("code_map_lint"));
    assert_eq!(
        SubsystemRegistry::subsystem_id_for_tool("code_map_lint"),
        Some(SubsystemId::Code)
    );

    let ws = Workspace::with_files(&[
        (
            "acdd.yaml",
            r#"roots: [src, docs]
architecture:
  zones:
    core: ["src/core/**"]
    infra: ["src/infra/**"]
response:
  max_output_bytes: 2048
"#,
        ),
        (
            "docs/architecture/rules.md",
            r#"---
title: Architecture Rules
doc_type: architecture
---
# Architecture Rules

> - INV-TEST-CORE-ISOLATION: `core` must never import `infra`.

```acdd-rule
id: INV-TEST-CORE-ISOLATION
kind: forbid_dependency
severity: error
edge_kinds: [imports]
from:
  zones: [core]
to:
  zones: [infra]
message: "core must not depend on infra"
```

```acdd-rule
id: WARN-NO-TODO
kind: ast_pattern
severity: warn
from:
  paths: ["src/**"]
  languages: [rust]
ast_pattern: "connect()"
message: "avoid direct connect calls"
```
"#,
        ),
        ("src/infra/db.rs", "pub fn connect() {}\n"),
        (
            "src/core/service.rs",
            "use crate::infra::db::connect;\npub fn run() { connect(); }\npub fn todo_placeholder() {}\n",
        ),
        ("src/core/clean.rs", "pub fn ok() -> i32 { 1 }\n"),
    ]);
    ws.build();

    let mut mcp = McpTestHarness::new(&ws).expect("MCP harness");
    let listed = mcp
        .request_value("tools/list", json!({}))
        .expect("tools/list");
    let tool_names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(tool_names.len(), 18);
    assert!(tool_names
        .iter()
        .all(|name| acdd_core::ports::is_builtin_mcp_tool(name)));
    assert!(tool_names.contains(&"code_map_lint"));
    assert!(!tool_names.contains(&"code_map_tests"));
    assert!(!tool_names.contains(&"code_map_prove_removal"));

    // Table-driven CLI vs MCP JSON parity & exit code matrix.
    struct Case<'a> {
        label: &'a str,
        cli_args: Vec<&'a str>,
        mcp_args: Value,
        expect_cli_ok: bool,
        expected_errors: u64,
        expected_warnings: u64,
    }

    let cases = [
        Case {
            label: "default categories catch arch error + warn",
            cli_args: vec!["lint"],
            mcp_args: json!({}),
            expect_cli_ok: false,
            expected_errors: 1,
            expected_warnings: 1,
        },
        Case {
            label: "severity=error filters out warning",
            cli_args: vec!["lint", "--severity", "error"],
            mcp_args: json!({"severity": "error"}),
            expect_cli_ok: false,
            expected_errors: 1,
            expected_warnings: 0,
        },
        Case {
            label: "rule filter selects only warning -> exit 0",
            cli_args: vec!["lint", "--rule", "WARN-NO-TODO"],
            mcp_args: json!({"rules": ["WARN-NO-TODO"]}),
            expect_cli_ok: true,
            expected_errors: 0,
            expected_warnings: 1,
        },
        Case {
            label: "clean target -> 0 violations and exit 0",
            cli_args: vec!["lint", "src/core/clean.rs"],
            mcp_args: json!({"target": "src/core/clean.rs"}),
            expect_cli_ok: true,
            expected_errors: 0,
            expected_warnings: 0,
        },
        Case {
            label: "categories=syntax -> 0 violations and exit 0",
            cli_args: vec!["lint", "--categories", "syntax"],
            mcp_args: json!({"categories": ["syntax"]}),
            expect_cli_ok: true,
            expected_errors: 0,
            expected_warnings: 0,
        },
    ];

    for case in &cases {
        let cli_out = run_cli_json(&ws, &case.cli_args);
        assert_eq!(
            cli_out.status.success(),
            case.expect_cli_ok,
            "{}: stderr={}",
            case.label,
            String::from_utf8_lossy(&cli_out.stderr)
        );
        let cli_json: Value = serde_json::from_slice(&cli_out.stdout)
            .unwrap_or_else(|e| panic!("{}: invalid CLI JSON: {e}", case.label));
        let mcp_json = mcp
            .payload("code_map_lint", case.mcp_args.clone())
            .unwrap_or_else(|e| panic!("{}: MCP error: {e}", case.label));

        assert_eq!(
            cli_json["summary"]["errors"], case.expected_errors,
            "{}: {cli_json}",
            case.label
        );
        assert_eq!(
            cli_json["summary"]["warnings"], case.expected_warnings,
            "{}: {cli_json}",
            case.label
        );
        assert_eq!(cli_json["summary"], mcp_json["summary"], "{}", case.label);
        assert_eq!(cli_json["items"], mcp_json["items"], "{}", case.label);
    }

    // Markdown surface parity & workspace-confined file:/// URIs.
    let cli_md = run_cli(&ws, &["lint"]);
    assert!(!cli_md.status.success());
    let cli_md_text = String::from_utf8_lossy(&cli_md.stdout);
    let mcp_md_compact = mcp
        .text("code_map_lint", json!({"format": "markdown"}))
        .expect("MCP markdown");
    let mcp_md_uris = mcp
        .text(
            "code_map_lint",
            json!({"format": "markdown", "file_uris": true}),
        )
        .expect("MCP markdown with URIs");

    let expected_prefix = format!("file://{}", ws.root().canonicalize().unwrap().display());
    for (surface, md) in [
        ("CLI", cli_md_text.as_ref()),
        ("MCP", mcp_md_compact.as_str()),
    ] {
        assert!(md.contains("INV-TEST-CORE-ISOLATION"), "{surface}: {md}");
        assert!(
            !md.contains("file://"),
            "{surface} compact default omits file:// URIs: {md}"
        );
        assert!(
            !md.contains(".."),
            "{surface} leaked parent traversal: {md}"
        );
    }
    assert!(
        mcp_md_uris.contains(&expected_prefix),
        "MCP opt-in missing confined file:/// URI: {mcp_md_uris}"
    );

    // Response budgeting & paging under configured 2048-byte limit.
    let mut big_src = String::from("use crate::infra::db::connect;\n");
    for i in 0..20 {
        big_src.push_str(&format!("pub fn todo_placeholder_{i}() {{ connect(); }}\n"));
    }
    ws.write("src/core/service.rs", &big_src);
    let mut extra_rules =
        String::from("---\ntitle: Extra Rules\ndoc_type: architecture\n---\n# Extra Rules\n");
    for i in 0..15 {
        extra_rules.push_str(&format!(
            "```acdd-rule\nid: WARN-EXTRA-{i}\nkind: ast_pattern\nseverity: warn\nfrom:\n  paths: [\"src/core/service.rs\"]\n  languages: [rust]\nast_pattern: \"pub fn todo_placeholder_{i}() {{ $$$BODY }}\"\nmessage: \"extra diagnostic payload padding {i} for response budget test\"\n```\n"
        ));
    }
    ws.write("docs/architecture/extra.md", &extra_rules);
    ws.build();
    let mut mcp_budget = McpTestHarness::new(&ws).expect("MCP harness refresh");
    let budgeted = mcp_budget
        .payload("code_map_lint", json!({"detail": "compact", "limit": 50}))
        .expect("budgeted code_map_lint");
    assert!(
        budgeted["items"].as_array().unwrap().len() < 15 || budgeted["byte_limited"] == true,
        "expected response budget compaction/trimming: {budgeted}"
    );
    let paged_md = mcp_budget
        .text("code_map_lint", json!({"limit": 1, "format": "markdown"}))
        .expect("paged markdown");
    assert!(
        paged_md.contains("`generation=") && !paged_md.contains("`generation=`"),
        "continuation line must not contain nested backticks: {paged_md}"
    );
}
