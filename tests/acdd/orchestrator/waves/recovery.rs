use super::harness;
use crate::acdd::support::git_command;
use serde_json::Value;
use std::fs;

#[test]
fn recovery_route_state_matrix() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = super::root_fixture::RootGateHarness::with_dependencies();
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
    let options = h.run_options();
    for _ in 0..2 {
        let error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
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
fn pre_task_bound_recovery_matrix() {
    use acdd_tasks::engine::{candidate_fingerprint, orchestrator::waves::run_milestone_waves};
    let mut h = harness::WaveHarness::with_minimal_task_gates(true, true);
    let shared = h.checkpoint();
    enum Recovery {
        Partial { retry: bool },
        Unchanged,
        Changed { committed: bool },
        CorruptChain,
        BypassReplay,
        FreshClaim,
        CurrentProof,
    }
    let cases = [
        Recovery::Partial { retry: false },
        Recovery::Partial { retry: true },
        Recovery::Unchanged,
        Recovery::Changed { committed: false },
        Recovery::Changed { committed: true },
        Recovery::CorruptChain,
        Recovery::BypassReplay,
        Recovery::FreshClaim,
        Recovery::CurrentProof,
    ];
    for case in cases {
        h.restore(&shared);
        h.options.target_base = crate::acdd::support::git_head(h.root());
        match case {
            Recovery::FreshClaim => {
                use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

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
                let temporary =
                    acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
                drop(std::mem::replace(&mut h.task.store, temporary));
                h.task.store =
                    contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
                let skipped: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')",
        [], |row| row.get(0)).unwrap();
                assert_eq!(skipped, 0);
                assert_eq!(h.task.claim_next().unwrap().status, "in_progress");

                continue;
            }
            Recovery::CurrentProof => {
                use acdd_tasks::engine::{
                    claim, manage, orchestrator::waves::run_milestone_waves, Claim, Manage,
                    ManageAction,
                };

                let mut profile = h.read_profile();
                profile.gates.insert(0, harness::blueprint_synthesis_gate());
                profile.gates[0].gates[1].max_attempts = Some(1);
                profile.gates[0].gates[0].proof = serde_json::from_value(serde_json::json!({
                    "scheme": {"type": "object", "properties": {"tasks": {"type": "object"}}}
                }))
                .unwrap();
                h.commit_profile(&profile, "exhausted blueprint fixture");
                fs::write(h.root().join("root-proof.json"), r#"{"tasks":{"first":{"layer":"obsolete","summary":"old","blackboard_decision_id":"old"}}}"#).unwrap();
                let error =
                    run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
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
                let temporary =
                    acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
                drop(std::mem::replace(&mut h.task.store, temporary));
                h.task.store =
                    contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
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

                continue;
            }
            Recovery::Partial { retry } => h.partial_pre_task_checks(retry),
            _ => h.pre_task_checks(!matches!(case, Recovery::BypassReplay)),
        }
        if matches!(case, Recovery::BypassReplay) {
            let mut profile = h.read_profile();
            profile.gates[0].on_success = Some("conditional".into());
            for (index,gate) in [
                serde_json::json!({"id":"bypassed","command":"preflight","proof":"none"}),
                serde_json::json!({"id":"conditional","when":"doc","command":"preflight","proof":"none"}),
                serde_json::json!({"id":"required","command":"preflight","proof":"none"}),
            ].into_iter().enumerate(){profile.gates.insert(index+1,serde_json::from_value(gate).unwrap());}
            h.commit_profile(&profile, "persisted bypass route");
            assert!(format!("{:#}", h.task.claim_next().unwrap_err())
                .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
            fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
        }
        let original = fs::read(h.root().join("src/lib.rs")).unwrap();
        let initial = candidate_fingerprint(h.root()).unwrap();
        let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        if !matches!(case, Recovery::BypassReplay) {
            assert!(
                first
                    .to_string()
                    .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
                "{first:#}"
            );
        }
        match case {
            Recovery::Partial { retry } => {
                assert_ne!(candidate_fingerprint(h.root()).unwrap(), initial);
                if retry {
                    assert_eq!(
                        fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
                        "seenseenseen"
                    );
                }
                fs::write(h.root().join("src/lib.rs"), &original).unwrap();
                if retry {
                    fs::remove_file(h.root().join(".acdd/preflight-retry-done")).unwrap();
                }
                assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
            }
            Recovery::Changed { committed } => {
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
            }
            Recovery::CorruptChain => {
                h.task.store.connection.execute("UPDATE gate_transitions SET proof_payload=json_set(proof_payload, '$.previous_observation', id) WHERE id=(SELECT max(id) FROM gate_transitions WHERE proof_kind='orchestrator')",[]).unwrap();
            }
            Recovery::BypassReplay => {
                let temporary =
                    acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
                drop(std::mem::replace(&mut h.task.store, temporary));
                h.task.store =
                    contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
                let before:Vec<(String,String)>=h.task.store.connection.prepare("SELECT gate_path,action FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('preflight','bypassed','conditional','required') ORDER BY id").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?))).unwrap().map(Result::unwrap).collect();
                assert_eq!(
                    before,
                    [
                        ("preflight".into(), "pass".into()),
                        ("required".into(), "pass".into())
                    ]
                );
                assert_eq!(
                    h.task.store.inspect(&h.task.id).unwrap().status,
                    "completed"
                );
            }
            _ => {}
        }
        for _ in 0..2 {
            let resumed =
                run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
            if matches!(case, Recovery::CorruptChain) {
                assert!(
                    resumed.to_string().contains("MILESTONE_CHECKPOINT_INVALID"),
                    "{resumed:#}"
                );
            } else if matches!(case, Recovery::BypassReplay) {
                let skipped:i64=h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')",[],|row|row.get(0)).unwrap();
                assert_eq!(skipped, 0);
                continue;
            } else {
                assert!(
                    resumed
                        .to_string()
                        .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
                    "{resumed:#}"
                );
            }
            assert_eq!(
                fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
                if matches!(case, Recovery::Changed { .. }) {
                    "seenseen"
                } else {
                    "seen"
                }
            );
            if let Recovery::Partial { retry } = case {
                assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
                assert_eq!(
                    fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
                    if retry {
                        "seenseenseenseenseenseen"
                    } else {
                        "seenseen"
                    }
                );
            }
        }
    }
}

#[test]
fn post_task_resume_matrix() {
    use acdd_tasks::engine::orchestrator::{types::OrchestratorEvent, waves::run_milestone_waves};
    let mut h = harness::WaveHarness::with_minimal_task_gates(true, false);
    let baseline = h.checkpoint();
    let mut delivered_baseline = None;
    let stop = h.root().join(".acdd/stop-wave-review");
    enum Resume {
        PreflightRefresh,
        ChangedContent,
        Unchanged,
    }
    for case in [
        Resume::PreflightRefresh,
        Resume::ChangedContent,
        Resume::Unchanged,
    ] {
        if matches!(case, Resume::Unchanged) {
            h.restore(delivered_baseline.as_ref().unwrap());
        } else {
            h.restore(&baseline);
            h.options.target_base = crate::acdd::support::git_head(h.root());
            if matches!(case, Resume::PreflightRefresh) {
                h.pre_task_checks(false);
            }
            fs::write(&stop, "").unwrap();
            let error =
                run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
            assert!(
                error.to_string().contains("WORKFLOW_RUNNER_FAILED"),
                "{error:#}"
            );
            let delivered = h.task.store.inspect(&h.task.id).unwrap();
            assert_eq!(delivered.status, "completed");
            assert!(delivered.receipt.is_some());
            let claims: u64 = h
                .task
                .store
                .connection
                .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
                .unwrap();
            assert!(claims > 0);
            if matches!(case, Resume::ChangedContent) {
                delivered_baseline = Some(h.checkpoint());
            }
        }
        if matches!(case, Resume::ChangedContent) {
            fs::write(
                h.root().join("src/lib.rs"),
                "pub fn example() { /* polish */ }\n",
            )
            .unwrap();
            for args in [
                vec!["add", "src/lib.rs"],
                vec!["commit", "-m", "fixture polish"],
            ] {
                assert!(git_command(h.root(), &args).status().unwrap().success());
            }
        }
        fs::remove_file(&stop).unwrap();
        let mut fresh_claims = Vec::new();
        let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
            if let OrchestratorEvent::TaskClaimed { task_id, .. } = event {
                fresh_claims.push(task_id);
            }
        });
        assert!(resumed.is_ok(), "{resumed:#?}");
        assert!(fresh_claims.is_empty(), "{fresh_claims:?}");
        let contexts: Vec<Value> = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        match case {
            Resume::PreflightRefresh => assert_eq!(
                fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
                "seenseen"
            ),
            Resume::ChangedContent => {
                let polish: Vec<_> = contexts
                    .iter()
                    .filter(|row| row["gate"] == "polish")
                    .collect();
                assert_eq!(polish.len(), 2);
                assert_ne!(
                    polish[0]["subject_context"]["binding"],
                    polish[1]["subject_context"]["binding"]
                );
            }
            Resume::Unchanged => assert_eq!(
                contexts
                    .iter()
                    .map(|row| row["gate"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["polish", "milestone_review", "milestone_review"]
            ),
        }
        assert!(h
            .root()
            .join("docs/milestones/archive/011-wave.md")
            .exists());
    }
}
