use super::*;

use std::path::PathBuf;

#[test]
fn checkpoint_header_rejects_unsupported_versions_and_unresolved_intents() -> Result<()> {
    let checkpoint = ActiveCheckpoint {
        version: 1,
        repository: "repo".into(),
        project: "project".into(),
        milestone_id: "milestone".into(),
        task_id: "repo/milestone:task".into(),
        workspace: PathBuf::from("/unused"),
        task: None,
        max_revision: 0,
        claims: Vec::new(),
        claim_session_fingerprints: BTreeMap::new(),
        gates: Vec::new(),
        findings: Vec::new(),
        scopes: Vec::new(),
        dependencies: Vec::new(),
        submissions: Vec::new(),
        prepared_proofs: BTreeSet::new(),
        intent: None,
        cohort: Vec::new(),
        cohort_id: String::new(),
    };
    for (version, intent, valid) in [
        (1, None, true),
        (99, None, false),
        (1, Some("unresolved".to_owned()), false),
    ] {
        let mut candidate = checkpoint.clone();
        candidate.version = version;
        candidate.intent = intent;
        let result = candidate.validate_cohort();
        if valid {
            result?;
        } else {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("TASK_RECOVERY_AMBIGUOUS"));
        }
    }
    Ok(())
}

#[test]
fn orphaned_intent_preimage_rejects_corruption() -> Result<()> {
    let source = "---\nid: m-test\nstatus: active\n---\n```yaml\ntask_ref: first\ntarget: First\nproof_policy: seam-test-first\nscope: [src/lib.rs]\n```\n";
    let milestone = crate::tasks::Milestone::parse(source, "repo")?;
    let mut store = TasksStore::open_in_memory("repo", "repo")?;
    store.sync(&milestone, "docs/milestone.md", Path::new("/"))?;
    let task = store.inspect(&milestone.task_id(&milestone.tasks[0]))?;
    let acknowledged = ActiveCheckpoint {
        version: 1,
        repository: "repo".into(),
        project: "repo".into(),
        milestone_id: "m-test".into(),
        task_id: task.task_id.clone(),
        workspace: PathBuf::from("/"),
        task: Some(task),
        max_revision: 1,
        claims: vec![(1, "worker".into(), "/".into(), 1, false)],
        claim_session_fingerprints: BTreeMap::new(),
        gates: vec![("contract".into(), 1, "pending".into(), None)],
        findings: Vec::new(),
        scopes: Vec::new(),
        dependencies: Vec::new(),
        submissions: Vec::new(),
        prepared_proofs: BTreeSet::new(),
        intent: None,
        cohort: Vec::new(),
        cohort_id: String::new(),
    };
    let mut genuine = acknowledged.clone();
    genuine.intent = Some("1-1".into());
    genuine.max_revision += 1;
    replay::validate_intent_preimage(&genuine, &acknowledged)?;
    for case in [
        "worker",
        "claim",
        "claims",
        "gates",
        "cohort",
        "cohort_id",
        "token",
        "project",
    ] {
        let mut candidate = genuine.clone();
        match case {
            "worker" => candidate.task.as_mut().unwrap().worker_id = Some("foreign".into()),
            "claim" => candidate.task.as_mut().unwrap().claim_revision += 10,
            "claims" => candidate.claims.clear(),
            "gates" => candidate.gates.clear(),
            "cohort" => candidate.cohort.push((PathBuf::from("/"), "ref".into())),
            "cohort_id" => candidate.cohort_id = "foreign".into(),
            "token" => candidate.intent = Some("invalid-token".into()),
            "project" => candidate.project = "foreign".into(),
            _ => unreachable!(),
        }
        assert!(
            replay::validate_intent_preimage(&candidate, &acknowledged).is_err(),
            "{case}"
        );
    }
    for (max, floor, valid) in [
        (1, 1, true),
        (2, 1, true),
        (101, 1, false),
        (u64::MAX, u64::MAX, true),
    ] {
        assert_eq!(replay::validate_reservation(max, floor).is_ok(), valid);
    }
    let mut staging = TasksStore::open_in_memory("repo", "repo")?;
    staging.begin_recovery_stage()?;
    let mut foreign = acknowledged.clone();
    foreign.project = "foreign".into();
    let error = staging
        .import_checkpoint(&foreign, acknowledged.task.as_ref().unwrap())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("TASK_RECOVERY_NAMESPACE_MISMATCH"));
    let mut peer = acknowledged.clone();
    replay::validate_acknowledged_peer(&acknowledged, &peer)?;
    peer.cohort_id = "foreign".into();
    assert!(replay::validate_acknowledged_peer(&acknowledged, &peer).is_err());
    peer = genuine.clone();
    peer.intent = Some("2-2".into());
    assert!(replay::validate_matching_token(&genuine, &peer).is_err());
    replay::validate_matching_token(&genuine, &genuine)?;
    Ok(())
}

#[test]
fn pending_manifest_publication_replays_for_non_git_task_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let manifest = workspace.join("docs/milestones/010-scope.md");
    std::fs::create_dir_all(manifest.parent().context("manifest parent")?)?;
    let before = b"# Scope\n";
    let after = b"# Scope\n\nExtended scope.\n";
    std::fs::write(&manifest, before)?;

    let replacement =
        ManifestReplacement::new(&workspace, "docs/milestones/010-scope.md", before, after)?;
    let database = temp.path().join(".acdd/tasks.sqlite");
    let mut store = TasksStore::open_project(&database, "repo", "project")?;
    let context = store.checkpoint_context()?;
    assert!(
        context.is_none(),
        "fixture must not have Git checkpoint context"
    );

    // Existing pending publications keep deserializing after this additive field.
    let legacy: Publication = serde_json::from_str(r#"{"active":[],"retired":[]}"#)?;
    assert!(legacy.manifest_replacement.is_none());

    let mut tx = transaction(&mut store.connection, context)?;
    tx.stage_manifest_replacement(replacement)?;
    let error = tx
        .commit_with_publisher(|_| Err(anyhow::anyhow!("injected publication failure")))
        .expect_err("injected failure must leave the committed publication pending");
    assert!(is_pending(&error));
    assert_eq!(std::fs::read(&manifest)?, before);
    assert!(ensure_observable(&store.connection).is_err());

    std::fs::write(&manifest, b"# Third-party edit\n")?;
    let error = store
        .reconcile_checkpoints()
        .expect_err("recovery must fail closed on unexpected manifest content");
    assert!(is_pending(&error));
    assert!(ensure_observable(&store.connection).is_err());

    std::fs::write(&manifest, before)?;
    assert!(store.reconcile_checkpoints()?);
    assert_eq!(std::fs::read(&manifest)?, after);
    assert!(!store.reconcile_checkpoints()?);
    ensure_observable(&store.connection)?;
    Ok(())
}

#[test]
fn git_command_reports_missing_worktree_clearly() {
    let non_existent = Path::new("/tmp/non_existent_worktree_1234567890");
    let err = crate::git::git_command(non_existent, &["status"]).unwrap_err();
    assert!(err.to_string().contains("TASK_GIT_WORKTREE_MISSING"));
    assert!(err.to_string().contains(non_existent.to_str().unwrap()));
}
