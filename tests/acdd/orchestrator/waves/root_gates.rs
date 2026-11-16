use super::harness;
use super::root_fixture::RootGateHarness;
use crate::acdd::support::git_command;
use crate::acdd::support::git_head;
use contextunity_acdd::acdd_tasks::tasks::profile;
use std::fs;

#[test]
fn task_claim_fails_closed_until_pre_task_root_gates_pass() {
    let mut h = RootGateHarness::with_dependencies();
    h.pre_task_checks(false);

    let error = h.task.claim_next().unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{message}"
    );
    assert!(message.contains("preflight"), "{message}");
    assert!(message.contains("gate_path"), "{message}");
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
}

#[test]
fn task_next_surfaces_pre_task_root_gates_before_wave_tasks() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = RootGateHarness::with_dependencies();
    h.pre_task_checks(false);
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
    assert_eq!(action["stage"], "preflight");
    assert_eq!(action["active_stage_id"], "preflight");
    assert_eq!(action["gate"]["command"], "preflight");
    let task = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(task.status, "ready");
    assert!(
        task.worker_id.is_none(),
        "root gate dispatch claimed a wave task"
    );
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_claims WHERE task_id=?1",
            [&task.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 0, "root gate dispatch persisted a wave claim");
}

#[test]
fn task_next_keeps_the_barrier_closed_after_a_pre_task_gate_exhausts() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = RootGateHarness::with_dependencies();
    h.pre_task_checks(true);
    let options = h.run_options();
    let run_error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{run_error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{run_error:#}"
    );

    let before = h.task.store.inspect(&h.task.id).unwrap();
    let error = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );

    let after = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(after.status, "ready");
    assert_eq!(after.claim_revision, before.claim_revision);
    assert!(after.worker_id.is_none());
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_claims WHERE task_id=?1",
            [&h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        claims, 0,
        "an exhausted root barrier persisted a task claim"
    );
}

#[test]
fn pre_task_root_transitions_are_persisted_for_the_milestone_subject() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = RootGateHarness::with_dependencies();
    h.pre_task_checks(true);
    let options = h.run_options();
    let error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{error:#}"
    );

    h.task
        .store
        .connection
        .execute(
            "UPDATE gate_transitions SET proof_kind='blueprint' WHERE gate_path='preflight' AND action='pass'",
            [],
        )
        .unwrap();

    let claim_error = h.task.claim_next().unwrap_err();
    let claim_message = format!("{claim_error:#}");
    assert!(
        claim_message.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{claim_message}"
    );
    assert!(
        claim_message.contains("stop_before_tasks"),
        "{claim_message}"
    );

    let (milestone_id, subject): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT milestone_id, subject FROM gate_transitions WHERE gate_path='preflight' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!milestone_id.is_empty());
    assert_eq!(subject, "milestone");
}

#[test]
fn task_next_surfaces_post_task_root_gates_after_all_tasks_complete() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = RootGateHarness::new();
    h.task.advance_to_completion().unwrap();

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
    assert_eq!(action["stage"], "polish");
    assert_eq!(action["active_stage_id"], "polish");
    assert_eq!(action["role"], "wave_role");
    assert_eq!(action["workflow_guidance"]["role"], "wave_role");
    assert_eq!(
        action["workflow_guidance"]["role_guide"],
        "Exercise the real task gate protocol."
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
}

fn install_post_task_route_profile(h: &RootGateHarness) {
    let path = h.root().join(".acdd/profile.yaml");
    let mut profile =
        profile::validate_profile_document(&fs::read_to_string(&path).unwrap()).unwrap();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let mut verify = profile
        .gates
        .iter()
        .find(|gate| gate.id == "verify")
        .unwrap()
        .clone();
    verify.on_success = Some("documentation".into());
    verify.on_fail = Some("polish".into());
    verify.max_attempts = Some(2);
    let mut polish = profile
        .gates
        .iter()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .clone();
    polish.on_success = Some("milestone_review".into());
    let mut milestone_review = profile
        .gates
        .iter()
        .find(|gate| gate.id == "milestone_review")
        .unwrap()
        .clone();
    milestone_review.on_success = Some("verify".into());
    let documentation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "documentation",
        "role": "wave_role",
        "proof": "none",
        "pass": "handoff"
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![
        task_container,
        verify,
        polish,
        milestone_review,
        documentation,
        handoff,
    ];
    profile.validate().unwrap();
    fs::write(
        path,
        format!(
            "inherit: none\n{}",
            serde_yaml::to_string(&profile).unwrap()
        ),
    )
    .unwrap();
}

fn record_milestone_root_transition(
    h: &RootGateHarness,
    gate_path: &str,
    action: &str,
    attempt: u32,
) {
    let manifest = fs::read_to_string(h.root().join(harness::MANIFEST)).unwrap();
    let milestone =
        contextunity_acdd::acdd_tasks::tasks::Milestone::parse(&manifest, "acdd").unwrap();
    let milestone_identity = format!(
        "{}/{}/{}",
        milestone.repository, milestone.project, milestone.id
    );
    let payload = serde_json::json!({"attempt":attempt,"findings":null});
    h.task
        .store
        .connection
        .execute(
            "INSERT INTO gate_transitions (
                milestone_id, subject, gate_path, action, proof_kind, proof_payload,
                git_tree_sha, worker_id
            ) VALUES (?1, 'milestone', ?2, ?3, 'orchestrator', ?4, ?5, 'root-route-test')",
            rusqlite::params![
                milestone_identity,
                gate_path,
                action,
                payload.to_string(),
                git_head(h.root())
            ],
        )
        .unwrap();
}

#[test]
fn task_next_follows_verify_pass_edge_to_documentation() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = RootGateHarness::new();
    h.task.advance_to_completion().unwrap();
    install_post_task_route_profile(&h);
    record_milestone_root_transition(&h, "verify", "pass", 1);

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
    assert_eq!(action["stage"], "documentation");
    assert_eq!(action["gate"]["id"], "documentation");
}

#[test]
fn pre_task_missing_reachable_gate_keeps_claim_blocked() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = RootGateHarness::new();
    h.pre_task_checks(true);
    let options = h.run_options();
    let outcome = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {});
    assert!(outcome.is_err());
    let error = h.task.claim_next().unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );
}

#[test]
fn root_command_verify_blocks_unobserved_pass_and_bypassed_polish_allows_independent_milestone_review(
) {
    use crate::acdd::support::passing_review_proof;
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };

    let mut h = RootGateHarness::new();
    let mut profile = h.read_profile();
    profile.commands.insert(
        "root-verify-pass".into(),
        serde_json::from_value(serde_json::json!({
            "command": "true",
            "output": { "require_counts": false }
        }))
        .unwrap(),
    );
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let verify: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "verify",
        "command": "root-verify-pass",
        "proof": "command",
        "pass": "milestone_review",
        "fail": "polish"
    }))
    .unwrap();
    let polish: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "polish",
        "role": "wave_role",
        "proof": "none",
        "pass": "milestone_review"
    }))
    .unwrap();
    let milestone_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "milestone_review",
        "role": "wave_role",
        "proof": "review",
        "independent_from": "polish"
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![task_container, verify, polish, milestone_review, handoff];
    h.commit_profile(&profile, "verified review root gate profile");
    h.task.advance_to_completion().unwrap();
    let milestone_id = "m-wave".to_owned();
    let worktree = h.root().to_string_lossy().into_owned();

    let verify_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(verify_claim["stage"], "verify");

    let unobserved_err = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "command_proof": {
                        "command": "root-verify-pass",
                        "exit_code": 0
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        unobserved_err.contains("TASK_PROOF_UNOBSERVED"),
        "{unobserved_err}"
    );

    let verified = manage(
        h.root(),
        Manage {
            action: ManageAction::Verify,
            task_id: Some(milestone_id.clone()),
            worktree: Some(worktree.clone()),
            worker_id: Some("architect-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(verified["passed"], true);

    let verify_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": verified["proof"].clone()
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(verify_submit["action"], "pass");

    // Passing `verify` routes directly to `milestone_review`, bypassing `polish`,
    // so `independent_from: polish` must still allow claiming `milestone_review`.
    let review_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "milestone_review".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_claim["stage"], "milestone_review");

    let review_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "milestone_review".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": passing_review_proof()
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_submit["action"], "pass");
}

#[test]
fn task_pinned_inherit_none_profile_cannot_bypass_milestone_root_barriers() {
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };
    use sha2::Digest;

    let h = RootGateHarness::new();
    let milestone_yaml = "\
inherit: none
version: 2
roles:
  actor: {mode: subagent}
gates:
  - id: preparation
    role: actor
    proof:
      scheme:
        type: object
        required: [tasks]
        properties:
          tasks:
            type: object
  - tasks:
      gates:
        - id: build
          role: actor
          proof: none
        - id: deliver
          role: actor
          proof: delivery
          receipt: true
          auto_commit: false
  - id: handoff
    receipt: true
    proof: delivery
";
    let task_yaml = "\
inherit: none
version: 2
roles:
  actor: {mode: subagent}
gates:
  - tasks:
      gates:
        - id: build
          role: actor
          proof: none
        - id: deliver
          role: actor
          proof: delivery
          receipt: true
          auto_commit: false
";
    let pin = |path: &str, yaml: &str| {
        let digest = format!("{:x}", sha2::Sha256::digest(yaml.as_bytes()));
        format!("{path}:{}", &digest[..12])
    };
    let milestone_pin = pin("profiles/milestone.yaml", milestone_yaml);
    let task_pin = pin("profiles/task.yaml", task_yaml);
    fs::create_dir_all(h.root().join("profiles")).unwrap();
    fs::write(h.root().join("profiles/milestone.yaml"), milestone_yaml).unwrap();
    fs::write(h.root().join("profiles/task.yaml"), task_yaml).unwrap();

    let manifest_rel = "docs/milestones/001-pinned-barrier.md";
    let spec = format!(
        "---\nid: m-pinned-barrier\ntitle: Pinned Barrier\ndoc_type: contract\nstatus: active\nacdd_profile: \"{milestone_pin}\"\n---\n# Pinned Barrier\n```yaml\ntask_ref: t1\ntarget: Task with inherit none pin\nacdd_profile: \"{task_pin}\"\nproof_policy: seam-test-first\nscope: [src/]\n```\n"
    );
    fs::write(h.root().join(manifest_rel), spec).unwrap();
    fs::remove_file(h.root().join(harness::MANIFEST)).unwrap();
    fs::remove_file(h.root().join(".acdd/tasks.sqlite")).unwrap();
    let cfg = fs::read_to_string(h.root().join("acdd.yaml")).unwrap();
    let cleaned_cfg = cfg
        .lines()
        .filter(|line| !line.starts_with("acdd_profile:"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(h.root().join("acdd.yaml"), format!("{cleaned_cfg}\n")).unwrap();
    for args in [
        vec!["rm", harness::MANIFEST],
        vec!["add", "acdd.yaml", "profiles", manifest_rel],
        vec!["commit", "-m", "pinned milestone and task profiles"],
    ] {
        assert!(git_command(h.root(), &args).status().unwrap().success());
    }

    let synced = manage(
        h.root(),
        Manage {
            action: ManageAction::Sync,
            milestone_ref: Some(manifest_rel.into()),
            ..Default::default()
        },
    )
    .unwrap();
    let task_id = synced["tasks"][0]["task_id"].as_str().unwrap().to_owned();
    let worktree = h.root().to_string_lossy().into_owned();

    let next_before = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("coord-1".into()),
            worktree: Some(worktree.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(next_before["subject"], "milestone");
    assert_eq!(next_before["stage"], "preparation");

    let early_claim_err = claim(
        h.root(),
        Claim {
            task_id: task_id.clone(),
            stage: "build".into(),
            worker_id: "worker-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        early_claim_err.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{early_claim_err}"
    );

    claim(
        h.root(),
        Claim {
            task_id: "m-pinned-barrier".into(),
            stage: "preparation".into(),
            worker_id: "coord-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: "m-pinned-barrier".into(),
            stage: "preparation".into(),
            evidence: serde_json::json!({
                "worker_id": "coord-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            "t1": { "summary": "pinned blueprint slice" }
                        }
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    let allowed = claim(
        h.root(),
        Claim {
            task_id,
            stage: "build".into(),
            worker_id: "worker-1".into(),
            worktree,
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(allowed["stage"], "build");
    let inherited = allowed["context_bundle"]["inherited_proofs"]
        .as_array()
        .unwrap();
    assert_eq!(inherited.len(), 1, "{allowed:?}");
    assert_eq!(inherited[0]["gate_path"], "preparation");
    assert_eq!(inherited[0]["proof"]["summary"], "pinned blueprint slice");
}
