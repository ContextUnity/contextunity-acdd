use crate::common::mcp_harness as in_process;
#[path = "../mcp/cli.rs"]
mod in_process_cli;

use super::*;

#[test]
fn diagnostics_summarize_directories_and_page_exact_files() {
    let workspace = Workspace::new();
    workspace.write("pkg/clean.py", "def clean(): return 1\n");
    for file in 0..12 {
        let mut code = String::from("def entry():\n");
        for line in 0..45 {
            code.push_str(&format!("    missing_{line}()\n"));
        }
        workspace.write(format!("pkg/file_{file:02}.py"), &code);
    }
    workspace.build();
    let conn = workspace.open();
    let summary = reader::analyze_paged(&conn, "pkg", None, &options(7)).unwrap();
    assert_eq!(summary["scope"], "summary");
    assert_eq!(summary["total_unresolved"], 540);
    assert_eq!(summary["top_files"].as_array().unwrap().len(), 5);
    assert!(summary.get("resolution").is_none());
    assert_eq!(summary["cycles"]["omitted"], true);
    let mut query = options(7);
    let mut lines = Vec::new();
    loop {
        let value = reader::analyze_paged(&conn, "pkg/file_00.py", None, &query).unwrap();
        assert_eq!(value["scope"], "file");
        let page = &value["resolution"];
        assert_eq!(page["total"], 45);
        lines.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["line"].as_u64().unwrap()),
        );
        if !continue_page(&mut query, page) {
            break;
        }
    }
    assert_eq!(lines, (2..47).collect::<Vec<_>>());

    let mut exact_file_options = options(7);
    let exact_file =
        reader::analyze_paged(&conn, "pkg/file_00.py", None, &exact_file_options).unwrap();
    assert_eq!(exact_file["scope"], "file");
    assert_eq!(exact_file["total_errors"], 0);
    assert_eq!(exact_file["resolution"]["total"], 45);
    assert!(exact_file["resolution"]["has_more"].as_bool().unwrap());
    assert!(exact_file.get("top_files").is_none());
    assert!(continue_page(
        &mut exact_file_options,
        &exact_file["resolution"]
    ));
    let exact_file_continuation =
        reader::analyze_paged(&conn, "pkg/file_00.py", None, &exact_file_options).unwrap();
    assert_eq!(exact_file_continuation["scope"], "file");
    assert_eq!(exact_file_continuation["resolution"]["total"], 45);
    assert_eq!(exact_file_continuation["resolution"]["offset"], 7);
    assert!(exact_file_continuation.get("top_files").is_none());

    let clean = reader::analyze_paged(&conn, "pkg/clean.py", None, &options(7)).unwrap();
    assert_eq!(clean["scope"], "file");
    assert_eq!(clean["total_errors"], 0);
    assert_eq!(clean["total_unresolved"], 0);
    assert_eq!(clean["total_external_imports"], 0);
    assert_eq!(clean["errors"]["total"], 0);
    assert_eq!(clean["resolution"]["total"], 0);
    assert_eq!(clean["external_imports"]["total"], 0);
    assert!(clean.get("top_files").is_none());

    for mode in ["diagnostics", "cycles"] {
        let error = reader::analyze_paged(&conn, mode, None, &options(7))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "analysis target '{mode}' is interpreted as a path, not an analysis mode"
            )),
            "{error}"
        );
        assert!(!error.contains("PATH_NOT_INDEXED"), "{error}");
    }
    let missing = reader::analyze_paged(&conn, "nonexistent.rs", None, &options(7))
        .unwrap_err()
        .to_string();
    assert_eq!(
        missing,
        "PATH_NOT_INDEXED: 'nonexistent.rs' was not found in the indexed inventory"
    );
}

#[test]
fn sql_pages_count_full_results_preserve_order_and_reject_writes() {
    let workspace = Workspace::new();
    workspace.write(
        "code.py",
        &(0..73)
            .map(|i| format!("def f{i:03}(): return {i}\n"))
            .collect::<String>(),
    );
    workspace.build();
    let conn = workspace.open();
    let sql = "WITH funcs AS (SELECT name FROM nodes WHERE kind='function') SELECT name FROM funcs ORDER BY name DESC";
    let mut query = options(8);
    let mut names = Vec::new();
    loop {
        let value = reader::analyze_paged(&conn, sql, None, &query).unwrap();
        let page = &value["rows"];
        assert_eq!(page["total"], 73);
        names.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["name"].as_str().unwrap().to_owned()),
        );
        if !continue_page(&mut query, page) {
            break;
        }
    }
    assert_eq!(
        names,
        (0..73)
            .rev()
            .map(|i| format!("f{i:03}"))
            .collect::<Vec<_>>()
    );
    let before = persisted_state(&conn);
    let attachment = workspace
        .db()
        .parent()
        .unwrap()
        .join("sql-attachment.sqlite");
    let attach_sql = format!("ATTACH DATABASE '{}' AS injected", attachment.display());
    for sql in [
        "DELETE FROM nodes",
        "DELETE FROM files",
        "WITH n AS (SELECT 1) DELETE FROM nodes",
        "UPDATE nodes SET kind='hacked'",
        "UPDATE metadata SET value='mutated' WHERE key='workspace_root'",
        "INSERT INTO metadata(key,value) VALUES ('sql-audit-sentinel','changed')",
        "DROP TABLE nodes",
        "PRAGMA user_version=32767",
        "PRAGMA journal_mode=WAL",
        "SELECT 1; SELECT 2",
        "SELECT 1; DELETE FROM nodes",
    ]
    .into_iter()
    .chain(std::iter::once(attach_sql.as_str()))
    {
        assert!(
            reader::analyze_paged(&conn, sql, None, &options(8)).is_err(),
            "accepted unsafe SQL: {sql}"
        );
        assert_eq!(
            persisted_state(&conn),
            before,
            "SQL changed database state: {sql}"
        );
        assert!(!attachment.exists(), "SQL created attached database: {sql}");
    }
}

#[test]
fn removal_safety_uses_complete_evidence_when_the_visible_page_is_empty() {
    let workspace = Workspace::new();
    workspace.write(
        "code.py",
        "def target(): return 1\ndef caller(): return target()\n",
    );
    workspace.build();
    let conn = workspace.open();
    let initial = traversal::removal_paged_with_path(&conn, "target", None, &options(1)).unwrap();
    assert_eq!(initial["incoming_dependencies"]["total"], 1);
    assert_eq!(initial["safe_to_remove"], false);
    let mut query = options(1);
    query.offset = 20;
    query.generation = Some(initial["generation"].as_str().unwrap().to_owned());
    let empty = traversal::removal_paged_with_path(&conn, "target", None, &query).unwrap();
    assert!(empty["incoming_dependencies"]["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(empty["incoming_dependencies"]["total"], 1);
    assert_eq!(empty["safe_to_remove"], false);
}

#[test]
fn source_preview_preserves_utf8_crlf_boundaries_and_line_continuation() {
    let workspace = Workspace::new();
    let body = format!(
        "def selected():\r\n{}    return 'Привіт'\r\n",
        (0..60)
            .map(|i| format!("    value_{i} = {i}\r\n"))
            .collect::<String>()
    );
    workspace.write("code.py", &format!("def previous():\r\n    return 'PREVIOUS_SECRET'\r\n\r\n# selected documentation\r\n{body}"));
    let cases = [116, 200, 201, 500, 527].map(|lines| {
        let name = format!("selected_{lines}");
        let body = format!(
            "def {name}():\r\n{}    return 'Привіт'\r\n",
            "    pass\r\n".repeat(lines - 2)
        );
        workspace.write(format!("preview_{lines}.py"), &body);
        (lines, name, body)
    });
    workspace.build();
    let conn = workspace.open();
    let policy = ResponsePolicy::default();
    let disabled = SourceOptions::resolve(&policy, None, None, None, 0).unwrap();
    assert!(symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &disabled,
            coverage: CoverageOptions::default(),
            page: &options(30)
        }
    )
    .unwrap()
    .get("source")
    .is_none());
    let preview = SourceOptions::resolve(&policy, Some(true), None, Some(35), 0).unwrap();
    let first = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &preview,
            coverage: CoverageOptions::default(),
            page: &options(30),
        },
    )
    .unwrap();
    let source = first["source"].as_str().unwrap();
    assert!(source.contains("# selected documentation\r\n"));
    assert!(!source.contains("PREVIOUS_SECRET"));
    assert!(!source.contains("def previous"));
    assert_eq!(first["source_preview"]["body_lines"], 35);
    assert_eq!(first["source_preview"]["next_source_offset"], 35);
    let mut query = options(30);
    query.generation = Some(first["generation"].as_str().unwrap().to_owned());
    let next = SourceOptions::resolve(&policy, Some(true), None, Some(35), 35).unwrap();
    let last = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &next,
            coverage: CoverageOptions::default(),
            page: &query,
        },
    )
    .unwrap();
    assert_eq!(last["source_preview"]["has_more"], false);
    assert!(last["source"]
        .as_str()
        .unwrap()
        .ends_with("return 'Привіт'\r\n"));
    let first_body = source.split_once("def selected").unwrap().1;
    assert_eq!(
        format!(
            "def selected{first_body}{}",
            last["source"].as_str().unwrap()
        ),
        body
    );
    let finished = SourceOptions::resolve(&policy, Some(true), None, Some(35), 62).unwrap();
    let empty = symbols::explain_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::SymbolExplainOptions {
            explain: reader::ExplainOptions {
                direction: reader::ExplainDirection::Both,
                show_doc: true,
                page: &query,
            },
            source: &finished,
            coverage: CoverageOptions::default(),
        },
    )
    .unwrap();
    assert_eq!(empty["source"], "");
    assert!(empty["source_preview"]["end_line"].is_null());
    let cli_first = in_process_cli::run_cli_json(
        &workspace,
        &["query", "inspect", "selected_201", "--show-source"],
    );
    common::assertions::assert_command_succeeded(&cli_first);
    let cli_first: Value = serde_json::from_slice(&cli_first.stdout).unwrap();
    let cli_offset = cli_first["source_preview"]["next_source_offset"]
        .as_u64()
        .expect("large CLI source must produce a continuation cursor");
    let cli_generation = cli_first["generation"].as_str().unwrap();
    let cli_missing = in_process_cli::run_cli_json(
        &workspace,
        &[
            "query",
            "inspect",
            "selected_201",
            "--show-source",
            "--source-offset",
            &cli_offset.to_string(),
        ],
    );
    assert!(!cli_missing.status.success());
    assert!(String::from_utf8_lossy(&cli_missing.stderr).contains("previous page's generation"));
    let cli_valid = in_process_cli::run_cli_json(
        &workspace,
        &[
            "query",
            "inspect",
            "selected_201",
            "--show-source",
            "--source-offset",
            &cli_offset.to_string(),
            "--generation",
            cli_generation,
        ],
    );
    common::assertions::assert_command_succeeded(&cli_valid);
    let cli_valid: Value = serde_json::from_slice(&cli_valid.stdout).unwrap();
    assert_eq!(cli_valid["source_preview"]["body_offset"], cli_offset);
    assert!(symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &next,
            coverage: CoverageOptions::default(),
            page: &options(30)
        }
    )
    .is_err());
    let mut client = in_process::McpTestHarness::new(&workspace).expect("MCP initialize");
    let mcp_first = client
        .payload(
            "acdd_read",
            serde_json::json!({"selector":"selected","max_body_lines":35,"format":"json"}),
        )
        .expect("MCP tool should succeed");
    assert_eq!(mcp_first["source_preview"]["next_source_offset"], 35);
    let (_, inferred_response) = client
        .call(
            "acdd_read",
            serde_json::json!({
                "selector":"selected",
                "max_body_lines":35,
                "source_offset":mcp_first["source_preview"]["next_source_offset"],
                "format":"json"
            }),
        )
        .expect("MCP transport");
    assert_eq!(
        inferred_response["result"]["isError"], true,
        "source continuation must require an explicit generation even in the same session: {inferred_response}"
    );
    let resumed = client
        .payload(
            "acdd_read",
            serde_json::json!({
                "selector":"selected",
                "max_body_lines":35,
                "source_offset":35,
                "generation":mcp_first["generation"],
            }),
        )
        .expect("MCP tool should succeed");
    assert_eq!(resumed["source_preview"]["body_offset"], 35);
    let mut failures = Vec::new();
    let bounded_workspace = Workspace::new();
    bounded_workspace.write(
        "acdd.yaml",
        "roots: [.]\nresponse:\n  max_output_bytes: 4096\n  source_context:\n    max_body_lines: 16\n",
    );
    let bounded_body = (0..700)
        .map(|index| format!("    value_{index:04} = '{}\\\"Привіт'\n", "x".repeat(36)))
        .collect::<String>();
    let bounded_source = format!("def bounded():\n{bounded_body}    return value_0000\n");
    bounded_workspace.write("bounded.py", &bounded_source);
    let mut bounded_client =
        in_process::McpTestHarness::new(&bounded_workspace).expect("MCP initialize");
    let markdown_number = |text: &str, field: &str| -> Option<usize> {
        let marker = format!("**{field}**: `");
        let (_, remainder) = text.split_once(marker.as_str())?;
        let (number, _) = remainder.split_once('`')?;
        number.parse().ok()
    };
    for (label, format) in [("JSON", Some("json")), ("default Markdown", None)] {
        let mut arguments = serde_json::json!({
            "selector":"bounded.py:bounded",
            "leading_lines":0,
            "max_body_lines":800
        });
        if let Some(format) = format {
            arguments["format"] = serde_json::json!(format);
        }
        let (first_bytes, first_response) = bounded_client
            .call("acdd_read", arguments.clone())
            .expect("MCP transport");
        if first_response["result"]["isError"] == true {
            failures.push(format!(
                "{label} bounded first page errored: {first_response}"
            ));
            continue;
        }
        if first_bytes > 4096 || first_bytes <= 2048 {
            failures.push(format!(
                "{label} page was not a bounded 4-KiB response: {first_bytes} bytes"
            ));
        }
        let cursor = if format.is_some() {
            let page = common::assertions::mcp_payload(&first_response);
            let preview = &page["source_preview"];
            if preview["body_lines"]
                .as_u64()
                .zip(preview["total_body_lines"].as_u64())
                .is_none_or(|(body, total)| body >= total)
                || preview["omitted_body_lines"].as_u64().unwrap_or_default() == 0
            {
                failures.push(format!(
                    "{label} response did not retain a trimmed source page: {preview}"
                ));
            }
            preview["next_source_offset"]
                .as_u64()
                .map(|offset| offset as usize)
        } else {
            let text = first_response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            let body_lines = markdown_number(text, "body_lines");
            let total_lines = markdown_number(text, "total_body_lines");
            if body_lines
                .zip(total_lines)
                .is_none_or(|(body, total)| body >= total)
            {
                failures.push(format!(
                    "{label} response did not retain a trimmed source page"
                ));
            }
            markdown_number(text, "next_source_offset")
        };
        let Some(cursor) = cursor.filter(|cursor| *cursor > 0) else {
            failures.push(format!("{label} bounded first page has no retained cursor"));
            continue;
        };
        let mut continuation = arguments;
        continuation["source_offset"] = serde_json::json!(cursor);
        let (_, missing_generation) = bounded_client
            .call("acdd_read", continuation.clone())
            .expect("MCP transport");
        assert_eq!(missing_generation["result"]["isError"], true);
        let generation = if format.is_some() {
            common::assertions::mcp_payload(&first_response)["generation"].clone()
        } else {
            let text = first_response["result"]["content"][0]["text"]
                .as_str()
                .unwrap();
            let encoded = text
                .split_once("**generation**: `")
                .unwrap()
                .1
                .split('`')
                .next()
                .unwrap();
            serde_json::from_str(encoded).unwrap()
        };
        continuation["generation"] = generation;
        let (_, next_response) = bounded_client
            .call("acdd_read", continuation)
            .expect("MCP transport");
        if next_response["result"]["isError"] == true {
            failures.push(format!(
                "{label} trimmed page rejected explicit generation at {cursor}: {next_response}"
            ));
            continue;
        }
        if format.is_some() {
            let first = common::assertions::mcp_payload(&first_response);
            let next = common::assertions::mcp_payload(&next_response);
            if next["source_preview"]["body_offset"].as_u64() != Some(cursor as u64)
                || next["generation"] != first["generation"]
            {
                failures.push(format!(
                    "{label} continuation lost the retained cursor/generation: {next}"
                ));
            }
        } else {
            let text = next_response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            if !text.contains(&format!("**body_offset**: `{cursor}`")) {
                failures.push(format!(
                    "{label} continuation did not resume at {cursor}: {text}"
                ));
            }
        }
    }

    let line_workspace = Workspace::new();
    line_workspace.write(
        "acdd.yaml",
        "roots: [.]\nresponse:\n  max_output_bytes: 4096\n",
    );
    line_workspace.write(
        "single_line.py",
        &format!("def budget_line():\n    return '{}'\n", "\\\\".repeat(950)),
    );
    let mut line_client = in_process::McpTestHarness::new(&line_workspace).expect("MCP initialize");
    let first_line = line_client
        .payload(
            "acdd_read",
            serde_json::json!({"selector":"budget_line","leading_lines":0,"max_body_lines":1}),
        )
        .expect("MCP tool should succeed");
    for format in ["json", "markdown"] {
        let (bytes, response) = line_client
            .call(
                "acdd_read",
                serde_json::json!({
                    "selector":"budget_line","leading_lines":0,"max_body_lines":1,
                    "source_offset":1,"generation":first_line["generation"],"format":format,
                }),
            )
            .expect("MCP transport");
        assert!(bytes <= 4096, "{format}: {bytes}");
        assert_eq!(response["result"]["isError"], true, "{format}: {response}");
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("line exceeds"));
    }

    for (lines, name, body) in cases {
        for requested in [None, Some(35), Some(500)] {
            for explain in [false, true] {
                let mut offset = 0;
                let mut query = options(30);
                let mut reconstructed = String::new();
                loop {
                    let source =
                        SourceOptions::resolve(&policy, Some(true), Some(0), requested, offset)
                            .unwrap();
                    let result = if explain {
                        symbols::explain_with_path_options(
                            &conn,
                            workspace.root(),
                            &name,
                            None,
                            &symbols::SymbolExplainOptions {
                                explain: reader::ExplainOptions {
                                    direction: reader::ExplainDirection::Both,
                                    show_doc: true,
                                    page: &query,
                                },
                                source: &source,
                                coverage: CoverageOptions::default(),
                            },
                        )
                    } else {
                        symbols::inspect_with_path_options(
                            &conn,
                            workspace.root(),
                            &name,
                            None,
                            &symbols::InspectOptions {
                                show_doc: false,
                                source: &source,
                                coverage: CoverageOptions::default(),
                                page: &query,
                            },
                        )
                    }
                    .unwrap();
                    let preview = &result["source_preview"];
                    assert_eq!(preview["total_body_lines"], lines);
                    assert_eq!(preview["truncated_line"], false);
                    assert!(serde_json::to_vec(&result["source"]).unwrap().len() <= 8192);
                    if offset == 0 {
                        let expected_lines = lines.min(requested.unwrap_or(100));
                        if preview["body_lines"] != expected_lines {
                            failures.push(format!("{name}, requested={requested:?}, explain={explain}: expected {expected_lines} body lines, got {}", preview["body_lines"]));
                        }
                        let expected_more = lines > expected_lines;
                        if preview["has_more"] != expected_more {
                            failures.push(format!("{name}, requested={requested:?}, explain={explain}: expected has_more={expected_more}, got {}", preview["has_more"]));
                        }
                        if !expected_more && !preview["next_source_offset"].is_null() {
                            failures.push(format!(
                                "{name}: complete preview must not require continuation"
                            ));
                        }
                    }
                    reconstructed.push_str(result["source"].as_str().unwrap());
                    if !preview["has_more"].as_bool().unwrap() {
                        assert!(preview["next_source_offset"].is_null());
                        break;
                    }
                    let next_offset = preview["next_source_offset"].as_u64().unwrap() as usize;
                    assert!(next_offset > offset && next_offset <= lines);
                    assert_eq!(
                        next_offset,
                        offset + preview["body_lines"].as_u64().unwrap() as usize
                    );
                    offset = next_offset;
                    query.generation = Some(result["generation"].as_str().unwrap().to_owned());
                }
                assert_eq!(reconstructed, body, "{name}, explain={explain}");
            }
        }
    }
    workspace.write("code.py", "def selected(): return 99\n");
    assert!(symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &preview,
            coverage: CoverageOptions::default(),
            page: &query
        }
    )
    .is_err());
    drop(conn);
    client.finish().expect("server must finish successfully");
    workspace.write(
        "preview_201.py",
        "def selected_201():\n    replacement = True\n    return replacement\n",
    );
    workspace.build();
    let cli_stale = in_process_cli::run_cli_json(
        &workspace,
        &[
            "query",
            "inspect",
            "selected_201",
            "--show-source",
            "--source-offset",
            &cli_offset.to_string(),
            "--generation",
            cli_generation,
        ],
    );
    assert!(!cli_stale.status.success());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    bounded_client
        .finish()
        .expect("server must finish successfully");
    line_client
        .finish()
        .expect("server must finish successfully");
}

#[test]
fn oversized_source_lines_fail_without_partial_code() {
    let workspace = Workspace::new();
    workspace.write(
        "large.py",
        &format!("def huge(): return '{}'\n", "é\\\"".repeat(12000)),
    );
    workspace.build();
    let conn = workspace.open();
    let source =
        SourceOptions::resolve(&ResponsePolicy::default(), Some(true), None, None, 0).unwrap();
    let error = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "huge",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &source,
            coverage: CoverageOptions::default(),
            page: &options(30),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("line exceeds"), "{error}");
    assert!(error.contains("large.py"), "{error}");
}

#[test]
fn continuation_rejects_a_new_index_generation() {
    let workspace = Workspace::new();
    workspace.write("code.py", "def first(): pass\ndef second(): pass\n");
    workspace.build();
    let conn = workspace.open();
    let first = symbols::search_with_options(
        &conn,
        "*i*",
        &symbols::SearchOptions {
            kind: Some("function"),
            path: None,
            include_docs: false,
            exact: false,
            classification: None,
            page: &options(1),
        },
    )
    .unwrap();
    let generation = first["nodes"]["generation"].as_str().unwrap().to_owned();
    drop(conn);
    workspace.write(
        "code.py",
        "def first(): pass\ndef third(): pass\ndef second(): pass\n",
    );
    writer::delta_with_selection(
        workspace.root(),
        &workspace.db(),
        &[PathBuf::from("code.py")],
        None,
        None,
    )
    .unwrap();
    let conn = reader::open(&workspace.db(), workspace.root()).unwrap();
    let mut query = options(1);
    query.offset = 1;
    query.generation = Some(generation);
    assert!(symbols::search_with_options(
        &conn,
        "*i*",
        &symbols::SearchOptions {
            kind: Some("function"),
            path: None,
            include_docs: false,
            exact: false,
            classification: None,
            page: &query
        }
    )
    .unwrap_err()
    .to_string()
    .contains("generation changed"));
    query.generation = None;
    assert!(symbols::search_with_options(
        &conn,
        "*i*",
        &symbols::SearchOptions {
            kind: Some("function"),
            path: None,
            include_docs: false,
            exact: false,
            classification: None,
            page: &query
        }
    )
    .is_err());
}

#[cfg(feature = "lang-typescript")]
#[test]
fn source_preview_does_not_include_neighboring_symbols_on_the_same_line() {
    let workspace = Workspace::new();
    workspace.write("same.ts", "function before() { return 'SECRET'; } function selected() { return 'selected'; } function after() { return 'AFTER'; }\n");
    workspace.build();
    let conn = workspace.open();
    let source =
        SourceOptions::resolve(&ResponsePolicy::default(), Some(true), None, None, 0).unwrap();
    let result = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &source,
            coverage: CoverageOptions::default(),
            page: &options(30),
        },
    )
    .unwrap();
    assert_eq!(
        result["source"],
        "function selected() { return 'selected'; }"
    );
    drop(conn);
    workspace.write("anonymous.ts", "(function () { return 42; })();\n");
    workspace.build();
    let conn = workspace.open();
    let anonymous: String = conn
        .query_row(
            "SELECT n.id FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path='anonymous.ts' AND n.kind='function'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let result = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        &anonymous,
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &source,
            coverage: CoverageOptions::default(),
            page: &options(30),
        },
    )
    .unwrap();
    assert_eq!(result["source"], "function () { return 42; }");
}

#[cfg(feature = "lang-vue")]
#[test]
fn source_preview_resolves_vue_script_boundaries() {
    let workspace = Workspace::new();
    workspace.write("page.vue", "<template><div>{{ 'visible' }}</div></template>\n<script setup lang=\"ts\">function before() { return 'SECRET'; } function selected() { return 'visible'; }</script>\n");
    workspace.build();
    let conn = workspace.open();
    let source =
        SourceOptions::resolve(&ResponsePolicy::default(), Some(true), None, None, 0).unwrap();
    let result = symbols::inspect_with_path_options(
        &conn,
        workspace.root(),
        "selected",
        None,
        &symbols::InspectOptions {
            show_doc: false,
            source: &source,
            coverage: CoverageOptions::default(),
            page: &options(30),
        },
    )
    .unwrap();
    assert_eq!(
        result["source"],
        "function selected() { return 'visible'; }"
    );
}

#[test]
fn overview_reports_language_breakdown_and_compiled_profiles() {
    let workspace = Workspace::new();
    workspace.write("a.py", "def f(): return unknown_func()\n");
    #[cfg(feature = "lang-rust")]
    workspace.write("b.rs", "pub fn f() {}\n");
    workspace.build();
    let conn = workspace.open();
    let overview = reader::overview_with_options(
        &conn,
        &reader::OverviewOptions {
            aspects: None,
            page: &options(1),
        },
    )
    .unwrap();
    let empty_aspects: Vec<String> = Vec::new();
    let empty_aspect_overview = reader::overview_with_options(
        &conn,
        &reader::OverviewOptions {
            aspects: Some(&empty_aspects),
            page: &options(1),
        },
    )
    .unwrap();
    assert_eq!(empty_aspect_overview, overview);
    assert!(empty_aspect_overview.get("cycles").is_none());
    assert!(overview["generation"].is_string());
    assert!(overview["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["key"] == "workspace_root"));
    assert!(!overview["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["key"] == "output_root" || row["key"] == "corpus_hash"));
    let langs = overview["languages"]["items"].as_array().unwrap();
    let py = langs.iter().find(|l| l["language"] == "python").unwrap();
    assert!(py["files"].as_u64().unwrap() >= 1);
    assert!(py["unresolved"].as_u64().unwrap() >= 1);
    let profiles = overview["compiled_profiles"].as_array().unwrap();
    assert!(profiles.iter().any(|p| p.as_str() == Some("python")));
    #[cfg(feature = "lang-rust")]
    {
        let mut page_options = options(1);
        assert_eq!(overview["languages"]["total"], 2);
        assert!(continue_page(&mut page_options, &overview["languages"]));
        let next = reader::overview_with_options(
            &conn,
            &reader::OverviewOptions {
                aspects: None,
                page: &page_options,
            },
        )
        .unwrap();
        assert_eq!(next["languages"]["total"], 2);
        assert_eq!(next["languages"]["items"][0]["language"], "rust");
        assert!(!next["languages"]["has_more"].as_bool().unwrap());
    }

    use contextunity_acdd::format::{self, OutputFormat};
    let mut missing = Vec::new();
    // These are recorded resolver outcomes, not inferred labels or visible-page totals.
    let fixture = Workspace::new();
    let linked = Workspace::new();
    fixture.write("scope.py", &"# recorded reference\n".repeat(120));
    linked.write("linked_scope.py", "def linked_target(): pass\n");
    fixture.write(
        "acdd.yaml",
        &format!(
            "roots: [.]\nlinked_workspaces:\n  - name: library\n    path: '{}'\n    roots: [.]\n",
            linked.root().display()
        ),
    );
    fixture.build();
    drop(fixture.open());
    let recorded = Connection::open(fixture.db()).unwrap();
    for (unknown, external, health) in [(0, 0, "80.0%"), (0, 12, "80.0%"), (1, 12, "79.2%")] {
        recorded
            .execute_batch(
                "DELETE FROM coverage_language_counts;
                 DELETE FROM coverage_owner_language;
                 DELETE FROM resolution_coverage;",
            )
            .unwrap();
        let populations = [
            (80, "resolved", "target", "exact indexed target"),
            (
                6,
                "unresolved",
                "callback",
                "callee is shadowed by a parameter or local binding of unknown callable identity",
            ),
            (
                4,
                "unresolved",
                "receiver.call",
                "callee is shadowed by a parameter or local binding of unknown callable identity",
            ),
            (
                3,
                "unresolved",
                "framework",
                "diagnostic_reason=framework_magic",
            ),
            (
                2,
                "unresolved",
                "external.call",
                "call through external import package; callable target unverified",
            ),
            (5, "ambiguous", "candidate", "multiple indexed candidates"),
            (
                unknown,
                "unresolved",
                "unknown",
                "unclassified resolver evidence",
            ),
            (
                external,
                "external",
                "external.import",
                "call through external import package; callable target unverified",
            ),
        ];
        let mut line = 0;
        for (count, status, expression, evidence) in populations {
            recorded
                .execute(
                    "INSERT OR IGNORE INTO coverage_expressions(expression) VALUES(?1)",
                    [expression],
                )
                .unwrap();
            recorded
                .execute(
                    "INSERT OR IGNORE INTO coverage_evidence(evidence) VALUES(?1)",
                    [evidence],
                )
                .unwrap();
            for _ in 0..count {
                line += 1;
                recorded.execute(
                    "INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id) VALUES((SELECT path_id FROM path_dictionary WHERE path='scope.py'),?1,(SELECT expression_id FROM coverage_expressions WHERE expression=?2),?3,(SELECT evidence_id FROM coverage_evidence WHERE evidence=?4))",
                    rusqlite::params![line, expression, status, evidence],
                ).unwrap();
            }
        }
        recorded
            .execute(
                "INSERT OR IGNORE INTO coverage_expressions(expression) VALUES('linked target')",
                [],
            )
            .unwrap();
        recorded
            .execute(
                "INSERT OR IGNORE INTO coverage_evidence(evidence) VALUES('exact indexed linked target')",
                [],
            )
            .unwrap();
        recorded
            .execute(
                "INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id) VALUES((SELECT path_id FROM path_dictionary WHERE path='[library]/linked_scope.py'),1,(SELECT expression_id FROM coverage_expressions WHERE expression='linked target'),'resolved',(SELECT evidence_id FROM coverage_evidence WHERE evidence='exact indexed linked target'))",
                [],
            )
            .unwrap();
        // Keep the persisted language summary consistent with the recorded population.
        recorded.execute("INSERT INTO coverage_owner_language(path_id,line,expression_id,status,evidence_id,language) SELECT path_id,line,expression_id,status,evidence_id,'python' FROM resolution_coverage", []).unwrap();
        recorded.execute("INSERT INTO coverage_language_counts(path_id,language,status,records) SELECT path_id,'python',status,count(*) FROM resolution_coverage GROUP BY path_id,status", []).unwrap();
        let diagnostics =
            reader::analyze_paged(&recorded, "scope.py", None, &options(100)).unwrap();
        let diagnostic_rows = diagnostics["resolution"]["items"].as_array().unwrap();
        for (reason, count) in [
            ("dynamic_callback", 6),
            ("unresolved_receiver_type", 4),
            ("framework_magic", 3),
            ("external_unindexed", 2),
            ("ambiguous_candidates", 5),
        ] {
            assert_eq!(
                diagnostic_rows
                    .iter()
                    .filter(|row| row["reason"] == reason && row["status"] != "external")
                    .count(),
                count,
                "fixture must use the canonical exact-file reason classifier: {reason}"
            );
        }
        assert_eq!(
            diagnostic_rows
                .iter()
                .filter(|row| row["reason"].is_null())
                .count(),
            unknown
        );
        let full_page = reader::overview_with_options(
            &recorded,
            &reader::OverviewOptions {
                aspects: None,
                page: &options(100),
            },
        )
        .unwrap();
        for limit in [1, 100] {
            let mut page_options = options(limit);
            if limit == 1 {
                page_options.offset = 1;
                page_options.generation = full_page["generation"].as_str().map(str::to_owned);
            }
            let overview = reader::overview_with_options(
                &recorded,
                &reader::OverviewOptions {
                    aspects: None,
                    page: &page_options,
                },
            )
            .unwrap();
            assert_eq!(overview["counts"]["unresolved"], 20 + unknown);
            assert_eq!(overview["counts"]["external_imports"], external);
            let json = format::render_for_operation(
                &overview,
                OutputFormat::Json,
                "code_map_overview",
                None,
                format::OperationKind::Navigation,
            );
            assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), overview);
            let markdown = format::render_for_operation(
                &overview,
                OutputFormat::Markdown,
                "code_map_overview",
                None,
                format::OperationKind::Navigation,
            );
            let mut expected_rows = vec![
                format!("Resolution Health: {health}"),
                "dynamic_callback): 6".to_owned(),
                "unresolved_receiver_type): 4".to_owned(),
                "framework_magic): 3".to_owned(),
                "external_unindexed, status=unresolved): 2".to_owned(),
                "ambiguous_candidates): 5".to_owned(),
                "Reason (ambiguous_candidates, status=ambiguous): 5".to_owned(),
                format!("External references: {external} (excluded from health)"),
                "external crate count: unavailable".to_owned(),
            ];
            if external > 0 {
                expected_rows.push(format!(
                    "reason=external_unindexed, status=external): {external}"
                ));
            }
            for expected in expected_rows {
                if !markdown.contains(&expected) {
                    missing.push(format!("unknown={unknown}, external={external}, limit={limit}: missing {expected:?}"));
                }
            }
            if unknown > 0
                && !(markdown.to_ascii_lowercase().contains("unknown") && markdown.contains('1'))
            {
                missing.push(
                    "unclassified recorded evidence must remain visible as unknown".to_owned(),
                );
            }
        }
        let scopes = full_page["coverage_by_workspace"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(full_page["coverage_by_workspace"]["total"], 2);
        let primary = scopes
            .iter()
            .find(|scope| scope["name"] == "primary")
            .unwrap();
        let linked_scope = scopes
            .iter()
            .find(|scope| scope["name"] == "library")
            .unwrap();
        assert_eq!(
            format!(
                "{:.1}%",
                primary["resolution_health"]["percent"].as_f64().unwrap()
            ),
            health
        );
        assert_eq!(linked_scope["resolution_health"]["resolved"], 1);
        assert_eq!(linked_scope["resolution_health"]["denominator"], 1);
        let mut second_scope_page = options(1);
        second_scope_page.offset = 1;
        second_scope_page.generation = full_page["generation"].as_str().map(str::to_owned);
        let paged = reader::overview_with_options(
            &recorded,
            &reader::OverviewOptions {
                aspects: None,
                page: &second_scope_page,
            },
        )
        .unwrap();
        assert_eq!(paged["coverage_by_workspace"]["items"][0], *primary);
    }
    let empty = Workspace::new();
    empty.write("empty.py", "# no references\n");
    empty.build();
    let empty_conn = empty.open();
    let empty_overview = reader::overview_with_options(
        &empty_conn,
        &reader::OverviewOptions {
            aspects: None,
            page: &options(1),
        },
    )
    .unwrap();
    let empty_markdown = format::render_for_operation(
        &empty_overview,
        OutputFormat::Markdown,
        "code_map_overview",
        None,
        format::OperationKind::Navigation,
    );
    if !empty_markdown.contains("Resolution Health: unavailable") {
        missing.push("zero denominator must report unavailable, never an invented 100%".to_owned());
    }

    {
        let projected = Workspace::new();
        projected.write("source_tree/leaf.py", "def leaf(): return 1\n");
        projected.write(
            "cycles.py",
            "def recursive(): return recursive()\ndef unresolved(): return missing_target()\n",
        );
        projected.write("syntax_error.py", "def broken(:\n    pass\n");
        projected.build();
        let conn = projected.open();
        let requested_aspects =
            ["components", "cycles", "languages", "metadata"].map(str::to_owned);
        let overview = reader::overview_with_options(
            &conn,
            &reader::OverviewOptions {
                aspects: Some(&requested_aspects),
                page: &options(100),
            },
        )
        .unwrap();

        let component_page = &overview["components"];
        let components = component_page["items"].as_array().unwrap();
        let directory = components
            .iter()
            .find(|component| {
                component["path"]
                    .as_str()
                    .is_some_and(|path| path.contains("source_tree"))
            })
            .expect("the real reader should report the source directory component");
        let directory_path = directory["path"].as_str().unwrap();

        let cycles = &overview["cycles"];
        assert!(cycles["total"].as_u64().unwrap() > 0);
        let cycle = &cycles["largest"][0];
        let cycle_path = cycle["path"].as_str().unwrap();
        let cycle_line = cycle["line"].as_u64().unwrap().to_string();

        let languages = overview["languages"]["items"].as_array().unwrap();
        let python = languages
            .iter()
            .find(|language| language["language"] == "python")
            .expect("the real reader should report the Python language row");
        for field in [
            "files",
            "resolved",
            "ambiguous",
            "unresolved",
            "external_imports",
            "parse_errors",
        ] {
            assert!(
                python[field].as_u64().is_some(),
                "the reader did not supply Python {field}: {python}"
            );
        }
        assert!(python["resolved"].as_u64().unwrap() > 0);
        assert!(python["unresolved"].as_u64().unwrap() > 0);

        let metadata = overview["metadata"].as_array().unwrap();
        let schema_version = metadata
            .iter()
            .find(|row| row["key"] == "schema_version")
            .expect("schema version should come from the reader metadata")["value"]
            .as_str()
            .unwrap();
        let workspace_root = metadata
            .iter()
            .find(|row| row["key"] == "workspace_root")
            .expect("workspace root should come from the reader metadata")["value"]
            .as_str()
            .unwrap();

        let markdown = format::render_for_operation(
            &overview,
            OutputFormat::Markdown,
            "code_map_overview",
            None,
            format::OperationKind::Navigation,
        );
        assert!(
            markdown
                .to_ascii_lowercase()
                .contains("directory component"),
            "directory nodes must stay distinct from manifest-backed packages: {markdown}"
        );
        assert!(markdown.contains(directory_path), "{markdown}");
        assert!(
            markdown.to_ascii_lowercase().contains("cycle")
                && markdown.contains(cycle_path)
                && markdown.contains(&cycle_line),
            "the explicitly requested nonempty cycle summary must remain visible: {markdown}"
        );
        assert!(
            markdown.to_ascii_lowercase().contains("schema")
                && markdown.contains(schema_version)
                && markdown.contains(workspace_root),
            "the explicitly requested reader metadata must remain visible: {markdown}"
        );

        let language_section = markdown
            .split("## Languages")
            .nth(1)
            .and_then(|section| section.split("\n## ").next())
            .expect("overview Markdown should retain its language section");
        for (field, label) in [
            ("files", "files"),
            ("resolved", "resolved"),
            ("ambiguous", "ambiguous"),
            ("unresolved", "unresolved"),
            ("external_imports", "external"),
            ("parse_errors", "parse"),
        ] {
            assert!(
                language_section.to_ascii_lowercase().contains(label)
                    && language_section.contains(&python[field].as_u64().unwrap().to_string()),
                "the Python {field} value supplied by the reader must remain visible: {language_section}"
            );
        }
    }

    assert!(
        missing.is_empty(),
        "discovery overview contract: {}",
        missing.join("\n")
    );
}

#[test]
#[cfg(feature = "lang-python")]
fn overview_splits_prod_and_tests_resolution_health() {
    use contextunity_acdd::format::{self, OutputFormat};

    let render = |overview: &Value| {
        format::render_for_operation(
            overview,
            OutputFormat::Markdown,
            "code_map_overview",
            None,
            format::OperationKind::Navigation,
        )
    };
    let overview_for = |ws: &Workspace| {
        reader::overview_with_options(
            &ws.open(),
            &reader::OverviewOptions { aspects: None, page: &options(100) },
        )
        .unwrap()
    };
    let mixed = Workspace::new();
    mixed.write("src/service.py", "def helper(): return 1\ndef run():\n    helper()\n    helper()\n    missing_prod()\n");
    mixed.write("tests/test_service.py", "from src.service import helper\ndef test_ok():\n    helper()\n    missing_test_a()\n");
    mixed.write("benchmarks/bench_service.py", "def bench():\n    missing_bench()\n");
    mixed.build();
    let overview = overview_for(&mixed);
    let scope = &overview["coverage_by_workspace"]["items"][0];
    let (health, prod, tests) = (
        &scope["resolution_health"],
        &scope["resolution_health"]["prod"],
        &scope["resolution_health"]["tests"],
    );
    assert_eq!(health["availability"], "available", "{scope}");
    assert_eq!(prod["availability"], "available", "{scope}");
    assert_eq!(tests["availability"], "available", "{scope}");
    assert_eq!(prod["unresolved"], 1, "{prod}");
    assert_eq!(tests["unresolved"], 2, "{tests}");
    assert!(prod["percent"].as_f64().unwrap() > tests["percent"].as_f64().unwrap(), "{health}");
    for field in ["resolved", "unresolved", "ambiguous", "external_imports"] {
        assert_eq!(
            prod[field].as_u64().unwrap() + tests[field].as_u64().unwrap(),
            scope[field].as_u64().unwrap(),
            "{field}: {scope}"
        );
    }
    let markdown = render(&overview);
    let prod_bullet = format!("\n\n- Prod: {:.1}%", prod["percent"].as_f64().unwrap());
    let tests_bullet = format!("\n- Tests: {:.1}%", tests["percent"].as_f64().unwrap());
    let prod_pos = markdown.find(&prod_bullet).expect(&markdown);
    let after_list = markdown[prod_pos + 2..].find("\n\n").map_or(markdown.len(), |i| prod_pos + 2 + i);
    let list_block = &markdown[prod_pos + 2..after_list];
    assert!(
        markdown.find("Resolved: ").unwrap() < markdown.find("External references: ").unwrap()
            && markdown.find("External references: ").unwrap() < prod_pos
            && list_block.contains(&tests_bullet)
            && !list_block.contains("Resolved:")
            && !list_block.contains("External references:"),
        "aggregate counts must remain outside the Prod/Tests Markdown list:\n{markdown}"
    );

    let prod_only = Workspace::new();
    prod_only.write("src/only.py", "def helper(): return 1\ndef run(): return helper()\n");
    prod_only.build();
    let prod_only_overview = overview_for(&prod_only);
    let prod_only_health = &prod_only_overview["coverage_by_workspace"]["items"][0]["resolution_health"];
    assert_eq!(prod_only_health["prod"]["availability"], "available");
    assert_eq!(prod_only_health["tests"]["availability"], "unavailable");
    assert!(render(&prod_only_overview).contains("- Tests: unavailable"));
}

#[test]
fn analyze_graph_and_pagerank_modes_contract_matrix() {
    use acdd_index::db::analyze::{
        analyze_with_mode,
        hotspots::{pagerank_centrality, PageRankConfig},
        AnalyzeMode, AnalyzeRequest,
    };
    use acdd_index::models::stable_hash64;
    use std::time::Duration;

    // 1. AnalyzeMode parsing and closed-enum round-trip
    assert_eq!(AnalyzeMode::default(), AnalyzeMode::Summary);
    for mode in AnalyzeMode::ALL {
        assert_eq!(mode.as_str().parse::<AnalyzeMode>().unwrap(), *mode);
        assert_eq!(mode.to_string(), mode.as_str());
    }
    let bad_mode = "bogus".parse::<AnalyzeMode>().unwrap_err().to_string();
    assert!(
        bad_mode.contains("unsupported analyze mode 'bogus'"),
        "{bad_mode}"
    );

    // 2. Shared indexed workspace fixture (Python with Click entrypoint + uncalled public roots)
    let workspace = Workspace::new();
    workspace.write("requirements.txt", "click>=8.0\n");
    workspace.write("pkg/util.py", "def leaf_sink():\n    return 1\n");
    workspace.write(
        "pkg/core.py",
        "import json\n\
         from pkg.util import leaf_sink\n\n\
         def _private_helper():\n    return 1\n\n\
         def internal_only_pub():\n    return _private_helper()\n\n\
         def imported_only_pub():\n    return 2\n\n\
         def central_hub():\n    a = internal_only_pub()\n    b = leaf_sink()\n    return json.dumps([a, b])\n\n\
         def tested_leaf():\n    return 7\n\n\
         def tested_service():\n    return tested_leaf()\n",
    );
    workspace.write(
        "pkg/api.py",
        "import click\n\
         from pkg.core import central_hub, imported_only_pub\n\n\
         def caller_a():\n    return central_hub()\n\n\
         def caller_b():\n    return central_hub()\n\n\
         def caller_c():\n    return central_hub()\n\n\
         def hop3():\n    return central_hub()\n\n\
         def hop2():\n    return hop3()\n\n\
         def hop1():\n    return hop2()\n\n\
         def direct_and_long():\n    return central_hub() + hop1()\n\n\
         def alt_route():\n    return caller_a() + caller_b()\n\n\
         @click.command()\n\
         def cli_entry():\n    return caller_c()\n",
    );
    workspace.write(
        "tests/test_app.py",
        "from pkg.core import tested_service\n\n\
         def test_tested_service():\n    return tested_service()\n",
    );
    workspace.write(
        "tests/test_lexical.py",
        "def test_central_hub():\n    return 1\n",
    );
    workspace.build();

    let conn = workspace.open();
    let before = persisted_state(&conn);
    let page = options(50);

    // 3. Verified calls-edge test coverage strictly excludes lexical fallback
    let hash_for = |name: &str| -> i64 {
        let id: String = conn
            .query_row(
                "SELECT id FROM nodes WHERE name=?1 AND is_test=0 LIMIT 1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        stable_hash64(&id)
    };
    let h_hub = hash_for("central_hub");
    let h_tested = hash_for("tested_service");
    let h_tested_leaf = hash_for("tested_leaf");
    let covered =
        symbols::verified_calls_covered_hashes(&conn, Some(&[h_hub, h_tested, h_tested_leaf]))
            .unwrap();
    assert!(
        covered.contains(&h_tested),
        "direct calls-edge test target must be covered"
    );
    assert!(
        covered.contains(&h_tested_leaf),
        "transitive calls-edge target must be covered"
    );
    assert!(
        !covered.contains(&h_hub),
        "lexical-only test_central_hub must not count as verified coverage"
    );

    // 4. Deterministic PageRank + hotspots + budget enforcement
    let db_pr = pagerank_centrality(&conn, &PageRankConfig::default()).unwrap();
    let db_pr_repeat = pagerank_centrality(&conn, &PageRankConfig::default()).unwrap();
    assert_eq!(db_pr.scores, db_pr_repeat.scores);
    assert_eq!(db_pr.iterations, db_pr_repeat.iterations);
    assert!(db_pr.converged && db_pr.scores[&h_hub] > db_pr.scores[&h_tested]);
    for (cfg, expected_err) in [
        (
            PageRankConfig {
                max_nodes: 2,
                ..Default::default()
            },
            "node budget",
        ),
        (
            PageRankConfig {
                max_edges: 1,
                ..Default::default()
            },
            "edge budget",
        ),
        (
            PageRankConfig {
                budget: Duration::ZERO,
                ..Default::default()
            },
            "two second budget",
        ),
    ] {
        let err = pagerank_centrality(&conn, &cfg).unwrap_err().to_string();
        assert!(
            err.contains(expected_err),
            "expected {expected_err}, got: {err}"
        );
    }

    let hotspots = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Hotspots,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(hotspots["mode"], "hotspots");
    assert_eq!(hotspots["pagerank"]["damping"], 0.85);
    let hs_items = hotspots["hotspots"]["items"].as_array().unwrap();
    let hub_row = hs_items
        .iter()
        .find(|r| r["name"] == "central_hub")
        .unwrap();
    let tested_row = hs_items
        .iter()
        .find(|r| r["name"] == "tested_service")
        .unwrap();
    assert_eq!(hub_row["test_covered"], false);
    assert_eq!(hub_row["test_gap"], true);
    assert!(hub_row["fan_in"].as_u64().unwrap() >= 3);
    assert_eq!(tested_row["test_covered"], true);
    assert_eq!(tested_row["test_gap"], false);
    assert!(hub_row["pagerank"].as_f64().unwrap() > tested_row["pagerank"].as_f64().unwrap());

    // 5. Boundary: inbound external callers, grouped outbound deps, internal-only pub symbols
    let boundary = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Boundary,
            target: "pkg/core.py",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(boundary["mode"], "boundary");
    let inbound_callers = boundary["inbound_callers"]["items"].as_array().unwrap();
    assert_eq!(
        boundary["summary"]["inbound_external_callers"].as_u64().unwrap(),
        5,
        "only canonical calls edges (caller_a/b/c, hop3, direct_and_long) count as inbound callers, never imports: {inbound_callers:?}"
    );
    assert!(inbound_callers
        .iter()
        .all(|c| c["caller_path"] == "pkg/api.py" && c["edge_kind"] == "calls"));
    let outbound = boundary["outbound_dependencies"]["items"]
        .as_array()
        .unwrap();
    assert!(outbound
        .iter()
        .any(|g| g["scope"] == "internal" && g["target"] == "pkg/util.py"));
    assert!(outbound
        .iter()
        .any(|g| g["scope"] == "external" && g["target"] == "json"));
    let internal_pub = boundary["internal_only_public"]["items"]
        .as_array()
        .unwrap();
    let internal_pub_names: BTreeSet<&str> = internal_pub
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(
        internal_pub_names.contains("internal_only_pub"),
        "{internal_pub_names:?}"
    );
    assert!(
        internal_pub_names.contains("imported_only_pub"),
        "import-only reference must not remove pub symbol from zero-external-callers: {internal_pub_names:?}"
    );
    assert!(
        !internal_pub_names.contains("central_hub"),
        "externally called hub must not be internal-only"
    );
    assert!(
        !internal_pub_names.contains("_private_helper"),
        "private symbol must not be in internal_only_public"
    );

    // 6. Path: fail-closed missing `to`, shortest + top-K (including 1-hop + 4-hop), self-path, unreachable
    for missing_to in [None, Some("   ")] {
        let err = analyze_with_mode(
            &conn,
            &AnalyzeRequest {
                mode: AnalyzeMode::Path,
                target: "alt_route",
                to: missing_to,
                include_cycles: None,
                page: &page,
                root: None,
                base: None,
                policy: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("requires a destination selector in 'to'"),
            "{err}"
        );
    }
    let path_res = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "alt_route",
            to: Some("central_hub"),
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(path_res["reachable"], true);
    assert_eq!(path_res["shortest_hops"], 2);
    assert_eq!(path_res["total_paths"], 2);
    let paths = path_res["paths"].as_array().unwrap();
    assert!(paths
        .iter()
        .all(|p| p["hops"] == 2 && p["edges"][0]["kind"] == "calls"));
    let mixed_len = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "direct_and_long",
            to: Some("central_hub"),
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(mixed_len["shortest_hops"], 1);
    assert_eq!(mixed_len["total_paths"], 2);
    assert_eq!(mixed_len["paths"][0]["hops"], 1);
    assert_eq!(mixed_len["paths"][1]["hops"], 4);
    let unreachable = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "leaf_sink",
            to: Some("alt_route"),
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(unreachable["reachable"], false);
    assert_eq!(unreachable["total_paths"], 0);
    let self_path = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "leaf_sink",
            to: Some("leaf_sink"),
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(self_path["reachable"], true);
    assert_eq!(self_path["shortest_hops"], 0);

    // 7. Entrypoints: reverse tracing to stored entrypoint + uncalled public roots + workspace mode
    let ep_res = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "leaf_sink",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    let roots = ep_res["roots"]["items"].as_array().unwrap();
    let explicit = roots
        .iter()
        .find(|r| r["name"] == "cli_entry")
        .expect("explicit cli_command entrypoint");
    assert_eq!(explicit["root_kind"], "entrypoint");
    assert_eq!(explicit["category"], "cli_command");
    assert_eq!(explicit["hops"], 3);
    let pub_root = roots
        .iter()
        .find(|r| r["name"] == "alt_route")
        .expect("uncalled public root");
    assert_eq!(pub_root["root_kind"], "uncalled_public_root");
    assert_eq!(pub_root["category"], "public_root");
    assert!(
        !roots.iter().any(|r| r["name"] == "caller_a"),
        "called intermediate must not be uncalled root"
    );
    let ws_ep = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    let ws_roots = ws_ep["roots"]["items"].as_array().unwrap();
    assert_eq!(ws_roots[0]["root_kind"], "entrypoint");
    assert_eq!(ws_roots[0]["name"], "cli_entry");

    // 8. Pure read-only guarantee (INV-READONLY-OVER-INDEX)
    assert_eq!(persisted_state(&conn), before);

    // 9. In-memory adversarial graph contract checks:
    //    (a) eligible-only PageRank node/edge budgets (no silent truncation from ineligible rows),
    //    (b) 150x150 wide converging graph finds depth-4 shortest path without state explosion,
    //    (c) possible_calls does not mask uncalled_public_root, and unresolved deps stay distinct from external.
    let mem = Connection::open_in_memory().unwrap();
    mem.execute_batch(acdd_index::db::schema::SCHEMA_DDL)
        .unwrap();
    mem.execute(
        "INSERT INTO metadata(key,value) VALUES('output_root','mem-gen')",
        [],
    )
    .unwrap();
    let (h_a, h_b, h_c, h_t1) = (
        stable_hash64("fn:src:a"),
        stable_hash64("fn:src:b"),
        stable_hash64("fn:src:c"),
        stable_hash64("fn:tests:t1"),
    );
    mem.execute_batch(&format!(
        "INSERT INTO path_dictionary(path_id,path) VALUES(1,'archive/old.rs'),(2,'src/lib.rs'),(3,'tests/t.rs');
         INSERT INTO files(path,status,digest,size,language,is_test,generated,path_hash) VALUES
           ('archive/old.rs','indexed','d1',10,'rust',0,0,1),
           ('src/lib.rs','indexed','d2',10,'rust',0,0,2),
           ('tests/t.rs','indexed','d3',10,'rust',1,0,3);
         INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'exact');
         INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details) VALUES
           ({},'fn:archive:old','function','old','old',1,1,1,2,0,'rust',0,'{{}}'),
           ({h_a},'fn:src:a','function','a','a',2,2,1,2,0,'rust',0,'{{\"is_public\":true}}'),
           ({h_b},'fn:src:b','function','b','b',2,2,3,4,0,'rust',0,'{{\"is_public\":true}}'),
           ({h_c},'fn:src:c','function','c','c',2,2,5,6,0,'rust',0,'{{\"is_public\":true}}'),
           ({h_t1},'fn:tests:t1','function','t1','t1',3,3,1,2,1,'rust',0,'{{\"test_role\":\"test_case\"}}');
         INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES
           ({h_a},'calls',{h_b},2,2,1,1,1),
           ({h_a},'possible_calls',{h_c},2,2,1,1,1),
           ({h_t1},'calls',{h_a},3,2,1,1,1),
           ({h_t1},'calls',{h_b},3,2,1,1,1);
         INSERT INTO dependencies(owner_id,ordinal,target_hash,kind,symbol,resolution) VALUES
           (2,0,NULL,'import','serde','external'),
           (2,1,NULL,'import','missing_mod','unresolved');",
        i64::MIN
    ))
    .unwrap();
    // (a) max_nodes=2 must fail closed even though low-hash archive/old.rs sorts first
    let node_cap_err = pagerank_centrality(
        &mem,
        &PageRankConfig {
            max_nodes: 2,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(node_cap_err.contains("node budget"), "{node_cap_err}");
    // Test edges must not consume production edge budget (2 prod edges: 1->2 calls, 1->3 possible_calls)
    assert!(pagerank_centrality(
        &mem,
        &PageRankConfig {
            max_edges: 2,
            ..Default::default()
        }
    )
    .is_ok());
    // (c) possible_calls to `c` must NOT suppress uncalled_public_root in either workspace or targeted mode
    for ep_target in ["", "fn:src:c"] {
        let ep = analyze_with_mode(
            &mem,
            &AnalyzeRequest {
                mode: AnalyzeMode::Entrypoints,
                target: ep_target,
                to: None,
                include_cycles: None,
                page: &page,
                root: None,
                base: None,
                policy: None,
            },
        )
        .unwrap();
        let items = ep["roots"]["items"].as_array().unwrap();
        assert!(
            items.iter().any(|r| r["name"] == "c" && r["root_kind"] == "uncalled_public_root"),
            "target={ep_target:?} must classify possible_calls-only public symbol as uncalled_public_root: {items:?}"
        );
    }
    // Unresolved dependency must stay distinct from external dependency in boundary
    let mem_boundary = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Boundary,
            target: "src/lib.rs",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(mem_boundary["summary"]["outbound_external_groups"], 1);
    assert_eq!(mem_boundary["summary"]["outbound_unresolved_groups"], 1);

    // (e) QueryBudget preserves caller-installed SQLite progress_handler AND PageRank never returns Ok past deadline
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let cancel_cb = std::sync::Arc::clone(&cancel);
    mem.progress_handler(
        1,
        Some(move || cancel_cb.load(std::sync::atomic::Ordering::SeqCst)),
    );
    assert!(mem
        .query_row("SELECT count(*) FROM edges", [], |r| r.get::<_, i64>(0))
        .is_err());
    cancel.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Boundary,
            target: "src/lib.rs",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .is_ok());
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        mem.query_row("SELECT count(*) FROM edges", [], |r| r.get::<_, i64>(0))
            .is_err(),
        "QueryBudget must preserve caller-installed SQLite progress_handler"
    );
    mem.progress_handler(0, None::<fn() -> bool>);

    let empty_slow = Connection::open_in_memory().unwrap();
    empty_slow
        .execute_batch(acdd_index::db::schema::SCHEMA_DDL)
        .unwrap();
    let slept = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slept_cb = std::sync::Arc::clone(&slept);
    empty_slow.progress_handler(
        1,
        Some(move || {
            if !slept_cb.swap(true, std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }),
    );
    let late_err = pagerank_centrality(
        &empty_slow,
        &PageRankConfig {
            budget: Duration::from_millis(20),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        late_err.contains("two second budget")
            || late_err.to_ascii_lowercase().contains("interrupt"),
        "empty graph past deadline must fail closed, got: {late_err}"
    );

    // (b) Dense converging graphs + irrelevant-branch direct shortest-path check (set-oriented SQL):
    //     (i) equal-length 3-hop routes (S -> 150 L -> 150 R -> Teq) with limit=1 and limit=5,
    //     (ii) single depth-4 winning branch (S -> 150 L -> 150 R, R0 -> mid -> T),
    //     (iii) irrelevant huge sibling branch (S2 -> T2 and S2 -> B -> 20,000 leaves) with limit=1.
    let (h_ws, h_wmid, h_wt, h_weq, h_s2, h_t2, h_b2) = (
        stable_hash64("fn:wide:S"),
        stable_hash64("fn:wide:mid"),
        stable_hash64("fn:wide:T"),
        stable_hash64("fn:wide:Teq"),
        stable_hash64("fn:irr:S"),
        stable_hash64("fn:irr:T"),
        stable_hash64("fn:irr:B"),
    );
    mem.execute_batch(&format!(
        "BEGIN;
         INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details) VALUES
           ({h_ws},'fn:wide:S','function','wide_s','wide_s',2,2,10,11,0,'rust',0,'{{}}'),
           ({h_wmid},'fn:wide:mid','function','wide_mid','wide_mid',2,2,12,13,0,'rust',0,'{{}}'),
           ({h_wt},'fn:wide:T','function','wide_t','wide_t',2,2,14,15,0,'rust',0,'{{}}'),
           ({h_weq},'fn:wide:Teq','function','wide_teq','wide_teq',2,2,16,17,0,'rust',0,'{{}}'),
           ({h_s2},'fn:irr:S','function','irr_s','irr_s',2,2,18,19,0,'rust',0,'{{}}'),
           ({h_t2},'fn:irr:T','function','irr_t','irr_t',2,2,20,21,0,'rust',0,'{{}}'),
           ({h_b2},'fn:irr:B','function','irr_b','irr_b',2,2,22,23,0,'rust',0,'{{}}');
         WITH RECURSIVE seq(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM seq WHERE i < 149)
         INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details)
         SELECT 2000+i,'fn:wide:l1_'||i,'function','l1_'||i,'l1_'||i,2,2,20,21,0,'rust',0,'{{}}' FROM seq
         UNION ALL
         SELECT 3000+i,'fn:wide:l2_'||i,'function','l2_'||i,'l2_'||i,2,2,30,31,0,'rust',0,'{{}}' FROM seq;
         WITH RECURSIVE seq(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM seq WHERE i < 149)
         INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count)
         SELECT {h_ws},'calls',2000+i,2,20,1,1,1 FROM seq
         UNION ALL SELECT 3000+i,'calls',{h_weq},2,31,1,1,1 FROM seq
         UNION ALL SELECT 2000+a.i,'calls',3000+b.i,2,25,1,1,1 FROM seq a CROSS JOIN seq b;
         INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES
           (3000,'calls',{h_wmid},2,35,1,1,1),({h_wmid},'calls',{h_wt},2,36,1,1,1),
           ({h_s2},'calls',{h_t2},2,18,1,1,1),({h_s2},'calls',{h_b2},2,19,1,1,1);
         WITH RECURSIVE leaves(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM leaves WHERE i < 19999)
         INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details)
         SELECT 100000+i,'fn:irr:leaf_'||i,'function','leaf_'||i,'leaf_'||i,2,2,40,41,0,'rust',0,'{{}}' FROM leaves;
         WITH RECURSIVE leaves(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM leaves WHERE i < 19999)
         INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count)
         SELECT {h_b2},'calls',100000+i,2,40,1,1,1 FROM leaves;
         COMMIT;"
    ))
    .unwrap();
    let wide_path = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "fn:wide:S",
            to: Some("fn:wide:T"),
            include_cycles: None,
            page: &options(1),
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(wide_path["reachable"], true);
    assert_eq!(wide_path["shortest_hops"], 4);
    assert_eq!(wide_path["total_paths"], 1);
    let irr_path = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "fn:irr:S",
            to: Some("fn:irr:T"),
            include_cycles: None,
            page: &options(1),
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(irr_path["reachable"], true);
    assert_eq!(irr_path["shortest_hops"], 1);
    assert_eq!(irr_path["total_paths"], 1);
    assert_eq!(irr_path["paths"][0]["edges"][0]["kind"], "calls");
    for limit in [1, 5] {
        let eq_path = analyze_with_mode(
            &mem,
            &AnalyzeRequest {
                mode: AnalyzeMode::Path,
                target: "fn:wide:S",
                to: Some("fn:wide:Teq"),
                include_cycles: None,
                page: &options(limit),
                root: None,
                base: None,
                policy: None,
            },
        )
        .unwrap();
        assert_eq!(eq_path["reachable"], true);
        assert_eq!(eq_path["shortest_hops"], 3);
        assert_eq!(eq_path["total_paths"], limit);
    }

    // (d) Entrypoints: 33-edge chain fails closed (no silent omission), 32-edge chain returns explicit root,
    //     and targeted mode never reports an is_test=1 public node as a production uncalled_public_root.
    let h_tpub = stable_hash64("fn:tests:t_pub");
    let mut deep_sql = format!(
        "BEGIN; INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details) VALUES \
         ({h_tpub},'fn:tests:t_pub','function','t_pub','t_pub',2,2,40,41,1,'rust',0,'{{\"is_public\":true}}');"
    );
    let chain_ids: Vec<String> = (0..=33).map(|i| format!("fn:deep:n{i}")).collect();
    for (i, id) in chain_ids.iter().enumerate() {
        let h = stable_hash64(id);
        let details = if i == 0 {
            "{\"entrypoint\":\"cli_command\"}"
        } else {
            "{}"
        };
        deep_sql.push_str(&format!(
            "INSERT INTO nodes(node_hash,id,kind,name,qualname,path_id,owner_path_id,line,end_line,is_test,language,generated,details) VALUES \
             ({h},'{id}','function','deep_n{i}','deep_n{i}',2,2,{}, {},0,'rust',0,'{details}');",
            50 + i,
            51 + i
        ));
        if i > 0 {
            let prev_h = stable_hash64(&chain_ids[i - 1]);
            deep_sql.push_str(&format!(
                "INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES ({prev_h},'calls',{h},2,{},1,1,1);",
                50 + i
            ));
        }
    }
    deep_sql.push_str("COMMIT;");
    mem.execute_batch(&deep_sql).unwrap();

    let deep_33_err = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "fn:deep:n33",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        deep_33_err.contains("exceeds depth budget"),
        "{deep_33_err}"
    );
    let deep_32 = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "fn:deep:n32",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(deep_32["roots"]["total"], 1);
    assert_eq!(deep_32["roots"]["items"][0]["id"], "fn:deep:n0");
    assert_eq!(deep_32["roots"]["items"][0]["hops"], 32);

    let test_root = analyze_with_mode(
        &mem,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "fn:tests:t_pub",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    assert_eq!(
        test_root["roots"]["total"], 0,
        "is_test=1 public node must not be classified as a production uncalled_public_root"
    );
}

#[test]
fn analyze_diff_impact_cochange_and_markdown_contract_matrix() {
    use acdd_core::{config::Adapter, path_policy::WorkspacePathPolicy};
    use acdd_index::db::{
        analyze::{analyze_with_mode, AnalyzeMode, AnalyzeRequest},
        source::check_sensitive_path_with_case,
    };
    use contextunity_acdd::format::{render_for_operation, OperationKind, OutputFormat};
    use serde_json::json;

    // 1. Canonical sensitive-path case-sensitivity parity (shared by reader.rs & Git diagnostics)
    let adapter = Adapter {
        sensitive: vec!["secrets.txt".to_owned()],
        ..Adapter::default()
    };
    assert!(check_sensitive_path_with_case(&adapter, "SECRETS.TXT", true).is_err());
    assert!(check_sensitive_path_with_case(&adapter, "SECRETS.TXT", false).is_ok());

    // 2. Shared Git fixture (single build for diff_impact, cochange, sensitive/policy filtering & Markdown formatting)
    let page = options(20);
    let w = Workspace::new();
    w.write(
        "pkg/service.py",
        "def alpha(x):\n    y = x + 1\n    return y\n\n\
         def beta(z):\n    return alpha(z)\n",
    );
    w.write(
        "pkg/caller.py",
        "from pkg.service import alpha\n\ndef invoke():\n    return alpha(1)\n",
    );
    w.write("pkg/unlinked_a.py", "def ua():\n    return 1\n");
    w.write(
        "pkg/unlinked_b_old.py",
        "def ub():\n    a = 1\n    b = 2\n    c = 3\n    return a + b + c\n",
    );
    w.write(
        "tests/test_service.py",
        "from pkg.service import alpha\n\ndef test_alpha_verified():\n    return alpha(2)\n",
    );
    w.write(
        "tests/test_via_secret.py",
        "from managed.secret_caller import hidden_caller\n\n\
         def test_via_secret():\n    return hidden_caller()\n",
    );
    w.write(
        "tests/test_lexical_only.py",
        "def test_beta():\n    return 99\n",
    );
    w.write(
        "docs/rules.md",
        "# Service Rules\n\n\
         > [!IMPORTANT]\n\
         > **Invariant: `INV-SERVICE-RULE`** — `alpha` in [service.py](../pkg/service.py) must remain deterministic.\n",
    );
    w.write(
        "acdd.yaml",
        "sensitive:\n  - \".env\"\n  - \"managed/secret_caller.py\"\n",
    );
    w.write(".env", "SECRET_TOKEN=baseline\n");
    w.write(".acdd/profile.yaml", "version: 2\n");
    w.write("managed/spec.md", "baseline spec\n");
    w.write(
        "managed/secret_caller.py",
        "from pkg.service import alpha\n\ndef hidden_caller():\n    return alpha(7)\n",
    );
    w.write(
        "managed/test_secret.py",
        "from pkg.service import alpha\n\ndef test_hidden_alpha():\n    return alpha(8)\n",
    );
    w.write(
        "managed/secret_rule.md",
        "# Hidden Rule\n\n\
         > [!IMPORTANT]\n\
         > **Invariant: `INV-SECRET-RULE`** — `alpha` in [service.py](../pkg/service.py) hidden rule.\n",
    );
    w.seed_git().expect("seed Git fixture");

    // Rename pkg/unlinked_b_old.py -> pkg/unlinked_b.py and create cochange history (commit 0 + commit 1 => count 2)
    w.run_git(&["mv", "pkg/unlinked_b_old.py", "pkg/unlinked_b.py"])
        .expect("git mv");
    w.write("pkg/unlinked_a.py", "def ua():\n    return 10\n");
    w.write(
        "pkg/unlinked_b.py",
        "def ub():\n    a = 10\n    b = 2\n    c = 3\n    return a + b + c\n",
    );
    w.write("pkg/unlinked_c.py", "def uc():\n    return 30\n");
    w.write(
        "pkg/service.py",
        "def alpha(x):\n    y = x + 2\n    return y\n\n\
         def beta(z):\n    return alpha(z)\n",
    );
    w.write(
        "pkg/caller.py",
        "from pkg.service import alpha\n\ndef invoke():\n    return alpha(2)\n",
    );
    w.write(".env", "SECRET_TOKEN=commit1\n");
    w.write(".acdd/profile.yaml", "version: 2 # c1\n");
    w.write("managed/spec.md", "commit1 spec\n");
    w.run_git(&["add", "-A"]).expect("stage commit 1");
    w.commit_git("cochange commit 1", None).expect("commit 1");

    // Uncommitted edits: body-only edit on alpha (line 2 > nodes.line=1) + declaration-line edit on beta (line 5 == nodes.line)
    w.write(
        "pkg/service.py",
        "def alpha(x):\n    y = x + 99\n    return y\n\n\
         def beta(z, extra=0):\n    return alpha(z) + extra\n",
    );
    w.write(".env", "SECRET_TOKEN=dirty\n");
    w.write(".acdd/profile.yaml", "version: 2 # dirty\n");
    w.write("managed/spec.md", "dirty managed spec\n");
    w.build();
    let conn = w.open();

    // Non-Git standalone root and nested ancestor-only directory both fail closed with GIT_REPO_REQUIRED
    let non_git_dir = Workspace::new();
    let nested_dir = w.path("pkg");
    for bad_root in [non_git_dir.root(), nested_dir.as_path()] {
        for mode in [AnalyzeMode::DiffImpact, AnalyzeMode::Cochange] {
            let err = analyze_with_mode(
                &conn,
                &AnalyzeRequest {
                    mode,
                    target: "",
                    to: None,
                    include_cycles: None,
                    page: &page,
                    root: Some(bad_root),
                    base: None,
                    policy: None,
                },
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("GIT_REPO_REQUIRED"),
                "{mode} @ {bad_root:?}: {err}"
            );
        }
    }

    let policy = WorkspacePathPolicy::from_workspace_config(
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
        vec![
            "managed/spec.md".to_owned(),
            "managed/secret_caller.py".to_owned(),
            "managed/test_secret.py".to_owned(),
            "managed/secret_rule.md".to_owned(),
        ],
    );

    // 4. diff_impact assertions (uncommitted against HEAD)
    let diff = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::DiffImpact,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: None,
            policy: Some(&policy),
        },
    )
    .expect("diff_impact should succeed");
    assert_eq!(diff["mode"], "diff_impact");
    assert_eq!(diff["complete"], true, "{diff}");

    let changed = diff["changed_symbols"]["items"].as_array().unwrap();
    let alpha_row = changed
        .iter()
        .find(|r| r["name"] == "alpha")
        .expect("alpha changed");
    let beta_row = changed
        .iter()
        .find(|r| r["name"] == "beta")
        .expect("beta changed");
    assert_eq!(
        alpha_row["signature_touched"], false,
        "body-only edit on line > nodes.line must yield signature_touched=false: {alpha_row}"
    );
    assert_eq!(
        beta_row["signature_touched"], true,
        "declaration edit overlapping nodes.line must yield signature_touched=true: {beta_row}"
    );

    // Reaching tests must include verified static calls test_case and exclude lexical-only test_beta
    let test_names: BTreeSet<&str> = diff["tests"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(test_names.contains("test_alpha_verified"), "{test_names:?}");
    assert!(
        !test_names.contains("test_beta"),
        "lexical fallback must be excluded: {test_names:?}"
    );

    // Uncovered symbols must include beta (despite lexical test_beta) and exclude verified-covered alpha
    let uncovered_names: BTreeSet<&str> = diff["uncovered_symbols"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(uncovered_names.contains("beta"), "{uncovered_names:?}");
    assert!(!uncovered_names.contains("alpha"), "{uncovered_names:?}");

    // Governed INV-* affected_docs must surface docs/rules.md
    let doc_paths: BTreeSet<&str> = diff["affected_docs"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["path"].as_str())
        .collect();
    assert!(doc_paths.contains("docs/rules.md"), "{diff}");

    // Sensitive (.env), internal .acdd/ whitelist, and policy-excluded graph-derived paths must never leak
    let diff_str = diff.to_string();
    for forbidden in [
        ".env",
        ".acdd/profile.yaml",
        "managed/spec.md",
        "managed/secret_caller.py",
        "managed/test_secret.py",
        "managed/secret_rule.md",
    ] {
        assert!(
            !diff_str.contains(forbidden),
            "forbidden path '{forbidden}' leaked in diff_impact: {diff_str}"
        );
    }
    assert_eq!(diff["tests"]["total"], 1, "post-filter test total: {diff}");
    assert_eq!(
        diff["affected_docs"]["total"], 1,
        "post-filter affected_docs total: {diff}"
    );
    assert!(
        !test_names.contains("test_via_secret"),
        "allowed test reaching via excluded intermediate must be blocked: {test_names:?}"
    );

    // Same exposure policy applies on canonical changed_impact_with_path_options (code_map_impact)
    let impact_opts = traversal::TraversalOptions {
        depth: 4,
        direction: traversal::ImpactDirection::Inbound,
        mode: traversal::TraversalMode::Calls,
        edge_types: None,
        page: &page,
    };
    let impact_head = traversal::impact_with_path_options(
        &conn,
        w.root(),
        None,
        Some("head"),
        None,
        &impact_opts,
    )
    .expect("changed_impact_with_path_options");
    let impact_str = impact_head.to_string();
    assert!(
        !impact_str.contains("managed/secret_caller.py") && !impact_str.contains("test_via_secret"),
        "excluded intermediate leaked in changed_impact tests: {impact_str}"
    );
    assert!(impact_str.contains("test_alpha_verified"), "{impact_str}");

    // Direct excluded edge path between allowed test_alpha_verified and allowed alpha is blocked in both surfaces
    let mem_ex = common::sqlite::PersistedSnapshot::from_connection(&conn).open();
    mem_ex
        .execute(
            "UPDATE edges SET path_id=(SELECT path_id FROM path_dictionary WHERE path='managed/secret_caller.py') \
             WHERE kind='calls' AND src_hash=(SELECT node_hash FROM nodes WHERE name='test_alpha_verified') \
               AND dst_hash=(SELECT node_hash FROM nodes WHERE name='alpha')",
            [],
        )
        .expect("tag direct excluded edge in memory");
    let direct_ex_impact = traversal::impact_with_path_options(
        &mem_ex,
        w.root(),
        None,
        Some("head"),
        None,
        &impact_opts,
    )
    .expect("direct excluded edge impact");
    assert!(
        !direct_ex_impact["tests"]
            .to_string()
            .contains("test_alpha_verified"),
        "direct excluded edge must block reaching test in changed_impact: {direct_ex_impact}"
    );
    let direct_ex_diff = analyze_with_mode(
        &mem_ex,
        &AnalyzeRequest {
            mode: AnalyzeMode::DiffImpact,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: None,
            policy: Some(&policy),
        },
    )
    .expect("direct excluded edge diff_impact");
    assert_eq!(
        direct_ex_diff["tests"]["total"], 0,
        "direct excluded edge must block reaching test with post-filter total=0: {direct_ex_diff}"
    );

    // 3-way 10-layer branching calls graph (3^10 > 10,000 states while depth-4 changed_nodes stays small) must fail closed on both entry points
    {
        use acdd_index::models::stable_hash64;
        let alpha_hash: i64 = mem_ex
            .query_row("SELECT node_hash FROM nodes WHERE name='alpha'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let pid: i64 = mem_ex
            .query_row(
                "SELECT path_id FROM path_dictionary WHERE path='pkg/service.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut sql = String::from("BEGIN;");
        let mut prev = vec![alpha_hash];
        for layer in 1..=10usize {
            let mut cur = Vec::with_capacity(3);
            for k in 0..3usize {
                let id = format!("py:pkg/service.py:1:fan_{layer}_{k}");
                let h = stable_hash64(&id);
                let (is_test, details) = if layer == 10 {
                    (1, r#"{"test_role":"test_case"}"#)
                } else {
                    (0, "{}")
                };
                sql.push_str(&format!(
                    "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) \
                     VALUES('{id}','function','fan_{layer}_{k}','pkg.service.fan_{layer}_{k}',{pid},1,1,{is_test},'python',0,'{details}',{h},{pid});"
                ));
                for &dst in &prev {
                    sql.push_str(&format!(
                        "INSERT INTO edges(src_hash,kind,dst_hash,path_id,line,evidence_id,confidence_id,occurrence_count) \
                         VALUES({h},'calls',{dst},{pid},1,1,1,1);"
                    ));
                }
                cur.push(h);
            }
            prev = cur;
        }
        sql.push_str("COMMIT;");
        mem_ex
            .execute_batch(&sql)
            .expect("seed 3-way layered calls graph");
        let diff_budget_err = analyze_with_mode(
            &mem_ex,
            &AnalyzeRequest {
                mode: AnalyzeMode::DiffImpact,
                target: "",
                to: None,
                include_cycles: None,
                page: &page,
                root: Some(w.root()),
                base: None,
                policy: Some(&policy),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            diff_budget_err.contains("exceeds the 10000-state work budget"),
            "diff_impact must fail closed on verified-test state overflow: {diff_budget_err}"
        );
        let impact_budget_err = traversal::impact_with_path_options(
            &mem_ex,
            w.root(),
            None,
            Some("head"),
            None,
            &impact_opts,
        )
        .unwrap_err()
        .to_string();
        assert!(
            impact_budget_err.contains("exceeds the 10000-state work budget"),
            "changed_impact must fail closed on verified-test state overflow: {impact_budget_err}"
        );
    }

    // Invalid base ref fails closed
    let bad_base = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::DiffImpact,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: Some("--bad-opt"),
            policy: Some(&policy),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(bad_base.contains("invalid git revision"), "{bad_base}");

    // Optional base ref comparing HEAD~1 -> working tree succeeds after committing changes
    w.commit_git("commit service edits", Some(&["pkg/service.py"]))
        .expect("commit service edits");
    let diff_base = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::DiffImpact,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: Some("HEAD~1"),
            policy: Some(&policy),
        },
    )
    .expect("diff_impact with base=HEAD~1 should succeed");
    assert_eq!(diff_base["base"], "HEAD~1");
    assert_eq!(diff_base["changed_symbols"]["total"], 2, "{diff_base}");

    // Add single-file commits (one alongside policy-excluded .env) so per-file commit totals & confidence include single-file commits
    w.write("pkg/unlinked_a.py", "def ua():\n    return 11\n");
    w.write(".env", "SECRET_TOKEN=solo_a\n");
    w.commit_git(
        "solo unlinked_a with sensitive",
        Some(&["pkg/unlinked_a.py", ".env"]),
    )
    .expect("commit solo a");
    w.write(
        "pkg/unlinked_b.py",
        "def ub():\n    a = 11\n    b = 2\n    c = 3\n    return a + b + c\n",
    );
    w.commit_git("solo unlinked_b", Some(&["pkg/unlinked_b.py"]))
        .expect("commit solo b");

    // 5. cochange assertions: unlinked pairs surfaced, statically linked pairs subtracted, rename tracked, sensitive/policy excluded, single-file commits counted in confidence
    let cochange = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: None,
            policy: Some(&policy),
        },
    )
    .expect("cochange should succeed");
    assert_eq!(cochange["mode"], "cochange");
    let pairs = cochange["pairs"]["items"].as_array().unwrap();
    let pair_set: BTreeSet<(&str, &str)> = pairs
        .iter()
        .map(|p| (p["file_a"].as_str().unwrap(), p["file_b"].as_str().unwrap()))
        .collect();
    assert!(
        pair_set.contains(&("pkg/unlinked_a.py", "pkg/unlinked_b.py")),
        "expected unlinked cochange pair: {pair_set:?}"
    );
    assert!(
        !pair_set.contains(&("pkg/caller.py", "pkg/service.py")),
        "statically linked pair must be subtracted: {pair_set:?}"
    );
    let ab = pairs
        .iter()
        .find(|p| p["file_a"] == "pkg/unlinked_a.py" && p["file_b"] == "pkg/unlinked_b.py")
        .unwrap();
    assert_eq!(ab["cochange_count"], 2, "{ab}");
    assert_eq!(
        ab["commits_a"], 3,
        "solo commit must be counted in commits_a: {ab}"
    );
    assert_eq!(
        ab["commits_b"], 3,
        "solo commit must be counted in commits_b: {ab}"
    );
    assert_eq!(
        ab["confidence"], 0.666667,
        "confidence must reflect solo commits (2/3): {ab}"
    );
    assert_eq!(ab["static_edge"], false);

    let cochange_str = cochange.to_string();
    for forbidden in [
        ".env",
        ".acdd/profile.yaml",
        "managed/spec.md",
        "code-map.sqlite",
    ] {
        assert!(
            !cochange_str.contains(forbidden),
            "forbidden path '{forbidden}' leaked in cochange: {cochange_str}"
        );
    }

    // Streaming bounded Git stdout terminates and fails closed on overflow
    let overflow_err = acdd_core::git::collect_bounded_log_name_status(w.root(), None, 200, 16)
        .unwrap_err()
        .to_string();
    assert!(
        overflow_err.contains("exceeds the 16-byte work budget"),
        "{overflow_err}"
    );

    // Moving a mutable base ref while HEAD stays unchanged invalidates cochange continuation
    w.run_git(&["branch", "-f", "base-track", "HEAD~2"])
        .expect("create mutable base ref");
    let base_page1 = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &options(1),
            root: Some(w.root()),
            base: Some("base-track"),
            policy: Some(&policy),
        },
    )
    .expect("cochange with mutable base ref");
    let mut base_page2_opts = options(1);
    base_page2_opts.offset = 1;
    base_page2_opts.generation = Some(base_page1["generation"].as_str().unwrap().to_owned());
    w.run_git(&["branch", "-f", "base-track", "HEAD~1"])
        .expect("move mutable base ref without moving HEAD");
    let base_move_err = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &base_page2_opts,
            root: Some(w.root()),
            base: Some("base-track"),
            policy: Some(&policy),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        base_move_err.contains("history or policy changed"),
        "moving base ref without moving HEAD must invalidate continuation: {base_move_err}"
    );

    // Continuation invalidation on policy change AND on HEAD movement (reusing cochange generation)
    let mut page2_opts = options(1);
    page2_opts.offset = 1;
    page2_opts.generation = Some(cochange["generation"].as_str().unwrap().to_owned());

    // Changing policy invalidates continuation
    let changed_policy = WorkspacePathPolicy::default();
    let policy_err = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &page2_opts,
            root: Some(w.root()),
            base: None,
            policy: Some(&changed_policy),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        policy_err.contains("history or policy changed"),
        "{policy_err}"
    );

    // Moving HEAD invalidates continuation under the original policy
    w.run_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-qm",
        "move HEAD",
    ])
    .expect("commit move HEAD");
    let head_err = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &page2_opts,
            root: Some(w.root()),
            base: None,
            policy: Some(&policy),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(head_err.contains("history or policy changed"), "{head_err}");

    // Skipped >30-file mass-rename commit must still update rename tracking so historical pairs resolve to current paths and static edges stay subtracted
    w.run_git(&["mv", "pkg/unlinked_b.py", "pkg/unlinked_b_mass.py"])
        .expect("mass mv unlinked_b");
    w.run_git(&["mv", "pkg/caller.py", "pkg/caller_mass.py"])
        .expect("mass mv caller");
    for i in 0..30usize {
        w.write(format!("bulk/f{i:02}.txt"), "bulk\n");
    }
    w.run_git(&["add", "-A"]).expect("stage mass rename commit");
    w.commit_git("mass rename >30 files", None)
        .expect("commit mass rename");
    let mem_mass = common::sqlite::PersistedSnapshot::from_connection(&conn).open();
    mem_mass
        .execute(
            "UPDATE path_dictionary SET path='pkg/caller_mass.py' WHERE path='pkg/caller.py'",
            [],
        )
        .expect("update caller path in memory");
    let cochange_mass = analyze_with_mode(
        &mem_mass,
        &AnalyzeRequest {
            mode: AnalyzeMode::Cochange,
            target: "",
            to: None,
            include_cycles: None,
            page: &page,
            root: Some(w.root()),
            base: None,
            policy: Some(&policy),
        },
    )
    .expect("cochange after >30-file rename");
    assert!(
        cochange_mass["summary"]["commits_skipped_large"]
            .as_u64()
            .unwrap()
            >= 1,
        "{cochange_mass}"
    );
    let mass_pairs = cochange_mass["pairs"]["items"].as_array().unwrap();
    let mass_set: BTreeSet<(&str, &str)> = mass_pairs
        .iter()
        .map(|p| (p["file_a"].as_str().unwrap(), p["file_b"].as_str().unwrap()))
        .collect();
    assert!(
        mass_set.contains(&("pkg/unlinked_a.py", "pkg/unlinked_b_mass.py")),
        "mass-rename historical pair must resolve to current path: {mass_set:?}"
    );
    assert!(
        !mass_set.contains(&("pkg/unlinked_a.py", "pkg/unlinked_b.py")),
        "stale pre-rename path must not remain after >30-file rename: {mass_set:?}"
    );
    assert!(
        !mass_set.contains(&("pkg/caller_mass.py", "pkg/service.py"))
            && !mass_set.contains(&("pkg/caller.py", "pkg/service.py")),
        "statically linked renamed pair must stay subtracted after >30-file rename: {mass_set:?}"
    );

    // 6. Compact Markdown presentation snapshots across all 6 code_map_analyze modes
    let hotspots = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Hotspots,
            target: "pkg",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    let boundary = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Boundary,
            target: "pkg/service.py",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    let path_val = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Path,
            target: "invoke",
            to: Some("alpha"),
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();
    let entrypoints = analyze_with_mode(
        &conn,
        &AnalyzeRequest {
            mode: AnalyzeMode::Entrypoints,
            target: "alpha",
            to: None,
            include_cycles: None,
            page: &page,
            root: None,
            base: None,
            policy: None,
        },
    )
    .unwrap();

    for (mode, val, needles) in [
        (
            "hotspots",
            &hotspots,
            vec!["# code_map_analyze (hotspots)", "## Hotspots", "score `"],
        ),
        (
            "boundary",
            &boundary,
            vec![
                "# code_map_analyze (boundary)",
                "## Inbound external callers",
            ],
        ),
        (
            "path",
            &path_val,
            vec!["# code_map_analyze (path)", "## Paths", "--["],
        ),
        (
            "entrypoints",
            &entrypoints,
            vec!["# code_map_analyze (entrypoints)", "## Entrypoint roots"],
        ),
        (
            "diff_impact",
            &diff,
            vec![
                "# code_map_analyze (diff_impact)",
                "## Changed symbols",
                "[signature_touched]",
                "## Uncovered symbols",
                "## Affected tests",
                "## Governed documents",
            ],
        ),
        (
            "cochange",
            &cochange,
            vec![
                "# code_map_analyze (cochange)",
                "## Unlinked co-change pairs",
                "<->",
            ],
        ),
    ] {
        let md = render_for_operation(
            val,
            OutputFormat::Markdown,
            "code_map_analyze",
            None,
            OperationKind::Navigation,
        );
        for needle in needles {
            assert!(md.contains(needle), "mode={mode} missing '{needle}':\n{md}");
        }
    }

    // Summary mode fallback remains untouched (no diagnostic mode suffix)
    let summary_md = render_for_operation(
        &json!({"diagnostics":{"total":0,"offset":0,"limit":10,"items":[]}}),
        OutputFormat::Markdown,
        "code_map_analyze",
        None,
        OperationKind::Navigation,
    );
    assert!(
        summary_md.starts_with("# code_map_analyze\n"),
        "{summary_md}"
    );
    assert!(summary_md.contains("**diagnostics**"), "{summary_md}");
}
