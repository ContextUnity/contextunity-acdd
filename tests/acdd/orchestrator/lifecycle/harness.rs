use crate::acdd::{support::*, tasks::harness::TaskHarness};
use forge_tasks::{
    engine::orchestrator::task_runner::TaskRunOptions,
    tasks::profile::{GateDef, ProofKind},
};
use std::{fs, path::Path, time::Duration};

pub(super) const MANIFEST: &str = "docs/milestones/010-lifecycle.md";

pub(super) struct LifecycleHarness {
    pub task: TaskHarness,
    pub options: TaskRunOptions,
}

impl LifecycleHarness {
    pub fn new() -> Self {
        Self::with_profile(|_| {})
    }

    pub fn with_profile(
        configure: impl FnOnce(&mut forge_tasks::tasks::profile::GateProfile),
    ) -> Self {
        Self::with_profile_and_scope(&["src/lib.rs"], configure)
    }

    pub fn with_profile_and_scope(
        scope: &[&str],
        configure: impl FnOnce(&mut forge_tasks::tasks::profile::GateProfile),
    ) -> Self {
        let mut profile = runner_profile(DEFAULT_SEAM, "");
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/acdd/orchestrator/lifecycle/role_fixture.py"
        );
        let executable = env!("CARGO_BIN_EXE_contextunity-forge-mcp");
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        for role in profile.roles.values_mut() {
            role.guidance =
                Some("Execute the external fixture protocol using real task operations.".into());
            role.runner = Some(format!(
                "python3 {} {} {{session_id}} '{{prompt}}'",
                quote(fixture),
                quote(executable)
            ));
            role.prompt_template =
                Some("Execute {role} for {task_id} at {stage} in {worktree}. {goal}".into());
        }
        let block = profile
            .gates
            .iter_mut()
            .find_map(|gate| gate.tasks.as_mut())
            .unwrap();
        for gate in &mut block.gates {
            gate.guidance.clear();
        }
        let delivery = block
            .gates
            .iter()
            .position(|gate| gate.proof.kind() == Some(ProofKind::Delivery))
            .unwrap();
        let check: GateDef = serde_json::from_value(json!({
            "id": "runtime-check",
            "command": "rust-check-and-test",
            "proof": "command"
        }))
        .unwrap();
        block.gates.insert(delivery, check);
        configure(&mut profile);
        let source = MilestoneFixtureBuilder::new("m-headless-lifecycle")
            .title("Task lifecycle execution")
            .task("run", scope)
            .task_target("Execute the configured task lifecycle")
            .task_proof_policy("seam-test-first")
            .task_subtasks(&[
                contextunity_forge_mcp::core::tasks::SubtaskSpec {
                    subtask_ref: "verify".into(),
                    title: "Verify task behavior".into(),
                    status: "completed".into(),
                    evidence: Some("canonical task seam verified".into()),
                },
                contextunity_forge_mcp::core::tasks::SubtaskSpec {
                    subtask_ref: "optional".into(),
                    title: "Explicitly cancelled work".into(),
                    status: "cancelled".into(),
                    evidence: None,
                },
            ])
            .build();
        let task = TaskHarness::disk_with_profile(&source, MANIFEST, profile);
        fs::create_dir_all(task.root().join(".forge/lifecycle")).unwrap();
        fs::write(
            task.root().join("src/lib.rs"),
            "pub fn example() -> bool { true }\n",
        )
        .unwrap();
        let options = TaskRunOptions {
            worktree: task.root().to_owned(),
            worker_id: "lifecycle-coordinator".into(),
            session_fingerprint: "fixture-coordinator-session".into(),
            runner_timeout: Duration::from_secs(20),
        };
        Self { task, options }
    }

    pub fn records(&self) -> Vec<Value> {
        fs::read_to_string(self.task.root().join(".forge/lifecycle/roles.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn with_repairable_check() -> Self {
        let h = Self::with_profile(|profile| {
            profile.commands.insert("repairable-check".into(), serde_json::from_value(json!({
                "command": "if test -e .forge/lifecycle/fail-check; then echo 'test result: FAILED. 0 passed; 1 failed'; exit 1; else .forge/runner_fixture; fi",
                "output": {"require_counts": true, "seam_ref_pattern": "--seam-ref (\\S+)", "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
            })).unwrap());
            let gate = profile.find_gate_mut("runtime-check").unwrap();
            gate.command = Some("repairable-check".into());
            gate.on_fail = Some("build".into());
        });
        fs::write(h.root().join(".forge/lifecycle/fail-check"), "").unwrap();
        h
    }

    pub fn add_second_case(&mut self) -> String {
        let subtasks = [
            contextunity_forge_mcp::core::tasks::SubtaskSpec {
                subtask_ref: "verify".into(),
                title: "Verify task behavior".into(),
                status: "completed".into(),
                evidence: Some("canonical task seam verified".into()),
            },
            contextunity_forge_mcp::core::tasks::SubtaskSpec {
                subtask_ref: "optional".into(),
                title: "Explicitly cancelled work".into(),
                status: "cancelled".into(),
                evidence: None,
            },
        ];
        let source = MilestoneFixtureBuilder::new("m-headless-second")
            .title("Task lifecycle execution")
            .task("run", &["src/lib.rs"])
            .task_target("Execute the configured task lifecycle")
            .task_proof_policy("seam-test-first")
            .task_subtasks(&subtasks)
            .build();
        let path = "docs/milestones/011-lifecycle-case.md";
        fs::write(self.root().join(path), &source).unwrap();
        for args in [
            &["add", path][..],
            &["commit", "-m", "second lifecycle fixture"][..],
        ] {
            git_command(self.root(), args)
                .status()
                .unwrap()
                .success()
                .then_some(())
                .expect("second lifecycle fixture must be committed");
        }
        let milestone = Milestone::parse(&source, "forge-mcp").unwrap();
        let id = milestone.task_id(&milestone.tasks[0]);
        let root = self.root().to_owned();
        self.task.store.sync(&milestone, path, &root).unwrap();
        id
    }

    pub fn check_exits(&self) -> Vec<Option<i64>> {
        self.task
            .store
            .execution_traces(&self.task.id, Some("tasks.runtime-check"))
            .unwrap()
            .into_iter()
            .map(|trace| trace.exit_code.map(i64::from))
            .collect()
    }

    pub fn write_control(&self, name: &str, value: &Value) {
        fs::write(
            self.task.root().join(".forge/lifecycle").join(name),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    pub fn root(&self) -> &Path {
        self.task.root()
    }
}
