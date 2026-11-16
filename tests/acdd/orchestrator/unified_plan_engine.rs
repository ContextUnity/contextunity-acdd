use crate::acdd::{
    harness::plans::{PlanFixture, PlanHarness},
    support::*,
};
use acdd_tasks::{
    engine::orchestrator::{
        plan::{run_plan_workflow, PlanDocument},
        task_runner::{run_task_lifecycle, TaskRunOptions},
    },
    tasks::profile,
};
use std::{fs, path::Path, time::Duration};

fn read_plan(root: &Path, path: &str) -> PlanDocument {
    let source = fs::read_to_string(root.join(path)).unwrap();
    let sections = contextunity_acdd::engine::docs::extract_with_details(path, &source, 0.0)
        .unwrap()
        .into_iter()
        .map(|(section, _)| section)
        .collect::<Vec<_>>();
    PlanDocument::from_sections(Path::new(path), source, &sections).unwrap()
}

fn run_options(root: &Path) -> TaskRunOptions {
    TaskRunOptions {
        worktree: root.to_owned(),
        worker_id: "plan-coordinator".into(),
        session_fingerprint: "plan-contract-session".into(),
        runner_timeout: Duration::from_secs(10),
    }
}

fn plan_rows(root: &Path, path: &str) -> Vec<acdd_tasks::store::Task> {
    tasks::store(root)
        .unwrap()
        .list(Some(path), "all", None)
        .unwrap()
}

#[test]
fn plan_immutable_drift_warns_and_requires_rebinding_verification_and_independent_review() {
    use crate::acdd::{support::tasks, tasks::harness::TaskHarness};

    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let original = fs::read_to_string(root.path(harness.path)).unwrap();
    for (path, selector, snapshot) in [
        (harness.path, "linear", "sha_snapshot: true"),
        (
            "docs/plans/ordinary.md",
            "ordinary",
            "save: { post: snapshot }",
        ),
    ] {
        root.write(path, &original);
        root.write("src/first.rs", "initial\n");
        let _ = fs::remove_file(root.path("proof-order.log"));
        root.write(
            "profiles/rebind.yaml",
            &r#"
version: 2
inherit: none
save: snapshot
roles:
  independent_reviewer:
    runner: "exit 23"
commands:
  prepare:
    command: "printf 'prepare\\n' >> proof-order.log"
    output: { require_counts: false }
  verify:
    command: "printf 'verify\\n' >> proof-order.log"
    output: { require_counts: false }
gates:
  - tasks:
      gates:
        - id: prepare
          command: prepare
          sha_snapshot: true
        - id: verify
          command: verify
        - id: review
          role: independent_reviewer
          proof: none
          independent_from: prepare
          save: { immutable: true }
        - id: finish
          receipt: true
          save: snapshot
"#
            .replace("sha_snapshot: true", snapshot),
        );
        let head = git_head(root.root());
        let index = git_command(root.root(), &["ls-files", "--stage"])
            .output()
            .unwrap()
            .stdout;
        // Stop the unavailable external reviewer after real build/verification.
        let stopped = TaskHarness::run_cli_for_root(
            root.root(),
            &["--profile", "profiles/rebind.yaml", "plan", "run", selector],
        );
        assert!(!stopped.status.success());
        let rows = tasks::store(root.root())
            .unwrap()
            .list(Some(path), "all", None)
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "immutable Plan recovery requires canonical SQLite task rows"
        );
        let first = rows.iter().find(|row| row.stage == "review").unwrap();
        let mut task = TaskHarness::attach(&root, &first.task_id);
        if first.status == "in_progress" {
            task.store
                .release_failed_claim(&first.task_id, first.claim_revision)
                .unwrap();
        }
        let prior = task
            .store
            .build_snapshot_candidate(&task.id)
            .unwrap()
            .unwrap()
            .0;
        let claimed = task.claim_next().unwrap();
        let proof = task.evidence_for_gate(&claimed).unwrap();
        let source = fs::read(root.path(path)).unwrap();
        root.write("src/first.rs", "changed-again\n");
        let response = task
            .submit_evidence(&proof)
            .expect("ordinary immutable Plan drift must be recoverable");
        let warning = &response["candidate_drift_warning"];
        assert!(warning.is_object(), "{response}");
        assert!(warning["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("src/first.rs")));
        assert_ne!(warning["prior_binding"], warning["current_binding"]);
        assert_eq!(warning["needs_verification"], true);
        assert_eq!(warning["needs_independent_review"], true);
        let pending = task.store.inspect(&task.id).unwrap();
        assert_ne!(pending.stage, "finish");
        assert!(pending.receipt.is_none());
        assert_eq!(fs::read(root.path(path)).unwrap(), source);
        task.store = tasks::store_for_task(root.root(), &task.id).unwrap();
        assert!(
            task.submit_evidence(&proof).is_err(),
            "stale review cannot discharge a rebound obligation"
        );
        task.advance_to("review").unwrap();
        let rebound = task
            .store
            .build_snapshot_candidate(&task.id)
            .unwrap()
            .unwrap()
            .0;
        assert_ne!(prior, rebound);
        assert_eq!(
            fs::read_to_string(root.path("proof-order.log")).unwrap(),
            "prepare\nverify\nprepare\nverify\n"
        );
        let builder = task.persisted_gate_evidence("prepare")["worker_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let error = task
            .claim_gate_in_session("review", &builder, Some("reused-builder-session"))
            .unwrap_err();
        assert!(format!("{error:#}").contains("TASK_REVIEW_NOT_INDEPENDENT"));
        task.advance_to("finish").unwrap();
        let reviewed = task.persisted_gate_evidence("review");
        assert_eq!(reviewed["commit"], rebound);
        task.advance_to_completion().unwrap();
        assert!(task.store.inspect(&task.id).unwrap().receipt.is_some());
        assert!(fs::read_to_string(root.path(path))
            .unwrap()
            .contains("**Status**: completed"));
        assert_eq!(git_head(root.root()), head);
        assert_eq!(
            git_command(root.root(), &["ls-files", "--stage"])
                .output()
                .unwrap()
                .stdout,
            index
        );
        assert_eq!(
            fs::read_to_string(root.path("src/first.rs")).unwrap(),
            "changed-again\n"
        );
    }
}

#[test]
fn plan_steps_persist_in_tasks_store_and_resume_without_command_replay() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    let head = git_head(root.root());
    let plan = read_plan(root.root(), harness.path);
    let first = run_plan_workflow(root.root(), plan, &profile, true).unwrap();
    assert_eq!(first["status"], "completed");
    let store = tasks::store(root.root()).unwrap();
    let rows: Vec<String> = store
        .connection
        .prepare("SELECT task_id FROM tasks WHERE milestone_ref=?1 ORDER BY task_id")
        .unwrap()
        .query_map([harness.path], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "Plan steps must use the canonical task registry: {rows:?}"
    );
    for id in &rows {
        let task = store.inspect(id).unwrap();
        assert_eq!(task.status, "completed");
        assert!(task.receipt.is_some(), "SQLite owns the snapshot receipt");
        assert!(store
            .task_gates(id)
            .unwrap()
            .iter()
            .any(|gate| gate.2 == "passed"));
        assert_eq!(
            run_task_lifecycle(root.root(), id, &run_options(root.root()), &mut |_| {}).unwrap()
                ["status"],
            "completed"
        );
    }
    drop(store);
    let plan = read_plan(root.root(), harness.path);
    assert_eq!(
        run_plan_workflow(root.root(), plan, &profile, true).unwrap()["status"],
        "completed"
    );
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\nstep\n"
    );
    assert_eq!(git_head(root.root()), head);
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn blocked_plan_delivery_retains_real_subtask_lifecycle_state() {
    let harness = PlanHarness::fixture(PlanFixture::PendingSubtask);
    let root = harness.materialize(harness.path);
    let profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    let plan = read_plan(root.root(), harness.path);
    let result = run_plan_workflow(root.root(), plan, &profile, true);
    assert!(
        result.is_err(),
        "pending subtasks must withhold snapshot completion"
    );
    let mut store = tasks::store(root.root()).unwrap();
    let states: Vec<String> = store.connection.prepare(
        "SELECT s.status FROM task_subtasks s JOIN tasks t ON t.task_id=s.task_id WHERE t.milestone_ref=?1 AND s.subtask_ref='check'",
    ).unwrap().query_map([harness.path], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        states,
        ["ready"],
        "pending Plan subtasks must survive in TasksStore for claim/resume"
    );
    assert!(!fs::read_to_string(root.path(harness.path))
        .unwrap()
        .contains("**Status**: completed"));

    let rows = plan_rows(root.root(), harness.path);
    let id = &rows[0].task_id;
    if rows[0].status == "in_progress" {
        store
            .release_failed_claim(id, rows[0].claim_revision)
            .unwrap();
    }
    assert_eq!(store.subtask_list(id).unwrap()[0].status, "ready");
    assert!(store
        .task_gates(id)
        .unwrap()
        .iter()
        .any(|gate| gate.0 == "record" && gate.2 == "passed"));
    drop(store);

    // Markdown -> SQLite is exercised above; management -> Markdown -> restart
    // below must retain actual transitions rather than importing a terminal fixture.
    let posted = tasks::blackboard::blackboard(
        root.root(),
        serde_json::from_value(json!({
            "action":"post", "scope":"task", "task_id":id, "author":"plan-author",
            "topic":"decisions", "payload":"Verify the Plan subtask before snapshot delivery"
        }))
        .unwrap(),
        "contract-test",
    )
    .unwrap();
    let message_id = posted["id"].as_u64().unwrap();
    let inspected = tasks::blackboard::blackboard(
        root.root(),
        serde_json::from_value(json!({
            "action":"inspect", "message_id":message_id
        }))
        .unwrap(),
        "contract-test",
    )
    .unwrap();
    assert_eq!(inspected["message"]["task_id"], *id);
    assert_eq!(inspected["message"]["author"], "plan-author");
    assert_eq!(inspected["message"]["topic"], "decisions");
    assert_eq!(
        inspected["message"]["payload"],
        "Verify the Plan subtask before snapshot delivery"
    );
    for (fixture, source, status, expected_error) in [
        (
            PlanFixture::PendingSubtask,
            include_str!("../fixtures/plans/pending-subtask.md"),
            "in_progress",
            Some("TASK_SUBTASKS_INCOMPLETE"),
        ),
        (
            PlanFixture::MissingSubtaskEvidence,
            include_str!("../fixtures/plans/missing-subtask-evidence.md"),
            "completed",
            Some("TASK_SUBTASK_EVIDENCE_MISSING"),
        ),
        (
            PlanFixture::CompletedSubtask,
            include_str!("../fixtures/plans/completed-subtask.md"),
            "completed",
            None,
        ),
    ] {
        // Terminal fixtures supply transition input, never a pre-completed run.
        let fixture = PlanHarness::fixture(fixture);
        let declared =
            PlanDocument::from_sections(Path::new(fixture.path), source.into(), &fixture.sections)
                .unwrap();
        let evidence = declared.steps[0].subtasks[0].evidence.as_deref();
        tasks::manage(
            root.root(),
            tasks::Manage {
                action: tasks::ManageAction::SubtaskUpdate,
                task_id: Some(id.clone()),
                subtask_ref: Some("check".into()),
                subtask_status: Some(status.into()),
                evidence: evidence.map(str::to_owned),
                ..Default::default()
            },
        )
        .unwrap();
        let projected = read_plan(root.root(), harness.path);
        assert_eq!(projected.steps[0].subtasks[0].status, status);
        assert_eq!(
            projected.steps[0].subtasks[0].evidence.as_deref(),
            evidence.filter(|s| !s.is_empty())
        );
        let result = run_task_lifecycle(root.root(), id, &run_options(root.root()), &mut |_| {});
        if let Some(code) = expected_error {
            assert!(format!("{:#}", result.unwrap_err()).contains(code));
            assert!(plan_rows(root.root(), harness.path)[0].receipt.is_none());
        } else {
            assert_eq!(result.unwrap()["status"], "completed");
        }
    }
    let completed = read_plan(root.root(), harness.path);
    assert_eq!(completed.steps[0].status, "completed");
    assert!(completed.steps[0].evidence.is_some());
    assert_eq!(
        completed.steps[0].subtasks[0].evidence.as_deref(),
        Some("Canonical fixture verification")
    );
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\n",
        "resume must not replay the passed command gate"
    );
    assert_eq!(plan_rows(root.root(), harness.path)[0].task_id, *id);
    let reopened = tasks::store(root.root()).unwrap();
    let messages = reopened
        .blackboard_read(id, Some("decisions"), None)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, message_id);
    assert_eq!(messages[0].task_id.as_deref(), Some(id.as_str()));
    assert_eq!(messages[0].author, "plan-author");
    assert_eq!(messages[0].topic, "decisions");
    assert_eq!(
        messages[0].payload.as_deref(),
        Some("Verify the Plan subtask before snapshot delivery")
    );
}

#[test]
fn plan_clear_and_archive_prune_only_the_selected_sqlite_subject() {
    use crate::acdd::tasks::harness::TaskHarness;
    use acdd_tasks::store::recovery::read_checkpoint;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let harness = PlanHarness::fixture(PlanFixture::PendingSubtask);
    let root = harness.materialize(harness.path);
    let mut profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    profile.gates[0].tasks.as_mut().unwrap().gates[0]
        .persistence
        .post = Some(profile::PersistenceAction::Snapshot);

    // The unrelated Milestone shares the actual configured DB and keeps an
    // active claim, subtask and genuine recovery checkpoint throughout cleanup.
    let milestone_path = "docs/milestones/unrelated.md";
    let milestone_source = MilestoneFixtureBuilder::new("unrelated")
        .task("keep", &["src/milestone.rs"])
        .task_subtasks(&[serde_json::from_value(json!({
            "subtask_ref":"keep-check", "title":"Keep milestone state", "status":"ready"
        }))
        .unwrap()])
        .build();
    let profile_source = fs::read_to_string(root.path("profiles/plan.yaml")).unwrap();
    let digest = format!("{:x}", Sha256::digest(profile_source.as_bytes()));
    let milestone_source = milestone_source.replacen(
        "---\n",
        &format!("---\nacdd_profile: profiles/plan.yaml:{}\n", &digest[..12]),
        1,
    );
    root.write(milestone_path, &milestone_source);
    root.write("src/milestone.rs", "// unrelated milestone\n");
    tasks::manage(
        root.root(),
        tasks::Manage {
            action: tasks::ManageAction::Sync,
            milestone_ref: Some(milestone_path.into()),
            ..Default::default()
        },
    )
    .unwrap();
    let milestone_id = plan_rows(root.root(), milestone_path)[0].task_id.clone();
    let mut milestone = TaskHarness::attach(&root, &milestone_id);
    milestone.claim_next().unwrap();
    drop(milestone);

    let source = fs::read_to_string(root.path(harness.path)).unwrap();
    for path in [
        harness.path,
        "docs/plans/clear.md",
        "docs/plans/archive-me.md",
    ] {
        root.write(path, &source);
        assert!(
            run_plan_workflow(root.root(), read_plan(root.root(), path), &profile, true).is_err()
        );
    }
    let head = git_head(root.root());
    let checkpoint_ref = |id: &str| format!("refs/acdd/state/acdd/{}", id.replace(':', "/"));
    let refs = || -> BTreeMap<String, String> {
        let output = git_command(
            root.root(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/acdd/",
            ],
        )
        .output()
        .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let (name, sha) = line.split_once(' ').unwrap();
                (name.to_owned(), sha.to_owned())
            })
            .collect()
    };
    let namespace = format!(
        "{:x}",
        Sha256::digest(
            root.root()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_bytes()
        )
    );
    let owned_snapshots = |all: &BTreeMap<String, String>, path: &str, id: &str| {
        let tag = format!("{:x}", Sha256::digest(path.as_bytes()));
        let owner = id.replace(':', "/");
        let prefixes = [
            format!("refs/acdd/snapshots/{namespace}/plan-{}-", &tag[..16]),
            format!("refs/acdd/snapshots/{namespace}/plan-gate-{}-", &tag[..16]),
            format!("refs/acdd/snapshots/{owner}/"),
        ];
        all.iter()
            .filter(|(name, _)| {
                *name == &format!("refs/acdd/snapshots/{owner}")
                    || prefixes.iter().any(|prefix| name.starts_with(prefix))
            })
            .map(|(name, sha)| (name.clone(), sha.clone()))
            .collect::<BTreeMap<_, _>>()
    };

    // Bounded per-owner readback includes full rows, not just counts/digests.
    // Direct SQLite access is the persistence/rollback assertion and fault seam.
    let owner_rows = |id: &str| {
        let store = tasks::store(root.root()).unwrap();
        let (prefix, task_ref) = id.split_once(':').unwrap();
        let identity = format!("acdd/{prefix}");
        let mut result = BTreeMap::new();
        for (table, predicate) in [
            ("tasks", "t.task_id=o.id"),
            ("task_revisions", "t.task_id=o.id"),
            ("task_subtasks", "t.task_id=o.id"),
            ("task_claims", "t.task_id=o.id"),
            ("task_gates", "t.task_id=o.id"),
            ("task_findings", "t.task_id=o.id"),
            ("task_scope_paths", "t.task_id=o.id"),
            (
                "task_dependencies",
                "t.task_id=o.id OR t.dependency_id=o.id",
            ),
            ("task_submissions", "t.task_id=o.id"),
            (
                "task_blackboard",
                "t.task_id=o.id OR t.recipient_task_id=o.id",
            ),
            ("gate_transitions", "t.subject=o.id"),
            ("gate_execution_traces", "t.subject=o.id"),
            ("gate_execution_runs", "t.subject=o.id"),
            (
                "gate_execution_attempts",
                "t.run_id IN (SELECT id FROM gate_execution_runs WHERE subject=o.id)",
            ),
            (
                "command_execution_proofs",
                "t.run_id IN (SELECT id FROM gate_execution_runs WHERE subject=o.id)",
            ),
            (
                "gate_checkpoints",
                "t.milestone_id=o.milestone AND t.subject=o.subject",
            ),
            ("task_store_metadata", "t.key='checkpoint_home:' || o.id"),
        ] {
            let sql = format!("WITH owner(id,milestone,subject) AS (VALUES (?1,?2,?3)) SELECT t.* FROM {table} t, owner o WHERE {predicate} ORDER BY t.rowid");
            let mut statement = store.connection.prepare(&sql).unwrap();
            let width = statement.column_count();
            let rows = statement
                .query_map(rusqlite::params![id, identity, task_ref], |row| {
                    (0..width)
                        .map(|i| row.get::<_, rusqlite::types::Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            result.insert(table, rows);
        }
        result
    };

    // Seed real on-demand SQLite checkpoints alongside the engine's Git state.
    // Their task_ref subject is the canonical breadcrumb owner, not a fake row.
    let store = tasks::store(root.root()).unwrap();
    for task in store.list(None, "all", None).unwrap() {
        let (prefix, task_ref) = task.task_id.split_once(':').unwrap();
        let breadcrumb = format!("acdd/{prefix}/record/{task_ref}/scratch");
        store
            .save_checkpoint_payload(
                &breadcrumb,
                &head,
                json!({"note":"retain until owner cleanup"}),
            )
            .unwrap();
        assert!(store.get_checkpoint(&breadcrumb).unwrap().is_some());
        let checkpoint = read_checkpoint(root.root(), &checkpoint_ref(&task.task_id)).unwrap();
        assert_eq!(checkpoint.task_id, task.task_id);
        assert!(checkpoint.task.is_some());
    }
    drop(store);
    let milestone_before = owner_rows(&milestone_id);
    assert_eq!(milestone_before["tasks"].len(), 1);
    assert_eq!(milestone_before["task_subtasks"].len(), 1);
    assert_eq!(milestone_before["task_claims"].len(), 1);
    assert!(!milestone_before["gate_checkpoints"].is_empty());
    let foreign = plan_rows(root.root(), harness.path);
    assert_eq!(
        foreign.len(),
        1,
        "foreign Plan must be durably registered before cleanup"
    );

    for (action, selector, path) in [
        ("clear", "clear", "docs/plans/clear.md"),
        ("archive", "archive-me", "docs/plans/archive-me.md"),
    ] {
        let selected = plan_rows(root.root(), path);
        assert_eq!(selected.len(), 1);
        let id = &selected[0].task_id;
        let before = owner_rows(id);
        assert_eq!(before["tasks"].len(), 1);
        assert_eq!(before["task_subtasks"].len(), 1);
        assert!(!before["task_claims"].is_empty());
        assert!(!before["gate_execution_traces"].is_empty());
        assert!(!before["gate_checkpoints"].is_empty());
        let refs_before = refs();
        let snapshots_before = owned_snapshots(&refs_before, path, id);
        assert!(!snapshots_before.is_empty());
        let selected_checkpoint = checkpoint_ref(id);
        assert!(refs_before.contains_key(&selected_checkpoint));
        let unselected_refs = refs_before
            .iter()
            .filter(|(name, _)| {
                !snapshots_before.contains_key(*name) && *name != &selected_checkpoint
            })
            .map(|(name, sha)| (name.clone(), sha.clone()))
            .collect::<BTreeMap<_, _>>();
        let siblings = tasks::store(root.root())
            .unwrap()
            .list(None, "all", None)
            .unwrap()
            .into_iter()
            .filter(|task| &task.task_id != id)
            .map(|task| (task.task_id.clone(), owner_rows(&task.task_id)))
            .collect::<BTreeMap<_, _>>();
        assert!(siblings.contains_key(&foreign[0].task_id));
        assert!(siblings.contains_key(&milestone_id));

        // Snapshot refs are pruned before a real SQLite delete failure. SQL
        // rollback must preserve claims/checkpoints; retry finishes the cleanup.
        let store = tasks::store(root.root()).unwrap();
        store.connection.execute_batch(&format!(
            "CREATE TRIGGER reject_plan_cleanup BEFORE DELETE ON tasks WHEN OLD.task_id='{}' BEGIN SELECT RAISE(ABORT, 'injected plan cleanup failure'); END;",
            id.replace('\'', "''")
        )).unwrap();
        drop(store);
        let failed = TaskHarness::run_cli_for_root(root.root(), &["plan", action, selector]);
        assert!(
            !failed.status.success(),
            "{action} must report a partial cleanup failure"
        );
        assert!(String::from_utf8_lossy(&failed.stderr).contains("injected plan cleanup failure"));
        let partial_refs = refs();
        assert!(
            owned_snapshots(&partial_refs, path, id).is_empty(),
            "fault must follow the first physical cleanup effect"
        );
        assert_eq!(
            owner_rows(id),
            before,
            "{action}: no half-deleted rows or stale claims"
        );
        assert_eq!(
            partial_refs.get(&selected_checkpoint),
            refs_before.get(&selected_checkpoint)
        );
        assert!(read_checkpoint(root.root(), &selected_checkpoint)
            .unwrap()
            .task
            .is_some());
        assert!(
            root.path(path).exists(),
            "failed archive must retain discoverable authority for retry"
        );
        for (sibling, rows) in &siblings {
            assert_eq!(
                &owner_rows(sibling),
                rows,
                "{action}: failed cleanup changed {sibling}"
            );
        }
        for (name, sha) in &unselected_refs {
            assert_eq!(
                partial_refs.get(name),
                Some(sha),
                "{action}: failed cleanup changed {name}"
            );
        }
        tasks::store(root.root())
            .unwrap()
            .connection
            .execute_batch("DROP TRIGGER reject_plan_cleanup")
            .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let hooks = git_command(
                root.root(),
                &["rev-parse", "--git-path", "hooks/reference-transaction"],
            )
            .output()
            .unwrap();
            let hook = root.path(String::from_utf8(hooks.stdout).unwrap().trim());
            fs::create_dir_all(hook.parent().unwrap()).unwrap();
            fs::write(&hook, format!("#!/bin/sh\nif [ \"$1\" = prepared ]; then\n  while read old new ref; do\n    if [ \"$ref\" = '{selected_checkpoint}' ] && [ \"$new\" = 0000000000000000000000000000000000000000 ]; then exit 1; fi\n  done\nfi\nexit 0\n")).unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            let failed = TaskHarness::run_cli_for_root(root.root(), &["plan", action, selector]);
            assert!(!failed.status.success());
            assert!(String::from_utf8_lossy(&failed.stderr).contains("TASK_CHECKPOINT_PENDING"));
            assert!(root.path(path).exists());
            assert!(refs().contains_key(&selected_checkpoint));
            fs::remove_file(hook).unwrap();
        }

        let output = TaskHarness::run_cli_for_root(root.root(), &["plan", action, selector]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(plan_rows(root.root(), path).is_empty());
        let after = owner_rows(id);
        for (table, rows) in &after {
            if *table != "task_store_metadata" {
                assert!(
                    rows.is_empty(),
                    "{action}: leftover selected {table}: {rows:?}"
                );
            }
        }
        let refs_after = refs();
        assert!(
            owned_snapshots(&refs_after, path, id).is_empty(),
            "{action}: selected snapshots must be exactly zero"
        );

        // Preserve genuine allocation fences if the checkpoint owner retains
        // one, but never retain a claim, lifecycle state, intent or recovery cohort.
        if refs_after.contains_key(&selected_checkpoint) {
            let checkpoint = read_checkpoint(root.root(), &selected_checkpoint).unwrap();
            assert!(checkpoint.task.is_none());
            let state = serde_json::to_value(checkpoint).unwrap();
            assert!(state["intent"].is_null());
            for field in [
                "claims",
                "gates",
                "findings",
                "scopes",
                "dependencies",
                "submissions",
                "prepared_proofs",
                "cohort",
            ] {
                assert_eq!(
                    state[field],
                    json!([]),
                    "{action}: retained live checkpoint {field}"
                );
            }
        }
        for (sibling, rows) in &siblings {
            assert_eq!(
                &owner_rows(sibling),
                rows,
                "{action}: cleanup changed {sibling}"
            );
        }
        for (name, sha) in &unselected_refs {
            assert_eq!(
                refs_after.get(name),
                Some(sha),
                "{action}: cleanup changed {name}"
            );
        }
        assert_eq!(root.path(path).exists(), action == "clear");
        if action == "archive" {
            assert!(root.path("docs/plans/archive/archive-me.md").exists());
        }
        let retry = TaskHarness::run_cli_for_root(root.root(), &["plan", "clear", selector]);
        assert!(retry.status.success(), "cleanup must remain safe to retry");
        assert_eq!(owner_rows(id), after);
        assert!(owned_snapshots(&refs(), path, id).is_empty());
    }
    assert_eq!(git_head(root.root()), head);
}

#[test]
fn first_immutable_plan_gate_does_not_require_a_nonexistent_predecessor() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let mut profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    let gates = &mut profile.gates[0].tasks.as_mut().unwrap().gates;
    gates[0].persistence.immutable = true;
    profile.commands.insert(
        "record".into(),
        serde_json::from_value(json!({"command":"true", "output":{"require_counts":false}}))
            .unwrap(),
    );
    let result = run_plan_workflow(
        root.root(),
        read_plan(root.root(), harness.path),
        &profile,
        true,
    );
    assert!(
        result.is_ok(),
        "read-only first immutable gate has no predecessor to compare: {result:#?}"
    );
    assert_eq!(result.unwrap()["status"], "completed");
}

#[test]
fn plan_completion_preserves_nested_headings_and_projects_evidence_to_the_step() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let nested = "### Implementation notes\n\nKeep this paragraph and fenced example intact.\n\n```text\n- **Status**: ready\n- **Subtasks**: example\n```\n\n";
    let source = fs::read_to_string(root.path(harness.path))
        .unwrap()
        .replace(
            "- **Status**: ready\n",
            &format!("{nested}- **Status**: ready\n"),
        );
    root.write(harness.path, &source);
    let profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    // Hold the valid indexed step projection fixed: this scenario targets
    // complete_source's span, including a status field after a nested heading.
    let plan =
        PlanDocument::from_sections(Path::new(harness.path), source, &harness.sections).unwrap();
    let result = run_plan_workflow(root.root(), plan, &profile, true).unwrap();
    assert_eq!(result["status"], "completed");
    let updated = fs::read_to_string(root.path(harness.path)).unwrap();
    assert!(updated.contains(nested));
    let first = updated.split("## 2.").next().unwrap();
    assert!(first.contains("- **Status**: completed"));
    assert!(first.contains("- **Evidence**:"));
    assert_eq!(first.matches("- **Status**: ready").count(), 1);
    assert_eq!(read_plan(root.root(), harness.path).steps.len(), 2);
}

#[test]
fn plan_command_placeholders_consume_the_preceding_role_artifact() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    crate::acdd::snapshot::stage_role_helper(root.root());
    let mut profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    profile.roles.insert(
        "author".into(),
        serde_json::from_value(json!({"runner":format!(".acdd/role_fixture plan-artifact-runner '{}' '{{prompt}}'", env!("CARGO_BIN_EXE_acdd"))})).unwrap(),
    );
    profile.commands.insert("record".into(), serde_json::from_value(json!({"command":"printf '%s|%s|%s\\n' {plan_name} {plan_slug} {artifact_path} >> expanded.log", "output":{"require_counts":false}})).unwrap());
    profile.gates[0].tasks.as_mut().unwrap().gates.insert(0, serde_json::from_value(json!({
        "id":"synthesize", "role":"author", "proof":{"scheme":{"type":"object", "additionalProperties":true, "properties":{"plan_slug":{"type":"string"},"artifact_path":{"type":"string"}}}}
    })).unwrap());
    let result = run_plan_workflow(
        root.root(),
        read_plan(root.root(), harness.path),
        &profile,
        true,
    )
    .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("expanded.log")).unwrap(),
        "old-plan|old-plan|old/path\nnew-plan|new-plan|new/path\n"
    );
}

#[test]
fn failed_plan_command_propagates_current_diagnostics_to_the_repair_role() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    crate::acdd::snapshot::stage_role_helper(root.root());
    let mut profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    profile.commands.insert("record".into(), serde_json::from_value(json!({
        "command":"printf 'plan verification diagnostic {artifact_path} {plan_slug}\\n' >&2; exit 37", "output":{"require_counts":false}
    })).unwrap());
    profile.roles.insert("author".into(), serde_json::from_value(json!({"runner":format!(".acdd/role_fixture failed-artifact '{}' '{{prompt}}'", env!("CARGO_BIN_EXE_acdd"))})).unwrap());
    profile.roles.insert(
        "repairer".into(),
        serde_json::from_value(json!({
            "runner":".acdd/role_fixture inspect-findings '{prompt}'"
        }))
        .unwrap(),
    );
    let gates = &mut profile.gates[0].tasks.as_mut().unwrap().gates;
    gates[0] = serde_json::from_value(json!({"id":"record", "command":"record", "fail":"repair"}))
        .unwrap();
    gates.insert(
        1,
        serde_json::from_value(json!({"id":"repair", "role":"repairer", "proof":"none"})).unwrap(),
    );
    gates.insert(0, serde_json::from_value(json!({"id":"synthesize", "role":"author", "proof":{"scheme":{"type":"object", "additionalProperties":true, "properties":{"plan_slug":{"type":"string"},"artifact_path":{"type":"string"}}}}})).unwrap());
    assert!(run_plan_workflow(
        root.root(),
        read_plan(root.root(), harness.path),
        &profile,
        true
    )
    .is_err());
    let context: Value =
        serde_json::from_str(&fs::read_to_string(root.path(".acdd/repair-context.json")).unwrap())
            .unwrap();
    let findings = &context["findings"];
    assert_eq!(findings["code"], "COMMAND_FAILED");
    assert_eq!(findings["gate"], "record");
    let diagnostics: Vec<(String, Value)> =
        serde_json::from_value(findings["diagnostics"].clone()).unwrap();
    assert_eq!(diagnostics.len(), 1);
    let (command_id, observation) = &diagnostics[0];
    assert_eq!(command_id, "record");
    assert_eq!(
        observation["command"],
        "printf 'plan verification diagnostic generated/path generated\\n' >&2; exit 37"
    );
    assert_eq!(observation["exit_code"], 37);
    assert_eq!(observation["timed_out"], false);
    assert_eq!(
        observation["diagnostic"].as_str().unwrap().trim(),
        "plan verification diagnostic generated/path generated"
    );
    let rows = plan_rows(root.root(), harness.path);
    assert!(rows.iter().all(|task| task.receipt.is_none()));
    let mut store = tasks::store(root.root()).unwrap();
    let id = &rows
        .iter()
        .find(|task| task.stage == "repair")
        .unwrap()
        .task_id;
    store
        .subtask_add(id, "added-check", "Check dynamically added work")
        .unwrap();
    store
        .subtask_update(id, "added-check", "completed", Some("verified"))
        .unwrap();
    let projected = read_plan(root.root(), harness.path);
    assert!(projected
        .steps
        .iter()
        .flat_map(|step| &step.subtasks)
        .any(|subtask| subtask.subtask_ref == "added-check"
            && subtask.evidence.as_deref() == Some("verified")));
    assert!(!read_plan(root.root(), harness.path)
        .steps
        .iter()
        .any(|step| step.status == "completed"));
}
