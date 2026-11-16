#![cfg(target_os = "linux")]

use crate::acdd::{support::*, tasks::harness::TaskHarness};
use acdd_tasks::{
    engine::orchestrator::{
        spawner::{
            monitor_runner_process, render_role_prompt, spawn_role_runner, RunnerSessionPolicy,
        },
        task_runner::run_task_lifecycle,
        types::{OrchestratorEvent, RunnerInvocationContext},
    },
    tasks::profile::{GateDriver, ProofKind, RunnerConfig},
};
use std::{fs, process::Command, time::Duration};

mod harness;
use harness::{LifecycleHarness, MANIFEST};

#[derive(Debug, PartialEq, Eq)]
enum SeedEntry {
    Directory(std::path::PathBuf, u32),
    File(std::path::PathBuf, Vec<u8>, u32),
    Symlink(std::path::PathBuf, std::path::PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
struct WorkspaceSeed {
    root: std::path::PathBuf,
    root_mode: u32,
    entries: Vec<SeedEntry>,
}

impl WorkspaceSeed {
    fn capture(root: &std::path::Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fn visit(
            root: &std::path::Path,
            directory: &std::path::Path,
            entries: &mut Vec<SeedEntry>,
        ) {
            let mut children = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for path in children {
                let relative = path.strip_prefix(root).unwrap().to_owned();
                // Git only appends immutable objects in these matrices. Keep the
                // object database, but snapshot all mutable Git metadata (HEAD,
                // refs, index, config and reflogs) along with the workspace.
                if relative == std::path::Path::new(".git/objects") {
                    continue;
                }
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.is_symlink() {
                    entries.push(SeedEntry::Symlink(relative, fs::read_link(path).unwrap()));
                } else if metadata.is_dir() {
                    entries.push(SeedEntry::Directory(
                        relative,
                        metadata.permissions().mode(),
                    ));
                    visit(root, &path, entries);
                } else {
                    assert!(
                        metadata.is_file(),
                        "unexpected fixture entry: {}",
                        path.display()
                    );
                    entries.push(SeedEntry::File(
                        relative,
                        fs::read(path).unwrap(),
                        metadata.permissions().mode(),
                    ));
                }
            }
        }
        let root = root.canonicalize().unwrap();
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("contextunity-test-{}-", std::process::id())));
        let mut entries = Vec::new();
        visit(&root, &root, &mut entries);
        let root_mode = fs::metadata(&root).unwrap().permissions().mode();
        Self {
            root,
            root_mode,
            entries,
        }
    }

    fn restore(&self) {
        use std::os::unix::fs::{symlink, PermissionsExt};
        fn make_directories_writable(root: &std::path::Path, path: &std::path::Path) {
            if path == root.join(".git/objects") {
                return;
            }
            let metadata = fs::symlink_metadata(path).unwrap();
            if !metadata.is_dir() || metadata.is_symlink() {
                return;
            }
            if metadata.permissions().mode() & 0o700 != 0o700 {
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
                )
                .unwrap();
            }
            for entry in fs::read_dir(path).unwrap() {
                make_directories_writable(root, &entry.unwrap().path());
            }
        }
        make_directories_writable(&self.root, &self.root);
        fn path_of(entry: &SeedEntry) -> &std::path::Path {
            match entry {
                SeedEntry::Directory(path, _)
                | SeedEntry::File(path, ..)
                | SeedEntry::Symlink(path, _) => path,
            }
        }
        let current = Self::capture(&self.root);
        let baseline_by_path: std::collections::HashMap<_, _> = self
            .entries
            .iter()
            .map(|entry| (path_of(entry), entry))
            .collect();
        let current_by_path: std::collections::HashMap<_, _> = current
            .entries
            .iter()
            .map(|entry| (path_of(entry), entry))
            .collect();
        for entry in current.entries.iter().rev() {
            let path = path_of(entry);
            let baseline = baseline_by_path.get(path).copied();
            let retain_directory = matches!(
                (entry, baseline),
                (SeedEntry::Directory(..), Some(SeedEntry::Directory(..)))
            );
            if baseline == Some(entry) || retain_directory {
                continue;
            }
            let path = self.root.join(path);
            if matches!(entry, SeedEntry::Directory(..)) {
                fs::remove_dir(path).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
        for entry in &self.entries {
            if current_by_path.get(path_of(entry)).copied() == Some(entry) {
                continue;
            }
            match entry {
                SeedEntry::Directory(path, _) => fs::create_dir_all(self.root.join(path)).unwrap(),
                SeedEntry::File(path, bytes, mode) => {
                    let path = self.root.join(path);
                    fs::write(&path, bytes).unwrap();
                    fs::set_permissions(path, fs::Permissions::from_mode(*mode)).unwrap();
                }
                SeedEntry::Symlink(path, target) => symlink(target, self.root.join(path)).unwrap(),
            }
        }
        for entry in self.entries.iter().rev() {
            if let SeedEntry::Directory(path, mode) = entry {
                fs::set_permissions(self.root.join(path), fs::Permissions::from_mode(*mode))
                    .unwrap();
            }
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(self.root_mode)).unwrap();
        assert_eq!(
            Self::capture(&self.root),
            *self,
            "complete workspace seed must be restored"
        );
    }

    fn close_store(h: &mut LifecycleHarness) {
        let store = std::mem::replace(
            &mut h.task.store,
            acdd_tasks::store::TasksStore::open_in_memory("acdd", "acdd").unwrap(),
        );
        drop(store);
    }

    fn capture_lifecycle(h: &mut LifecycleHarness) -> Self {
        Self::close_store(h);
        let seed = Self::capture(h.root());
        h.task.store = tasks::store_for_task(h.root(), &h.task.id).unwrap();
        seed
    }

    fn restore_lifecycle(&self, h: &mut LifecycleHarness) {
        assert_eq!(h.root().canonicalize().unwrap(), self.root);
        Self::close_store(h);
        self.restore();
        h.task.store = tasks::store_for_task(h.root(), &h.task.id).unwrap();
    }
}

#[test]
fn taskless_rejection_reuses_configured_role_sessions() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;

    let root = WorkflowHarness::agents().materialize();
    let mut profile: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    profile["gates"][0]["proof"] = profile["gates"][1]["proof"].clone();
    profile["gates"][0]["fail"] = json!("build");
    profile["gates"][0]["max_attempts"] = json!(4);
    profile["roles"]["builder"]["reuse_on_reject"] = json!(true);
    profile["roles"]["builder"]["runner"] =
        json!("./workflow-role-fixture workflow-runner per-gate '{prompt}'");
    profile["roles"]["reviewer"]["runner"] =
        json!("./workflow-role-fixture workflow-runner per-gate '{prompt}'");
    root.write(
        "docs/workflows/agents.yaml",
        &serde_yaml::to_string(&profile).unwrap(),
    );

    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
    assert!(result.is_ok(), "{result:#?}");
    let records: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let builders: Vec<_> = records
        .iter()
        .filter(|record| record["gate"] == "build")
        .collect();
    let reviewers: Vec<_> = records
        .iter()
        .filter(|record| record["gate"] == "review")
        .collect();
    assert_eq!(builders.len(), 3, "{records:?}");
    assert_eq!(reviewers.len(), 2, "{records:?}");
    assert_eq!(builders[0]["session"], builders[1]["session"]);
    assert_eq!(builders[1]["session"], builders[2]["session"]);
    for reviewer in reviewers {
        assert_ne!(reviewer["session"], builders[1]["session"]);
    }
}

#[test]
fn workflow_artifact_matrix() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;

    struct Case {
        profile_replacements: &'static [(&'static str, &'static str)],
        artifact_mode: Option<&'static str>,
        error: Option<&'static str>,
        arguments: Option<&'static str>,
        rejected_marker: bool,
        gates: Option<&'static [&'static str]>,
        completed: bool,
    }
    let root = WorkflowHarness::artifact_refresh().materialize();
    let profile = include_str!("../fixtures/workflows/artifact-refresh.yaml");
    let cases = [
        Case {
            profile_replacements: &[("{plan_name} {plan_slug} {artifact_path} {prerequisite}", "{artifact_path}"), ("    proof: *artifact_proof\n", "    proof: *artifact_proof\n    command: check\n"), ("    fail: synthesize", "    fail: prerequisite")],
            artifact_mode: Some("bootstrap"),
            error: Some("WORKFLOW_COMMAND_UNRESOLVED"), arguments: Some("bootstrap"), rejected_marker: true, gates: None, completed: false,
        },
        Case {
            profile_replacements: &[("id: prerequisite", "id: z_prerequisite"), ("id: synthesize", "id: a_synthesize"), ("fail: synthesize", "fail: a_synthesize")],
            artifact_mode: Some("earlier"),
            error: None, arguments: Some("new-plan new-plan new/path prerequisite"), rejected_marker: false, gates: None, completed: true,
        },
        Case {
            profile_replacements: &[], artifact_mode: None, error: None,
            arguments: Some("new-plan new-plan new/path prerequisite"), rejected_marker: false,
            gates: Some(&["prerequisite", "synthesize", "review", "synthesize", "review", "preflight"]), completed: false,
        },
        Case {
            profile_replacements: &[("{artifact_path}", "{removed}")], artifact_mode: None,
            error: Some("WORKFLOW_COMMAND_UNRESOLVED"), arguments: None, rejected_marker: false, gates: None, completed: false,
        },
        Case {
            profile_replacements: &[], artifact_mode: Some("unsafe"),
            error: Some("WORKFLOW_COMMAND_INVALID"), arguments: None, rejected_marker: false, gates: None, completed: false,
        },
        Case {
            profile_replacements: &[("    proof: *artifact_proof\n", "    proof: *artifact_proof\n    fail: preflight\n    max_attempts: 2\n")],
            artifact_mode: Some("reject-synth"),
            error: Some("WORKFLOW_COMMAND_UNRESOLVED"), arguments: None, rejected_marker: false, gates: None, completed: false,
        },
    ];
    let seed = WorkspaceSeed::capture(root.root());
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let locked = root.path("untracked/locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("child"), "fixture").unwrap();
        symlink(std::env::temp_dir(), locked.join("outside")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(root.root(), fs::Permissions::from_mode(0o555)).unwrap();
        seed.restore();
    }
    for case in cases {
        seed.restore();
        for path in [
            "synthesized",
            "reviewed",
            "synth-rejected",
            "validated-arguments",
        ] {
            if root.path(path).exists() {
                fs::remove_file(root.path(path)).unwrap();
            }
        }
        let mut source = profile.to_owned();
        for (from, to) in case.profile_replacements {
            source = source.replace(from, to);
        }
        root.write("docs/workflows/artifact-refresh.yaml", &source);
        if let Some(mode) = case.artifact_mode {
            root.write(".acdd/artifact-mode", mode);
        }
        let result = run_standalone_workflow(
            root.root(),
            Path::new("docs/workflows/artifact-refresh.yaml"),
            None,
        );
        if let Some(code) = case.error {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains(code), "{error:#}");
        } else {
            let result = result.unwrap();
            if case.completed {
                assert_eq!(result["status"], "completed");
            }
            if let Some(expected) = case.gates {
                let gates: Vec<_> = result["outcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["gate"].as_str().unwrap())
                    .collect();
                assert_eq!(gates, expected);
            }
        }
        if case.rejected_marker {
            assert!(root.path("synth-rejected").exists());
        }
        if let Some(expected) = case.arguments {
            assert_eq!(
                fs::read_to_string(root.path("validated-arguments")).unwrap(),
                expected
            );
        } else {
            assert!(!root.path("validated-arguments").exists());
        }
    }
}

#[test]
fn workflow_observed_proof_matrix() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let command = "printf 'test suite::passes ... ok\\n1 passed; 0 failed\\n'";
    let seam_profile = format!(
        "version: 2\ninherit: none\nsave: none\nroles:\n  builder: {{ runner: \"./workflow-role-fixture workflow-runner '{{prompt}}'\" }}\ncommands:\n  check:\n    command: {}\n    output:\n      counts: {{ passed: '(\\d+) passed', failed: '(\\d+) failed' }}\n      tests: {{ passed: 'test (\\S+) \\.\\.\\. ok' }}\ngates:\n  - id: build\n    role: builder\n    command: check\n    proof: seam_test\n    max_attempts: 1\n",
        serde_json::to_string(command).unwrap()
    );
    struct Case {
        profile: String,
        proof: Value,
        accepted: bool,
    }
    let seed = WorkspaceSeed::capture(root.root());
    for case in [
        Case {
            profile: include_str!("../fixtures/workflows/count-free-role.yaml").to_owned(),
            proof: json!({"command":"check","exit_code":0,"tests_passed":99,"tests_failed":0}),
            accepted: false,
        },
        Case {
            profile: seam_profile.clone(),
            proof: json!({"exit_code":0,"seam_test_refs":["suite::passes"],"tests_passed":1,"tests_failed":0}),
            accepted: true,
        },
        Case {
            profile: seam_profile.clone(),
            proof: json!({"exit_code":0,"seam_test_refs":["suite::other"],"tests_passed":1,"tests_failed":0}),
            accepted: false,
        },
        Case {
            profile: seam_profile,
            proof: json!({"exit_code":0,"seam_test_refs":["suite::passes"],"tests_passed":2,"tests_failed":0}),
            accepted: false,
        },
    ] {
        seed.restore();
        root.write("docs/workflows/agents.yaml", &case.profile);
        root.write("root-proof.json", &case.proof.to_string());
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        if case.accepted {
            assert!(result.is_ok(), "{result:#?}");
        } else {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
                "{error:#}"
            );
        }
        assert!(root.path("workflow-role-sessions.jsonl").exists());
        fs::remove_file(root.path("workflow-role-sessions.jsonl")).unwrap();
    }
}

#[test]
fn workflow_role_contract_requires_observed_runtime_red() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let seed = WorkspaceSeed::capture(root.root());
    for (command, accepted) in [
        (
            "printf 'assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            true,
        ),
        ("exit 0 # seam:tests/acdd.rs::red", false),
        (
            "printf 'error[E1234]: assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            false,
        ),
    ] {
        seed.restore();
        root.write("docs/workflows/agents.yaml", &format!(
            "version: 2\ninherit: none\nsave: none\nroles:\n  builder: {{ runner: \"./workflow-role-fixture workflow-runner '{{prompt}}'\" }}\ncommands:\n  check:\n    command: {}\n    output: {{ require_counts: false, seam_ref_pattern: 'seam:(\\S+)' }}\ngates:\n  - id: build\n    role: builder\n    command: check\n    proof: contract\n    expected_failure: {{ exit_codes: [101], stderr_contains: 'assertion failed' }}\n    max_attempts: 1\n",
            serde_json::to_string(command).unwrap()
        ));
        root.write(
            "root-proof.json",
            &json!({"seam_test_ref":"tests/acdd.rs::red","red_exit_code":101}).to_string(),
        );
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        if accepted {
            assert!(result.is_ok(), "{result:#?}");
        } else {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
                "{error:#}"
            );
        }
        assert!(root.path("workflow-role-sessions.jsonl").exists());
        fs::remove_file(root.path("workflow-role-sessions.jsonl")).unwrap();
    }
}

#[test]
fn claimed_role_process_submits_through_real_task_cli() {
    let mut h = LifecycleHarness::new();
    let mut starts = 0;
    loop {
        let gate = h
            .task
            .store
            .resolve_gate_for_task(&h.task.id, h.root().to_str().unwrap(), None)
            .unwrap();
        if gate.driver() != Some(GateDriver::Agent)
            || gate.proof.kind() == Some(ProofKind::Delivery)
        {
            break;
        }
        let claimed = h.task.claim_next().unwrap();
        let context =
            TaskHarness::manage_task(h.root(), &h.task.id, tasks::ManageAction::Context).unwrap();
        let profile = h.task.profile().unwrap();
        let role_id = gate.role.as_ref().unwrap();
        let role = &profile.roles[role_id];
        let invocation = RunnerInvocationContext {
            task_id: h.task.id.clone(),
            stage: claimed.stage.clone(),
            role: role_id.clone(),
            model: role
                .models
                .first()
                .map(|model| model.model.clone())
                .unwrap_or_default(),
            reasoning: role
                .models
                .first()
                .and_then(|model| model.reasoning.clone())
                .unwrap_or_default(),
            session_id: TaskHarness::session_for(claimed.worker_id.as_deref().unwrap()),
            prompt: render_role_prompt(&context)
                .unwrap_or_else(|error| panic!("role context: {error}: {context}")),
            worktree: h.root().to_owned(),
        };
        let config = RunnerConfig::from_role(role).unwrap();
        let process = spawn_role_runner(&config, &invocation, RunnerSessionPolicy::Resume).unwrap();
        let mut output = Vec::new();
        let outcome = monitor_runner_process(process, Duration::from_secs(20), &mut |_, bytes| {
            output.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            outcome.exit_code,
            Some(0),
            "{}: {}",
            claimed.stage,
            String::from_utf8_lossy(&output)
        );
        assert!(!outcome.timed_out);
        let after = h.task.store.inspect(&h.task.id).unwrap();
        assert_ne!(after.stage, claimed.stage);
        assert_eq!(after.status, "ready");
        assert!(!h.task.persisted_gate_evidence(&claimed.stage)["proof"].is_null());
        starts += 1;
    }
    assert!(starts > 0);
    assert_eq!(h.records().len(), starts);
}

#[test]
fn task_run_advances_profile_gates_and_records_delivery() {
    let h = LifecycleHarness::new();
    let mut events = Vec::new();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        events.push(event)
    });
    assert!(
        result.is_ok(),
        "assertion failed: configured task lifecycle must reach delivery: {result:?}"
    );
    let result = result.unwrap();
    let accepted = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(accepted.status, "completed");
    assert_eq!(result["status"], "completed");
    let receipt = accepted.receipt.unwrap();
    let delivered = receipt.commit.unwrap();
    assert_eq!(git_head(h.root()), delivered);
    assert!(delivered.starts_with(result["receipt"]["commit"].as_str().unwrap()));
    let (candidate, _) = h
        .task
        .store
        .build_snapshot_candidate(&h.task.id)
        .unwrap()
        .unwrap();
    let source = |commit: &str| {
        git_command(h.root(), &["show", &format!("{commit}:src/lib.rs")])
            .output()
            .unwrap()
    };
    let candidate_source = source(&candidate);
    let delivered_source = source(&delivered);
    assert!(candidate_source.status.success() && delivered_source.status.success());
    assert_eq!(candidate_source.stdout, delivered_source.stdout);
    let manifest = Milestone::parse(
        &fs::read_to_string(h.root().join(MANIFEST)).unwrap(),
        "acdd",
    )
    .unwrap();
    assert_eq!(manifest.tasks[0].status.as_deref(), Some("completed"));
    assert_eq!(manifest.tasks[0].receipt.as_ref().unwrap().decision, "pass");
    assert!(!h.task.persisted_gate_proof("runtime-check")["command_proof"].is_null());
    let profile = h.task.profile().unwrap();
    let records = h.records();
    assert!(records.iter().any(|record| {
        let gate = profile.by_stage(record["stage"].as_str().unwrap()).unwrap();
        gate.command.is_some() && gate.driver() == Some(GateDriver::Agent)
    }));
    for record in &records {
        let gate = profile.by_stage(record["stage"].as_str().unwrap()).unwrap();
        assert_ne!(gate.proof.kind(), Some(ProofKind::Delivery));
        assert_eq!(record["session"], record["environment_session"]);
        assert_eq!(
            h.task
                .persisted_claim_session(record["claim_revision"].as_u64().unwrap()),
            record["session"].as_str().unwrap()
        );
    }
    let builder = records
        .iter()
        .find(|record| record["stage"] == "build")
        .unwrap();
    let reviewer = records
        .iter()
        .find(|record| record["stage"] == "review")
        .unwrap();
    assert_ne!(builder["session"], reviewer["session"]);
    assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::TaskDelivered { task_id, .. } if task_id == &h.task.id)));
}

#[test]
fn task_run_resumes_persisted_cursor_without_replaying_accepted_gates() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("build").unwrap();
    let prior = h.task.persisted_gate_evidence("contract");
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(result.is_ok(), "persisted cursor must resume: {result:?}");
    assert_eq!(h.task.persisted_gate_evidence("contract"), prior);
    assert!(h
        .records()
        .iter()
        .all(|record| record["stage"] != "contract"));
    let completed = h.task.store.inspect(&h.task.id).unwrap();
    let head = git_head(h.root());
    let records = h.records();
    let mut events = Vec::new();
    let repeated = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        events.push(event)
    });
    assert!(
        repeated.is_ok(),
        "completed task must be idempotent: {repeated:?}"
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().claim_revision,
        completed.claim_revision
    );
    assert_eq!(git_head(h.root()), head);
    assert_eq!(h.records(), records);
    assert!(events.is_empty());
}

#[test]
fn task_rejection_resume_matrix() {
    struct Case {
        findings: Value,
        target: &'static str,
        persisted: bool,
    }
    let cases = [
        Case {
            findings: json!({"kind":"scope","details":[{"message":"retain structured feedback {model}","path":"src/lib.rs"}]}),
            target: "build",
            persisted: false,
        },
        Case {
            findings: json!({"kind":"symbols","details":[{"message":"retain structured feedback {model}","path":"src/lib.rs"}]}),
            target: "contract",
            persisted: false,
        },
        Case {
            findings: json!({"kind":"scope","scope_findings":[{"path":"src/lib.rs","message":"repair before delivery"}]}),
            target: "build",
            persisted: true,
        },
    ];
    let mut h = LifecycleHarness::new();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    for case in cases {
        seed.restore_lifecycle(&mut h);
        let saved = if case.persisted {
            h.task.advance_to("review").unwrap();
            let builder = h.task.persisted_gate_evidence("build");
            let session = h
                .task
                .persisted_claim_session(builder["claim_revision"].as_u64().unwrap());
            let review = h.task.claim_next().unwrap();
            let mut evidence = h.task.evidence_for_gate(&review).unwrap();
            evidence.proof = review_proof("reject");
            let reviewer_session = TaskHarness::session_for(&evidence.worker_id);
            h.task
                .submit_rejection_in_session(
                    &evidence,
                    Some(&reviewer_session),
                    Some(&case.findings),
                )
                .unwrap();
            Some((builder, session))
        } else {
            h.task.advance_to(case.target).unwrap();
            h.write_control("reject.json", &case.findings);
            None
        };
        let mut events = Vec::new();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            events.push(event)
        });
        assert!(
            result.is_ok(),
            "rejection must repair and deliver: {result:?}; role records: {:?}",
            h.records()
        );
        let records = h.records();
        if let Some((builder, session)) = saved {
            assert_eq!(records[0]["stage"], "build");
            assert_eq!(records[0]["worker_id"], builder["worker_id"]);
            assert_eq!(records[0]["session"], session);
            assert_eq!(records[0]["rejection"]["findings"], case.findings);
        } else {
            let target_runs: Vec<_> = records
                .iter()
                .filter(|record| record["stage"] == case.target)
                .collect();
            assert_eq!(target_runs.len(), 2, "{}: {records:?}", case.target);
            assert_eq!(target_runs[0]["worker_id"], target_runs[1]["worker_id"]);
            assert_eq!(target_runs[0]["session"], target_runs[1]["session"]);
            assert_eq!(target_runs[1]["rejection"]["findings"], case.findings);
            assert_eq!(target_runs[1]["rejection"]["rejected_from"], "review");
            let review_runs: Vec<_> = records
                .iter()
                .filter(|record| record["stage"] == "review")
                .collect();
            assert_eq!(review_runs.len(), 2);
            assert_ne!(review_runs[0]["session"], target_runs[1]["session"]);
            assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::GateRejected { findings: observed, .. } if observed == &case.findings)));
        }
        assert_eq!(
            h.task.store.inspect(&h.task.id).unwrap().status,
            "completed"
        );
    }
}

#[test]
fn task_command_repair_matrix() {
    struct Case {
        command: &'static str,
        proof: &'static str,
        exits: &'static [Option<i64>],
    }
    let mut h = LifecycleHarness::with_compact_profile_and_scope(
        &["src/lib.rs", "src/other.md"],
        |profile| {
            let counts = json!({"passed":"(\\d+) passed","failed":"(\\d+) failed"});
            profile.commands.insert("zero-check".into(), serde_json::from_value(json!({
            "command":"if test -e .acdd/lifecycle/fail-check; then echo 'test result: ok. 0 passed; 0 failed'; else .acdd/runner_fixture; fi",
            "output":{"require_counts":true,"counts":counts}
        })).unwrap());
            profile.commands.insert("multi-contract".into(), serde_json::from_value(json!({
            "by_pattern":{
                "src/*.rs":": --seam-ref tests/acdd/tasks.rs && ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture",
                "src/*.md":": --seam-ref tests/acdd/tasks.rs && if test -e .acdd/lifecycle/fail-check; then .acdd/runner_fixture; else ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture; fi"
            }, "output":{"require_counts":false,"seam_ref_pattern":"--seam-ref (\\S+)"}
        })).unwrap());
            profile.commands.insert("seam-repair".into(), serde_json::from_value(json!({
            "command":": target=tests/runner_fixture.rs && if test -e .acdd/lifecycle/fail-check; then echo 'test result: ok. 1 passed; 0 failed'; else .acdd/runner_fixture; fi",
            "output":{"require_counts":true,"counts":counts,"tests":{"target":"target=(tests/runner_fixture\\.rs)","passed":"test\\s+([A-Za-z0-9_:]+)\\s+\\.\\.\\.\\s+ok"}}
        })).unwrap());
            profile.commands.insert("repair-command".into(), serde_json::from_value(json!({
            "command":"if test -e .acdd/lifecycle/fail-check; then echo 'test result: FAILED. 0 passed; 1 failed'; exit 1; else .acdd/runner_fixture; fi",
            "output":{"require_counts":true,"seam_ref_pattern":"--seam-ref (\\S+)","counts":counts}
        })).unwrap());
            profile.commands.insert("repair-contract".into(), serde_json::from_value(json!({
            "command":": --seam-ref tests/acdd/tasks.rs && if test -e .acdd/lifecycle/fail-check; then .acdd/runner_fixture; else ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture; fi",
            "output":{"require_counts":false,"seam_ref_pattern":"--seam-ref (\\S+)","counts":counts}
        })).unwrap());
            profile.find_gate_mut("runtime-check").unwrap().on_fail = Some("build".into());
        },
    );
    fs::write(h.root().join("src/other.md"), "fixture\n").unwrap();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    let cases = [
        Case {
            command: "zero-check",
            proof: "command",
            exits: &[Some(0), Some(0)],
        },
        Case {
            command: "multi-contract",
            proof: "contract",
            exits: &[Some(101), Some(0), Some(101), Some(101)],
        },
        Case {
            command: "seam-repair",
            proof: "seam_test",
            exits: &[Some(0), Some(0)],
        },
        Case {
            command: "repair-command",
            proof: "command",
            exits: &[Some(1), Some(0)],
        },
        Case {
            command: "repair-contract",
            proof: "contract",
            exits: &[Some(0), Some(101)],
        },
    ];
    for case in cases {
        seed.restore_lifecycle(&mut h);
        let path = h.root().join(".acdd/profile.yaml");
        let source = fs::read_to_string(&path).unwrap();
        let mut profile = acdd_tasks::tasks::profile::validate_profile_document(&source).unwrap();
        let gate = profile.find_gate_mut("runtime-check").unwrap();
        gate.command = Some(case.command.into());
        gate.proof = serde_json::from_value(json!(case.proof)).unwrap();
        gate.expected_failure = if case.proof == "contract" {
            Some(serde_json::from_value(json!({"exit_codes":[101]})).unwrap())
        } else {
            None
        };
        fs::write(
            &path,
            format!(
                "inherit: none\n{}",
                serde_yaml::to_string(&profile).unwrap()
            ),
        )
        .unwrap();
        for args in [
            &["add", "-f", ".acdd/profile.yaml"][..],
            &["commit", "-m", "repair fixture profile"][..],
        ] {
            assert!(git_command(h.root(), args).status().unwrap().success());
        }
        fs::write(h.root().join(".acdd/lifecycle/fail-check"), "").unwrap();
        h.task.advance_to("runtime-check").unwrap();
        let previous = h.check_exits().len();
        let mut rejected = false;
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            if matches!(event, OrchestratorEvent::GateRejected { .. }) {
                rejected = true;
                fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
            }
        });
        assert!(
            result.is_ok(),
            "{}: repair must complete: {result:?}",
            case.command
        );
        assert!(rejected, "{}", case.command);
        assert_eq!(&h.check_exits()[previous..], case.exits, "{}", case.command);
    }
}

#[test]
fn failed_observation_is_not_reused_for_same_claim_after_repair() {
    let mut h = LifecycleHarness::with_repairable_check();
    h.task.advance_to("runtime-check").unwrap();
    h.task.claim_next().unwrap();
    let failed = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(failed["evidence"].is_null());
    assert_eq!(h.check_exits(), vec![Some(1)]);
    fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
    let passed = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(!passed["evidence"].is_null(), "{passed}");
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
    let cached = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(!cached["evidence"].is_null());
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
}

#[test]
fn command_rejection_survives_store_reopen_before_repair() {
    let mut h = LifecycleHarness::with_repairable_check();
    h.task.advance_to("build").unwrap();
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            if matches!(event, OrchestratorEvent::GateRejected { .. }) {
                panic!("simulate coordinator restart after rejection");
            }
        });
    }));
    assert!(stopped.is_err());
    let reopened = tasks::store_for_task(h.root(), &h.task.id).unwrap();
    assert_eq!(reopened.inspect(&h.task.id).unwrap().status, "ready");
    assert_eq!(h.check_exits(), vec![Some(1)]);
    drop(reopened);
    fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(
        result.is_ok(),
        "reopened rejection must recover: {result:?}"
    );
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
}

#[test]
fn task_first_rejection_matrix() {
    let mut h = LifecycleHarness::with_profile(|profile| {
        profile.find_gate_mut("contract").unwrap().on_fail = Some("contract".into());
        profile.commands.insert("seam-fixture".into(), serde_json::from_value(json!({
            "command": ": target=tests/runner_fixture.rs && .acdd/runner_fixture",
            "output": {
                "require_counts": true,
                "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"},
                "tests": {"target": "target=(tests/runner_fixture\\.rs)", "passed": "test\\s+([A-Za-z0-9_:]+)\\s+\\.\\.\\.\\s+ok"}
            }
        })).unwrap());
        let gates = &mut profile
            .gates
            .iter_mut()
            .find_map(|gate| gate.tasks.as_mut())
            .unwrap()
            .gates;
        let mut polish = gates
            .iter()
            .find(|gate| gate.id == "build")
            .unwrap()
            .clone();
        polish.id = "polish".into();
        polish.on_fail = Some("polish".into());
        let mut integrate = polish.clone();
        integrate.id = "integrate".into();
        integrate.on_fail = Some("integrate".into());
        integrate.proof = serde_json::from_value(json!("seam_test")).unwrap();
        integrate.command = Some("seam-fixture".into());
        gates.retain(|gate| gate.id != "seam");
        gates.retain(|gate| gate.id != "runtime-check");
        let review = gates.iter().position(|gate| gate.id == "review").unwrap();
        gates.insert(review, polish);
        gates.insert(
            review + 1,
            acdd_tasks::tasks::profile::GateDef {
                id: "seam".into(),
                gates: vec![integrate],
                ..Default::default()
            },
        );
    });
    let first = h.task.id.clone();
    let second = h.add_second_case();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    struct Case {
        stage: &'static str,
        nested: bool,
        reject_build: bool,
        message: &'static str,
    }
    let cases = [
        Case {
            stage: "polish",
            nested: false,
            reject_build: false,
            message: "first attempt",
        },
        Case {
            stage: "seam.integrate",
            nested: true,
            reject_build: false,
            message: "first attempt",
        },
        Case {
            stage: "contract",
            nested: false,
            reject_build: true,
            message: "retry first attempt",
        },
    ];
    for case in cases {
        seed.restore_lifecycle(&mut h);
        h.task.id = if case.nested {
            second.clone()
        } else {
            first.clone()
        };
        h.task.advance_to(case.stage).unwrap();
        let claim = h.task.claim_next().unwrap();
        let details = h.task.store.inspect_details(&h.task.id).unwrap();
        assert!(
            details["gates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|gate| gate["stage"] != case.stage || gate["state"] != "passed"),
            "{}: no prior pass is allowed",
            case.stage
        );
        let evidence = if case.nested {
            let context =
                TaskHarness::manage_task(h.root(), &h.task.id, tasks::ManageAction::Context)
                    .unwrap();
            serde_json::from_value(
                context["workflow_guidance"]["expected_evidence_template"].clone(),
            )
            .unwrap()
        } else {
            h.task.evidence_for_gate(&claim).unwrap()
        };
        let session = TaskHarness::session_for(&evidence.worker_id);
        let findings = json!({"kind":"repair","message":case.message});
        h.task
            .submit_rejection_in_session(&evidence, Some(&session), Some(&findings))
            .unwrap();
        let root = h.root().to_owned();
        let task_id = h.task.id.clone();
        let options = h.options.clone();
        let mut build_session = None;
        let result = run_task_lifecycle(&root, &task_id, &options, &mut |event| {
            if case.reject_build
                && matches!(event, OrchestratorEvent::RunnerExited { ref stage, .. } if stage == "contract_review")
            {
                let claim = h.task.claim_next().unwrap();
                let evidence = h.task.evidence_for_gate(&claim).unwrap();
                let session = TaskHarness::session_for(&evidence.worker_id);
                h.task
                    .submit_rejection_in_session(&evidence, Some(&session), Some(&findings))
                    .unwrap();
                build_session = Some(session);
            }
        });
        assert!(result.is_ok(), "{} must recover: {result:?}", case.stage);
        let records = h.records();
        let run = records
            .iter()
            .find(|record| record["stage"] == case.stage)
            .unwrap();
        assert_ne!(run["session"], session, "{}", case.stage);
        assert_eq!(run["rejection"]["findings"], findings, "{}", case.stage);
        if case.reject_build {
            let run = records
                .iter()
                .find(|record| record["stage"] == "build")
                .unwrap();
            assert_ne!(run["session"], build_session.unwrap(), "build");
            assert_eq!(run["rejection"]["findings"], findings, "build");
            assert_eq!(result.unwrap()["status"], "completed");
        }
    }
}

#[test]
fn task_runner_admission_matrix() {
    struct Case {
        corrupt: Option<&'static str>,
        diagnostic: &'static str,
    }
    let cases = [
        Case { corrupt: None, diagnostic: "TASK_ALREADY_CLAIMED" },
        Case { corrupt: Some("UPDATE task_claims SET session_fingerprint=NULL WHERE task_id=?1 AND revision=?2"), diagnostic: "TASK_RESUME_IDENTITY_MISSING" },
        Case { corrupt: Some("UPDATE task_claims SET worker_id='inconsistent-worker' WHERE task_id=?1 AND revision=?2"), diagnostic: "TASK_RESUME_IDENTITY_MISSING" },
    ];
    let mut h = LifecycleHarness::new();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    for case in cases {
        seed.restore_lifecycle(&mut h);
        let mut identity = None;
        let foreign = if let Some(corrupt) = case.corrupt {
            h.task.advance_to("review").unwrap();
            let claim = h.task.claim_next().unwrap();
            let mut evidence = h.task.evidence_for_gate(&claim).unwrap();
            evidence.proof = review_proof("reject");
            let reviewer_session = TaskHarness::session_for(&evidence.worker_id);
            h.task
                .submit_rejection_in_session(
                    &evidence,
                    Some(&reviewer_session),
                    Some(&json!({"kind":"scope","message":"repair"})),
                )
                .unwrap();
            let revision = h.task.persisted_gate_evidence("build")["claim_revision"]
                .as_u64()
                .unwrap();
            let (worker, session): (String, Option<String>) = h.task.store.connection.query_row(
                "SELECT worker_id, session_fingerprint FROM task_claims WHERE task_id=?1 AND revision=?2",
                rusqlite::params![h.task.id, revision], |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
            h.task
                .store
                .connection
                .execute(corrupt, rusqlite::params![h.task.id, revision])
                .unwrap();
            identity = Some((worker, session, revision));
            None
        } else {
            Some(h.task.claim_next().unwrap())
        };
        let error = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {}).unwrap_err();
        assert!(error.to_string().contains(case.diagnostic), "{error:#}");
        if let Some((worker, session, revision)) = identity {
            h.task.store.connection.execute(
                "UPDATE task_claims SET worker_id=?1, session_fingerprint=?2 WHERE task_id=?3 AND revision=?4",
                rusqlite::params![worker, session, h.task.id, revision],
            ).unwrap();
        }
        if let Some(foreign) = foreign {
            let retained = h.task.store.inspect(&h.task.id).unwrap();
            assert_eq!(retained.claim_revision, foreign.claim_revision);
            assert_eq!(retained.worker_id, foreign.worker_id);
            assert_eq!(retained.worktree, foreign.worktree);
            assert_eq!(retained.status, "in_progress");
            assert!(h.records().is_empty());
        }
    }
}

#[test]
fn task_runner_outcome_matrix() {
    struct Case {
        mode: &'static str,
        exit: Option<i32>,
        timed_out: bool,
        no_submission: bool,
    }
    let mut h = LifecycleHarness::new();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    for case in [
        Case {
            mode: "no-submit",
            exit: Some(0),
            timed_out: false,
            no_submission: true,
        },
        Case {
            mode: "exit-17",
            exit: Some(17),
            timed_out: false,
            no_submission: false,
        },
        Case {
            mode: "sleep",
            exit: None,
            timed_out: true,
            no_submission: false,
        },
    ] {
        seed.restore_lifecycle(&mut h);
        h.options.runner_timeout = if case.timed_out {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(20)
        };
        h.write_control("mode.json", &json!(case.mode));
        let mut events = Vec::new();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            events.push(event)
        });
        assert!(
            result.is_err(),
            "{} must stop execution; an exit code is not accepted gate evidence",
            case.mode
        );
        assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::RunnerExited { exit_code, timed_out, .. } if *exit_code == case.exit && *timed_out == case.timed_out)));
        let task = h.task.store.inspect(&h.task.id).unwrap();
        assert_ne!(task.status, "completed");
        assert!(task.receipt.is_none());
        if case.no_submission {
            assert_eq!(task.status, "in_progress");
            assert_eq!(h.records().len(), 1);
            assert!(h.task.store.inspect_details(&h.task.id).unwrap()["gates"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn task_run_delivery_preserves_subtask_guard() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("deliver").unwrap();
    TaskHarness::claim_in_worktree(
        h.root(),
        &h.task.id,
        "",
        &h.options.worker_id,
        h.root(),
        Some(&h.options.session_fingerprint),
    )
    .unwrap();
    let head = git_head(h.root());
    let manifest = fs::read(h.root().join(MANIFEST)).unwrap();
    let candidate = h.task.store.build_snapshot_candidate(&h.task.id).unwrap();
    let revision = h.task.store.inspect(&h.task.id).unwrap().claim_revision;
    h.task
        .store
        .subtask_update(&h.task.id, "verify", "ready", Some(""))
        .unwrap();
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    for (status, evidence, expected) in [
        ("ready", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("in_progress", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("blocked", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("completed", None, "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("completed", Some(""), "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("completed", Some("   "), "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("pending", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
    ] {
        seed.restore_lifecycle(&mut h);
        if status == "pending" {
            h.task.set_all_subtasks_legacy_pending().unwrap();
            h.task
                .store
                .subtask_update(&h.task.id, "optional", "cancelled", None)
                .unwrap();
        } else {
            h.task
                .store
                .subtask_update(&h.task.id, "verify", status, evidence)
                .unwrap();
        }
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
        let error = result.unwrap_err().to_string();
        assert!(error.contains(expected), "{status}/{evidence:?}: {error}");
        let task = h.task.store.inspect(&h.task.id).unwrap();
        assert_eq!(task.status, "in_progress");
        assert_eq!(task.claim_revision, revision);
        assert!(task.receipt.is_none());
        assert_eq!(git_head(h.root()), head);
        assert_eq!(fs::read(h.root().join(MANIFEST)).unwrap(), manifest);
        assert_eq!(
            h.task.store.build_snapshot_candidate(&h.task.id).unwrap(),
            candidate
        );
    }
}

#[test]
fn task_run_delivery_withholds_receipt_on_candidate_drift() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("deliver").unwrap();
    let head = git_head(h.root());
    let candidate = h.task.store.build_snapshot_candidate(&h.task.id).unwrap();
    let manifest = fs::read(h.root().join(MANIFEST)).unwrap();
    let changed = "pub fn example() -> bool { false }\n";
    fs::write(h.root().join("src/lib.rs"), changed).unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    let error = result.unwrap_err().to_string();
    assert!(error.contains("TASK_RECEIPT_MISSING"), "{error}");
    let pending = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(pending.stage, "build");
    assert!(pending.receipt.is_none());
    assert_eq!(git_head(h.root()), head);
    assert_eq!(
        h.task.persisted_gate_evidence("build")["commit"],
        candidate.unwrap().0
    );
    assert_eq!(fs::read(h.root().join(MANIFEST)).unwrap(), manifest);
    assert_eq!(
        fs::read_to_string(h.root().join("src/lib.rs")).unwrap(),
        changed
    );
}

#[test]
fn task_run_cli_accepts_in_place_and_uses_lifecycle() {
    let h = LifecycleHarness::new();
    let output = Command::new(env!("CARGO_BIN_EXE_acdd"))
        .args([
            "--root",
            h.root().to_str().unwrap(),
            "--format",
            "json",
            "task",
            "run",
            &h.task.id,
            "--in-place",
        ])
        .env("ACDD_AGENT_SESSION_ID", "fixture-cli-coordinator")
        .current_dir(h.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "task run --in-place: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "completed");
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert!(!h.records().is_empty());
}

#[test]
fn malformed_emitted_review_templates_are_rejected_by_task_submission() {
    let mut h = LifecycleHarness::with_compact_profile_and_scope(&["src/lib.rs"], |_| {});
    let cases = [
        (
            "corrupt-template",
            "missing review decision must be rejected",
        ),
        ("wrong-wrapper", "wrong proof wrapper must be rejected"),
    ];
    let seed = WorkspaceSeed::capture_lifecycle(&mut h);
    for (marker, reason) in cases {
        seed.restore_lifecycle(&mut h);
        let path = h.root().join(".acdd/lifecycle").join(marker);
        let log = h.root().join(".acdd/lifecycle/roles.jsonl");
        if log.exists() {
            fs::remove_file(log).unwrap();
        }
        fs::write(&path, "").unwrap();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
        assert!(result.is_err(), "{reason}: {result:?}");
        let task = h.task.store.inspect(&h.task.id).unwrap();
        assert_ne!(task.status, "completed", "{marker}");
        assert_eq!(task.stage, "review", "{marker}");
        assert!(
            h.records().iter().any(|record| record["stage"] == "review"),
            "{marker}: review runner must execute"
        );
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn renamed_workflow_gate_preserves_role_proof_contract() {
    let root = crate::acdd::harness::workflows::WorkflowHarness::agents().materialize();
    let path = root.path("docs/workflows/agents.yaml");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("id: review", "id: inspection");
    fs::write(path, source).unwrap();
    let result = TaskHarness::run_cli_for_root(
        root.root(),
        &["--format", "json", "workflow", "run", "agents"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["status"], "completed");
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    assert!(records
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["gate"] == "inspection"));
}

#[test]
fn workflow_role_decision_framing_and_consistency() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let seed = WorkspaceSeed::capture(root.root());
    for (mode, prepass, succeeds) in [
        ("bare", true, true),
        ("fenced", true, true),
        ("array", true, false),
        ("fenced-array", true, false),
        ("duplicate", true, false),
        ("fenced-duplicate", true, false),
        ("nested-match", false, true),
        ("nested-conflict", false, false),
        ("nested-sibling-match", false, true),
        ("nested-sibling-conflict", false, false),
        ("embedded", false, true),
        ("fenced-embedded", false, true),
        ("mismatch", true, false),
        ("identity", true, false),
        ("missing", false, false),
        ("empty", false, false),
        ("conflict", false, false),
        ("trailing", true, false),
        ("incomplete", true, false),
        ("multiple", true, false),
    ] {
        seed.restore();
        for path in ["review-rejected", "workflow-role-sessions.jsonl"] {
            if root.path(path).exists() {
                fs::remove_file(root.path(path)).unwrap();
            }
        }
        fs::write(root.path("role-mode"), mode).unwrap();
        if prepass {
            fs::write(root.path("review-rejected"), "").unwrap();
        }
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        assert_eq!(result.is_ok(), succeeds, "{mode}: {result:?}");
        if matches!(
            mode,
            "embedded" | "fenced-embedded" | "nested-match" | "nested-sibling-match"
        ) {
            let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
            assert!(records
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|record| record["gate"] == "build"
                    && record["findings"]["step_findings"][0] == "Repair this step"));
        }
    }
}

#[test]
fn workflow_role_command_matrix() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let path = root.path("docs/workflows/agents.yaml");
    let baseline = fs::read_to_string(&path).unwrap();
    struct Case {
        command: &'static str,
        accepted: bool,
    }
    let seed = WorkspaceSeed::capture(root.root());
    for case in [
        Case { command: "  check:\n    command: 'touch command-observed'\n    output: { require_counts: false }\n", accepted: true },
        Case { command: "  check: 'touch command-observed; exit 1'\n", accepted: false },
    ] {
        seed.restore();
        let source = baseline
            .replace("roles:\n", &format!("commands:\n{}roles:\n", case.command))
            .replace("  - id: build\n    role: builder", "  - id: build\n    role: builder\n    command: check\n    proof: none");
        fs::write(&path, source).unwrap();
        if case.accepted { fs::write(root.path("review-rejected"), "").unwrap(); }
        let result = run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        if case.accepted {
            assert!(result.is_ok(), "{result:?}");
        } else {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"), "{error:#}");
        }
        assert!(root.path("command-observed").exists());
        let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
        let gates: Vec<_> = records.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()["gate"].as_str().unwrap().to_owned()).collect();
        assert!(gates.iter().any(|gate| gate == "build"));
        if !case.accepted { assert_eq!(gates, ["build"]); }
        fs::remove_file(root.path("command-observed")).unwrap();
        fs::remove_file(root.path("workflow-role-sessions.jsonl")).unwrap();
        if root.path("review-rejected").exists() { fs::remove_file(root.path("review-rejected")).unwrap(); }
    }
}
