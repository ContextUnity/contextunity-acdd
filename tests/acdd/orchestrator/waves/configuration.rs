use super::harness;
use super::run_cli;
use crate::acdd::harness::plans::PlanHarness;
use crate::acdd::harness::workflows::WorkflowHarness;
use contextunity_acdd::acdd_tasks::tasks::profile;
use serde_json::Value;
use std::fs;

#[test]
fn selected_server_worktree_rebases_config_profile_and_index_without_leaking_root() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let primary = plan.materialize(plan.path);
    let selected = plan.materialize(plan.path);
    selected.write("src/lib.rs", "pub fn worktree_selected() {}\n");
    let settings = harness::selected_settings(
        primary.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/selected-index.sqlite",
    );
    let mut client = settings.run(|| {
        McpTestHarness::with_root_and_db(primary.root(), &settings.code_db, "worktree-server")
            .unwrap()
    });
    let found = client.payload("code_map_search", serde_json::json!({"pattern":"worktree_selected","exact":true,"worktree":selected.root()})).unwrap();
    assert_eq!(found["nodes"]["total"], 1);
    assert!(selected.path(".acdd/selected-index.sqlite").exists());
    assert!(!settings.code_db.exists());
    client
        .payload("task_list", serde_json::json!({"worktree":selected.root()}))
        .unwrap();
    assert!(selected.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!primary.path(".acdd/custom-tasks.sqlite").exists());
    let primary_result = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"worktree_selected","exact":true}),
        )
        .unwrap();
    assert_eq!(primary_result["nodes"]["total"], 0);
    assert!(settings.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn milestone_preflight_enforces_selected_module_policy() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "docs/milestones/001-selected.md",
        &MilestoneFixtureBuilder::new("m-selected")
            .task("one", &["src/first.rs"])
            .build(),
    );
    root.write("configs/minimal.yaml", "modules: {profile: minimal}\n");
    let error = run_cli(
        root.root(),
        &[
            "--config",
            "configs/minimal.yaml",
            "--profile",
            "profiles/plan.yaml",
            "milestone",
            "init",
            "docs/milestones/001-selected.md",
            "--in-place",
        ],
    )
    .unwrap_err();
    assert!(error.to_string().contains("FEATURE_DISABLED"), "{error:#}");
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    let result = run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "milestone",
            "init",
            "docs/milestones/001-selected.md",
            "--in-place",
        ],
    )
    .unwrap();
    assert_eq!(result["milestone_id"], "m-selected");
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn selected_config_reaches_cli_and_mcp_guide_validation() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected_guide() {}\n");
    let result = run_cli(
        root.root(),
        &["--config", "configs/index.yaml", "guide", "validate"],
    )
    .unwrap();
    assert_eq!(
        result["roots"],
        serde_json::json!([root.path("selected").canonicalize().unwrap()])
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/index.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "guide-server").unwrap()
    });
    let mcp = client
        .payload("forge_guide", serde_json::json!({"topic":"validate"}))
        .unwrap();
    assert_eq!(mcp["roots"], result["roots"]);
    assert_eq!(mcp["files"], result["files"]);
    assert!(!selection.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn validation_attributes_the_explicit_selected_profile() {
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    let result = run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "validate",
        ],
    )
    .unwrap();
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "PROFILE_VALID"
            && diagnostic["path"] == "profiles/plan.yaml"));
}

#[test]
fn selected_adapter_resolves_linked_sources_in_cli_and_mcp_ast_queries() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "configs/linked.yaml",
        "roots: [src]\nlinked_workspaces:\n  - name: extra\n    path: linked\n",
    );
    root.write("linked/lib.rs", "pub fn linked_selected() {}\n");
    let cli = run_cli(
        root.root(),
        &[
            "--db",
            ".acdd/linked-index.sqlite",
            "--config",
            "configs/linked.yaml",
            "ast",
            "grep",
            "pub fn $NAME() {}",
            "--language",
            "rust",
            "--path",
            "[extra]/lib.rs",
        ],
    )
    .unwrap();
    assert_eq!(
        cli["matches"]["items"][0]["path"], "[extra]/lib.rs",
        "{cli}"
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/linked.yaml",
        "profiles/plan.yaml",
        ".acdd/linked-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "linked-server").unwrap()
    });
    let mcp = client.payload("ast_grep_search", serde_json::json!({"pattern":"pub fn $NAME() {}","language":"rust","path":"[extra]/lib.rs"})).unwrap();
    assert_eq!(mcp["matches"]["items"][0]["path"], "[extra]/lib.rs");
    client.finish().unwrap();
}

#[test]
fn server_claim_uses_retained_explicit_profile_over_configured_profile() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "docs/milestones/001-selected.md",
        &MilestoneFixtureBuilder::new("m-selected")
            .task("one", &["src/first.rs"])
            .build(),
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "profile-server").unwrap()
    });
    // Configuration points to rejected.yaml; the explicitly selected, already
    // validated profile remains bound even when its source changes afterward.
    root.write(
        "profiles/plan.yaml",
        include_str!("../../fixtures/plans/rejected-profile.yaml"),
    );
    client
        .payload(
            "task_manage",
            serde_json::json!({"action":"sync","milestone_ref":"docs/milestones/001-selected.md"}),
        )
        .unwrap();
    let result = client.payload("task_claim", serde_json::json!({"task_id":"forge-mcp/m-selected:one","stage":"record","worker_id":"selected-profile-worker","worktree":root.root()})).unwrap();
    assert_eq!(result["stage"], "record");
    assert!(root.path(".acdd/custom-tasks.sqlite").is_file());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!selection.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn server_selection_survives_transport_lifetime_and_refreshes() {
    use crate::common::{mcp_client::invalidate_mcp_connection_cache, mcp_harness::McpTestHarness};
    use acdd_tasks::tasks::configuration::current;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "configs/index.yaml",
        "roots: [selected]\ntasks_db: .acdd/server-tasks.sqlite\n",
    );
    root.write("selected/lib.rs", "pub fn before_refresh() {}\n");
    let selection = harness::selected_settings(
        root.root(),
        "configs/index.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let db = root.path(".acdd/server-index.sqlite");
    let mut client =
        selection.run(|| McpTestHarness::spawn(root.root(), &db, "selected-server").unwrap());
    assert!(current(root.root()).is_none());
    let initialized = client.request_value("initialize", serde_json::json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"selection-proof","version":"1"}})).unwrap();
    assert!(initialized.get("error").is_none(), "{initialized}");
    client
        .notify("notifications/initialized", serde_json::json!({}))
        .unwrap();
    let before = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"before_refresh","exact":true}),
        )
        .unwrap();
    assert_eq!(before["nodes"]["total"], 1);
    root.write("selected/lib.rs", "pub fn after_refresh() {}\n");
    invalidate_mcp_connection_cache(&db).unwrap();
    let after = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"after_refresh","exact":true}),
        )
        .unwrap();
    assert_eq!(after["nodes"]["total"], 1);
    client.payload("task_list", serde_json::json!({})).unwrap();
    assert!(root.path(".acdd/server-tasks.sqlite").is_file());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(current(root.root()).is_none());
    client.finish().unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        Some(root.path("configs/index.yaml").canonicalize().unwrap())
    );
}

#[test]
fn simultaneous_servers_keep_selected_module_and_task_policies_independent() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/minimal.yaml", "modules: {profile: minimal}\n");
    let minimal = harness::selected_settings(
        root.root(),
        "configs/minimal.yaml",
        "profiles/plan.yaml",
        ".acdd/minimal-index.sqlite",
    );
    let full = harness::selected_settings(
        root.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/full-index.sqlite",
    );
    let parent = (
        std::env::var_os("ACDD_CONFIG"),
        std::env::var_os("ACDD_PROFILE"),
    );
    std::thread::scope(|threads| {
        let first = threads.spawn(|| {
            let mut client = minimal.run(|| {
                McpTestHarness::with_root_and_db(root.root(), &minimal.code_db, "minimal-server")
                    .unwrap()
            });
            let listed = client
                .request_value("tools/list", serde_json::json!({}))
                .unwrap();
            assert!(!listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "task_list"));
            let error = client
                .payload("task_list", serde_json::json!({}))
                .unwrap_err();
            assert!(error.to_string().contains("FEATURE_DISABLED"), "{error}");
            client.finish().unwrap();
        });
        let second = threads.spawn(|| {
            let mut client = full.run(|| {
                McpTestHarness::with_root_and_db(root.root(), &full.code_db, "full-server").unwrap()
            });
            let listed = client
                .request_value("tools/list", serde_json::json!({}))
                .unwrap();
            assert!(listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "task_list"));
            client.payload("task_list", serde_json::json!({})).unwrap();
            client.finish().unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();
    });
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert_eq!(
        (
            std::env::var_os("ACDD_CONFIG"),
            std::env::var_os("ACDD_PROFILE")
        ),
        parent
    );
}

#[test]
fn cli_query_rebuilds_existing_index_with_the_selected_policy() {
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected_query() {}\n");
    run_cli(root.root(), &["--db", ".acdd/query-index.sqlite", "build"]).unwrap();
    run_cli(
        root.root(),
        &[
            "--db",
            ".acdd/query-index.sqlite",
            "--config",
            "configs/index.yaml",
            "query",
            "overview",
        ],
    )
    .unwrap();
    let conn =
        contextunity_acdd::db::reader::open(&root.path(".acdd/query-index.sqlite"), root.root())
            .unwrap();
    let inventory = contextunity_acdd::db::reader::inventory_snapshot(&conn).unwrap();
    assert!(inventory
        .iter()
        .any(|entry| entry.path == "selected/lib.rs"));
    assert!(!inventory.iter().any(|entry| entry.path == "src/first.rs"));
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        Some(root.path("configs/index.yaml").canonicalize().unwrap())
    );
}

#[test]
fn cli_delta_selects_policy_and_preserves_legacy_persisted_selection() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected() {}\n");
    let db = root.path(".acdd/selected-index.sqlite");
    run_cli(root.root(), &["--db", db.to_str().unwrap(), "build"]).unwrap();
    run_cli(
        root.root(),
        &[
            "--db",
            db.to_str().unwrap(),
            "--config",
            "configs/index.yaml",
            "delta",
            "--modified",
            "selected/lib.rs",
        ],
    )
    .unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    let selected = contextunity_acdd::core::config::read_policy(root.root(), &conn).unwrap();
    assert_eq!(
        selected.adapter_path.as_deref(),
        Some(
            root.path("configs/index.yaml")
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    assert_eq!(
        contextunity_acdd::db::reader::inventory_snapshot(&conn)
            .unwrap()
            .iter()
            .filter(|entry| !entry.is_doc)
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["selected/lib.rs"]
    );
    drop(conn);
    root.write("selected/lib.rs", "pub fn selected_after_delta() {}\n");
    run_cli(
        root.root(),
        &[
            "--db",
            db.to_str().unwrap(),
            "delta",
            "--modified",
            "selected/lib.rs",
        ],
    )
    .unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        selected.adapter_path
    );
    assert!(!root.path(".acdd/code-map.sqlite").exists());
}

#[test]
fn cli_missing_or_malformed_policy_never_changes_delta_generation() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let db = root.path(".acdd/selected-index.sqlite");
    run_cli(root.root(), &["--db", db.to_str().unwrap(), "build"]).unwrap();
    root.write("configs/broken.yaml", "roots: [\n");
    let before = fs::read(&db).unwrap();
    for config in ["configs/absent.yaml", "configs/broken.yaml"] {
        for command in ["serve", "delta"] {
            let mut args = vec!["--db", db.to_str().unwrap(), "--config", config, command];
            if command == "delta" {
                args.extend(["--modified", "src/first.rs"]);
            }
            assert!(run_cli(root.root(), &args).is_err());
            assert_eq!(fs::read(&db).unwrap(), before);
        }
    }
}

#[test]
fn explicit_missing_configuration_or_profile_fails_before_plan_execution() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    for flag in ["--config", "--profile"] {
        let error = run_cli(
            root.root(),
            &[flag, "missing.yaml", "plan", "run", "linear"],
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("CLI_PATH_MISSING"),
            "{error:#}"
        );
        assert!(!root.path("plan-execution.log").exists());
    }
}

#[test]
fn global_config_selects_existing_task_store_without_using_index_db() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--db",
            ".acdd/selected-index.sqlite",
            "task",
            "list",
        ],
    )
    .unwrap();
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".acdd/selected-index.sqlite").exists());
}

#[test]
fn global_settings_survive_interleaved_future_polls_without_leaking() {
    use contextunity_acdd::acdd_tasks::{
        tasks::configuration::{current, InvocationSettings},
        TaskSettings,
    };
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let profile = std::sync::Arc::new(
        profile::validate_profile_document(include_str!("../../fixtures/plans/snapshot.yaml"))
            .unwrap(),
    );
    let default = InvocationSettings::new(
        root.root().to_owned(),
        Some(root.path("acdd.yaml")),
        profile.clone(),
        None,
        root.path(".acdd/first-index.sqlite"),
    );
    let custom = InvocationSettings::new(
        root.root().to_owned(),
        Some(root.path("configs/custom.yaml")),
        profile,
        None,
        root.path(".acdd/second-index.sqlite"),
    );
    let workspace_root = root.root();
    let check = |expected: &'static str| async move {
        for _ in 0..3 {
            assert_eq!(
                TaskSettings::load(workspace_root)
                    .unwrap()
                    .tasks_db
                    .to_str(),
                Some(expected)
            );
            tokio::task::yield_now().await;
        }
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::join!(
                default.clone().scope(check(".acdd/tasks.sqlite")),
                custom.scope(check(".acdd/custom-tasks.sqlite"))
            );
        });
    assert!(current(root.root()).is_none());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        default.run(|| panic!("restore scoped settings"))
    }));
    assert!(current(root.root()).is_none());
    assert_eq!(
        TaskSettings::load(root.root()).unwrap().tasks_db.to_str(),
        Some(".acdd/tasks.sqlite")
    );
}

#[test]
fn global_selections_reach_role_children_without_parent_environment_mutation() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    WorkflowHarness::agents().write_to(&root);
    let original = (
        std::env::var_os("ACDD_CONFIG"),
        std::env::var_os("ACDD_PROFILE"),
        std::env::var_os("ACDD_DB"),
    );
    run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "--db",
            ".acdd/selected-index.sqlite",
            "workflow",
            "run",
            "agents",
        ],
    )
    .unwrap();
    let canonical_root = root.root().canonicalize().unwrap();
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    for record in records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
    {
        assert_eq!(
            record["config"],
            canonical_root.join("configs/custom.yaml").to_str().unwrap()
        );
        assert_eq!(
            record["profile"],
            canonical_root.join("profiles/plan.yaml").to_str().unwrap()
        );
        assert_eq!(
            record["db"],
            canonical_root
                .join(".acdd/selected-index.sqlite")
                .to_str()
                .unwrap()
        );
    }
    assert_eq!(
        (
            std::env::var_os("ACDD_CONFIG"),
            std::env::var_os("ACDD_PROFILE"),
            std::env::var_os("ACDD_DB")
        ),
        original
    );
}
