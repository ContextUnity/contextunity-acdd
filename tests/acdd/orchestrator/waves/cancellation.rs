use super::harness;
use super::run_cli;
use crate::acdd::harness::plans::PlanHarness;
use crate::acdd::harness::workflows::WorkflowHarness;
use contextunity_acdd::acdd_tasks::tasks::profile;
use std::fs;

fn configure_post_task_retry(h: &mut harness::WaveHarness, command: &str, limit: Option<u32>) {
    let mut profile = h.read_profile();
    profile.commands.insert(
        "check_full_repo".into(),
        profile::CommandDef::Script(command.into()),
    );
    let task = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let mut verify = profile.find_gate("verify").unwrap().clone();
    verify.on_fail = Some("verify".into());
    verify.on_success = None;
    verify.max_attempts = limit;
    let handoff = profile.find_gate("handoff").unwrap().clone();
    profile.gates = vec![task, verify, handoff];
    h.commit_profile(&profile, "post-task retry fixture");
}

#[cfg(unix)]
#[test]
fn milestone_post_task_signal_preserves_missing_exit_and_stops_retry() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    configure_post_task_retry(&mut h, "if test -f .acdd/post-task-signal; then printf 'test result: ok. 1 passed; 0 failed\\n'; exit 0; fi; touch .acdd/post-task-signal; kill -TERM $$", None);
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: verify"),
        "{error:#}"
    );
    let rows = h.task.store.connection.prepare("SELECT status, exit_code FROM gate_execution_traces WHERE subject='milestone' AND gate_path='verify' ORDER BY id").unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i32>>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows, [("completed".into(), None)]);
}

#[cfg(unix)]
#[test]
fn workflow_timeouts_reap_the_owned_process_group() {
    let root = WorkflowHarness::gate_profile(
        "docs/workflows/timeout.yaml",
        include_str!("../../fixtures/workflows/timeout.yaml"),
    )
    .materialize();
    let source = fs::read_to_string(root.path("docs/workflows/timeout.yaml")).unwrap();
    let cases = [
        ("timeout", source.clone()),
        (
            "unbounded-timeout",
            source.replace("    max_attempts: 1\n", ""),
        ),
    ];
    for (name, profile) in cases {
        root.write(format!("docs/workflows/{name}.yaml"), &profile);
        let started = std::time::Instant::now();
        let error = run_cli(root.root(), &["workflow", "run", name]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_COMMAND_TIMEOUT: timeout"),
            "{name}: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{name}"
        );
        let pid = fs::read_to_string(root.path("timeout.pid")).unwrap();
        let pid: libc::pid_t = pid.trim().parse().unwrap();
        assert!(pid > 0, "{name}");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "{name}: process {pid} is still alive"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "{name}"
        );
        assert!(!root.path(".acdd").exists());
        fs::remove_file(root.path("timeout.pid")).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn workflow_signal_interruption_stops_unbounded_route() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/signal.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  interrupted: "if test -f signal-visited; then exit 0; fi; touch signal-visited; kill -TERM $$"
gates:
  - id: interrupted
    command: interrupted
    fail: interrupted
"#,
    );
    let error = run_cli(root.root(), &["workflow", "run", "signal"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: interrupted"),
        "{error:#}"
    );
    assert!(root.path("signal-visited").exists());
}

#[cfg(unix)]
#[test]
fn milestone_signal_interruption_stops_unbounded_root_route() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::CommandDef;
    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    profile.commands.insert("signal_root".into(), CommandDef::Script("if test -f .acdd/signal-visited; then exit 0; fi; touch .acdd/signal-visited; kill -TERM $$".into()));
    profile.gates.insert(0, serde_json::from_value(serde_json::json!({"id":"signal_root","command":"signal_root","proof":"none","fail":"signal_root"})).unwrap());
    h.commit_profile(&profile, "signal root fixture");
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: signal_root"),
        "{error:#}"
    );
    assert!(h.root().join(".acdd/signal-visited").exists());
}

#[test]
fn plan_unbounded_timeout_stops_without_retry() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "profiles/plan.yaml",
        r#"version: 2
inherit: none
save: snapshot
commands:
  slow:
    command: "if test -f timeout-visited; then exit 0; fi; touch timeout-visited; sleep 30"
    timeout_seconds: 1
gates:
  - tasks:
      gates:
        - id: slow
          command: slow
          fail: slow
        - id: finish
          receipt: true
          save: snapshot
"#,
    );
    let started = std::time::Instant::now();
    let error = run_cli(root.root(), &["plan", "run", "linear", "--in-place"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_COMMAND_TIMEOUT: slow"),
        "{error:#}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(root.path("timeout-visited").exists());
}

#[test]
fn milestone_post_task_nonzero_exit_consumes_failure_budget() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    configure_post_task_retry(&mut h, "exit 19", Some(2));
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: verify"),
        "{error:#}"
    );
    let rows = h.task.store.connection.prepare("SELECT status, exit_code FROM gate_execution_traces WHERE subject='milestone' AND gate_path='verify' ORDER BY id").unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i32>>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        rows,
        [
            ("completed".into(), Some(19)),
            ("completed".into(), Some(19))
        ]
    );
}
