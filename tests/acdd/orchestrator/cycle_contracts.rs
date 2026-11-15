use crate::acdd::{harness::workflows::WorkflowHarness, support::*, tasks::harness::TaskHarness};
use acdd_tasks::{
    engine::{
        candidate_fingerprint,
        orchestrator::{
            task_runner::TaskRunOptions,
            waves::{run_milestone_waves, MilestoneRunOptions},
            workflow::run_standalone_workflow,
        },
    },
    tasks::profile::GateDef,
};
use std::{fs, path::Path};

const ROOT_ID: &str = "m-cycle";
const MANIFEST: &str = "docs/milestones/012-cycle.md";

// Root contracts compose the canonical disk harness; no task runner subprocess is needed.
fn root_harness(pre: Vec<Value>, post: Vec<Value>) -> TaskHarness {
    let mut profile = runner_profile(DEFAULT_SEAM, "");
    profile.commands.insert("cycle-check".into(), serde_json::from_value(json!({
        "command": "if test -f .acdd/cycle-repaired.log; then printf 'test result: ok. 1 passed; 0 failed\\n'; else printf 'cycle failing command diagnostic\\n' >&2; exit 37; fi",
        "output": {"require_counts": true, "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
    })).unwrap());
    profile.commands.insert(
        "cycle-stop".into(),
        serde_json::from_value(json!({
            "command":"exit 53", "output":{"require_counts":false}
        }))
        .unwrap(),
    );
    profile.roles.insert("observer".into(), serde_json::from_value(json!({
        "runner": format!("python3 {} '{{prompt}}'", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py"))
    })).unwrap());
    let mut repair = profile.roles["observer"].clone();
    repair.runner = Some(format!("python3 -c 'import runpy;runpy.run_path(\"{}\");open(\".acdd/cycle-repaired.log\",\"w\").write(\"repaired\")' '{{prompt}}'", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py")));
    let mut mutating_review = repair.clone();
    mutating_review.runner = Some(format!("python3 -c 'import runpy;runpy.run_path(\"{}\");open(\"src/lib.rs\",\"a\").write(\"// renewed drift\\n\")' '{{prompt}}'", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py")));
    profile
        .roles
        .insert("mutating_review".into(), mutating_review);
    profile.roles.insert("repair".into(), repair);
    let tasks = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let handoff = profile.find_gate("handoff").unwrap().clone();
    profile.gates = pre
        .into_iter()
        .map(|gate| serde_json::from_value::<GateDef>(gate).unwrap())
        .collect();
    profile.gates.push(tasks);
    profile.gates.extend(
        post.into_iter()
            .map(|gate| serde_json::from_value::<GateDef>(gate).unwrap()),
    );
    profile.gates.push(handoff);
    profile.validate().unwrap();
    let source = MilestoneFixtureBuilder::new(ROOT_ID)
        .task("first", &["src/"])
        .build();
    TaskHarness::disk_with_profile(&source, MANIFEST, profile)
}

fn findings_gates() -> Vec<Value> {
    vec![
        json!({"id":"check","command":"cycle-check","proof":"command","fail":"repair","pass":"stale","max_attempts":2}),
        json!({"id":"repair","role":"repair","proof":"none","pass":"check"}),
        json!({"id":"stale","command":"cycle-stop","proof":"none","fail":"recover","max_attempts":2}),
        json!({"id":"recover","command":"cycle-check","proof":"command"}),
        json!({"id":"inspect","role":"observer","proof":"none"}),
        json!({"id":"stop","command":"cycle-stop","proof":"none","max_attempts":1}),
    ]
}

fn root_submit(h: &TaskHarness, stage: &str, worker: &str, proof: Value) -> anyhow::Result<Value> {
    tasks::submit(
        h.root(),
        tasks::Submit {
            task_id: ROOT_ID.into(),
            stage: stage.into(),
            action: tasks::Action::Pass,
            evidence: json!({"worker_id":worker,"worktree":h.root(),"proof":proof}),
            findings: None,
            session_fingerprint: Some(TaskHarness::session_for(worker)),
        },
    )
}

fn root_observations(h: &TaskHarness, stage: &str) -> Vec<Value> {
    let mut statement = h.store.connection.prepare(
        "SELECT proof_payload FROM gate_transitions WHERE subject='milestone' AND gate_path=?1 ORDER BY id"
    ).unwrap();
    statement
        .query_map([stage], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect()
}

fn run_waves(h: &TaskHarness) -> anyhow::Result<Value> {
    run_milestone_waves(
        h.root(),
        ROOT_ID,
        &MilestoneRunOptions {
            task: TaskRunOptions {
                worktree: h.root().to_owned(),
                worker_id: "cycle-coordinator".into(),
                session_fingerprint: "cycle-coordinator-session".into(),
                runner_timeout: std::time::Duration::from_secs(20),
            },
            in_place: true,
            target_base: git_head(h.root()),
        },
        &mut |_| {},
    )
}

#[test]
fn review_drift_warns_and_requires_rebound_verification_and_review() {
    let source = MilestoneFixtureBuilder::new("m-drift")
        .task("first", &["src/", "tests/"])
        .build();
    let mut harness =
        TaskHarness::disk_with_profile(&source, MANIFEST, runner_profile(DEFAULT_SEAM, ""));
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
    let response = result.unwrap();
    let warning = &response["candidate_drift_warning"];
    assert!(warning.is_object(), "{response}");
    assert!(
        warning["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&json!("src/lib.rs")),
        "warning must identify changed paths: {warning}"
    );
    assert!(!warning["prior_binding"].is_null(), "{warning}");
    assert!(!warning["current_binding"].is_null(), "{warning}");
    assert_ne!(warning["prior_binding"], warning["current_binding"]);
    let pending = harness.store.inspect(&harness.id).unwrap();
    assert_ne!(
        pending.stage, "deliver",
        "stale review cannot authorize a receipt"
    );
    assert_ne!(pending.status, "completed");

    assert!(pending.receipt.is_none());
    assert_eq!(
        harness.persisted_gate_evidence("build")["commit"],
        prior,
        "drift cannot rewrite historical accepted proof"
    );
    // Reopening the store must retain the obligation, rather than making a stale pass valid.
    harness.store = tasks::store_for_task(harness.root(), &harness.id).unwrap();
    assert!(
        harness.submit_evidence(&proof).is_err(),
        "old claim cannot discharge drift"
    );
    harness.advance_to("deliver").unwrap();
    let rebound = harness.persisted_gate_evidence("build");
    let review = harness.persisted_gate_evidence("review");
    assert_ne!(
        rebound["commit"], prior,
        "verification must bind the changed source"
    );
    assert_eq!(review["commit"], rebound["commit"]);
    assert_ne!(review["worker_id"], rebound["worker_id"]);
    // A second drift, this time in a test path at the terminal gate, restarts recovery.
    let delivered_claim = harness.claim_next().unwrap();
    let stale_delivery = harness.evidence_for_gate(&delivered_claim).unwrap();
    fs::write(
        harness.root().join("tests/drift.rs"),
        "// changed verification input\n",
    )
    .unwrap();
    let response = harness.submit_evidence(&stale_delivery).unwrap();
    assert!(
        response["candidate_drift_warning"]["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&json!("tests/drift.rs")),
        "{response}"
    );
    assert!(harness
        .store
        .inspect(&harness.id)
        .unwrap()
        .receipt
        .is_none());
    assert_ne!(
        harness.store.inspect(&harness.id).unwrap().status,
        "completed"
    );
    assert!(harness.submit_evidence(&stale_delivery).is_err());
    harness.advance_to_completion().unwrap();
    let final_build = harness.persisted_gate_evidence("build");
    let final_review = harness.persisted_gate_evidence("review");
    assert_ne!(final_build["commit"], rebound["commit"]);
    assert_eq!(final_review["commit"], final_build["commit"]);
    assert_ne!(final_review["worker_id"], final_build["worker_id"]);
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
        build_count >= 3,
        "each source/test drift must renew observed verification"
    );
    assert!(harness
        .store
        .inspect(&harness.id)
        .unwrap()
        .receipt
        .is_some());
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
fn scoped_candidate_excludes_tracked_and_untracked_runtime_churn() {
    let source = MilestoneFixtureBuilder::new("m-scoped-runtime")
        .task("first", &[".acdd/"])
        .build();
    let mut h = TaskHarness::disk_with_profile(&source, MANIFEST, runner_profile(DEFAULT_SEAM, ""));
    fs::write(h.root().join(".git/info/exclude"), "").unwrap();
    fs::write(h.root().join(".acdd/runner.log"), "tracked runtime\n").unwrap();
    assert!(std::process::Command::new("git")
        .current_dir(h.root())
        .args(["add", "-f", ".acdd/runner.log"])
        .status()
        .unwrap()
        .success());
    h.advance_to("review").unwrap();
    let prior = h.store.build_snapshot_candidate(&h.id).unwrap().unwrap().0;
    fs::remove_file(h.root().join(".acdd/runner.log")).unwrap();
    fs::write(h.root().join(".acdd/new.log"), "untracked runtime\n").unwrap();
    let claim = h.claim_next().unwrap();
    let proof = h.evidence_for_gate(&claim).unwrap();
    let result = h.submit_evidence(&proof).unwrap();
    assert!(result["candidate_drift_warning"].is_null(), "{result}");
    assert_eq!(h.persisted_gate_evidence("build")["commit"], prior);
    fs::create_dir_all(h.root().join(".acdd/frameworks")).unwrap();
    fs::write(
        h.root().join(".acdd/frameworks/custom.yaml"),
        "authority: changed\n",
    )
    .unwrap();
    let claim = h.claim_next().unwrap();
    let proof = h.evidence_for_gate(&claim).unwrap();
    let result = h.submit_evidence(&proof).unwrap();
    assert!(
        result["candidate_drift_warning"]["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&json!(".acdd/frameworks/custom.yaml")),
        "{result}"
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
    source["guidance"] =
        json!({"contour.paths":"Inspect scoped changes and the command observation."});
    let schema = source["gates"][0]["proof"].clone();
    let mut failures = Vec::new();
    for (proof_kind, definition) in [("scheme_proof", schema), ("review_proof", json!("review"))] {
        source["gates"][0]["proof"] = definition;
        if proof_kind == "review_proof" {
            source["gates"][0]["contours"] = json!("guidance.contours");
        }
        root.write(
            "docs/workflows/agents.yaml",
            &serde_yaml::to_string(&source).unwrap(),
        );
        root.write("workflow-role-sessions.jsonl", "");
        if root.path("review-rejected").exists() {
            root.remove("review-rejected");
        }
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                failures.push(format!("{proof_kind}: {error:#}"));
                continue;
            }
        };
        let outcomes = result["outcomes"].as_array().unwrap();
        assert_eq!(outcomes[0]["passed"], false);
        assert_eq!(outcomes[1]["passed"], true);
        assert_eq!(
            outcomes[0]["evidence"]["proof"][proof_kind]["decision"],
            "reject"
        );
        let sessions: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(sessions[0]["session"], sessions[1]["session"]);
        assert_eq!(
            sessions[1]["findings"],
            json!({"scope_findings":["Apply the required correction"]})
        );
        assert!(
            sessions[1]["findings"].is_object(),
            "retry requires structured rejection findings"
        );
        assert_ne!(
            sessions[1]["session"], sessions[2]["session"],
            "review stays independent"
        );
        assert!(
            sessions[2]["findings"].is_null(),
            "accepted retry clears old findings"
        );
    }
    assert!(
        failures.is_empty(),
        "valid rejection after exit zero must enter repair: {failures:?}"
    );
}

#[test]
fn role_command_pass_requires_successful_observed_command() {
    let root = WorkflowHarness::agents().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    source["commands"] = json!({"check":{"command":"printf 'observed failure\\n' >&2; exit 37","output":{"require_counts":false}}});
    source["gates"][0]["command"] = json!("check");
    source["gates"][0]["proof"] = source["gates"][1]["proof"].clone();
    // The protocol fixture emits a valid pass. Only the real command observation can veto it.
    root.write("review-rejected", "");
    root.write(
        "docs/workflows/agents.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let error = run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
        "{error:#}"
    );
    let sessions = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    assert_eq!(
        sessions.lines().count(),
        1,
        "a false pass must not reach independent review"
    );
}

#[test]
fn waves_command_failure_findings_reach_repair_and_clear_on_pass() {
    let h = root_harness(findings_gates(), Vec::new());
    let result = run_waves(&h);
    assert!(format!("{:#}", result.unwrap_err()).contains("WORKFLOW_ATTEMPTS_EXHAUSTED"));
    let checks = root_observations(&h, "check");
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["passed"], false);
    assert!(
        checks[0]["findings"].is_object(),
        "failed command findings: {}",
        checks[0]["findings"]
    );
    let diagnostic = checks[0]["findings"].to_string();
    for expected in ["cycle-check", "37", "cycle failing command diagnostic"] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    assert_eq!(checks[1]["passed"], true);
    assert!(checks[1]["findings"].is_null());
    let stale = root_observations(&h, "stale")
        .into_iter()
        .find(|row| row["passed"] == false)
        .unwrap();
    assert!(
        stale["findings"].is_object(),
        "direct recovery must start with failure findings"
    );
    let recovered = root_observations(&h, "recover")
        .into_iter()
        .find(|row| row["passed"] == true)
        .unwrap();
    assert!(
        recovered["findings"].is_null(),
        "passing command clears preceding command findings"
    );
    let roles: Vec<Value> = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(roles[0]["gate"], "repair");
    assert_eq!(
        roles[0]["findings"], checks[0]["findings"],
        "RoleObservations must carry current command findings"
    );
    assert_eq!(roles[1]["gate"], "inspect");
    assert!(
        roles[1]["findings"].is_null(),
        "passing command must clear repair findings"
    );
}

#[test]
fn waves_verification_failure_findings_reach_repair_and_clear_on_pass() {
    let mut h = root_harness(Vec::new(), findings_gates());
    h.advance_to_completion().unwrap();
    let task_receipt = serde_json::to_value(h.store.inspect(&h.id).unwrap().receipt).unwrap();
    let result = run_waves(&h);
    assert!(format!("{:#}", result.unwrap_err()).contains("WORKFLOW_ATTEMPTS_EXHAUSTED"));
    let checks: Vec<_> = root_observations(&h, "check")
        .into_iter()
        .filter(|row| row.get("passed").is_some())
        .collect();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["passed"], false);
    assert!(
        checks[0]["findings"].is_object(),
        "failed verification findings: {}",
        checks[0]["findings"]
    );
    let diagnostic = checks[0]["findings"].to_string();
    for expected in ["cycle-check", "37", "cycle failing command diagnostic"] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    assert_eq!(checks[1]["passed"], true);
    assert!(checks[1]["findings"].is_null());
    let stale = root_observations(&h, "stale")
        .into_iter()
        .find(|row| row["passed"] == false)
        .unwrap();
    assert!(
        stale["findings"].is_object(),
        "direct recovery must start with failure findings"
    );
    let recovered = root_observations(&h, "recover")
        .into_iter()
        .find(|row| row["passed"] == true)
        .unwrap();
    assert!(
        recovered["findings"].is_null(),
        "passing verification clears preceding command findings"
    );
    let roles: Vec<Value> = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(roles[0]["findings"], checks[0]["findings"]);
    assert!(roles[1]["findings"].is_null());
    assert_eq!(
        serde_json::to_value(h.store.inspect(&h.id).unwrap().receipt).unwrap(),
        task_receipt
    );
    assert!(
        h.root().join(MANIFEST).exists(),
        "failed verification cannot issue handoff receipt"
    );
}

#[test]
fn root_verification_returns_failed_observation_without_passing_proof() {
    let h = root_harness(
        vec![json!({"id":"check","command":"cycle-check","proof":"command"})],
        Vec::new(),
    );
    let result = tasks::manage(
        h.root(),
        tasks::Manage {
            action: tasks::ManageAction::Verify,
            task_id: Some(ROOT_ID.into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            worker_id: Some("verifier".into()),
            ..Default::default()
        },
    );
    assert!(
        result.is_ok(),
        "completed failure must be an observation: {result:#?}"
    );
    let observation = result.unwrap();
    assert_eq!(observation["passed"], false);
    assert_eq!(observation["exit_code"], 37);
    assert!(observation["output"]
        .as_str()
        .unwrap()
        .contains("cycle failing command diagnostic"));
    assert!(
        observation["proof"].is_null(),
        "failed command cannot fabricate a successful command proof"
    );
    let error = root_submit(&h, "check", "verifier", json!({"command_proof":{"command":"cycle-check","exit_code":0,"tests_passed":1,"tests_failed":0}})).unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("TASK_AUTO_PROOF_UNAVAILABLE") || error.contains("TASK_PROOF_UNOBSERVED"),
        "{error}"
    );
    assert!(
        root_observations(&h, "check").is_empty(),
        "failure cannot become an accepted root gate"
    );
}

#[test]
fn explicit_candidate_receipt_corruption_still_fails_closed() {
    let mut h = TaskHarness::disk();
    for stage in ["review", "deliver"] {
        h.advance_to(stage).unwrap();
        let claimed = h.claim_next().unwrap();
        let valid = h.evidence_for_gate(&claimed).unwrap();
        for commit in ["corrupted-explicit-receipt".to_owned(), git_head(h.root())] {
            let mut invalid = valid.clone();
            invalid.commit = Some(commit);
            let result = h.submit_evidence(&invalid);
            assert!(
                result.is_err(),
                "{stage}: explicit mismatched receipt must fail closed"
            );
            let error = format!("{:#}", result.unwrap_err());
            assert!(
                error.contains("TASK_CANDIDATE_MISMATCH"),
                "{stage}: {error}"
            );
            let current = h.store.inspect(&h.id).unwrap();
            assert_eq!(current.claim_revision, claimed.claim_revision);
            assert_eq!(current.stage, stage);
            assert!(current.receipt.is_none());
        }
        h.submit_evidence(&valid).unwrap();
    }
}

#[test]
fn root_snapshots_and_fingerprints_share_runtime_and_authority_paths() {
    let runtime_paths = [
        ".acdd/tasks/current.json",
        ".acdd/checkpoints.json",
        ".acdd/checkpoints.json.lock",
        ".acdd/runtime.sqlite",
        ".acdd/runtime.sqlite-wal",
        ".acdd/runtime.sqlite-shm",
        ".acdd/runtime.sqlite-journal",
        ".acdd/runtime.sqlite.lock",
        ".acdd/runtime.sqlite.verified.json",
        ".acdd/runner.log",
        ".acdd/run.jsonl",
    ];
    let authority_paths = [
        ".acdd/profile.yaml",
        ".acdd/frameworks/custom.yaml",
        "src/lib.rs",
    ];
    let gates = (0..=runtime_paths.len() + authority_paths.len()).map(|index| json!({
        "id":format!("snapshot_{index}"), "role":"observer", "proof":"none", "save":{"post":"snapshot"}
    })).collect();
    let h = root_harness(gates, Vec::new());
    // Force tracked runtime paths so ignore rules cannot accidentally prove exclusion parity.
    for path in runtime_paths.iter().chain([&authority_paths[1]]) {
        fs::create_dir_all(h.root().join(path).parent().unwrap()).unwrap();
        fs::write(h.root().join(path), "baseline\n").unwrap();
        assert!(git_command(h.root(), &["add", "-f", path])
            .status()
            .unwrap()
            .success());
    }
    TaskHarness::claim_at(h.root(), ROOT_ID, "snapshot_0", "snapshotter").unwrap();
    root_submit(&h, "snapshot_0", "snapshotter", json!({"none_proof":{}})).unwrap();
    let baseline_commit = root_observations(&h, "snapshot_0")[0]["root_snapshot"]
        ["snapshot_commit"]
        .as_str()
        .unwrap()
        .to_owned();
    let baseline_tree = acdd_tasks::git::git_stdout(
        h.root(),
        &["rev-parse", &format!("{baseline_commit}^{{tree}}")],
        "SNAPSHOT_TEST",
    )
    .unwrap();
    let baseline = candidate_fingerprint(h.root()).unwrap();
    let mut mismatches = Vec::new();
    for (index, path) in runtime_paths
        .iter()
        .chain(authority_paths.iter())
        .enumerate()
    {
        let original = fs::read(h.root().join(path)).unwrap();
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n# changed input\n");
        fs::write(h.root().join(path), changed).unwrap();
        let stage = format!("snapshot_{}", index + 1);
        TaskHarness::claim_at(h.root(), ROOT_ID, &stage, "snapshotter").unwrap();
        let fingerprint = candidate_fingerprint(h.root()).unwrap();
        root_submit(&h, &stage, "snapshotter", json!({"none_proof":{}})).unwrap();
        let snapshot = root_observations(&h, &stage)[0]["root_snapshot"].clone();
        let commit = snapshot["snapshot_commit"].as_str().unwrap();
        let tree = acdd_tasks::git::git_stdout(
            h.root(),
            &["rev-parse", &format!("{commit}^{{tree}}")],
            "SNAPSHOT_TEST",
        )
        .unwrap();
        let runtime = runtime_paths.contains(path);
        if (fingerprint == baseline) != runtime || (tree == baseline_tree) != runtime {
            mismatches.push(format!(
                "{path}: fingerprint_unchanged={}, snapshot_unchanged={}",
                fingerprint == baseline,
                tree == baseline_tree
            ));
        }
        let files = acdd_tasks::git::git_stdout(
            h.root(),
            &["ls-tree", "-r", "--name-only", commit],
            "SNAPSHOT_TEST",
        )
        .unwrap();
        assert!(
            runtime_paths
                .iter()
                .all(|path| !files.lines().any(|file| file == *path)),
            "{files}"
        );
        assert!(
            authority_paths
                .iter()
                .all(|path| files.lines().any(|file| file == *path)),
            "{files}"
        );
        fs::write(h.root().join(path), original).unwrap();
    }
    assert!(
        mismatches.is_empty(),
        "shared exclusion policy violated: {mismatches:?}"
    );
}

#[test]
fn root_immutable_drift_warns_and_withholds_acceptance_across_resume() {
    let h = root_harness(
        vec![
            json!({"id":"prepare","role":"observer","proof":"none","save":{"post":"snapshot"}}),
            json!({"id":"inspect","role":"observer","proof":"none","independent_from":"prepare","save":{"immutable":true}}),
        ],
        Vec::new(),
    );
    TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
    root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
    let accepted = root_observations(&h, "prepare");
    TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn example() { let _ = 2; }\n",
    )
    .unwrap();
    let result = root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}}));
    assert!(
        result.is_ok(),
        "ordinary immutable drift must return a warning: {result:#?}"
    );
    let response = result.unwrap();
    let warning = &response["candidate_drift_warning"];
    assert!(warning.is_object(), "{response}");
    assert!(warning["changed_paths"]
        .as_array()
        .unwrap()
        .contains(&json!("src/lib.rs")));
    assert_ne!(warning["prior_binding"], warning["current_binding"]);
    assert_eq!(
        root_observations(&h, "prepare"),
        accepted,
        "historical snapshot is immutable"
    );
    let accepted_reviews: u64 = h.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='inspect' AND action='pass'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(
        accepted_reviews, 0,
        "drift cannot accept the stale root review"
    );
    let resumed = tasks::store_for_task(h.root(), &h.id).unwrap();
    assert!(resumed.inspect(&h.id).unwrap().receipt.is_none());
    let retry = root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}}));
    assert!(
        retry.is_err() || retry.unwrap()["candidate_drift_warning"].is_object(),
        "resume cannot erase pending rebinding"
    );
    TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
    root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
    TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
    root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}})).unwrap();
    assert!(
        TaskHarness::claim_at(h.root(), &h.id, "contract", "task-builder").is_ok(),
        "a phase without a command must finish recovery"
    );
    assert!(h.root().join(MANIFEST).exists());
}

#[test]
fn root_recovery_requires_command_execution_after_each_drift() {
    let h = root_harness(
        vec![
            json!({"id":"prepare","role":"observer","proof":"none","save":{"post":"snapshot"}}),
            json!({"id":"check","command":"cycle-check","proof":"command"}),
            json!({"id":"inspect","role":"observer","proof":"none","independent_from":"prepare","save":{"immutable":true}}),
        ],
        Vec::new(),
    );
    fs::write(h.root().join(".acdd/cycle-repaired.log"), "ready").unwrap();
    let original = fs::read(h.root().join("src/lib.rs")).unwrap();
    let verify = || {
        tasks::manage(
            h.root(),
            tasks::Manage {
                action: tasks::ManageAction::Verify,
                task_id: Some(ROOT_ID.into()),
                worktree: Some(h.root().to_string_lossy().into_owned()),
                worker_id: Some("verifier".into()),
                ..Default::default()
            },
        )
        .unwrap()["proof"]
            .clone()
    };
    for round in 0..2 {
        TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
        root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
        let old = verify();
        root_submit(&h, "check", "verifier", old.clone()).unwrap();
        TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
        fs::write(h.root().join("src/lib.rs"), format!("// drift {round}\n")).unwrap();
        assert!(
            root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}})).unwrap()
                ["candidate_drift_warning"]
                .is_object()
        );
        fs::write(h.root().join("src/lib.rs"), &original).unwrap();
        TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
        root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
        let error = root_submit(&h, "check", "verifier", old).unwrap_err();
        assert!(
            format!("{error:#}").contains("TASK_PROOF_UNOBSERVED"),
            "{error:#}"
        );
        let fresh = verify();
        root_submit(&h, "check", "verifier", fresh).unwrap();
        TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
        if round == 1 {
            root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}})).unwrap();
        } else {
            fs::write(h.root().join("src/lib.rs"), "// next drift\n").unwrap();
            assert!(
                root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}})).unwrap()
                    ["candidate_drift_warning"]
                    .is_object()
            );
            fs::write(h.root().join("src/lib.rs"), &original).unwrap();
        }
    }
}

#[test]
fn manual_root_claim_reads_native_post_snapshot() {
    let h = root_harness(
        vec![
            json!({"id":"prepare","role":"observer","proof":"none","save":{"post":"snapshot"}}),
            json!({"id":"inspect","role":"observer","proof":"none","independent_from":"prepare","save":{"immutable":true}}),
        ],
        Vec::new(),
    );
    TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
    root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
    let saved = root_observations(&h, "prepare").pop().unwrap();
    let snapshot = saved["root_snapshot"].clone();
    let identity = h.store.connection.query_row("SELECT milestone_id FROM gate_transitions WHERE gate_path='prepare' ORDER BY id DESC LIMIT 1", [], |row| row.get::<_,String>(0)).unwrap();
    h.store.record_gate_transition(&acdd_tasks::store::GateTransition {
        milestone_id:identity, subject:"milestone".into(),gate_path:"prepare".into(),action:"pass".into(),proof_kind:"orchestrator".into(),
        proof_payload:json!({"previous_observation":null,"root_snapshot_fingerprint":saved["root_snapshot_fingerprint"],"post_snapshot":snapshot.clone()}),git_tree_sha:git_head(h.root()),worker_id:"builder".into(),
    }).unwrap();
    let reopened = tasks::store_for_task(h.root(), &h.id).unwrap();
    assert!(reopened.inspect(&h.id).unwrap().receipt.is_none());
    fs::write(h.root().join("src/lib.rs"), "// native drift\n").unwrap();
    let result = TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
    assert!(
        result["candidate_drift_warning"]["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&json!("src/lib.rs")),
        "{result}"
    );
    assert!(h.root().join(MANIFEST).exists());
    TaskHarness::claim_at(h.root(), ROOT_ID, "prepare", "builder").unwrap();
    root_submit(&h, "prepare", "builder", json!({"none_proof":{}})).unwrap();
    TaskHarness::claim_at(h.root(), ROOT_ID, "inspect", "reviewer").unwrap();
    root_submit(&h, "inspect", "reviewer", json!({"none_proof":{}})).unwrap();
    let saved_ref = snapshot["snapshot_ref"].as_str().unwrap();
    let resolved = std::process::Command::new("git")
        .current_dir(h.root())
        .args(["rev-parse", saved_ref])
        .output()
        .unwrap();
    assert!(resolved.status.success());
    assert_eq!(
        String::from_utf8(resolved.stdout).unwrap().trim(),
        snapshot["snapshot_commit"].as_str().unwrap()
    );
}

#[test]
fn waves_immutable_drift_records_warning_and_blocks_handoff() {
    let h = root_harness(
        Vec::new(),
        vec![
            json!({"id":"prepare","role":"observer","proof":"none","save":{"post":"snapshot"}}),
            json!({"id":"check","command":"cycle-check","proof":"command"}),
            json!({"id":"milestone_review","role":"observer","proof":"none","independent_from":"prepare"}),
            json!({"id":"mutating_review","role":"mutating_review","proof":"none","independent_from":"milestone_review","save":{"immutable":true},"max_attempts":2}),
        ],
    );
    let mut h = h;
    h.advance_to_completion().unwrap();
    fs::write(h.root().join(".acdd/cycle-repaired.log"), "ready").unwrap();
    // Mutation occurs inside the real role process, crossing the waves immutable check.
    let result = run_waves(&h);
    assert!(
        result.is_err(),
        "unverified source cannot authorize handoff"
    );
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        !error.contains("TASK_CANDIDATE_MISMATCH"),
        "ordinary drift must be recoverable: {error}"
    );
    let rows = root_observations(&h, "mutating_review");
    assert!(
        rows.iter()
            .any(|row| row.to_string().contains("candidate_drift_warning")
                && row.to_string().contains("src/lib.rs")),
        "warning must be durable: {rows:?}; {error}"
    );
    assert!(
        rows.iter().all(|row| row["passed"] != true),
        "stale review cannot pass"
    );
    assert!(h.root().join(MANIFEST).exists());
    assert!(!h
        .root()
        .join("docs/milestones/archive/012-cycle.md")
        .exists());
}
