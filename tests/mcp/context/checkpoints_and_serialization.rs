use super::*;

#[test]
fn removed_checkpoint_surfaces_are_absent_and_blackboard_serialization_is_opaque() {
    let (workspace, harness) = McpTestHarness::with_files(&[
        (
            "acdd.yaml",
            "profile: full\nmodules: {state: true, blackboard: true}\n",
        ),
        (".gitignore", ".acdd/\n"),
        ("src/lib.rs", "original\n"),
        ("docs/milestones/010-snapshot.md", "---\nid: m-snapshot\ntitle: Snapshot\ndoc_type: milestone\nstatus: active\n---\n# Snapshot\n\n### task: snap-task\n```yaml\ntask_ref: snap-task\ntarget: Snapshot\nscope: [src/lib.rs]\n```\n"),
        (
            ".acdd/profile.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ngates:\n  - tasks:\n      gates:\n        - id: verify\n          role: worker\n          proof: none\n",
        ),
    ]);
    let git = |args: &[&str]| {
        acdd_core::git::run_git_stdout(workspace.root(), args, None, &[], None)
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"))
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

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
            json!({"action": "start", "spec_path": "docs/milestones/010-snapshot.md", "in_place": true}),
        )
        .expect("start run");
    let run_id = started["run"]["run_id"]
        .as_str()
        .or_else(|| started["run_id"].as_str())
        .unwrap()
        .to_owned();

    let r1 = "original\n";
    let r2 = "revision-two\n";
    let snapshot_refs = || {
        acdd_core::git::run_git_stdout(
            workspace.root(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/acdd/snapshots",
            ],
            None,
            &[],
            None,
        )
        .expect("list snapshot refs")
    };
    for (rev, body) in [(1, r1), (2, r2)] {
        workspace.write("src/lib.rs", body);
        let result = client.payload("acdd_state", json!({
            "action": "snapshot", "snapshot_action": "save", "task_ref": "snap-task", "rev": rev, "run": &run_id, "format": "json"
        })).expect("snapshot save");
        assert!(result.to_string().contains(&format!("/snap-task/r{rev}")), "save r{rev}: {result}");
    }
    let refs_before_missing = snapshot_refs();
    assert!(
        refs_before_missing.contains("/snap-task/r1") && refs_before_missing.contains("/snap-task/r2"),
        "{refs_before_missing}"
    );
    let missing = client.payload("acdd_state", json!({
        "action": "snapshot", "task_ref": "snap-task", "run": &run_id
    })).expect_err("snapshot_action is required");
    let missing_text = missing.to_string();
    assert!(
        missing_text.contains("ACDD_PROFILE_SCHEMA_INVALID")
            && missing_text.contains("'snapshot_action' is required for action 'snapshot'"),
        "{missing_text}"
    );
    assert_eq!(
        snapshot_refs(),
        refs_before_missing,
        "omitted snapshot_action must not retarget or add checkpoint refs"
    );
    workspace.write("src/lib.rs", "changed\n");
    let restored = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "task_ref": "snap-task", "rev": 1, "run": &run_id, "format": "json"
    })).expect("restore r1");
    assert!(restored.to_string().contains("/snap-task/r1") && !restored.to_string().contains("/snap-task/r2"), "{restored}");
    assert_eq!(std::fs::read_to_string(workspace.path("src/lib.rs")).unwrap(), r1);
    let cleared = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "clear", "task_ref": "snap-task", "run": &run_id, "format": "json"
    })).expect("clear task");
    assert!(cleared.to_string().contains("/snap-task/r1"), "{cleared}");
    assert!(!snapshot_refs().contains("refs/acdd/snapshots/"), "{}", snapshot_refs());
    let unrestorable = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "task_ref": "snap-task", "rev": 1, "run": &run_id
    })).expect_err("cleared revision");
    assert!(unrestorable.to_string().contains("r1") || unrestorable.to_string().contains("refs/acdd/snapshots"), "{unrestorable}");
    assert_eq!(
        std::fs::read_to_string(workspace.path("src/lib.rs")).unwrap(),
        r1,
        "failed restore after task clear must preserve the worktree"
    );
    workspace.write("src/lib.rs", r2);
    client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "save", "task_ref": "snap-task", "rev": 1, "run": &run_id
    })).expect("save before all");
    workspace.write("src/lib.rs", "dirty-all\n");
    let cleared_all = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "clear", "all": true, "run": &run_id, "format": "json"
    })).expect("clear all");
    assert!(cleared_all.to_string().contains("/snap-task/r1"), "{cleared_all}");
    assert!(!snapshot_refs().contains("refs/acdd/snapshots/"), "{}", snapshot_refs());
    let unrestorable_all = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "task_ref": "snap-task", "rev": 1, "run": &run_id
    })).expect_err("cleared revision after run-wide clear");
    assert!(
        unrestorable_all.to_string().contains("r1") || unrestorable_all.to_string().contains("refs/acdd/snapshots"),
        "{unrestorable_all}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path("src/lib.rs")).unwrap(),
        "dirty-all\n",
        "failed restore after run-wide clear must preserve the worktree"
    );

    let worktree = workspace.root().to_str().unwrap();
    let saved_adhoc = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "save", "name": "note", "rev": 1, "worktree": worktree,
    })).expect("save adhoc snapshot");
    assert!(saved_adhoc.to_string().contains("_adhoc/note/r1"), "{saved_adhoc}");
    let restored_adhoc = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "name": "note", "rev": 1, "worktree": worktree,
    })).expect("restore adhoc snapshot");
    assert!(restored_adhoc.to_string().contains("_adhoc/note/r1"), "{restored_adhoc}");
    let tree = git(&["write-tree"]);
    let backdated = std::process::Command::new("git")
        .current_dir(workspace.root())
        .env("GIT_AUTHOR_DATE", "1000000000 +0000")
        .env("GIT_COMMITTER_DATE", "1000000000 +0000")
        .args(["commit-tree", &tree, "-m", "backdated"])
        .output()
        .expect("create backdated commit");
    assert!(backdated.status.success(), "{}", String::from_utf8_lossy(&backdated.stderr));
    git(&["update-ref", "refs/acdd/snapshots/_adhoc/note/r2", std::str::from_utf8(&backdated.stdout).unwrap().trim()]);
    let listed_rev1 = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "list", "name": "note", "rev": 1, "worktree": worktree, "format": "json",
    })).expect("list filtered adhoc snapshot");
    let listed_refs = listed_rev1["refs"].as_array().expect("refs array");
    assert_eq!(listed_refs.len(), 1, "{listed_rev1}");
    assert_eq!(listed_refs[0].as_str(), Some("refs/acdd/snapshots/_adhoc/note/r1"));
    client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "save", "name": "note", "rev": 3, "worktree": worktree,
    })).expect("save fresh adhoc snapshot");
    let cleared_adhoc = client.payload("acdd_state", json!({
        "action": "snapshot", "delete": true, "name": "note", "older_than": "1d", "worktree": worktree,
    })).expect("clear expired adhoc snapshot");
    assert!(cleared_adhoc.to_string().contains("_adhoc/note/r2"), "{cleared_adhoc}");
    let surviving_adhoc = snapshot_refs();
    assert!(
        surviving_adhoc.contains("_adhoc/note/r3") && !surviving_adhoc.contains("_adhoc/note/r2"),
        "{surviving_adhoc}"
    );

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
