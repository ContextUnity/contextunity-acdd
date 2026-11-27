use super::*;

#[test]
fn removed_checkpoint_surfaces_are_absent_and_blackboard_serialization_is_opaque() {
    let (workspace, harness) = McpTestHarness::with_files(&[
        (
            "acdd.yaml",
            "preset: full\nmodules: {state: true, blackboard: true}\n",
        ),
        (".gitignore", ".acdd/\n"),
        ("src/lib.rs", "original\n"),
        ("docs/milestones/010-snapshot.md", "---\nid: m-snapshot\ntitle: Snapshot\ndoc_type: milestone\nstatus: active\n---\n# Snapshot\n\n### task: snap-task\n```yaml\ntask_ref: snap-task\ntarget: Snapshot\nscope: [src/lib.rs]\n```\n"),
        (
            ".acdd/workflows/default.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ngates:\n  - tasks:\n      gates:\n        - id: verify\n          role: worker\n          proof: none\n",
        ),
    ]);
    let git = |args: &[&str]| {
        let out = acdd_core::git::git_command(workspace.root(), args)
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
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
        git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/acdd/snapshots",
        ])
    };
    for (rev, body) in [(1, r1), (2, r2)] {
        workspace.write("src/lib.rs", body);
        let result = client.payload("acdd_state", json!({
            "action": "snapshot", "snapshot_action": "save", "task_ref": "snap-task", "rev": rev, "run": &run_id, "format": "json"
        })).expect("snapshot save");
        assert!(
            result.to_string().contains(&format!("/snap-task/r{rev}")),
            "save r{rev}: {result}"
        );
    }
    let refs_before_missing = snapshot_refs();
    assert!(
        refs_before_missing.contains("/snap-task/r1")
            && refs_before_missing.contains("/snap-task/r2"),
        "{refs_before_missing}"
    );
    let missing = client
        .payload(
            "acdd_state",
            json!({
                "action": "snapshot", "task_ref": "snap-task", "run": &run_id
            }),
        )
        .expect_err("snapshot_action is required");
    let missing_text = missing.to_string();
    assert!(
        missing_text.contains("ACDD_WORKFLOW_SCHEMA_INVALID")
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
    assert!(
        restored.to_string().contains("/snap-task/r1")
            && !restored.to_string().contains("/snap-task/r2"),
        "{restored}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path("src/lib.rs")).unwrap(),
        r1
    );
    let cleared = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "clear", "task_ref": "snap-task", "run": &run_id, "format": "json"
    })).expect("clear task");
    assert!(cleared.to_string().contains("/snap-task/r1"), "{cleared}");
    assert!(
        !snapshot_refs().contains("refs/acdd/snapshots/"),
        "{}",
        snapshot_refs()
    );
    let unrestorable = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "task_ref": "snap-task", "rev": 1, "run": &run_id
    })).expect_err("cleared revision");
    assert!(
        unrestorable.to_string().contains("r1")
            || unrestorable.to_string().contains("refs/acdd/snapshots"),
        "{unrestorable}"
    );
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
    assert!(
        cleared_all.to_string().contains("/snap-task/r1"),
        "{cleared_all}"
    );
    assert!(
        !snapshot_refs().contains("refs/acdd/snapshots/"),
        "{}",
        snapshot_refs()
    );
    let unrestorable_all = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "task_ref": "snap-task", "rev": 1, "run": &run_id
    })).expect_err("cleared revision after run-wide clear");
    assert!(
        unrestorable_all.to_string().contains("r1")
            || unrestorable_all.to_string().contains("refs/acdd/snapshots"),
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
    assert!(
        saved_adhoc.to_string().contains("_adhoc/note/r1"),
        "{saved_adhoc}"
    );
    let restored_adhoc = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "restore", "name": "note", "rev": 1, "worktree": worktree,
    })).expect("restore adhoc snapshot");
    assert!(
        restored_adhoc.to_string().contains("_adhoc/note/r1"),
        "{restored_adhoc}"
    );
    let tree = git(&["write-tree"]);
    let backdated = std::process::Command::new("git")
        .current_dir(workspace.root())
        .env("GIT_AUTHOR_DATE", "1000000000 +0000")
        .env("GIT_COMMITTER_DATE", "1000000000 +0000")
        .args(["commit-tree", &tree, "-m", "backdated"])
        .output()
        .expect("create backdated commit");
    assert!(
        backdated.status.success(),
        "{}",
        String::from_utf8_lossy(&backdated.stderr)
    );
    git(&[
        "update-ref",
        "refs/acdd/snapshots/_adhoc/note/r2",
        std::str::from_utf8(&backdated.stdout).unwrap().trim(),
    ]);
    let listed_rev1 = client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "list", "name": "note", "rev": 1, "worktree": worktree, "format": "json",
    })).expect("list filtered adhoc snapshot");
    let listed_refs = listed_rev1["refs"].as_array().expect("refs array");
    assert_eq!(listed_refs.len(), 1, "{listed_rev1}");
    assert_eq!(
        listed_refs[0].as_str(),
        Some("refs/acdd/snapshots/_adhoc/note/r1")
    );
    client.payload("acdd_state", json!({
        "action": "snapshot", "snapshot_action": "save", "name": "note", "rev": 3, "worktree": worktree,
    })).expect("save fresh adhoc snapshot");
    let cleared_adhoc = client.payload("acdd_state", json!({
        "action": "snapshot", "delete": true, "name": "note", "older_than": "1d", "worktree": worktree,
    })).expect("clear expired adhoc snapshot");
    assert!(
        cleared_adhoc.to_string().contains("_adhoc/note/r2"),
        "{cleared_adhoc}"
    );
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
        assert!(
            !cli_rejected.status.success(),
            "invalid CLI format {invalid:?}"
        );
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
