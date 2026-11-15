use super::harness;
use super::root_fixture::RootGateHarness;
use crate::acdd::support::git_command;
use serde_json::Value;
use std::fs;

#[test]
fn milestone_replay_reconstructs_visits_and_failure_budget() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    for (name, command) in [
        (
            "check",
            "echo check >> .acdd/replay-visits; if test -f .acdd/reviewed; then exit 1; fi",
        ),
        (
            "review",
            "echo review >> .acdd/replay-visits; touch .acdd/reviewed; exit 1",
        ),
        ("repair", "echo repair >> .acdd/replay-visits"),
    ] {
        profile.commands.insert(
            name.into(),
            serde_json::from_value(
                serde_json::json!({"command": command, "output": {"require_counts": false}}),
            )
            .unwrap(),
        );
    }
    for (index, gate) in [
        serde_json::json!({"id":"check","command":"check","proof":"none","max_attempts":2,"pass":"root_retry_review","fail":"repair"}),
        serde_json::json!({"id":"root_retry_review","command":"review","proof":"none","max_attempts":2,"fail":"check"}),
        serde_json::json!({"id":"repair","command":"repair","proof":"none","pass":"check"}),
    ].into_iter().enumerate() {
        profile.gates.insert(index, serde_json::from_value(gate).unwrap());
    }
    h.commit_profile(&profile, "replay retry budgets");
    for _ in 0..2 {
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: check"),
            "{error:#}"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/replay-visits")).unwrap(),
            "check\nreview\ncheck\nrepair\ncheck\n"
        );
    }
    let mut query = h.task.store.connection.prepare("SELECT gate_path, action, json_extract(proof_payload, '$.attempt') FROM gate_transitions WHERE subject='milestone' AND proof_kind='orchestrator' ORDER BY id").unwrap();
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            ("check".into(), "pass".into(), 1),
            ("root_retry_review".into(), "fail".into(), 1),
            ("check".into(), "fail".into(), 2),
            ("repair".into(), "pass".into(), 1),
            ("check".into(), "fail".into(), 3)
        ]
    );
}

#[test]
fn milestone_partial_root_prefix_reuses_refreshed_suffix_on_unchanged_restart() {
    use acdd_tasks::engine::{candidate_fingerprint, orchestrator::waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.partial_pre_task_checks(false);
    let original = fs::read(h.root().join("src/lib.rs")).unwrap();
    let initial = candidate_fingerprint(h.root()).unwrap();
    let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        first
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{first:#}"
    );
    assert_ne!(candidate_fingerprint(h.root()).unwrap(), initial);
    fs::write(h.root().join("src/lib.rs"), &original).unwrap();
    assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
    for _ in 0..2 {
        let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            resumed
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{resumed:#}"
        );
        assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
            "seen"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
            "seenseen"
        );
    }
}

#[test]
fn milestone_partial_root_prefix_preserves_failed_retry_route_on_restart() {
    use acdd_tasks::engine::{candidate_fingerprint, orchestrator::waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.partial_pre_task_checks(true);
    let original = fs::read(h.root().join("src/lib.rs")).unwrap();
    let initial = candidate_fingerprint(h.root()).unwrap();
    let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        first
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{first:#}"
    );
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
        "seenseenseen"
    );
    fs::write(h.root().join("src/lib.rs"), &original).unwrap();
    fs::remove_file(h.root().join(".acdd/preflight-retry-done")).unwrap();
    for _ in 0..2 {
        let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            resumed
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{resumed:#}"
        );
        assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
            "seen"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
            "seenseenseenseenseenseen"
        );
    }
}

#[test]
fn milestone_pre_task_observations_resume_unchanged_content() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    for _ in 0..2 {
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{error:#}"
        );
    }
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seen"
    );
}

#[test]
fn pre_task_bypass_route_survives_store_reopen_and_wave_replay() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_single_task(true);
    h.pre_task_checks(false);
    let mut profile = h.read_profile();
    profile.gates[0].on_success = Some("conditional".into());
    profile.gates.insert(
        1,
        serde_json::from_value(serde_json::json!({
            "id": "bypassed", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    profile.gates.insert(
        2,
        serde_json::from_value(serde_json::json!({
            "id": "conditional", "when": "doc", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    profile.gates.insert(
        3,
        serde_json::from_value(serde_json::json!({
            "id": "required", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    h.commit_profile(&profile, "persisted bypass route");
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let before: Vec<(String, String)> = h.task.store.connection.prepare(
        "SELECT gate_path, action FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('preflight','bypassed','conditional','required') ORDER BY id"
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        before,
        vec![
            ("preflight".into(), "pass".into()),
            ("required".into(), "pass".into())
        ]
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pre_task_bypass_reopens_before_fresh_task_claim() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = RootGateHarness::new();
    let mut profile = h.read_profile();
    for (index, gate) in [
        serde_json::json!({"id":"start","role":"wave_role","proof":"none","on_success":"conditional"}),
        serde_json::json!({"id":"bypassed","role":"wave_role","proof":"none"}),
        serde_json::json!({"id":"conditional","when":"doc","role":"wave_role","proof":"none"}),
        serde_json::json!({"id":"required","role":"wave_role","proof":"none"}),
    ].into_iter().enumerate() {
        profile.gates.insert(index, serde_json::from_value(gate).unwrap());
    }
    h.commit_profile(&profile, "fresh claim bypass fixture");
    let worktree = h.root().to_string_lossy().into_owned();
    for stage in ["start", "required"] {
        claim(
            h.root(),
            Claim {
                task_id: "m-wave".into(),
                stage: stage.into(),
                worker_id: "architect".into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
        submit(h.root(), Submit { task_id: "m-wave".into(), stage: stage.into(),
            evidence: serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"none_proof":{}}}),
            action: Action::Pass, findings: None, session_fingerprint: None }).unwrap();
    }
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let skipped: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')",
        [], |row| row.get(0)).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(h.task.claim_next().unwrap().status, "in_progress");
}

#[test]
fn milestone_pre_task_observations_invalidate_changed_implementation() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let baseline = h.checkpoint();
    for committed in [false, true] {
        h.restore(&baseline);
        fs::write(
            h.root().join("src/lib.rs"),
            "pub fn changed_after_preflight() {}\n",
        )
        .unwrap();
        if committed {
            for args in [
                vec!["add", "src/lib.rs"],
                vec!["commit", "-m", "changed implementation"],
            ] {
                assert!(git_command(h.root(), &args).status().unwrap().success());
            }
        }
        for _ in 0..2 {
            let error =
                run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
                "{error:#}"
            );
            assert_eq!(
                fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
                "seenseen",
                "committed={committed}"
            );
        }
    }
}

#[test]
fn milestone_pre_task_refresh_after_delivery_resumes_without_new_task_claims() {
    use acdd_tasks::engine::orchestrator::{types::OrchestratorEvent, waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(false);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    fs::remove_file(stop).unwrap();
    let mut claims = Vec::new();
    let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
        if let OrchestratorEvent::TaskClaimed { task_id, .. } = event {
            claims.push(task_id);
        }
    });
    assert!(resumed.is_ok(), "{resumed:#?}");
    assert!(claims.is_empty(), "{claims:?}");
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seenseen"
    );
}

#[test]
fn milestone_changed_content_invalidates_root_observation_reuse() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(true);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn example() { /* polish */ }\n",
    )
    .unwrap();
    assert!(git_command(h.root(), &["add", "src/lib.rs"])
        .status()
        .unwrap()
        .success());
    assert!(git_command(h.root(), &["commit", "-m", "fixture polish"])
        .status()
        .unwrap()
        .success());
    fs::remove_file(stop).unwrap();
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let contexts = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl")).unwrap();
    let polish = contexts
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|c| c["gate"] == "polish")
        .collect::<Vec<_>>();
    assert_eq!(polish.len(), 2);
    assert_ne!(
        polish[0]["subject_context"]["binding"],
        polish[1]["subject_context"]["binding"]
    );
}

#[test]
fn milestone_restart_reuses_bound_root_observations_and_delivered_tasks() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(true);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_RUNNER_FAILED"),
        "{error:#}"
    );
    let claims: u64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    fs::remove_file(stop).unwrap();
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let contexts = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl")).unwrap();
    let gates = contexts
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["gate"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(gates, ["polish", "milestone_review", "milestone_review"]);
    assert!(claims > 0);
    // Successful handoff prunes live claims only after accepting all existing receipts.
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
}

#[test]
fn milestone_corrupt_root_route_fails_closed_without_reexecuting_observations() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    h.task.store.connection.execute("UPDATE gate_transitions SET proof_payload=json_set(proof_payload, '$.previous_observation', id) WHERE id=(SELECT max(id) FROM gate_transitions WHERE proof_kind='orchestrator')", []).unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("MILESTONE_CHECKPOINT_INVALID"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seen"
    );
}

#[test]
fn pre_task_exhausted_native_run_is_replaced_by_current_observations() {
    use acdd_tasks::engine::{
        claim, manage, orchestrator::waves::run_milestone_waves, Claim, Manage, ManageAction,
    };

    let mut h = harness::WaveHarness::with_nested_blueprint(true);
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].max_attempts = Some(1);
    profile.gates[0].gates[0].proof = serde_json::from_value(serde_json::json!({
        "scheme": {"type": "object", "properties": {"tasks": {"type": "object"}}}
    }))
    .unwrap();
    h.commit_profile(&profile, "exhausted blueprint fixture");
    fs::write(h.root().join("root-proof.json"), r#"{"tasks":{"first":{"layer":"obsolete","summary":"old","blackboard_decision_id":"old"}}}"#).unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{error:#}"
    );
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));

    fs::write(
        h.root().join("root-proof.json"),
        r#"{"tasks":{"first":{"layer":"current","summary":"new","blackboard_decision_id":"new"}}}"#,
    )
    .unwrap();
    fs::write(h.root().join("src/lib.rs"), "// changed candidate\n").unwrap();
    let stop_review = h.root().join(".acdd/stop-blueprint-review");
    fs::write(&stop_review, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "blueprint_synthesis.review");
    fs::remove_file(stop_review).unwrap();
    // Stop before dispatch so the subsequent claim exercises a fresh store.
    h.options.task.runner_timeout = std::time::Duration::ZERO;
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let reviews: Vec<String> = h.task.store.connection.prepare(
        "SELECT action FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.review' ORDER BY id"
    ).unwrap().query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(reviews, ["fail", "pass"]);
    let task = h.task.store.inspect(&h.task.id).unwrap();
    let result = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: task.stage,
            worker_id: "fresh-worker".into(),
            worktree: h.root().to_string_lossy().into_owned(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    let inherited = &result["context_bundle"]["inherited_proofs"];
    assert!(inherited.to_string().contains("current"), "{result}");
    assert!(!inherited.to_string().contains("obsolete"), "{result}");
}
