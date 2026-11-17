use super::*;

#[test]
fn removed_checkpoint_surfaces_are_absent_and_blackboard_serialization_is_opaque() {
    let (workspace, harness) = McpTestHarness::with_files(&[
        (
            "acdd.yaml",
            "profile: full\nmodules: {state: true, blackboard: true}\n",
        ),
        (".gitignore", ".acdd/\n"),
        (
            ".acdd/profile.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ngates:\n  - tasks:\n      gates:\n        - id: verify\n          role: worker\n          proof: none\n",
        ),
    ]);
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(workspace.root())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(workspace.root())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(workspace.root())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(workspace.root())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(workspace.root())
            .status()
            .unwrap()
            .success()
    );

    let mut client = harness.expect("MCP initialize");
    let (_, removed_mcp) = client
        .call("session_checkpoint", json!({"action":"list"}))
        .expect("MCP transport");
    assert!(
        removed_mcp.get("error").is_some() || removed_mcp["result"]["isError"] == true,
        "session_checkpoint must not be registered in 0.6.0: {removed_mcp}"
    );
    let removed_cli = run_cli(&workspace, &["checkpoint", "list"]);
    assert!(
        !removed_cli.status.success(),
        "acdd checkpoint must not exist in 0.6.0"
    );

    let started = client
        .payload(
            "acdd_state",
            json!({"action": "start", "goal": "blackboard serialization contract", "in_place": true}),
        )
        .expect("start run");
    let run_id = started["run"]["run_id"]
        .as_str()
        .or_else(|| started["run_id"].as_str())
        .unwrap()
        .to_owned();

    let opaque = json!({
        "generation": "opaque-generation-token",
        "freshness": {"output_root": "user-root", "refresh": "none"},
        "items": [],
        "count": 0
    })
    .to_string();
    let posted = client
        .payload(
            "acdd_blackboard",
            json!({
                "action": "post",
                "run": &run_id,
                "topic": "notes",
                "author": "w3-tests",
                "payload": &opaque,
                "format": "json"
            }),
        )
        .expect("blackboard post");
    assert_ne!(posted["entry"]["id"], Value::Null, "{posted}");

    for invalid in ["yaml", "", "JSON"] {
        let (_, rejected) = client
            .call(
                "acdd_blackboard",
                json!({"action": "read", "run": &run_id, "format": invalid}),
            )
            .expect("MCP transport");
        assert!(
            rejected.get("error").is_some() || rejected["result"]["isError"] == true,
            "invalid MCP format {invalid:?}: {rejected}"
        );
        let cli_rejected = run_cli(&workspace, &["--format", invalid, "blackboard", "read"]);
        assert!(!cli_rejected.status.success(), "invalid CLI format {invalid:?}");
    }

    let read_json = client
        .payload(
            "acdd_blackboard",
            json!({"action": "read", "run": &run_id, "format": "json"}),
        )
        .expect("blackboard read");
    assert_eq!(
        read_json["items"][0]["payload"].as_str(),
        Some(opaque.as_str()),
        "{read_json}"
    );
    client.finish().expect("server must finish successfully");
}

#[tokio::test]
async fn transport_accepts_exact_bound_and_replaces_oversized_frames() {
    use tokio::io::AsyncWriteExt;

    let frame_for = |size: usize| {
        let prefix = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"text\":\"";
        let suffix = "\"}}\n";
        format!(
            "{prefix}{}{suffix}",
            "x".repeat(size - prefix.len() - suffix.len())
        )
    };
    for size in [MAX_OUTPUT_BYTES - 1, MAX_OUTPUT_BYTES] {
        let mut writer = response::BoundedWriter::new(tokio::io::sink());
        let frame = frame_for(size);
        assert_eq!(frame.len(), size);
        serde_json::from_str::<Value>(frame.trim_end()).unwrap();
        writer.write_all(frame.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }
    let mut output = Vec::new();
    let mut writer = response::BoundedWriter::new(&mut output);
    let oversized = frame_for(MAX_OUTPUT_BYTES + 1);
    writer.write_all(oversized.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
    assert!(output.len() < MAX_OUTPUT_BYTES);
    let error: Value = serde_json::from_slice(&output).unwrap();
    assert!(error.get("error").is_some());
}

#[test]
fn serialization_counts_second_escaping_and_bounds_a_single_item() {
    let policy = ResponsePolicy::default();
    let result = response::result_with_format(
        Ok(
            json!({"nodes":{"total":2,"offset":0,"limit":30,"items":[{"id":"first","text":"\"\\\n".repeat(6000)},{"id":"second","text":"\"\\\n".repeat(6000)}],"has_more":false,"next_offset":null,"generation":"snapshot"}}),
        ),
        &policy,
        OutputFormat::Json,
    );
    assert!(response::serialized_bytes(&result) <= MAX_OUTPUT_BYTES - 256);
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let wire = serde_json::to_value(result).unwrap();
    let payload: Value =
        serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(crate::json_array(&payload["nodes"]["items"]).len(), 1);
    assert_eq!(payload["nodes"]["total"], 2);
    assert_eq!(payload["nodes"]["limit"], 30);
    assert_eq!(payload["nodes"]["offset"], 0);
    assert_eq!(payload["nodes"]["generation"], "snapshot");
    assert_eq!(payload["nodes"]["has_more"], true);
    assert_eq!(payload["nodes"]["byte_limited"], true);
    assert_eq!(payload["nodes"]["next_offset"], 1);
    let item_hint = payload["nodes"]["continuation_hint"].as_str().unwrap();
    assert!(item_hint.contains("same tool and filters"));
    assert!(item_hint.contains("offset=1"));
    assert!(item_hint.contains("generation"));

    // In-place compaction must preserve full inactive generation tokens for tail trimming across formats.
    for format in [OutputFormat::Json, OutputFormat::Markdown] {
        let res = response::result_with_format(
            Ok(json!({
                "nodes": {
                    "total": 2,
                    "offset": 0,
                    "limit": 30,
                    "has_more": false,
                    "next_offset": null,
                    "generation": "snapshot-full-token-123456",
                    "items": [
                        {"id": "a", "text": "\"\\\n".repeat(6000)},
                        {"id": "b", "text": "\"\\\n".repeat(6000)}
                    ]
                }
            })),
            &policy,
            format,
        );
        assert_ne!(res.is_error, Some(true), "{format:?}: {res:?}");
        assert!(response::serialized_bytes(&res) <= MAX_OUTPUT_BYTES - 256);
        let text = res.content[0].as_text().unwrap().text.as_str();
        assert!(text.contains("snapshot-full-token-123456"), "{format:?}: {text}");
    }
    let result = response::result_with_format(
        Ok(json!({"scalar":"\"\\Привіт".repeat(30_000)})),
        &policy,
        OutputFormat::Json,
    );
    assert_eq!(result.is_error, Some(true));
    assert!(response::serialized_bytes(&result) <= MAX_OUTPUT_BYTES - 256);

    let fitting_text = "λ".repeat(36_000);
    assert!(fitting_text.len() > 64 * 1024);
    assert!(fitting_text.len() < 96 * 1024);
    let fitting = response::result_with_format(
        Ok(json!({
            "tasks": [{"task_id":"fit-96kib","target":fitting_text.clone()}],
            "pagination": {
                "total": 1,
                "limit": 1,
                "offset": 0,
                "returned": 1,
                "next_offset": null,
                "has_more": false,
                "generation": "task-view-fit",
                "byte_omitted": 0
            }
        })),
        &policy,
        OutputFormat::Json,
    );
    assert_ne!(fitting.is_error, Some(true), "{fitting:?}");
    assert_eq!(MAX_OUTPUT_BYTES, 98_304);
    assert_eq!(policy.max_output_bytes, 98_304);
    assert!(response::serialized_bytes(&fitting) <= MAX_OUTPUT_BYTES - 256);
    let fitting_wire = serde_json::to_value(fitting).unwrap();
    let fitting_payload: Value =
        serde_json::from_str(fitting_wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(fitting_payload["tasks"][0]["target"], fitting_text);

    let lower_policy = ResponsePolicy {
        max_output_bytes: 4_096,
        ..ResponsePolicy::default()
    };
    let lower_limit = response::result_with_format(
        Ok(json!({"items":[{"id":"configured-limit","text":"x".repeat(5_000)}]})),
        &lower_policy,
        OutputFormat::Json,
    );
    assert_eq!(lower_limit.is_error, Some(true));
    assert!(response::serialized_bytes(&lower_limit) <= 4_096 - 256);

    let task_statuses = ["completed", "ready", "in_progress", "blocked", "cancelled"];
    let task_records = (0..3)
        .map(|index| {
            json!({
                "task_id": format!("acdd/m-large:task-{index}"),
                "target": format!("λ{} \"\\", "λ".repeat(32_000)),
                "subtasks": task_statuses.iter().map(|status| json!({
                    "subtask_ref": status,
                    "status": status
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let trimmed_tasks = response::result_with_format(
        Ok(json!({
            "tasks": task_records,
            "pagination": {
                "total": 7,
                "limit": 3,
                "offset": 2,
                "returned": 3,
                "next_offset": 5,
                "has_more": true,
                "generation": "task-view-snapshot",
                "byte_omitted": 0,
                "continuation_hint": {
                    "workspace": "main",
                    "milestone_ref": "010-active",
                    "status": "all",
                    "detail": "full",
                    "generation": "task-view-snapshot"
                }
            }
        })),
        &policy,
        OutputFormat::Json,
    );
    assert_ne!(trimmed_tasks.is_error, Some(true), "{trimmed_tasks:?}");
    assert!(response::serialized_bytes(&trimmed_tasks) <= MAX_OUTPUT_BYTES - 256);
    let task_wire = serde_json::to_value(trimmed_tasks).unwrap();
    let task_page: Value =
        serde_json::from_str(task_wire["content"][0]["text"].as_str().unwrap()).unwrap();
    let emitted = crate::json_array(&task_page["tasks"]);
    assert_eq!(emitted.len(), 1);
    assert_eq!(crate::json_array(&emitted[0]["subtasks"]).len(), 5);
    assert_eq!(emitted[0]["subtasks"][0]["status"], "completed");
    assert_eq!(task_page["pagination"]["total"], 7);
    assert_eq!(task_page["pagination"]["limit"], 3);
    assert_eq!(task_page["pagination"]["offset"], 2);
    assert_eq!(task_page["pagination"]["returned"], 1);
    assert_eq!(task_page["pagination"]["byte_omitted"], 2);
    assert_eq!(task_page["pagination"]["next_offset"], 3);
    assert_eq!(task_page["pagination"]["has_more"], true);
    assert_eq!(task_page["pagination"]["generation"], "task-view-snapshot");
    let hint = task_page["pagination"]["continuation_hint"].to_string();
    for filter in ["main", "010-active", "all", "full", "task-view-snapshot"] {
        assert!(
            hint.contains(filter),
            "continuation hint omitted {filter}: {hint}"
        );
    }

    let final_page = response::result_with_format(
        Ok(json!({
            "tasks": [{"task_id":"last","target":"last intact record","subtasks":[]}],
            "pagination": {
                "total": 4,
                "limit": 3,
                "offset": 3,
                "returned": 1,
                "has_more": false,
                "generation": "task-view-final",
                "byte_omitted": 0
            }
        })),
        &policy,
        OutputFormat::Json,
    );
    assert_ne!(final_page.is_error, Some(true), "{final_page:?}");
    let final_wire = serde_json::to_value(final_page).unwrap();
    let final_payload: Value =
        serde_json::from_str(final_wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(final_payload["pagination"].get("next_offset").is_none());
    assert_eq!(final_payload["pagination"]["has_more"], false);

    let empty_page = response::result_with_format(
        Ok(json!({
            "tasks": [],
            "pagination": {
                "total": 0,
                "limit": 30,
                "offset": 0,
                "returned": 0,
                "has_more": false,
                "generation": "task-view-empty",
                "byte_omitted": 0
            }
        })),
        &policy,
        OutputFormat::Json,
    );
    assert_ne!(empty_page.is_error, Some(true), "{empty_page:?}");
    let empty_wire = serde_json::to_value(empty_page).unwrap();
    let empty_payload: Value =
        serde_json::from_str(empty_wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(crate::json_array(&empty_payload["tasks"]).is_empty());
    assert!(empty_payload["pagination"].get("next_offset").is_none());

    let oversized_task = response::result_with_format(
        Ok(json!({
            "tasks": [{"task_id":"too-large","target":"界".repeat(40_000),"subtasks":[]}],
            "pagination": {
                "total": 1,
                "limit": 1,
                "offset": 0,
                "returned": 1,
                "next_offset": null,
                "has_more": false,
                "generation": "task-view-oversized",
                "byte_omitted": 0
            }
        })),
        &policy,
        OutputFormat::Json,
    );
    assert_eq!(oversized_task.is_error, Some(true));
    assert!(response::serialized_bytes(&oversized_task) <= MAX_OUTPUT_BYTES - 256);
    let oversized_wire = serde_json::to_value(oversized_task).unwrap();
    let oversized_message = oversized_wire["content"][0]["text"].as_str().unwrap();
    assert!(
        oversized_message.to_ascii_lowercase().contains("compact")
            || oversized_message.to_ascii_lowercase().contains("narrow")
    );
    assert!(
        oversized_message.to_ascii_lowercase().contains("task")
            || oversized_message.to_ascii_lowercase().contains("record")
    );
    assert!(!oversized_message.contains("Output exceeds the response byte limit"));
}
