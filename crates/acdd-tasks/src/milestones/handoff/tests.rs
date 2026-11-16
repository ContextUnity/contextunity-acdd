use super::*;
mod harness;
use harness::{NativeHandoffHarness, ACTIVE, ARCHIVE};

#[test]
fn receipt_gate_history_survives_profile_evolution_and_requires_anchors() {
    let fixture = NativeHandoffHarness::new().unwrap();
    let id = fixture.milestone.task_id(&fixture.milestone.tasks[0]);
    fixture.write_shared_value(1).unwrap();
    fixture.complete_task(&id).unwrap();
    let store = fixture.store().unwrap();
    let task = store.inspect(&id).unwrap();
    let profile = crate::tasks::profile::load_for_workspace(fixture.root()).unwrap();
    let mut profile =
        crate::engine::tasks::claim::flattened_profile(&profile, Some(&task)).unwrap();
    profile.gates.retain(|gate| gate.id != "contract_review");
    let review = profile
        .gates
        .iter()
        .find(|gate| gate.id == "review")
        .unwrap()
        .clone();
    let build = profile
        .gates
        .iter()
        .position(|gate| gate.id == "build")
        .unwrap();
    let mut added = review;
    added.id = "contract_review".into();
    profile.gates.insert(build, added);
    let cases = [
        (
            Some(vec!["contract", "build", "review", "deliver"]),
            None,
            None,
            true,
        ),
        (
            Some(vec![
                "contract",
                "contract_review",
                "build",
                "review",
                "deliver",
            ]),
            None,
            None,
            false,
        ),
        (Some(vec!["build", "review", "deliver"]), None, None, false),
        (
            Some(vec!["contract", "review", "deliver"]),
            None,
            None,
            false,
        ),
        (
            Some(vec!["contract", "build", "deliver"]),
            None,
            None,
            false,
        ),
        (None, None, None, true),
        (None, Some("contract"), None, false),
        (None, Some("build"), None, false),
        (None, Some("review"), None, false),
        (None, Some("deliver"), None, false),
        (None, None, Some(("review", 0)), true),
        (
            Some(vec!["contract", "build", "review", "deliver"]),
            None,
            Some(("review", 0)),
            true,
        ),
        (
            Some(vec!["contract", "build", "review", "deliver"]),
            None,
            Some(("contract_review", task.claim_revision + 1)),
            false,
        ),
        (
            None,
            None,
            Some(("contract_review", task.claim_revision + 1)),
            false,
        ),
    ];
    assert!(task
        .receipt
        .as_ref()
        .unwrap()
        .gates
        .as_ref()
        .unwrap()
        .contains(&"contract_review".to_owned()));
    store
        .connection
        .execute(
            "DELETE FROM task_gates WHERE task_id=?1 AND gate='contract_review'",
            [&id],
        )
        .unwrap();
    for (recorded, missing, rejected, succeeds) in cases {
        store
            .connection
            .execute_batch("SAVEPOINT receipt_case")
            .unwrap();
        if let Some(gate) = missing {
            store
                .connection
                .execute(
                    "DELETE FROM task_gates WHERE task_id=?1 AND gate=?2",
                    rusqlite::params![id, gate],
                )
                .unwrap();
        }
        if let Some((gate, revision)) = rejected {
            store.connection.execute(
                "INSERT INTO task_gates (task_id, gate, revision, state, evidence) VALUES (?1, ?2, ?3, 'rejected', NULL)",
                rusqlite::params![id, gate, revision],
            ).unwrap();
        }
        let recorded = recorded.map(|ids| ids.into_iter().map(str::to_owned).collect::<Vec<_>>());
        let result = accepted_gate_evidence(
            fixture.root(),
            &store,
            &id,
            task.contract_revision,
            &task.spec.proof_policy,
            &profile,
            recorded.as_deref(),
        );
        if succeeds {
            result.unwrap();
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("TASK_RECEIPT_INVALID"));
        }
        store
            .connection
            .execute_batch("ROLLBACK TO receipt_case; RELEASE receipt_case")
            .unwrap();
    }
}

#[test]
fn handoff_and_retry_ignore_foreign_profile_stage() {
    let mut fixture = NativeHandoffHarness::new().unwrap();
    let config = fixture.root().join("acdd.yaml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{contents}handoff_pruning: false\n")).unwrap();
    for args in [
        vec!["add", "acdd.yaml"],
        vec!["commit", "-m", "retain handoff history"],
    ] {
        assert!(crate::core::git::git_command(fixture.root(), &args)
            .unwrap()
            .status
            .success());
    }
    fixture.base = fixture.head().unwrap();
    for (index, spec) in fixture.milestone.tasks.iter().enumerate() {
        fixture.write_shared_value(index + 1).unwrap();
        fixture
            .complete_task(&fixture.milestone.task_id(spec))
            .unwrap();
    }
    let store = fixture.store().unwrap();
    let mut foreign = store.list(None, "all", None).unwrap()[0].clone();
    // A target task with a mismatched path must still prevent handoff: identity,
    // not the mutable manifest path, is the ownership boundary.
    let mut rogue = foreign.clone();
    rogue.task_id = "forge-mcp/m-native-shared:rogue".into();
    rogue.milestone_ref = "docs/milestones/elsewhere.md".into();
    rogue.status = "in_progress".into();
    rogue.receipt = None;
    store
        .connection
        .execute(
            "INSERT INTO tasks(task_id,milestone_ref,descriptor) VALUES(?1,?2,?3)",
            rusqlite::params![
                rogue.task_id,
                rogue.milestone_ref,
                serde_json::to_string(&rogue).unwrap()
            ],
        )
        .unwrap();
    let error = handoff_context(fixture.root(), "m-native-shared", &fixture.base).unwrap_err();
    assert!(error.to_string().contains("incomplete tasks"), "{error:#}");
    store
        .connection
        .execute("DELETE FROM tasks WHERE task_id=?1", [&rogue.task_id])
        .unwrap();
    foreign.task_id = "forge-mcp/foreign-milestone:task".into();
    foreign.milestone_ref = "docs/milestones/foreign.md".into();
    foreign.stage = "foreign_profile_only_stage".into();
    foreign.status = "in_progress".into();
    foreign.authority_snapshot = None;
    foreign.receipt = None;
    let descriptor = serde_json::to_string(&foreign).unwrap();
    store
        .connection
        .execute(
            "INSERT INTO tasks(task_id,milestone_ref,descriptor) VALUES(?1,?2,?3)",
            rusqlite::params![foreign.task_id, foreign.milestone_ref, descriptor],
        )
        .unwrap();
    drop(store);
    let root = fixture.root();
    let context = handoff_context(root, "m-native-shared", &fixture.base).unwrap();
    let observed = execute_milestone_verification(root, "m-native-shared", &fixture.base).unwrap();
    let evidence = NativeHandoffHarness::evidence(
        &context,
        &fixture.base,
        NativeHandoffHarness::observed_verification(&observed["verification"]).unwrap(),
    )
    .unwrap();
    // Abort the actual prune after journal creation, then resume with the same
    // foreign task still present. This reaches journal replay, not just an
    // already-archived idempotent retry.
    let store = fixture.store().unwrap();
    store.connection.execute_batch(
        "CREATE TRIGGER stop_target_prune BEFORE UPDATE OF milestone_ref ON tasks \
         WHEN substr(OLD.task_id,1,length('forge-mcp/m-native-shared:')) = 'forge-mcp/m-native-shared:' \
              AND NEW.milestone_ref = 'docs/milestones/archive/010-native-shared.md' \
         BEGIN SELECT RAISE(ABORT, 'injected prune failure'); END;",
    ).unwrap();
    drop(store);
    let error = handoff(root, "m-native-shared", None, &evidence).unwrap_err();
    assert!(format!("{error:#}").contains("injected prune failure"));
    assert!(journal_path(root, &evidence.binding).unwrap().exists());
    let store = fixture.store().unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER stop_target_prune;")
        .unwrap();
    drop(store);
    handoff(root, "m-native-shared", None, &evidence).unwrap();
    handoff(root, "m-native-shared", None, &evidence).unwrap();
    let store = fixture.store().unwrap();
    let retained: String = store
        .connection
        .query_row(
            "SELECT descriptor FROM tasks WHERE task_id=?1",
            [&foreign.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, descriptor);
}

#[test]
fn handoff_accepts_completed_reviewed_shared_scope() {
    // Direct calls retain the native public handoff seam in the static index.
    let fixture = NativeHandoffHarness::new().unwrap();
    let root = fixture.root();
    let base = fixture.base.as_str();
    let mut landings = Vec::new();
    for (index, spec) in fixture.milestone.tasks.iter().enumerate() {
        let id = fixture.milestone.task_id(spec);
        fixture.write_shared_value(index + 1).unwrap();
        fixture.complete_task(&id).unwrap();
        landings.push((id, fixture.head().unwrap()));
    }
    let before = fixture.file_bytes(ACTIVE).unwrap();
    let context = handoff_context(root, "m-native-shared", base).unwrap();
    let evidence = NativeHandoffHarness::evidence(
        &context,
        base,
        NativeHandoffHarness::unobserved_verification("cargo test --test acdd", "check_full_repo"),
    )
    .unwrap();
    let missing_trace = handoff(root, "m-native-shared", None, &evidence).unwrap_err();
    assert!(missing_trace
        .to_string()
        .contains("HANDOFF_VERIFICATION_REQUIRED"));
    let observed = execute_milestone_verification(root, "m-native-shared", base).unwrap();
    let check = &observed["verification"];
    let evidence = NativeHandoffHarness::evidence(
        &context,
        base,
        NativeHandoffHarness::observed_verification(check).unwrap(),
    )
    .unwrap();
    let failed_check = fixture.fail_verification(&observed).unwrap();
    assert_eq!(failed_check.exit_code, Some(1));
    assert!(!failed_check.passed);
    let stale_success = handoff(root, "m-native-shared", None, &evidence).unwrap_err();
    assert!(stale_success
        .to_string()
        .contains("HANDOFF_VERIFICATION_REQUIRED"));

    let observed = execute_milestone_verification(root, "m-native-shared", base).unwrap();
    let check = &observed["verification"];
    assert_eq!(check["exit_code"], 0);

    // Changing code after verification creates a new candidate commit that must be re-verified.
    fixture.write_shared_value(42).unwrap();
    fixture
        .commit_shared_change("unverified edit after passing verification")
        .unwrap();
    let new_context = handoff_context(root, "m-native-shared", base).unwrap();
    let unverified_evidence = NativeHandoffHarness::evidence(
        &new_context,
        base,
        NativeHandoffHarness::unobserved_verification(
            check["command"].as_str().unwrap(),
            check["command_id"].as_str().unwrap(),
        ),
    )
    .unwrap();
    let candidate_mismatch_err =
        handoff(root, "m-native-shared", None, &unverified_evidence).unwrap_err();
    assert!(candidate_mismatch_err
        .to_string()
        .contains("HANDOFF_VERIFICATION_REQUIRED"));

    // Once verified on the actual current candidate, handoff succeeds.
    let observed_current = execute_milestone_verification(root, "m-native-shared", base).unwrap();
    assert_eq!(observed_current["status"], "passed", "{observed_current}");
    let check_current = &observed_current["verification"];
    assert_eq!(check_current["exit_code"], 0);
    let evidence = NativeHandoffHarness::evidence(
        &new_context,
        base,
        NativeHandoffHarness::observed_verification(check_current).unwrap(),
    )
    .unwrap();
    let result = handoff(root, "m-native-shared", None, &evidence);
    if result.is_err() {
        assert_eq!(fixture.file_bytes(ACTIVE).unwrap(), before);
        assert!(!fixture.exists(ARCHIVE));
    }
    let outcome =
        result.expect("a later completed reviewed task may change an earlier task's shared path");
    let verification_passport = &outcome["handoff"]["passport"]["automatic_project_verification"];
    assert_eq!(verification_passport["reused_verification"], true);
    assert_eq!(
        verification_passport["stdout_digest"].as_str(),
        check_current["stdout_sha256"].as_str()
    );
    let reopened = fixture.store().unwrap();
    let archived_doc = fixture.document(ARCHIVE).unwrap();
    let parsed = crate::core::tasks::Milestone::parse(&archived_doc, "forge-mcp").unwrap();
    assert!(reopened.list(None, "all", None).unwrap().is_empty());
    for table in [
        "gate_checkpoints",
        "gate_execution_traces",
        "gate_transitions",
    ] {
        let count = fixture.telemetry_rows(table).unwrap();
        assert_eq!(count, 0, "{table} should be pruned at handoff");
    }
    for (index, (_, landed)) in landings.iter().enumerate() {
        assert_eq!(
            parsed.tasks[index]
                .receipt
                .as_ref()
                .unwrap()
                .commit
                .as_deref(),
            Some(landed.as_str())
        );
    }
    assert!(!fixture.exists(ACTIVE));
}
