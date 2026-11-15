use crate::acdd::{support::*, tasks::harness::TaskHarness};
use acdd_tasks::{
    engine::orchestrator::{task_runner::TaskRunOptions, waves::MilestoneRunOptions},
    tasks::profile::GateDef,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) const MANIFEST: &str = "docs/milestones/011-wave.md";

pub(super) fn selected_settings(
    root: &Path,
    config: &str,
    profile: &str,
    database: &str,
) -> acdd_tasks::tasks::configuration::InvocationSettings {
    acdd_tasks::tasks::configuration::InvocationSettings::new(
        root.to_owned(),
        Some(root.join(config)),
        std::sync::Arc::new(
            acdd_tasks::tasks::profile::validate_profile_document(
                &fs::read_to_string(root.join(profile)).unwrap(),
            )
            .unwrap(),
        ),
        Some(root.join(profile)),
        root.join(database),
    )
}

pub(super) struct WaveHarness {
    pub task: TaskHarness,
    pub options: MilestoneRunOptions,
    pub verification_marker: PathBuf,
}

impl WaveHarness {
    pub fn new(in_place: bool) -> Self {
        Self::build(in_place, false, false)
    }

    pub fn with_single_task(in_place: bool) -> Self {
        Self::build(in_place, true, false)
    }

    pub fn with_blueprint_gate(in_place: bool) -> Self {
        Self::build(in_place, true, true)
    }

    pub fn with_nested_blueprint(in_place: bool) -> Self {
        let mut harness = Self::with_blueprint_gate(in_place);
        let mut profile = harness.read_profile();
        profile.gates[0] = serde_json::from_value(json!({
            "id": "blueprint_synthesis",
            "gates": [
                {"id": "synthesize", "role": "wave_role", "proof": "blueprint", "max_attempts": 3},
                {"id": "review", "role": "wave_role", "independent_from": "synthesize",
                 "proof": {"scheme": {"type": "object", "required": ["decision", "summary"],
                     "properties": {"decision": {"type": "string"}, "summary": {"type": "string"}}}},
                 "fail": "synthesize", "max_attempts": 3}
            ]
        })).unwrap();
        harness.commit_profile(&profile, "nested blueprint fixture");
        harness
    }

    fn build(in_place: bool, single_task: bool, blueprint_gate: bool) -> Self {
        let mut profile = runner_profile(DEFAULT_SEAM, "");
        let verification_marker = std::env::temp_dir().join(format!(
            "forge-wave-verification-{}-{}.marker",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        match profile.commands.get_mut("check_full_repo") {
            Some(acdd_tasks::tasks::profile::CommandDef::Detailed { command, .. }) => {
                command.push_str(&format!(
                    " && printf verified > '{}'",
                    verification_marker.display()
                ));
            }
            _ => panic!("wave fixture must register a detailed verification command"),
        }
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        for role in profile.roles.values_mut() {
            role.guidance = Some("Exercise the real task gate protocol.".into());
            role.runner = Some(format!(
                "python3 {} {} {} {{session_id}} '{{prompt}}'",
                quote(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/acdd/fixtures/workflows/wave-task-role.py"
                )),
                quote(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/acdd/orchestrator/lifecycle/role_fixture.py"
                )),
                quote(env!("CARGO_BIN_EXE_acdd"))
            ));
        }
        profile.roles.insert("wave_role".into(), serde_json::from_value(json!({"runner":format!("python3 {} '{{prompt}}'", quote(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py"))),"guidance":"Exercise the real task gate protocol."})).unwrap());
        let mut tasks = profile
            .gates
            .iter()
            .find(|g| g.tasks.is_some())
            .unwrap()
            .clone();
        for gate in &mut tasks.tasks.as_mut().unwrap().gates {
            gate.guidance.clear();
        }
        let task_containers = vec![tasks];
        let polish: GateDef = serde_json::from_value(
            json!({"id":"polish","proof":"none","role":"wave_role","max_attempts":2}),
        )
        .unwrap();
        let review: GateDef = serde_json::from_value(json!({"id":"milestone_review","role":"wave_role","independent_from":"polish","proof":{"scheme":{"type":"object","required":["decision","summary"],"properties":{"decision":{"type":"string"},"summary":{"type":"string"}}}},"fail":"polish","max_attempts":2})).unwrap();
        let mut verify = profile.find_gate("verify").unwrap().clone();
        verify.role = None;
        verify.command = Some("check_full_repo".into());
        verify.proof = acdd_tasks::tasks::profile::ProofDef::Kind(
            acdd_tasks::tasks::profile::ProofKind::Command,
        );
        let verify_again: GateDef = serde_json::from_value(
            json!({"id":"verify_again","command":"rust-check-and-test","proof":"command"}),
        )
        .unwrap();
        let handoff: GateDef = serde_json::from_value(
            json!({"id":"handoff","proof":"delivery","receipt":true,"save":"snapshot","auto_commit":false}),
        )
        .unwrap();
        profile.gates = if blueprint_gate {
            vec![serde_json::from_value(json!({
                "id": "preflight",
                "role": "wave_role",
                "proof": "blueprint",
                "max_attempts": 1
            }))
            .unwrap()]
        } else {
            Vec::new()
        };
        profile.gates.extend(task_containers);
        profile
            .gates
            .extend([polish, review, verify, verify_again, handoff]);
        profile.validate().unwrap();
        let builder = MilestoneFixtureBuilder::new("m-wave").task("first", &["src/lib.rs"]);
        let source = if single_task {
            builder.build()
        } else {
            builder
                .task_with_deps("second", &["src/second.rs"], &["first"])
                .build()
        };
        let task = TaskHarness::disk_with_profile(&source, MANIFEST, profile);
        assert_eq!(
            task.profile()
                .unwrap()
                .find_gate("build")
                .and_then(|gate| gate.role.as_deref()),
            Some("builder"),
            "the wave fixture keeps the inherited builder role when it binds a command"
        );
        fs::create_dir_all(task.root().join(".acdd/lifecycle")).unwrap();
        fs::write(task.root().join("src/second.rs"), "pub fn second() {}\n").unwrap();
        // Inputs needed by isolated child worktrees must be in their shared base.
        for args in [
            vec!["add", "src/second.rs"],
            vec!["commit", "-m", "wave fixture inputs"],
        ] {
            assert!(git_command(task.root(), &args).status().unwrap().success());
        }
        let exclude = task.root().join(".git/info/exclude");
        let mut ignored = fs::read_to_string(&exclude).unwrap_or_default();
        ignored.push_str("\n.worktrees/\n");
        fs::write(exclude, ignored).unwrap();
        let options = MilestoneRunOptions {
            task: TaskRunOptions {
                worktree: task.root().to_owned(),
                worker_id: "wave-coordinator".into(),
                session_fingerprint: "fixture-wave-coordinator".into(),
                runner_timeout: Duration::from_secs(20),
            },
            in_place,
            target_base: git_head(task.root()),
        };
        Self {
            task,
            options,
            verification_marker,
        }
    }

    pub fn root(&self) -> &Path {
        self.task.root()
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
        self.options.target_base = git_head(self.root());
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
        let path = self.root().join(".acdd/profile.yaml");
        let mut profile = acdd_tasks::tasks::profile::validate_profile_document(
            &fs::read_to_string(&path).unwrap(),
        )
        .unwrap();
        profile.commands.insert(
            "preflight".into(),
            serde_json::from_value(json!({"command": "printf seen >> .acdd/preflight.log", "output": {"require_counts": false}})).unwrap(),
        );
        profile.gates.insert(
            0,
            serde_json::from_value(json!({"id":"preflight","command":"preflight","proof":"none"}))
                .unwrap(),
        );
        if stop {
            profile.commands.insert(
                "stop-before-tasks".into(),
                acdd_tasks::tasks::profile::CommandDef::Script("exit 31".into()),
            );
            profile.gates.insert(
                1,
                serde_json::from_value(
                    json!({"id":"stop_before_tasks","command":"stop-before-tasks","proof":"none","max_attempts":1}),
                )
                .unwrap(),
            );
        }
        self.commit_profile(&profile, "pre-task observation fixture");
    }

    pub fn partial_pre_task_checks(&mut self, retry: bool) {
        self.pre_task_checks(true);
        let path = self.root().join(".acdd/profile.yaml");
        let mut profile = acdd_tasks::tasks::profile::validate_profile_document(
            &fs::read_to_string(&path).unwrap(),
        )
        .unwrap();
        let mut command = "printf seen >> .acdd/second-preflight.log; if [ ! -e .acdd/preflight-mutation-done ]; then printf '\\n// initial pre-task output\\n' >> src/lib.rs; touch .acdd/preflight-mutation-done; fi".to_owned();
        if retry {
            command.push_str("; if [ ! -e .acdd/preflight-retry-done ]; then touch .acdd/preflight-retry-done; exit 17; fi");
            let stop = profile
                .gates
                .iter_mut()
                .find(|gate| gate.id == "stop_before_tasks")
                .unwrap();
            stop.on_fail = Some("observe_once".into());
            stop.max_attempts = Some(2);
        }
        profile.commands.insert(
            "observe-once".into(),
            serde_json::from_value(
                json!({"command": command, "output": {"require_counts": false}}),
            )
            .unwrap(),
        );
        profile.gates.insert(1, serde_json::from_value(json!({"id":"observe_once","command":"observe-once","proof":"none","max_attempts":2,"fail":"observe_once"})).unwrap());
        self.commit_profile(&profile, "partial-prefix observation fixture");
    }
}

impl Drop for WaveHarness {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.verification_marker);
    }
}

#[test]
fn malformed_blueprint_guidance_cannot_be_repaired_by_role_fixture() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = WaveHarness::with_blueprint_gate(true);
    for marker in [
        "corrupt-blueprint-missing",
        "corrupt-blueprint-tasks",
        "corrupt-blueprint-nested",
    ] {
        fs::write(h.root().join(".acdd").join(marker), "").unwrap();
        let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
        assert!(result.is_err(), "{marker}: malformed guidance must fail");
        let passes: i64 = h.task.store.connection.query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='preflight' AND action='pass'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(passes, 0, "{marker}: no malformed blueprint may pass");
        fs::remove_file(h.root().join(".acdd").join(marker)).unwrap();
    }
}
