use crate::acdd::{harness::workflows::WorkflowHarness, support::*};
use acdd_tasks::engine::{candidate_fingerprint, orchestrator::workflow::run_standalone_workflow};
use std::{fs, path::Path};

#[test]
fn review_drift_warns_and_requires_rebound_verification_and_review() {
    use crate::acdd::tasks::harness::TaskHarness;

    let mut harness = TaskHarness::disk();
    harness.advance_to("review").unwrap();
    let claimed = harness.claim_next().unwrap();
    let proof = harness.evidence_for_gate(&claimed).unwrap();
    let prior = harness
        .store
        .build_snapshot_candidate(&harness.id)
        .unwrap()
        .unwrap()
        .0;
    fs::write(
        harness.root().join("src/lib.rs"),
        "pub fn example() { let _ = 1; }\n",
    )
    .unwrap();
    let result = harness.submit_evidence(&proof);
    assert!(
        result.is_ok(),
        "ordinary review drift must produce a recoverable warning: {result:#?}"
    );
    let warning = result.unwrap().to_string();
    assert!(warning.contains("candidate_drift_warning"), "{warning}");
    assert!(
        warning.contains("src/lib.rs"),
        "warning must identify changed paths: {warning}"
    );
    let pending = harness.store.inspect(&harness.id).unwrap();
    assert_ne!(
        pending.stage, "deliver",
        "stale review cannot authorize a receipt"
    );
    assert_ne!(pending.status, "completed");

    harness.advance_to_completion().unwrap();
    let rebound = harness.persisted_gate_evidence("build");
    let review = harness.persisted_gate_evidence("review");
    assert_ne!(
        rebound["commit"], prior,
        "verification must bind the changed source"
    );
    assert_eq!(review["commit"], rebound["commit"]);
    assert_ne!(review["worker_id"], rebound["worker_id"]);
    let build_count: u64 = harness
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_gates WHERE task_id=?1 AND gate='build' AND state='passed'",
            [&harness.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        build_count >= 2,
        "source drift must renew observed verification"
    );
}

#[test]
fn candidate_fingerprint_excludes_runtime_churn_and_retains_authority() {
    let root = Workspace::new();
    init_git(root.root());
    // Make runtime churn Git-visible: ignore rules must not supply the policy.
    root.write(".git/info/exclude", "");
    let baseline = candidate_fingerprint(root.root()).unwrap();
    let mut mismatches = Vec::new();
    for path in [
        ".acdd/tasks/current.json",
        ".acdd/checkpoints.json",
        ".acdd/tasks.sqlite",
        ".acdd/tasks.sqlite-wal",
        ".acdd/tasks.sqlite-shm",
        ".acdd/tasks.sqlite-journal",
        ".acdd/tasks.sqlite.lock",
        ".acdd/runner.log",
        ".acdd/run.jsonl",
    ] {
        root.write(path, "runtime observation\n");
        let actual = candidate_fingerprint(root.root()).unwrap();
        if actual != baseline {
            mismatches.push(path);
        }
        root.remove(path);
    }
    for path in [
        ".acdd/profile.yaml",
        ".acdd/frameworks/custom.yaml",
        "src/lib.rs",
    ] {
        let original = root
            .path(path)
            .exists()
            .then(|| fs::read(root.path(path)).unwrap());
        root.write(path, "changed authority\n");
        assert_ne!(
            candidate_fingerprint(root.root()).unwrap(),
            baseline,
            "{path}"
        );
        match original {
            Some(bytes) => fs::write(root.path(path), bytes).unwrap(),
            None => root.remove(path),
        }
    }
    assert!(
        mismatches.is_empty(),
        "runtime paths changed candidate binding: {mismatches:?}"
    );
}

#[test]
fn role_command_rejection_preserves_proof_and_retry_session() {
    let root = WorkflowHarness::agents().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    source["commands"] = json!({"check": {"command": "true", "output": {"require_counts": false}}});
    source["gates"][0]["proof"] = source["gates"][1]["proof"].clone();
    source["gates"][0]["command"] = json!("check");
    source["gates"][0]["fail"] = json!("build");
    source["roles"]["builder"]["reuse_on_reject"] = json!(true);
    root.write(
        "docs/workflows/agents.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
    assert!(
        result.is_ok(),
        "a valid rejection after exit zero must enter repair: {result:#?}"
    );
    let result = result.unwrap();
    let outcomes = result["outcomes"].as_array().unwrap();
    assert_eq!(outcomes[0]["passed"], false);
    assert_eq!(outcomes[1]["passed"], true);
    assert_eq!(
        outcomes[0]["evidence"]["proof"]["scheme_proof"]["decision"],
        "reject"
    );
    let sessions: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(sessions[0]["session"], sessions[1]["session"]);
    assert!(
        !sessions[1]["findings"].is_null(),
        "retry must receive rejection findings"
    );
}
