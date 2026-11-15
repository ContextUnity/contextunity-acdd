use crate::acdd::{harness::workflows::WorkflowHarness, support::*};
use acdd_tasks::engine::orchestrator::workflow::{read_workflow, run_standalone_workflow};
use std::{fs, path::Path};

#[test]
fn workflow_inherits_workspace_resources_without_task_topology() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    let mut workspace = fixture_profile();
    workspace.commands.extend(harness.profile.commands.clone());
    workspace
        .roles
        .extend(WorkflowHarness::agents().profile.roles);
    workspace.limits.max_command_timeout_seconds = 17;
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    source["inherit"] = json!("workspace");
    source.as_object_mut().unwrap().remove("commands");
    root.write(
        "docs/workflows/ordered.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let resolved = read_workflow(root.root(), Path::new("docs/workflows/ordered.yaml"));
    assert!(
        resolved.is_ok(),
        "inherit: workspace must resolve repository resources: {resolved:#?}"
    );
    let resolved = resolved.unwrap();
    assert_eq!(
        serde_json::to_value(&resolved.roles).unwrap(),
        serde_json::to_value(&workspace.roles).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&resolved.policies).unwrap(),
        serde_json::to_value(&workspace.policies).unwrap()
    );
    assert_eq!(resolved.limits.max_command_timeout_seconds, 17);
    assert_eq!(
        serde_json::to_value(&resolved.gates).unwrap(),
        serde_json::to_value(&harness.profile.gates).unwrap()
    );
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/ordered.yaml"), None)
            .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\n"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn workflow_save_log_appends_observations_without_task_rows() {
    let root = WorkflowHarness::ordered().materialize();
    let source =
        include_str!("../fixtures/workflows/ordered.yaml").replace("save: none", "save: log");
    root.write("docs/workflows/ordered.yaml", &source);
    let mut retained_logs: std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> =
        std::collections::BTreeMap::new();
    for run in 0..2 {
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/ordered.yaml"), None);
        assert!(
            result.is_ok(),
            "save: log must execute a taskless workflow: {result:#?}"
        );
        assert_eq!(result.unwrap()["status"], "completed");
        for (path, prior) in &retained_logs {
            assert!(
                fs::read(path).unwrap().starts_with(prior),
                "run {run} discarded earlier observations"
            );
        }
        // Discover runtime JSONL traces without fixing a particular run ID or
        // filename. A later run must preserve every earlier record byte-for-byte.
        for entry in walkdir::WalkDir::new(root.path(".acdd")) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("jsonl")
            {
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap();
            retained_logs.insert(entry.path().to_owned(), bytes);
        }
        assert!(
            !retained_logs.is_empty(),
            "save: log must persist structured run observations"
        );
    }
    let mut records = Vec::new();
    for bytes in retained_logs.values() {
        let text = std::str::from_utf8(bytes).unwrap();
        records.extend(
            text.lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()),
        );
    }
    assert!(
        records.len() >= 4,
        "both runs must retain structured gate observations: {records:?}"
    );
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\nfirst\nsecond\n"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".git").exists());
}
