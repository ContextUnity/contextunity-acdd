use super::harness::{milestone_profile, pre_task_profile, MANIFEST};
use crate::acdd::{support::*, tasks::harness::TaskHarness};
use acdd_tasks::{
    engine::orchestrator::{task_runner::TaskRunOptions, waves::MilestoneRunOptions},
    tasks::profile::GateDef,
};
use std::{fs, path::Path, time::Duration};

/// Root claim/dispatch APIs resolve manifests and profiles through the filesystem registry.
/// Keep that real seam without provisioning Python roles, wave options, or external markers.
pub(super) struct RootGateHarness {
    pub task: TaskHarness,
}

impl RootGateHarness {
    pub fn new() -> Self {
        Self::build(false)
    }

    pub fn with_dependencies() -> Self {
        Self::build(true)
    }

    fn build(with_dependencies: bool) -> Self {
        let builder = MilestoneFixtureBuilder::new("m-wave").task("first", &["src/lib.rs"]);
        let source = if with_dependencies {
            builder
                .task_with_deps("second", &["src/second.rs"], &["first"])
                .build()
        } else {
            builder.build()
        };
        Self {
            task: TaskHarness::disk_with_profile(&source, MANIFEST, milestone_profile(false)),
        }
    }

    pub fn root(&self) -> &Path {
        self.task.root()
    }

    pub fn run_options(&self) -> MilestoneRunOptions {
        MilestoneRunOptions {
            task: TaskRunOptions {
                worktree: self.root().to_owned(),
                worker_id: "wave-coordinator".into(),
                session_fingerprint: "fixture-wave-coordinator".into(),
                runner_timeout: Duration::from_secs(20),
            },
            in_place: true,
            target_base: git_head(self.root()),
        }
    }

    pub(super) fn read_profile(&self) -> acdd_tasks::tasks::profile::GateProfile {
        let path = self.root().join(".acdd/profile.yaml");
        acdd_tasks::tasks::profile::validate_profile_document(&fs::read_to_string(&path).unwrap())
            .unwrap()
    }

    pub(super) fn commit_profile(
        &mut self,
        profile: &acdd_tasks::tasks::profile::GateProfile,
        message: &str,
    ) {
        profile.validate().unwrap();
        let path = self.root().join(".acdd/profile.yaml");
        fs::write(
            path,
            format!("inherit: none\n{}", serde_yaml::to_string(profile).unwrap()),
        )
        .unwrap();
        for args in [
            vec!["add", "-f", ".acdd/profile.yaml"],
            vec!["commit", "-m", message],
        ] {
            assert!(git_command(self.root(), &args).status().unwrap().success());
        }
    }

    pub fn with_blueprint_synthesis_root_gates(&mut self) {
        let path = self.root().join(".acdd/profile.yaml");
        let mut profile = acdd_tasks::tasks::profile::validate_profile_document(
            &fs::read_to_string(&path).unwrap(),
        )
        .unwrap();
        let task_container = profile
            .gates
            .iter()
            .find(|gate| gate.tasks.is_some())
            .unwrap()
            .clone();
        let synthesize: GateDef = serde_json::from_value(json!({
            "id": "synthesize",
            "role": "wave_role",
            "proof": {
                "scheme": {
                    "type": "object",
                    "required": ["tasks"],
                    "properties": {
                        "tasks": {"type": "object"}
                    }
                }
            }
        }))
        .unwrap();
        let review: GateDef = serde_json::from_value(json!({
            "id": "blueprint_review",
            "role": "wave_role",
            "proof": "none",
            "independent_from": "synthesize",
            "fail": "synthesize"
        }))
        .unwrap();
        let blueprint_synthesis: GateDef = serde_json::from_value(json!({
            "id": "blueprint_synthesis",
            "gates": [synthesize, review]
        }))
        .unwrap();
        let polish: GateDef =
            serde_json::from_value(json!({"id": "polish", "role": "wave_role", "proof": "none"}))
                .unwrap();
        let handoff = profile
            .gates
            .iter()
            .find(|gate| gate.id == "handoff")
            .unwrap()
            .clone();
        profile.gates = vec![blueprint_synthesis, task_container, polish, handoff];
        self.commit_profile(&profile, "fixture gate profile");
    }
    pub fn pre_task_checks(&mut self, stop: bool) {
        let profile = pre_task_profile(self.read_profile(), stop);
        self.commit_profile(&profile, "pre-task observation fixture");
    }
}
