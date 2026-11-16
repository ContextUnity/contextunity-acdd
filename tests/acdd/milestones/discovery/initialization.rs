use super::*;

#[test]
fn migration_reconciles_manifests_and_creation_selects_one_task() {
    let root = MilestoneHarness::configured_workspace();
    root.write("src/lib.rs", "pub fn example() {}\n");
    root.write("docs/010-test.md", SPEC);
    let (milestone, mut store) = root.sync_document("docs/010-test.md", SPEC);
    let invoke = |action: &str, reference: Option<&str>| root.migrate(action, reference);
    for action in ["preview", "apply", "apply", "verify"] {
        let result = invoke(action, Some("docs/010-test.md")).unwrap();
        assert_eq!(result["mode"], action);
    }
    store
        .delete(None, Some("docs/010-test.md"), true, 0)
        .unwrap();
    let created = contextunity_acdd::engine::tasks::manage(
        root.root(),
        contextunity_acdd::engine::tasks::Manage {
            action: contextunity_acdd::engine::tasks::ManageAction::Create,
            milestone_ref: Some("docs/010-test.md".into()),
            task_ref: Some("first".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(created["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(store.list(None, "all", None).unwrap().len(), 1);
    assert_eq!(
        store.list(None, "all", None).unwrap()[0].task_id,
        milestone.task_id(&milestone.tasks[0])
    );
    assert!(invoke("verify", Some("docs/010-test.md")).is_err());
    assert!(invoke("apply", Some("docs/010-test.md")).is_ok());
    assert!(invoke("verify", Some("docs/010-test.md")).is_ok());
    root.write("docs/milestones/archive/010-test.md", SPEC);
    for action in ["preview", "apply", "verify"] {
        let result = invoke(action, None).unwrap();
        assert_eq!(result["manifests"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn milestone_init_allocates_next_number_and_renders_valid_frontmatter() {
    let root = MilestoneHarness::workspace(DEFAULT_SEAM, "");
    root.write(
        "docs/milestones/010-first.md",
        &minimal_milestone("m-first"),
    );
    root.write(
        "docs/milestones/archive/1050-prior.md",
        &minimal_milestone("m-prior"),
    );
    root.init(milestone_engine::Init {
        slug: Some("next-work".into()), title: Some("Next work".into()),
        stdin: "A concrete outcome.\n\n### task: first\n\n```yaml\ntask_ref: first\ntarget: First task\nproof_policy: seam-test-first\nscope: [src/]\n```\n\nTask notes: preserve this detail.\n\n### task: second\n\n```yaml\ntask_ref: second\ntarget: Second task\nproof_policy: seam-test-first\nscope: [src/]\n```\n".into(),
        ..milestone_init_options()
    }).unwrap();
    let planned = root.document("docs/milestones/1060-next-work.md");
    assert!(!planned.contains("started_at:"));
    assert!(planned.contains("A concrete outcome."));
    assert!(planned.contains("## Deferred and out-of-scope defects"));
    assert!(planned.contains("deferred_defects: []"));
    assert!(Milestone::parse(&planned, "acdd")
        .unwrap()
        .deferred_defects
        .is_empty());
    let note = planned.find("Task notes: preserve this detail.").unwrap();
    assert!(planned.find("task_ref: first").unwrap() < note);
    assert!(note < planned.find("task_ref: second").unwrap());
    root.init(milestone_engine::Init {
        num: Some("011".into()),
        slug: Some("active-work".into()),
        title: Some("Active work".into()),
        active: true,
        ..milestone_init_options()
    })
    .unwrap();
    let active = root.document("docs/milestones/011-active-work.md");
    for (document, expected_status) in [(&planned, "status: planned"), (&active, "status: active")]
    {
        assert!(
            document.contains(expected_status),
            "expected {expected_status} milestone: {document}"
        );
    }
    let started = active
        .lines()
        .find_map(|line| line.strip_prefix("started_at: "))
        .unwrap();
    let parsed = chrono::DateTime::parse_from_rfc3339(started).unwrap();
    assert!(
        (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc))
            .num_minutes()
            .abs()
            < 2
    );
    root.init(milestone_engine::Init {
        num: Some("012".into()),
        slug: Some("described".into()),
        title: Some("Described".into()),
        desc: Some("Explicit description".into()),
        depends_on: vec!["m-first".into()],
        ..milestone_init_options()
    })
    .unwrap();
    let text = root.document("docs/milestones/012-described.md");
    assert!(text.contains("Explicit description"));
    assert!(text.contains("m-first"));
    root.write(
        "docs/milestones/archive/18446744073709551616-legacy.md",
        &minimal_milestone("m-legacy"),
    );
    let overflow = root.init(milestone_engine::Init {
        slug: Some("later".into()),
        title: Some("Later".into()),
        ..milestone_init_options()
    });
    assert!(overflow.is_err());
}

#[test]
fn milestone_init_imports_plan_metadata_and_rejects_duplicate_number() {
    let root = MilestoneHarness::workspace(DEFAULT_SEAM, "");
    root.write("docs/plans/proposal.md", "---\ntitle: Planned title\ndoc_type: plan\ndepends_on: [m-prior]\n---\n# Planned title\n\nA scoped purpose.\n\n## Delivery notes\n\nA separate implementation note.\n");
    let invoke = || {
        root.init(milestone_engine::Init {
            num: Some("025".into()),
            slug: Some("planned-title".into()),
            plan: Some("docs/plans/proposal.md".into()),
            ..milestone_init_options()
        })
    };
    let response = invoke().unwrap();
    assert!(response["sync"].as_str().unwrap().contains("task sync"));
    assert!(response["claim"].as_str().unwrap().contains("task claim"));
    let text = root.document("docs/milestones/025-planned-title.md");
    assert!(text.contains("title: Planned title"));
    assert!(text.contains("m-prior"));
    assert!(text.contains("A scoped purpose."));
    assert!(text.contains("## Source plan notes"));
    assert!(text.find("## Source plan notes").unwrap() > text.find("A scoped purpose.").unwrap());
    assert!(text.contains("A separate implementation note."));
    assert!(invoke().is_err());
    let same_id = root.init(milestone_engine::Init {
        num: Some("035".into()),
        slug: Some("planned-title".into()),
        title: Some("Another title".into()),
        ..milestone_init_options()
    });
    assert!(same_id.is_err());
}

#[test]
fn in002_companion_runbooks_live_outside_milestone_discovery() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(milestone_engine::is_milestone_manifest_name(
        "IN-002-rename-inventory.md"
    ));
    assert!(!milestone_engine::is_milestone_manifest_name(
        "rename-inventory.md"
    ));
    assert!(!root
        .join("docs/milestones/IN-002-rename-inventory.md")
        .exists());
    assert!(!root
        .join("docs/milestones/IN-002-cutover-runbook.md")
        .exists());
    for relative in [
        "docs/runbooks/in002-rename-inventory.md",
        "docs/runbooks/in002-cutover-runbook.md",
    ] {
        let path = root.join(relative);
        assert!(path.is_file(), "missing companion runbook {relative}");
    }

    let manifest_path = [
        "docs/milestones/IN-002-contextunity-acdd-one-pass-rename.md",
        "docs/milestones/archive/IN-002-contextunity-acdd-one-pass-rename.md",
    ]
    .into_iter()
    .map(|rel| root.join(rel))
    .find(|path| path.is_file())
    .expect("read IN-002 manifest from active or archived milestones");
    let manifest_text = std::fs::read_to_string(manifest_path).expect("read IN-002 manifest");
    let parsed = Milestone::parse(&manifest_text, "acdd").expect("parse IN-002 manifest");
    assert_eq!(parsed.id, "IN-002-contextunity-acdd-one-pass-rename");

    let task = |task_ref: &str| {
        parsed
            .tasks
            .iter()
            .find(|task| task.task_ref == task_ref)
            .unwrap_or_else(|| panic!("missing task {task_ref}"))
    };
    let freeze = task("freeze-rename-and-cutover-contract");
    assert!(freeze.depends_on.is_empty());
    assert!(freeze
        .scope
        .contains(&"docs/runbooks/in002-rename-inventory.md".to_owned()));
    assert!(freeze
        .scope
        .contains(&"docs/runbooks/in002-cutover-runbook.md".to_owned()));
    assert!(freeze
        .scope
        .contains(&"tests/acdd/milestones/discovery/initialization.rs".to_owned()));

    let runtime = task("rename-runtime-and-workspace");
    assert_eq!(
        runtime.depends_on,
        vec!["freeze-rename-and-cutover-contract"]
    );
    assert!(runtime.scope.contains(&"crates/acdd-core/src/".to_owned()));

    let distro = task("rename-distribution-and-guidance");
    assert_eq!(distro.depends_on, vec!["rename-runtime-and-workspace"]);
    assert!(distro.scope.contains(&"skills/".to_owned()));

    let activate = task("activate-config-and-profile");
    assert_eq!(
        activate.depends_on,
        vec!["rename-distribution-and-guidance"]
    );
    assert!(activate.scope.contains(&"acdd.yaml".to_owned()));
    assert!(activate.scope.contains(&".acdd/profile.yaml".to_owned()));

    let isolated = MilestoneHarness::configured_workspace();
    let active_manifest = manifest_text.replacen("status: completed", "status: active", 1);
    for skill in &parsed.required_skills {
        isolated.write(
            format!("skills/{skill}/SKILL.md"),
            "# Required test skill\n",
        );
    }
    isolated.write(
        "docs/milestones/IN-002-contextunity-acdd-one-pass-rename.md",
        &active_manifest,
    );
    isolated.write("docs/runbooks/in002-rename-inventory.md", "# Inventory\n");
    isolated.write("docs/runbooks/in002-cutover-runbook.md", "# Cutover\n");
    let listed = isolated
        .run_cli(&["milestone", "list"])
        .expect("milestone list ignores docs/runbooks/");
    assert_eq!(listed["milestones"].as_array().unwrap().len(), 1);
    let initialized = isolated
        .run_cli(&[
            "milestone",
            "init",
            "docs/milestones/IN-002-contextunity-acdd-one-pass-rename.md",
            "--in-place",
        ])
        .expect("IN-002 manifest initializes cleanly alongside docs/runbooks/");
    assert_eq!(
        initialized["milestone_id"],
        "IN-002-contextunity-acdd-one-pass-rename"
    );

    // Placing an uppercase TAG-NUM companion doc under docs/milestones/ breaks discovery/sync.
    isolated.write(
        "docs/milestones/IN-002-rename-inventory.md",
        "# Companion\n",
    );
    assert!(isolated.run_cli(&["milestone", "list"]).is_err());
}
