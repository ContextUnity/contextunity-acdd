use crate::acdd::support::Workspace;
use contextunity_acdd::acdd_tasks::tasks::profile::{self, GateProfile};

/// Shared root-gate profile construction for ADR 0021 workflow fixtures.
/// Profile validation does not prove standalone workflow dispatch or execution.
pub(crate) struct WorkflowHarness {
    pub profile: GateProfile,
    source: &'static str,
    path: &'static str,
}

impl WorkflowHarness {
    pub fn simple() -> Self {
        Self::gate_profile(
            "docs/workflows/simple.yaml",
            include_str!("../fixtures/workflows/simple.yaml"),
        )
    }

    pub fn rejection_routing() -> Self {
        Self::gate_profile(
            "docs/workflows/rejection_routing.yaml",
            include_str!("../fixtures/workflows/rejection_routing.yaml"),
        )
    }

    pub fn ordered() -> Self {
        Self::gate_profile(
            "docs/workflows/ordered.yaml",
            include_str!("../fixtures/workflows/ordered.yaml"),
        )
    }

    pub fn agents() -> Self {
        Self::gate_profile(
            "docs/workflows/agents.yaml",
            include_str!("../fixtures/workflows/agents.yaml"),
        )
    }

    #[cfg(target_os = "linux")]
    pub fn artifact_refresh() -> Self {
        Self::gate_profile(
            "docs/workflows/artifact-refresh.yaml",
            include_str!("../fixtures/workflows/artifact-refresh.yaml"),
        )
    }

    pub fn materialize(&self) -> Workspace {
        let root = Workspace::new();
        self.write_to(&root);
        root
    }

    pub fn write_to(&self, root: &Workspace) {
        root.write(self.path, self.source);
        root.write(
            "workflow-runner.py",
            include_str!("../fixtures/workflows/runner.py"),
        );
        root.write(
            "artifact-runner.py",
            include_str!("../fixtures/workflows/artifact-runner.py"),
        );
        root.write(
            "artifact-check.py",
            include_str!("../fixtures/workflows/artifact-check.py"),
        );
    }

    pub(crate) fn gate_profile(path: &'static str, source: &'static str) -> Self {
        Self {
            profile: profile::validate_profile_document(source)
                .expect("canonical workflow gate profile must validate"),
            source,
            path,
        }
    }
}

#[test]
fn shipped_authoring_workflows_execute_repair_and_real_preflights() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    fn authoring_command(root: &Workspace, args: &[&str]) -> std::process::Output {
        let binary = std::path::Path::new(env!("CARGO_BIN_EXE_acdd"));
        let mut paths = vec![binary.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::process::Command::new(binary)
            .args(["--root", root.root().to_str().unwrap(), "--format", "json"])
            .args(args)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap()
    }
    let root = Workspace::new();
    root.write(
        "author-runner.py",
        include_str!("../fixtures/workflows/author-runner.py"),
    );
    root.write("author-draft", "Чернетка\n");
    root.write("acdd.yaml", "plans: [docs/plans]\n");
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.root())
        .status()
        .unwrap()
        .success());
    let plan = "---\ntitle: Приклад\ndoc_type: plan\npurpose: Приклад плану\ndepends_on: []\n---\n\n# Приклад\n\n## 1. Перший\n\n- **Target**: Змінити перший файл\n- **Scope**: src/first.rs\n- **Status**: ready\n\n## 2. Другий\n\n- **Target**: Змінити другий файл\n- **Scope**: src/second.rs\n- **Depends On**: [\"Перший\"]\n- **Status**: ready\n";
    let manifest = MilestoneFixtureBuilder::new("IN-001-pryklad")
        .title("Приклад")
        .task("first", &["src/first.rs"])
        .task_target("Виконати першу задачу")
        .build();
    let cases = [
        ("workflow", "version: 2\ninherit: none\nsave: none\ncommands:\n  check:\n    command: \"true\"\n    output: { require_counts: false }\ngates:\n  - id: check\n    command: check\n"),
        ("plan", plan),
        ("milestone", manifest.as_str()),
    ];
    for (name, final_content) in cases {
        let path = format!("docs/workflows/author-{name}.workflow.yaml");
        let source =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&path))
                .unwrap();
        let source = source
            .replace(
                "codex exec -C \"{worktree}\" -m \"{model}\" -c model_reasoning_effort=\"{reasoning}\" \"{prompt}\"",
                "python3 author-runner.py \"{prompt}\"",
            );
        root.write(&path, &source);
        root.write("author-final", final_content);
        if name == "plan" {
            root.write("docs/plans/pryklad.md", "Початковий план\n");
        }
        let output = authoring_command(&root, &["workflow", "run", &format!("author-{name}")]);
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "completed", "{name}");
        let outcomes = result["outcomes"].as_array().unwrap();
        assert_eq!(
            outcomes
                .iter()
                .map(|o| o["gate"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["synthesize", "review", "synthesize", "review", "preflight"],
            "{name}"
        );
        assert_eq!(outcomes[1]["passed"], false, "{name}");
        assert_ne!(
            outcomes[2]["evidence"]["session_id"], outcomes[3]["evidence"]["session_id"],
            "{name}"
        );
        match name {
            "workflow" => {
                let show = authoring_command(&root, &["workflow", "show", "pryklad"]);
                assert!(
                    show.status.success(),
                    "{}",
                    String::from_utf8_lossy(&show.stderr)
                );
                assert!(root.path("docs/workflows/pryklad.workflow.yaml").exists());
            }
            "plan" => {
                let show = authoring_command(&root, &["plan", "show", "pryklad"]);
                assert!(
                    show.status.success(),
                    "{}",
                    String::from_utf8_lossy(&show.stderr)
                );
                let shown = String::from_utf8(show.stdout).unwrap();
                for part in ["Перший", "Другий", "src/first.rs", "src/second.rs"] {
                    assert!(shown.contains(part), "{shown}");
                }
                let parsed: serde_json::Value = serde_json::from_str(&shown).unwrap();
                let steps = parsed["steps"].as_array().unwrap();
                assert_eq!(steps.len(), 2);
                assert!(steps[0]["title"].as_str().unwrap().contains("Перший"));
                assert!(steps[1]["title"].as_str().unwrap().contains("Другий"));
                assert_eq!(steps[1]["depends_on"], serde_json::json!(["Перший"]));
                assert_eq!(
                    std::fs::read_dir(root.path("docs/plans")).unwrap().count(),
                    1
                );
            }
            "milestone" => {
                let db = rusqlite::Connection::open(root.path(".acdd/tasks.sqlite")).unwrap();
                let claims: i64 = db
                    .query_row("SELECT count(*) FROM task_claims", [], |r| r.get(0))
                    .unwrap();
                let tasks: i64 = db
                    .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
                    .unwrap();
                let gates: i64 = db.query_row("SELECT count(*) FROM tasks WHERE task_id LIKE '%/synthesize' OR task_id LIKE '%/review' OR task_id LIKE '%/preflight'", [], |r| r.get(0)).unwrap();
                assert_eq!(claims, 0);
                assert_eq!(tasks, 1);
                assert_eq!(gates, 0);
            }
            _ => unreachable!(),
        }
        if name != "milestone" {
            assert!(!root.path(".acdd/tasks.sqlite").exists(), "{name}");
        }
        assert!(!root.path(".acdd/runs").exists(), "{name}");
        std::fs::remove_file(root.path("rejected")).unwrap();
    }
}

#[test]
fn simple_workflow_resolves_registered_commands_in_a_taskless_gate_profile() {
    let harness = WorkflowHarness::simple();
    let profile = &harness.profile;
    assert_eq!(profile.persistence, Some(profile::PersistenceAction::None));
    assert_eq!(
        profile
            .gates
            .iter()
            .map(|g| g.id.as_str())
            .collect::<Vec<_>>(),
        ["lint", "test"]
    );
    for gate in &profile.gates {
        assert!(gate.tasks.is_none());
        assert_eq!(gate.driver(), Some(profile::GateDriver::Command));
        assert!(profile
            .commands
            .contains_key(gate.command.as_ref().unwrap()));
    }
}

#[test]
fn rejection_workflow_resolves_roles_and_failure_target_in_its_gate_profile() {
    let harness = WorkflowHarness::rejection_routing();
    let profile = &harness.profile;
    assert_eq!(profile.persistence, Some(profile::PersistenceAction::None));
    assert_eq!(
        profile
            .gates
            .iter()
            .map(|g| g.id.as_str())
            .collect::<Vec<_>>(),
        ["build", "review"]
    );
    for gate in &profile.gates {
        assert!(gate.tasks.is_none());
        assert_eq!(gate.driver(), Some(profile::GateDriver::Agent));
        assert!(profile.roles.contains_key(gate.role.as_ref().unwrap()));
    }
    let review = profile.by_stage("review").unwrap();
    assert_eq!(review.on_fail.as_deref(), Some("build"));
    assert_eq!(review.independent_from.as_deref(), Some("build"));
}

#[test]
fn authoring_workflow_profiles_validate_cleanly_and_enforce_standalone_authoring_invariants() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected_commands = [
        (
            "docs/workflows/author-milestone.workflow.yaml",
            "skills/acdd/references/acdd-profile-examples/author-milestone.workflow.yaml",
            "acdd milestone init {manifest_path} --in-place",
        ),
        (
            "docs/workflows/author-plan.workflow.yaml",
            "skills/acdd/references/acdd-profile-examples/author-plan.workflow.yaml",
            "acdd plan show {plan_name}",
        ),
        (
            "docs/workflows/author-workflow.workflow.yaml",
            "skills/acdd/references/acdd-profile-examples/author-workflow.workflow.yaml",
            "acdd workflow show {workflow_name}",
        ),
    ];

    for (path, skill_path, expected_cmd) in expected_commands {
        let full_path = repo_root.join(path);
        let content = std::fs::read_to_string(&full_path)
            .unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let skill_content = std::fs::read_to_string(repo_root.join(skill_path))
            .unwrap_or_else(|e| panic!("failed to read {skill_path}: {e}"));
        assert_eq!(
            content, skill_content,
            "skill example {skill_path} drifted from {path}"
        );
        for candidate in [path, skill_path] {
            let parsed =
                contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::read_workflow(
                    repo_root,
                    std::path::Path::new(candidate),
                )
                .unwrap_or_else(|e| panic!("workflow in {candidate} failed validation: {e}"));
            let preflight = parsed.find_gate("preflight").unwrap();
            let command = &parsed.commands[preflight.command.as_ref().unwrap()];
            assert!(
                command
                    .output()
                    .is_some_and(|output| !output.require_counts),
                "{candidate} preflight must allow count-free output"
            );
        }
        let profile = profile::validate_profile_document(&content)
            .unwrap_or_else(|e| panic!("profile in {path} failed validation: {e}"));

        // Invariant 1: Standalone workflows require inherit: none and save: none
        assert_eq!(
            profile.persistence,
            Some(profile::PersistenceAction::None),
            "{path} must have save: none"
        );

        // Invariant 2: Pure root gate graph without tasks: container or receipts
        assert!(
            profile.gates.iter().all(|g| g.tasks.is_none()),
            "{path} must not contain tasks"
        );
        assert!(
            profile.gates.iter().all(|g| !g.receipt),
            "{path} must not declare task receipts"
        );

        // Invariant 3: Three-stage sequence (synthesize -> review -> preflight)
        let gate_ids: Vec<&str> = profile.gates.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(gate_ids, ["synthesize", "review", "preflight"], "{path}");

        // Invariant 4: Roles architect and independent_reviewer configured with tools, models, and executable runners
        let synthesize = profile.find_gate("synthesize").unwrap();
        assert_eq!(synthesize.role.as_deref(), Some("architect"));
        assert!(
            !synthesize.tools.is_empty(),
            "{path} synthesize needs tools"
        );
        assert!(
            matches!(synthesize.proof, profile::ProofDef::Scheme { .. }),
            "{path} synthesize needs inline proof.scheme"
        );

        let review = profile.find_gate("review").unwrap();
        assert_eq!(review.role.as_deref(), Some("independent_reviewer"));
        assert_eq!(review.independent_from.as_deref(), Some("synthesize"));
        assert_eq!(review.on_fail.as_deref(), Some("synthesize"));
        assert_eq!(review.on_success.as_deref(), Some("preflight"));
        assert!(
            matches!(review.proof, profile::ProofDef::Scheme { .. }),
            "{path} review needs inline proof.scheme"
        );

        for gate in [synthesize, review] {
            let role_id = gate.role.as_deref().unwrap();
            let role = profile.roles.get(role_id).expect("gate role must exist");
            assert_eq!(
                role.reuse_on_reject,
                Some(true),
                "{path} role {role_id} must set reuse_on_reject"
            );
            assert!(
                profile::RunnerConfig::from_role(role).is_some(),
                "{path} role {role_id} must define an executable runner"
            );
        }

        let workflow_stem = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".workflow.yaml"))
            .unwrap();
        let resolved =
            contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::resolve_workflow(
                repo_root,
                workflow_stem,
            )
            .unwrap_or_else(|e| panic!("failed to resolve workflow {workflow_stem}: {e}"));
        assert_eq!(resolved, std::path::Path::new(path));

        let preflight = profile.find_gate("preflight").unwrap();
        let command_name = preflight.command.as_ref().expect("preflight command name");
        let command_def = profile
            .commands
            .get(command_name)
            .expect("preflight command definition");
        assert_eq!(
            command_def.command_for_path(""),
            Some(expected_cmd),
            "{path}"
        );
    }
}

#[test]
fn shipped_polish_workflows_validate_and_reject_invalid_definitions() {
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = Workspace::new();
    let cases = [
        "docs/workflows/code-quality-and-polish.workflow.yaml",
        "docs/workflows/test-workflow-lifecycle.workflow.yaml",
    ];
    for path in cases {
        let parsed = workflow::read_workflow(root, Path::new(path)).unwrap();
        assert_eq!(
            parsed.persistence,
            Some(profile::PersistenceAction::None),
            "{path}"
        );
        assert_eq!(
            parsed
                .gates
                .iter()
                .map(|gate| gate.id.as_str())
                .collect::<Vec<_>>(),
            ["verify", "polish", "review"]
        );
        assert_eq!(parsed.gates[0].driver(), Some(profile::GateDriver::Command));
        assert_eq!(parsed.gates[0].on_fail.as_deref(), Some("polish"));
        for gate in &parsed.gates[1..] {
            assert_eq!(gate.driver(), Some(profile::GateDriver::Agent));
            assert!(
                profile::RunnerConfig::from_role(&parsed.roles[gate.role.as_ref().unwrap()])
                    .is_some()
            );
            let profile::ProofDef::Scheme { scheme } = &gate.proof else {
                panic!("{path}: {} requires decision proof", gate.id);
            };
            for decision in ["pass", "reject"] {
                assert!(scheme
                    .validate_value(
                        &serde_json::json!({"decision": decision, "summary": "observed"}),
                        "proof"
                    )
                    .is_ok());
            }
            assert!(scheme
                .validate_value(
                    &serde_json::json!({"decision": "other", "summary": "observed"}),
                    "proof"
                )
                .is_err());
            assert_eq!(gate.max_attempts, Some(3));
        }
        assert_eq!(parsed.gates[2].independent_from.as_deref(), Some("polish"));
        assert_ne!(parsed.gates[1].role, parsed.gates[2].role);
        assert_eq!(parsed.gates[2].on_fail.as_deref(), Some("polish"));

        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let runner = "    runner: 'codex exec -C \"{worktree}\" -m \"{model}\" -c model_reasoning_effort=\"{reasoning}\" \"{prompt}\"'\n";
        for (name, changed, execute) in [
            ("save", source.replace("save: none", "save: log"), false),
            ("runner", source.replacen(runner, "", 1), true),
            (
                "independence",
                source.replace("independent_from: polish", "independent_from: missing"),
                false,
            ),
            (
                "bounds",
                source.replacen("max_attempts: 3", "max_attempts: 0", 1),
                false,
            ),
        ] {
            let candidate = "docs/workflows/candidate.yaml";
            workspace.write(candidate, &changed);
            let result = if execute {
                workflow::run_standalone_workflow(workspace.root(), Path::new(candidate), None)
                    .map(|_| ())
            } else {
                workflow::read_workflow(workspace.root(), Path::new(candidate)).map(|_| ())
            };
            assert!(result.is_err(), "{path}: {name} should fail");
        }
    }
}

#[test]
fn shipped_polish_workflow_repairs_failed_verification_and_reviews_independently() {
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow;
    use std::path::Path;

    let workspace = Workspace::new();
    let path = "docs/workflows/code-quality-and-polish.workflow.yaml";
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
    // Substitute only the external model process and the expensive repository check.
    let source = source.replace(
        "codex exec -C \"{worktree}\" -m \"{model}\" -c model_reasoning_effort=\"{reasoning}\" \"{prompt}\"",
        "python3 polish-runner.py \"{prompt}\"",
    ).replace(
        "cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets",
        "if test -f repaired; then echo 'test result: ok. 1 passed; 0 failed; 0 ignored'; else echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'; exit 1; fi",
    );
    workspace.write(path, &source);
    workspace.write(
        "polish-runner.py",
        include_str!("../fixtures/workflows/polish-runner.py"),
    );
    let result =
        workflow::run_standalone_workflow(workspace.root(), Path::new(path), None).unwrap();
    assert_eq!(result["status"], "completed");
    let outcomes = result["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0]["gate"], "verify");
    assert_eq!(outcomes[0]["passed"], false);
    assert_eq!(outcomes[1]["gate"], "polish");
    assert_eq!(outcomes[1]["passed"], true);
    assert_eq!(outcomes[2]["gate"], "review");
    assert_eq!(outcomes[2]["passed"], true);
    assert!(workspace.path("repaired").exists());
    let sessions: Vec<serde_json::Value> =
        std::fs::read_to_string(workspace.path("polish-sessions.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["gate"], "polish");
    assert_eq!(sessions[1]["gate"], "review");
    assert_ne!(sessions[0]["session"], sessions[1]["session"]);
    assert!(!workspace.path(".acdd/runs").exists());
    assert!(!workspace.path(".acdd/tasks.sqlite").exists());
}

#[test]
fn gate_driver_selects_role_before_command_and_containers() {
    let agent = WorkflowHarness::agents().profile.gates[0].clone();
    let command = WorkflowHarness::simple().profile.gates[0].clone();
    let mut both = agent.clone();
    both.command = command.command.clone();
    let mut container = agent.clone();
    container.role = None;
    container.gates.push(agent.clone());
    for (gate, expected) in [
        (agent, profile::GateDriver::Agent),
        (command, profile::GateDriver::Command),
        (both, profile::GateDriver::Agent),
        (container, profile::GateDriver::Container),
    ] {
        assert_eq!(gate.driver(), Some(expected));
    }
}

#[cfg(unix)]
#[test]
fn workflow_discovery_and_execution_accept_a_symlinked_root_ancestor() {
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow;
    use std::{os::unix::fs::symlink, path::Path};

    let workspace = Workspace::new();
    workspace.write(
        "private/var/repo/docs/workflows/ordered.yaml",
        include_str!("../fixtures/workflows/ordered.yaml"),
    );
    symlink(
        workspace.root().join("private/var"),
        workspace.root().join("var"),
    )
    .unwrap();
    let root = workspace.root().join("var/repo");
    let expected = Path::new("docs/workflows/ordered.yaml");

    assert_eq!(workflow::list_workflows(&root).unwrap(), [expected]);
    let resolved = workflow::resolve_workflow(&root, "ordered").unwrap();
    assert_eq!(resolved, expected);
    let result = workflow::run_standalone_workflow(&root, &resolved, None).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(
        std::fs::read_to_string(root.join("workflow-order.log")).unwrap(),
        "first\nsecond\n"
    );
}

#[cfg(unix)]
#[test]
fn workflow_discovery_rejects_escaping_and_cyclic_links() {
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow;
    use std::os::unix::fs::symlink;

    let workspace = WorkflowHarness::ordered().materialize();
    let link = workspace.root().join("docs/workflows/invalid.yaml");
    let outside = Workspace::new();
    outside.write(
        "external.yaml",
        include_str!("../fixtures/workflows/ordered.yaml"),
    );
    for (target, expected) in [
        (
            outside.root().join("external.yaml"),
            Some("WORKFLOW_PATH_INVALID"),
        ),
        (link.clone(), None),
    ] {
        symlink(target, &link).unwrap();
        let error = workflow::list_workflows(workspace.root()).unwrap_err();
        if let Some(expected) = expected {
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(workflow::resolve_workflow(workspace.root(), "invalid").is_err());
        std::fs::remove_file(&link).unwrap();
    }
}
