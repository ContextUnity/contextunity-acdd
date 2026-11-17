//! Phase 2 use-case & seam matrix tests (`v7` §6.1 Phase 2).

use super::{
    execute_on, queries, StateCommand, StateCommandOutput, StateEngine, StateEngineConfig,
    StateQuery, StateQueryEngine, StateQueryOutput,
};
use crate::git_ops::commit::current_head_commit;
use crate::harness::RunHarness;
use crate::profile::resolve_profile_ref;
use crate::spec::materialize_ephemeral_goal_spec;
use crate::store::traits::{StateBackend, StateReader};
use crate::types::{
    BlackboardTopic, ClaimOutcome, FindingItem, FindingSeverity, GateClaimRecord, GateNodeRecord,
    InvariantsState, IsolationMode, NodeKind, NodeStatus, NodeStatusPatch, NodeVersionExpectation,
    RejectionFindings, ReviewContourResult, ReviewEvidence, RunGraphSnapshot, RunRecord, RunStatus,
    RunTombstoneRecord, StoragePreconditions, SubmitAction, SubtaskRecord, SubtaskStatus,
    TerminalStatus, TraceStartInput, TransitionPlan,
};
use crate::usecases::dto::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, ClaimReq, ClearReq, DraftReq,
    ExtendScopeReq, IntegrateReq, ListReq, NextReq, ObservabilityReq, ReopenReq, RollbackReq,
    RunReq, ShowReq, SnapshotReq, StartReq, StatusReq, SubmitReq, SubtaskReq, SyncReq, VerifyReq,
};
use acdd_core::config::SpecCollectionConfig;
use acdd_core::response::ResponsePolicy;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{CommitOid, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_core::ports::{
    ActionCapability, CheckCapability, CompositeDomainProvider, DomainActionReq, DomainActionResult,
    DomainCapabilities, DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
    DomainToolDef, DomainToolOutput, DomainToolReq,
};
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

struct MockHttpProvider {
    tool_calls: AtomicUsize,
    action_calls: AtomicUsize,
    lookup_calls: AtomicUsize,
}

impl MockHttpProvider {
    fn new() -> Self {
        Self {
            tool_calls: AtomicUsize::new(0),
            action_calls: AtomicUsize::new(0),
            lookup_calls: AtomicUsize::new(0),
        }
    }

    fn tool_calls(&self) -> usize {
        self.tool_calls.load(Ordering::SeqCst)
    }

    fn action_calls(&self) -> usize {
        self.action_calls.load(Ordering::SeqCst)
    }

    fn lookup_calls(&self) -> usize {
        self.lookup_calls.load(Ordering::SeqCst)
    }
}

impl DomainProvider for MockHttpProvider {
    fn domain_id(&self) -> &str {
        "http"
    }

    fn capabilities(&self) -> DomainCapabilities {
        let mut probe_props = BTreeMap::new();
        probe_props.insert("url".to_owned(), SchemaDef::string());
        let mut probe_schema = SchemaDef::object();
        probe_schema.properties = probe_props;
        probe_schema.required = vec!["url".to_owned()];
        probe_schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

        let mut dispatch_props = BTreeMap::new();
        dispatch_props.insert("target".to_owned(), SchemaDef::string());
        let mut dispatch_schema = SchemaDef::object();
        dispatch_schema.properties = dispatch_props;
        dispatch_schema.required = vec!["target".to_owned()];

        let mut context_lenses = BTreeSet::new();
        context_lenses.insert("endpoint_summary".to_owned());

        DomainCapabilities {
            actions: vec![
                ActionCapability {
                    name: "dispatch".to_owned(),
                    input_schema: dispatch_schema.clone(),
                    mutating: true,
                    supports_result_lookup: true,
                },
                ActionCapability {
                    name: "blind_mutate".to_owned(),
                    input_schema: dispatch_schema,
                    mutating: true,
                    supports_result_lookup: false,
                },
            ],
            checks: vec![CheckCapability {
                name: "headers".to_owned(),
                input_schema: SchemaDef::object(),
            }],
            context_lenses,
            tools: vec![DomainToolDef {
                name: "probe".to_owned(),
                description: "Probe HTTP endpoint metadata".to_owned(),
                input_schema: probe_schema,
                read_only: true,
            }],
        }
    }

    fn execute_action(
        &self,
        action: &str,
        params: &Value,
        idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        self.action_calls.fetch_add(1, Ordering::SeqCst);
        Ok(DomainActionResult {
            summary: format!("http.{action} executed"),
            output: json!({
                "idempotency_key": idempotency_key,
                "params": params,
            }),
        })
    }

    fn lookup_action_result(
        &self,
        action: &str,
        idempotency_key: &str,
    ) -> Result<Option<DomainActionResult>> {
        self.lookup_calls.fetch_add(1, Ordering::SeqCst);
        if action == "dispatch" {
            Ok(Some(DomainActionResult {
                summary: format!("recovered {action}"),
                output: json!({ "recovered_key": idempotency_key }),
            }))
        } else {
            Ok(None)
        }
    }

    fn run_check(
        &self,
        _check: &str,
        _params: &Value,
        _req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        Ok(DomainCheckOutcome::Passed {
            summary: "headers valid".to_owned(),
            execution_digest_salt: Some("http-salt-v1".to_owned()),
        })
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> Result<Value> {
        Ok(json!({ "lens": lens, "status": "ok" }))
    }

    fn call_tool(
        &self,
        tool: &str,
        args: &Value,
        _req: &DomainToolReq,
    ) -> Result<DomainToolOutput> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        Ok(DomainToolOutput::Json(json!({
            "tool": tool,
            "url": args.get("url").cloned().unwrap_or(Value::Null),
            "status": 200,
        })))
    }
}

fn make_temp_dir(label: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(&format!("acdd-usecases-{label}-"))
        .tempdir()
        .expect("create tempdir")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} in '{}' failed:\nstdout: {}\nstderr: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn init_git_repo(dir: &Path, branch: &str) -> CommitOid {
    fs::create_dir_all(dir).expect("create repo dir");
    git_ok(dir, &["init", "-b", branch]);
    git_ok(dir, &["config", "user.email", "test@contextunity.local"]);
    git_ok(dir, &["config", "user.name", "ACDD Test"]);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("README.md"), "# Test Repo\n").unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\n").unwrap();
    fs::write(dir.join(".gitignore"), ".acdd/\n.worktrees/\n").unwrap();
    git_ok(dir, &["add", "README.md", "src/lib.rs", ".gitignore"]);
    git_ok(dir, &["commit", "-m", "initial commit"]);
    current_head_commit(dir).expect("initial head commit")
}

fn default_collections() -> BTreeMap<String, SpecCollectionConfig> {
    let mut map = BTreeMap::new();
    map.insert(
        "milestones".to_owned(),
        SpecCollectionConfig {
            path: "docs/milestones".to_owned(),
            profile: Some("workspace".to_owned()),
            prefixes: vec!["AC".to_owned(), "M".to_owned()],
            digits: 3,
        },
    );
    map
}

fn base_config(root: &Path) -> StateEngineConfig {
    StateEngineConfig {
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        state_db_override: None,
        code_db_path: None,
        spec_collections: default_collections(),
        profiles_dirs: Vec::new(),
        linked_workspaces: BTreeMap::new(),
        modules: SubsystemRegistry::default(),
        response_policy: ResponsePolicy::default(),
        retain_traces: 5,
    }
}

fn build_provider_with_mock(
    modules: &SubsystemRegistry,
) -> (Arc<MockHttpProvider>, Arc<CompositeDomainProvider>) {
    let mock = Arc::new(MockHttpProvider::new());
    let mut flags = modules.domains.clone();
    flags.insert("code".to_owned(), modules.is_enabled("code"));
    flags.insert("docs".to_owned(), modules.is_enabled("docs"));
    flags.entry("http".to_owned()).or_insert(true);
    let composite = CompositeDomainProvider::with_module_flags(
        vec![mock.clone() as Arc<dyn DomainProvider>],
        flags,
    )
    .expect("build CompositeDomainProvider");
    (mock, Arc::new(composite))
}

fn build_rich_graph(root: &Path, run_id_str: &str, isolation: IsolationMode) -> RunGraphSnapshot {
    let profile = resolve_profile_ref("default", None, None).expect("resolve default profile");
    let profile_json = serde_json::to_string(&profile).expect("serialize profile");
    let run_id = RunId::new(run_id_str).unwrap();
    let run_name = RunName::new("sample-run").unwrap();
    let t1 = TaskRef::new("t1").unwrap();
    let t2 = TaskRef::new("t2").unwrap();
    let spec_rel = "docs/milestones/AC-001-sample.md";
    let spec_content = materialize_ephemeral_goal_spec(
        &run_id,
        &run_name,
        "Sample goal",
        &profile.profile_id,
        &["src/**".to_owned()],
        &[],
    )
    .expect("render spec");
    let _ = fs::create_dir_all(root.join("docs/milestones"));
    let _ = fs::write(root.join(spec_rel), &spec_content);

    let dummy_tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
    let run = RunRecord {
        run_id: run_id.clone(),
        run_name,
        collection: Some("milestones".to_owned()),
        workspace: "default".to_owned(),
        profile_id: profile.profile_id,
        profile_json,
        spec_path: Some(spec_rel.to_owned()),
        spec_digest: "deadbeef".repeat(8),
        is_ephemeral: false,
        isolation,
        target_base: None,
        target_base_commit: None,
        branch: None,
        worktree: Some(root.display().to_string()),
        trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
        status: RunStatus::Active,
        cancellation_reason: None,
        root_cycle_count: 0,
        generation: 1,
        created_at: "2026-11-17T10:00:00.000Z".to_owned(),
        completed_at: None,
    };

    let make_node = |node_id: &str,
                     parent_id: Option<&str>,
                     kind: NodeKind,
                     task_ref: Option<TaskRef>,
                     gate_id: &str,
                     seq_order: u32,
                     status: NodeStatus,
                     scope: Vec<String>| GateNodeRecord {
        run_id: run_id.clone(),
        node_id: node_id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        kind,
        task_ref,
        gate_id: gate_id.to_owned(),
        seq_order,
        status,
        node_version: 1,
        isolation,
        attempt: 0,
        cycle_count: 0,
        max_attempts: 3,
        claim_revision: 0,
        contract_revision: 1,
        worker_id: None,
        session_fingerprint: None,
        branch: None,
        worktree: Some(root.display().to_string()),
        baseline_commit: None,
        claim_tree: Some(dummy_tree.clone()),
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["default".to_owned()],
        invariants: InvariantsState::default(),
        scope,
        extended_scope: Vec::new(),
        active_span_id: None,
    };

    let nodes = vec![
        make_node(
            "tasks#0",
            None,
            NodeKind::Container,
            None,
            "tasks#0",
            0,
            NodeStatus::Ready,
            Vec::new(),
        ),
        make_node(
            "task:t1",
            Some("tasks#0"),
            NodeKind::Task,
            Some(t1.clone()),
            "__task__",
            1,
            NodeStatus::Ready,
            vec!["crates/a/**".to_owned()],
        ),
        make_node(
            "task:t1:build",
            Some("task:t1"),
            NodeKind::TaskGate,
            Some(t1.clone()),
            "build",
            2,
            NodeStatus::Ready,
            vec!["crates/a/**".to_owned()],
        ),
        make_node(
            "task:t1:review",
            Some("task:t1"),
            NodeKind::TaskGate,
            Some(t1),
            "review",
            3,
            NodeStatus::Pending,
            vec!["crates/a/**".to_owned()],
        ),
        make_node(
            "task:t2",
            Some("tasks#0"),
            NodeKind::Task,
            Some(t2.clone()),
            "__task__",
            4,
            NodeStatus::Pending,
            vec!["crates/b/**".to_owned()],
        ),
        make_node(
            "task:t2:build",
            Some("task:t2"),
            NodeKind::TaskGate,
            Some(t2.clone()),
            "build",
            5,
            NodeStatus::Pending,
            vec!["crates/b/**".to_owned()],
        ),
        make_node(
            "root:handoff",
            None,
            NodeKind::RootGate,
            None,
            "handoff",
            6,
            NodeStatus::Pending,
            Vec::new(),
        ),
    ];

    RunGraphSnapshot {
        run,
        nodes,
        dependencies: vec![crate::types::TaskDependencyRecord {
            run_id: run_id.clone(),
            task_ref: t2,
            depends_on_workspace: "default".to_owned(),
            depends_on_run_id: run_id.clone(),
            depends_on_ref: TaskRef::new("t1").unwrap(),
            satisfied: false,
        }],
        subtasks: vec![SubtaskRecord {
            run_id,
            node_id: "task:t1".to_owned(),
            subtask_ref: SubtaskRef::new("s1").unwrap(),
            seq_order: 0,
            title: "Implement seam".to_owned(),
            status: SubtaskStatus::Pending,
            evidence: None,
            updated_at: "2026-11-17T10:00:00.000Z".to_owned(),
        }],
        claims: Vec::new(),
        traces: Vec::new(),
        artifacts: Vec::new(),
    }
}

#[test]
fn seam_readonly_queries_matrix_and_zero_db_creation() {
    let empty_tmp = make_temp_dir("empty-ro");
    let cfg = base_config(empty_tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let ro = StateQueryEngine::open_read_only(cfg, provider.clone()).unwrap();

    let empty_queries = [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Show(ShowReq {
            run: None,
            node: None,
        }),
        StateQuery::Next(NextReq::default()),
        StateQuery::BlackboardRead(BlackboardReadReq::default()),
        StateQuery::Observability(ObservabilityReq {
            include_prometheus: true,
        }),
    ];

    for (idx, query) in empty_queries.into_iter().enumerate() {
        let res = ro.query(query);
        match idx {
            2 | 4 => assert!(
                matches!(res, Err(ref e) if e.code == ErrorCode::RunSelectorAmbiguous),
                "expected RunSelectorAmbiguous on empty workspace for query #{idx}, got {res:?}"
            ),
            _ => assert!(res.is_ok(), "query #{idx} failed on empty workspace: {res:?}"),
        }
    }
    assert!(
        !empty_tmp.path().join(".acdd/state.sqlite").exists(),
        "read-only queries must never create state.sqlite"
    );
    assert!(
        !empty_tmp.path().join(".acdd/state.bak.sqlite").exists(),
        "read-only queries must never create state.bak.sqlite"
    );

    let harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
    let ws = harness.workspace_root().unwrap().to_path_buf();
    let mut cfg_pop = base_config(&ws);
    cfg_pop.state_db_override = Some(harness.db_path().unwrap().to_path_buf());
    let snap = build_rich_graph(&ws, "run-ro-001", IsolationMode::InPlace);
    harness.seed_run(&snap).unwrap();

    let ro_pop = StateQueryEngine::open_read_only(cfg_pop, provider).unwrap();
    match ro_pop.query(StateQuery::Status(StatusReq::default())).unwrap() {
        StateQueryOutput::Status(s) => {
            assert_eq!(s.run.unwrap().run_id.as_str(), "run-ro-001");
            assert!(s.ready_nodes.contains(&"t1.build".to_owned()));
        }
        other => panic!("unexpected Status output: {other:?}"),
    }
    match ro_pop.query(StateQuery::Next(NextReq::default())).unwrap() {
        StateQueryOutput::Next(n) => {
            assert_eq!(n.ready_cursors, vec!["t1.build".to_owned()]);
            assert_eq!(n.waves, vec![vec!["t1".to_owned()], vec!["t2".to_owned()]]);
        }
        other => panic!("unexpected Next output: {other:?}"),
    }
    match ro_pop
        .query(StateQuery::Show(ShowReq {
            run: Some("run-ro-001".to_owned()),
            node: Some("t1.build".to_owned()),
        }))
        .unwrap()
    {
        StateQueryOutput::Show(s) => {
            assert_eq!(s.run.run_id.as_str(), "run-ro-001");
            assert_eq!(s.node.unwrap().node_id, "task:t1:build");
            assert!(s.context_preview.is_some());
        }
        other => panic!("unexpected Show output: {other:?}"),
    }
    match ro_pop
        .query(StateQuery::Observability(ObservabilityReq {
            include_prometheus: true,
        }))
        .unwrap()
    {
        StateQueryOutput::Observability(o) => {
            assert_eq!(o.snapshot.active_runs, 1);
            assert!(o
                .prometheus_text
                .as_deref()
                .is_some_and(|t| t.contains("acdd_runs_active 1")));
        }
        other => panic!("unexpected Observability output: {other:?}"),
    }
}

#[test]
fn seam_modules_matrix_enforces_feature_disabled_and_domain_capabilities() {
    let tmp = make_temp_dir("modules-matrix");
    let mut cfg = base_config(tmp.path());
    cfg.modules.disabled.insert("state".to_owned());
    let (_, provider) = build_provider_with_mock(&cfg.modules);

    let open_err = StateEngine::open(cfg.clone(), provider.clone())
        .err()
        .expect("StateEngine::open must fail when state=false");
    assert_eq!(open_err.code, ErrorCode::FeatureDisabled);
    assert!(!tmp.path().join(".acdd/state.sqlite").exists());

    let ro = StateQueryEngine::open_read_only(cfg.clone(), provider.clone()).unwrap();
    for query in [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Next(NextReq::default()),
        StateQuery::BlackboardRead(BlackboardReadReq::default()),
        StateQuery::Observability(ObservabilityReq::default()),
    ] {
        assert_eq!(ro.query(query).unwrap_err().code, ErrorCode::FeatureDisabled);
    }

    let harness = RunHarness::in_memory().unwrap();
    let cmd_err = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "blocked".to_owned(),
            prefix: None,
            title: None,
            profile: None,
        }),
    )
    .unwrap_err();
    assert_eq!(cmd_err.code, ErrorCode::FeatureDisabled);

    let mut bb_cfg = base_config(tmp.path());
    bb_cfg.modules.disabled.insert("blackboard".to_owned());
    let (_, bb_provider) = build_provider_with_mock(&bb_cfg.modules);
    assert_eq!(
        queries::execute_query(
            &bb_cfg,
            &harness,
            &bb_provider,
            StateQuery::BlackboardRead(BlackboardReadReq::default()),
        )
        .unwrap_err()
        .code,
        ErrorCode::FeatureDisabled
    );
    assert!(queries::execute_query(
        &bb_cfg,
        &harness,
        &bb_provider,
        StateQuery::Status(StatusReq::default()),
    )
    .is_ok());
    assert_eq!(
        execute_on(
            &bb_cfg,
            &harness,
            &bb_provider,
            StateCommand::BlackboardPost(BlackboardPostReq {
                run: None,
                node: None,
                recipient_node: None,
                subtask_ref: None,
                gate: None,
                author: "builder".to_owned(),
                topic: BlackboardTopic::Notes,
                payload: "note".to_owned(),
            }),
        )
        .unwrap_err()
        .code,
        ErrorCode::FeatureDisabled
    );

    let mut no_cd_cfg = base_config(tmp.path());
    no_cd_cfg.modules.disabled.insert("code".to_owned());
    no_cd_cfg.modules.disabled.insert("docs".to_owned());
    let (_, no_cd_provider) = build_provider_with_mock(&no_cd_cfg.modules);
    assert!(!no_cd_provider.is_domain_enabled("code"));
    assert!(!no_cd_provider.is_domain_enabled("docs"));
    assert!(no_cd_provider.is_domain_enabled("http"));
    assert_eq!(
        no_cd_provider
            .validate_check_ref("code.graph_fresh", &json!({}))
            .unwrap_err()
            .code,
        ErrorCode::DomainCapabilityUnavailable
    );

    let mut no_http_cfg = base_config(tmp.path());
    no_http_cfg.modules.domains.insert("http".to_owned(), false);
    let (_, no_http_provider) = build_provider_with_mock(&no_http_cfg.modules);
    let tool_err = execute_on(
        &no_http_cfg,
        &harness,
        &no_http_provider,
        StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: "http_probe".to_owned(),
            args: json!({ "url": "https://example.invalid" }),
            run: None,
            node: None,
            worktree: None,
        }),
    )
    .unwrap_err();
    assert_eq!(tool_err.code, ErrorCode::DomainCapabilityUnavailable);
}

#[test]
fn seam_mock_domain_provider_plugin_tool_and_gate_tools_matrix() {
    let tmp = make_temp_dir("plugin-tool");
    let cfg = base_config(tmp.path());
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();

    struct ToolCase {
        name: &'static str,
        tool_name: &'static str,
        args: Value,
        expect_err: Option<ErrorCode>,
        expect_calls_delta: usize,
    }

    let cases = [
        ToolCase {
            name: "valid_probe",
            tool_name: "http_probe",
            args: json!({ "url": "https://example.invalid/health" }),
            expect_err: None,
            expect_calls_delta: 1,
        },
        ToolCase {
            name: "missing_required_url",
            tool_name: "http_probe",
            args: json!({}),
            expect_err: Some(ErrorCode::ProfileSchemaInvalid),
            expect_calls_delta: 0,
        },
        ToolCase {
            name: "unexpected_extra_property",
            tool_name: "http_probe",
            args: json!({ "url": "https://x", "extra": 42 }),
            expect_err: Some(ErrorCode::ProfileSchemaInvalid),
            expect_calls_delta: 0,
        },
        ToolCase {
            name: "unknown_plugin_tool",
            tool_name: "http_nonexistent",
            args: json!({}),
            expect_err: Some(ErrorCode::DomainCapabilityUnavailable),
            expect_calls_delta: 0,
        },
    ];

    for case in cases {
        let before = mock.tool_calls();
        let res = engine.execute(StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: case.tool_name.to_owned(),
            args: case.args,
            run: None,
            node: None,
            worktree: None,
        }));
        let after = mock.tool_calls();
        assert_eq!(
            after - before,
            case.expect_calls_delta,
            "unexpected mock invocation delta for case `{}`",
            case.name
        );
        match case.expect_err {
            None => match res.unwrap() {
                StateCommandOutput::CallPluginTool(out) => {
                    assert_eq!(out.tool_name, case.tool_name);
                    assert_eq!(
                        out.output,
                        DomainToolOutput::Json(json!({
                            "tool": "probe",
                            "url": "https://example.invalid/health",
                            "status": 200,
                        }))
                    );
                }
                other => panic!("unexpected output for `{}`: {other:?}", case.name),
            },
            Some(expected_code) => {
                assert_eq!(res.unwrap_err().code, expected_code, "case `{}`", case.name);
            }
        }
    }

    let profile_dir = tmp.path().join(".acdd/profiles");
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("plugin_ok.yaml"),
        r#"version: 2
inherit: default
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output:
      require_counts: false
  check:
    command: "true"
  lint:
    command: "true"
gates:
  - id: probe_gate
    proof: none
    tools: [http_probe]
  - tasks:
      gates:
        - id: build
          proof: none
"#,
    )
    .unwrap();
    fs::write(
        profile_dir.join("plugin_bad.yaml"),
        r#"version: 2
inherit: default
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output:
      require_counts: false
  check:
    command: "true"
  lint:
    command: "true"
gates:
  - id: probe_gate
    proof: none
    tools: [http_missing_tool]
  - tasks:
      gates:
        - id: build
          proof: none
"#,
    )
    .unwrap();

    let bad_start = engine
        .execute(StateCommand::Start(StartReq {
            goal: Some("Check unknown plugin tool".to_owned()),
            profile: Some(".acdd/profiles/plugin_bad.yaml".to_owned()),
            ..StartReq::default()
        }))
        .unwrap_err();
    assert_eq!(bad_start.code, ErrorCode::DomainCapabilityUnavailable);

    let ok_start = engine
        .execute(StateCommand::Start(StartReq {
            goal: Some("Check enabled plugin tool".to_owned()),
            profile: Some(".acdd/profiles/plugin_ok.yaml".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap();
    assert!(matches!(ok_start, StateCommandOutput::Start(_)));
}

#[test]
fn seam_draft_sequential_numbering_and_archive_scan() {
    let tmp = make_temp_dir("draft");
    let milestones_dir = tmp.path().join("docs/milestones");
    fs::create_dir_all(milestones_dir.join("archive")).unwrap();
    fs::write(milestones_dir.join("AC-003-alpha.md"), "# AC-003\n").unwrap();
    fs::write(milestones_dir.join("archive/AC-007-omega.md"), "# AC-007\n").unwrap();

    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();

    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "new-feature".to_owned(),
            prefix: None,
            title: Some("New Feature".to_owned()),
            profile: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Draft(d) => {
            assert_eq!(d.spec_id, "AC-008");
            assert_eq!(d.spec_path, "docs/milestones/AC-008-new-feature.md");
            assert!(tmp.path().join(&d.spec_path).is_file());
        }
        other => panic!("unexpected draft output: {other:?}"),
    }

    let bad_slug = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(),
            slug: "---".to_owned(),
            prefix: None,
            title: None,
            profile: None,
        }),
    )
    .unwrap_err();
    assert_eq!(bad_slug.code, ErrorCode::ProfileSchemaInvalid);
}

#[test]
fn seam_start_ephemeral_goal_and_cross_workspace_depends_on_matrix() {
    let root_tmp = make_temp_dir("start-main");
    let other_tmp = make_temp_dir("start-other");

    let mut cfg = base_config(root_tmp.path());
    cfg.linked_workspaces
        .insert("other".to_owned(), other_tmp.path().to_path_buf());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider.clone()).unwrap();

    fs::create_dir_all(root_tmp.path().join(".acdd/profiles")).unwrap();
    fs::write(
        root_tmp.path().join(".acdd/profiles/bound.yaml"),
        r#"version: 2
inherit: default
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output:
      require_counts: false
  build_check:
    command: "true"
  verify_all:
    command: "true"
"#,
    )
    .unwrap();

    let both_err = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-001.md".to_owned()),
            goal: Some("Also goal".to_owned()),
            ..StartReq::default()
        }))
        .unwrap_err();
    assert_eq!(both_err.code, ErrorCode::ProfileSchemaInvalid);

    let StateCommandOutput::Start(start_resp) = engine
        .execute(StateCommand::Start(StartReq {
            goal: Some("Ship ephemeral run".to_owned()),
            profile: Some(".acdd/profiles/bound.yaml".to_owned()),
            scope: vec!["crates/a/**".to_owned()],
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    assert!(start_resp.run.is_ephemeral);
    let mat = start_resp.materialized_spec_path.unwrap();
    assert!(root_tmp.path().join(&mat).is_file());
    assert!(root_tmp
        .path()
        .join(format!(".acdd/run/{}/profile.resolved.yaml", start_resp.run.run_id))
        .is_file());

    engine
        .execute(StateCommand::Clear(ClearReq {
            run: Some(start_resp.run.run_id.to_string()),
            reason: Some("cleanup before next start".to_owned()),
            force: true,
        }))
        .unwrap();

    fs::create_dir_all(root_tmp.path().join("docs/milestones")).unwrap();
    let dep_spec = r#"---
id: AC-010
title: Dependent Run
status: planned
depends_on:
  - "other/AC-001:t1"
---
### task: t1

```yaml
task_ref: t1
scope: [crates/a/**]
acceptance_criteria:
  - Consumer works
```
"#;
    fs::write(root_tmp.path().join("docs/milestones/AC-010-dep.md"), dep_spec).unwrap();

    let other_cfg = base_config(other_tmp.path());
    let other_engine = StateEngine::open(other_cfg, provider).unwrap();
    let other_graph_1 = build_rich_graph(other_tmp.path(), "run-other-001", IsolationMode::InPlace);
    let cleared_tombstone = RunTombstoneRecord {
        run_id: RunId::new("run-other-001").unwrap(),
        run_name: RunName::new("other-cleared").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "default".to_owned(),
        spec_id: Some("AC-001".to_owned()),
        title: "Producer Cleared".to_owned(),
        profile_id: "default".to_owned(),
        is_ephemeral: false,
        generation: 1,
        terminal_status: TerminalStatus::Cleared,
        target_base: None,
        target_base_commit: None,
        archive_path: None,
        closed_commit: None,
        cleanup_pending: false,
        cleanup_manifest: Vec::new(),
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-17T09:10:00.000Z".to_owned(),
    };
    other_engine
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&other_graph_1)?;
            tx.archive_and_prune_run(
                &StoragePreconditions {
                    expected_run_generation: 1,
                    expected_node_versions: Vec::new(),
                    expected_claim_revision: None,
                    expected_contract_revision: None,
                    expected_worker_id: None,
                    required_predecessor_trees: Vec::new(),
                },
                &cleared_tombstone,
            )
        })
        .unwrap();

    let blocked = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()),
            profile: Some(".acdd/profiles/bound.yaml".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap_err();
    assert_eq!(blocked.code, ErrorCode::GateVerificationFailed);

    let mut other_graph_2 = build_rich_graph(other_tmp.path(), "run-other-002", IsolationMode::InPlace);
    other_graph_2.run.run_name = RunName::new("other-completed").unwrap();
    let completed_tombstone = RunTombstoneRecord {
        run_id: RunId::new("run-other-002").unwrap(),
        run_name: RunName::new("other-completed").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "default".to_owned(),
        spec_id: Some("AC-001".to_owned()),
        title: "Producer Completed".to_owned(),
        profile_id: "default".to_owned(),
        is_ephemeral: false,
        generation: 1,
        terminal_status: TerminalStatus::Completed,
        target_base: None,
        target_base_commit: None,
        archive_path: Some("docs/milestones/archive/AC-001.md".to_owned()),
        closed_commit: None,
        cleanup_pending: false,
        cleanup_manifest: Vec::new(),
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-17T09:20:00.000Z".to_owned(),
    };
    other_engine
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&other_graph_2)?;
            tx.archive_and_prune_run(
                &StoragePreconditions {
                    expected_run_generation: 1,
                    expected_node_versions: Vec::new(),
                    expected_claim_revision: None,
                    expected_contract_revision: None,
                    expected_worker_id: None,
                    required_predecessor_trees: Vec::new(),
                },
                &completed_tombstone,
            )
        })
        .unwrap();

    let admitted = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()),
            profile: Some(".acdd/profiles/bound.yaml".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap();
    assert!(matches!(admitted, StateCommandOutput::Start(_)));
}

#[test]
fn seam_in_memory_lifecycle_subtask_blackboard_extend_scope_reopen_sync_clear() {
    let tmp = make_temp_dir("lifecycle");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let mut graph = build_rich_graph(tmp.path(), "run-life-001", IsolationMode::Worktree);

    let t2_build = graph
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:t2:build")
        .unwrap();
    t2_build.status = NodeStatus::InProgress;
    t2_build.claim_revision = 1;
    t2_build.worker_id = Some(WorkerId::new("worker-b").unwrap());
    let t2_task = graph
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:t2")
        .unwrap();
    t2_task.status = NodeStatus::InProgress;
    graph.claims.push(GateClaimRecord {
        claim_id: None,
        run_id: RunId::new("run-life-001").unwrap(),
        node_id: "task:t2:build".to_owned(),
        claim_revision: 1,
        worker_id: WorkerId::new("worker-b").unwrap(),
        session_fingerprint: Some("host:1:1".to_owned()),
        worktree: tmp.path().display().to_string(),
        span_id: "0123456789abcdef".to_owned(),
        claim_tree: TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
        claimed_at: "2026-11-17T10:01:00.000Z".to_owned(),
        ended_at: None,
        outcome: ClaimOutcome::Active,
    });
    harness.seed_run(&graph).unwrap();

    let empty_ev = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Subtask(SubtaskReq {
            run: Some("run-life-001".to_owned()),
            task_ref: TaskRef::new("t1").unwrap(),
            subtask_ref: SubtaskRef::new("s1").unwrap(),
            status: SubtaskStatus::Completed,
            evidence: Some("   ".to_owned()),
        }),
    )
    .unwrap_err();
    assert_eq!(empty_ev.code, ErrorCode::ProfileSchemaInvalid);

    let sub_ok = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Subtask(SubtaskReq {
            run: Some("run-life-001".to_owned()),
            task_ref: TaskRef::new("t1").unwrap(),
            subtask_ref: SubtaskRef::new("s1").unwrap(),
            status: SubtaskStatus::Completed,
            evidence: Some("implemented in crates/a/src/lib.rs".to_owned()),
        }),
    )
    .unwrap();
    assert!(matches!(sub_ok, StateCommandOutput::Subtask(_)));

    let bb_post_out = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some("run-life-001".to_owned()),
            node: Some("t1.build".to_owned()),
            recipient_node: None,
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "worker-a".to_owned(),
            topic: BlackboardTopic::Notes,
            payload: "shared note context".to_owned(),
        }),
    )
    .unwrap();
    let bb_post_val = bb_post_out.into_untagged_json().unwrap();
    assert_eq!(bb_post_val["entry"]["payload"], "shared note context");

    for (author, payload, expected_field) in [
        (" ", "non-empty", "author"),
        ("worker-a", " ", "payload"),
    ] {
        let err = execute_on(
            &cfg,
            &harness,
            &provider,
            StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some("run-life-001".to_owned()),
                node: None,
                recipient_node: None,
                subtask_ref: None,
                gate: None,
                author: author.to_owned(),
                topic: BlackboardTopic::Notes,
                payload: payload.to_owned(),
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        assert!(
            err.message.contains(expected_field),
            "expected invalid field '{expected_field}' in message: {}",
            err.message
        );
    }

    let bb_query_out = queries::execute_query(
        &cfg,
        &harness,
        &provider,
        StateQuery::BlackboardRead(BlackboardReadReq {
            run: Some("run-life-001".to_owned()),
            topic: Some(BlackboardTopic::Notes),
            ..BlackboardReadReq::default()
        }),
    )
    .unwrap();
    let bb_query_val = bb_query_out.clone().into_untagged_json().unwrap();
    assert_eq!(bb_query_val["total"], 1);
    match bb_query_out {
        StateQueryOutput::BlackboardRead(bb) => {
            assert_eq!(bb.total, 1);
            assert_eq!(bb.items[0].payload, "shared note context");
        }
        other => panic!("unexpected BlackboardRead output: {other:?}"),
    }

    let ext_conflict = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::ExtendScope(ExtendScopeReq {
            run: Some("run-life-001".to_owned()),
            node: "t1".to_owned(),
            paths: vec!["crates/b/src/main.rs".to_owned()],
            reason: "needs sibling crate".to_owned(),
            worker_id: None,
        }),
    )
    .unwrap_err();
    assert_eq!(ext_conflict.code, ErrorCode::TaskScopeConflict);

    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::ExtendScope(ExtendScopeReq {
            run: Some("run-life-001".to_owned()),
            node: "t1".to_owned(),
            paths: vec!["crates/c/**".to_owned()],
            reason: "shared test helper".to_owned(),
            worker_id: None,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::ExtendScope(e) => {
            assert_eq!(e.contract_revision, 2);
            assert!(e.admitted_scope.contains(&"crates/c/**".to_owned()));
        }
        other => panic!("unexpected ExtendScope output: {other:?}"),
    }

    let snap = harness
        .load_run_graph(&RunId::new("run-life-001").unwrap())
        .unwrap()
        .unwrap();
    harness
        .exec_tx(|tx| {
            let t1_n = snap.nodes.iter().find(|n| n.node_id == "task:t1").unwrap();
            let t1_b = snap
                .nodes
                .iter()
                .find(|n| n.node_id == "task:t1:build")
                .unwrap();
            let tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
            let mut p1 = NodeStatusPatch::status_only("task:t1", NodeStatus::Completed);
            p1.accepted_tree = Some(Some(tree.clone()));
            let mut p2 = NodeStatusPatch::status_only("task:t1:build", NodeStatus::Completed);
            p2.accepted_tree = Some(Some(tree));
            let plan = TransitionPlan {
                preconditions: StoragePreconditions {
                    expected_run_generation: snap.run.generation,
                    expected_node_versions: vec![
                        NodeVersionExpectation {
                            node_id: t1_n.node_id.clone(),
                            expected_version: t1_n.node_version,
                        },
                        NodeVersionExpectation {
                            node_id: t1_b.node_id.clone(),
                            expected_version: t1_b.node_version,
                        },
                    ],
                    expected_claim_revision: None,
                    expected_contract_revision: None,
                    expected_worker_id: None,
                    required_predecessor_trees: Vec::new(),
                },
                bump_run_generation: true,
                new_run_status: None,
                new_root_cycle_count: None,
                completed_at: None,
                node_patches: vec![p1, p2],
                dependency_patches: Vec::new(),
                claim_patches: Vec::new(),
                new_artifacts: Vec::new(),
                new_blackboard: Vec::new(),
                blackboard_patches: Vec::new(),
                new_subtasks: Vec::new(),
                significant_transition: true,
            };
            tx.apply_transition_plan(&snap.run.run_id, &plan)
        })
        .unwrap();

    let reopen_no_cascade = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Reopen(ReopenReq {
            run: Some("run-life-001".to_owned()),
            node: "t1.build".to_owned(),
            cascade: false,
            reason: Some("fix missing edge".to_owned()),
        }),
    )
    .unwrap_err();
    assert_eq!(reopen_no_cascade.code, ErrorCode::DependentTaskCompleted);

    let reopen_cascade = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Reopen(ReopenReq {
            run: Some("run-life-001".to_owned()),
            node: "t1.build".to_owned(),
            cascade: true,
            reason: Some("cascade reopen".to_owned()),
        }),
    )
    .unwrap();
    assert!(matches!(reopen_cascade, StateCommandOutput::Reopen(_)));

    let updated_spec = r#"---
id: AC-001
title: Sample Updated
status: active
---
### task: t1

```yaml
task_ref: t1
scope: [crates/a/**]
acceptance_criteria:
  - One
```

### task: t2

```yaml
task_ref: t2
scope: [crates/b/**]
depends_on: [t1]
acceptance_criteria:
  - Two
```

### task: t3

```yaml
task_ref: t3
scope: [crates/d/**]
acceptance_criteria:
  - Three
```
"#;
    fs::write(tmp.path().join("docs/milestones/AC-001-sample.md"), updated_spec).unwrap();
    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Sync(SyncReq {
            run: Some("run-life-001".to_owned()),
            rebind_profile: false,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Sync(s) => assert!(s.added_nodes >= 2),
        other => panic!("unexpected Sync output: {other:?}"),
    }

    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Clear(ClearReq {
            run: Some("run-life-001".to_owned()),
            reason: Some("operator cancelled".to_owned()),
            force: true,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Clear(c) => {
            assert_eq!(c.tombstone.terminal_status, TerminalStatus::Cleared);
            assert!(harness
                .load_run_graph(&RunId::new("run-life-001").unwrap())
                .unwrap()
                .is_none());
        }
        other => panic!("unexpected Clear output: {other:?}"),
    }
}

#[test]
fn seam_run_workflow_runner_required_and_dry_run_in_place() {
    let tmp = make_temp_dir("run-cmd");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let snap = build_rich_graph(tmp.path(), "run-auto-001", IsolationMode::InPlace);
    harness.seed_run(&snap).unwrap();

    match execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Run(RunReq {
            run: Some("run-auto-001".to_owned()),
            jobs: Some(4),
            allow_main: false,
            dry_run: true,
        }),
    )
    .unwrap()
    {
        StateCommandOutput::Run(resp) => {
            assert_eq!(resp.report.waves_executed, 0);
            assert!(resp
                .report
                .halted_reason
                .as_deref()
                .is_some_and(|s| s.contains("jobs=1")));
        }
        other => panic!("unexpected Run dry_run output: {other:?}"),
    }

    let err = execute_on(
        &cfg,
        &harness,
        &provider,
        StateCommand::Run(RunReq {
            run: Some("run-auto-001".to_owned()),
            jobs: Some(1),
            allow_main: false,
            dry_run: false,
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::WorkflowRunnerRequired);

    let after = harness
        .load_run_graph(&RunId::new("run-auto-001").unwrap())
        .unwrap()
        .unwrap();
    let t1_build = after.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap();
    assert_eq!(t1_build.status, NodeStatus::Ready);
    assert_eq!(t1_build.claim_revision, 0);
}

#[test]
fn seam_git_e2e_claim_verify_submit_snapshot_rollback_integrate_and_archive() {
    let tmp = make_temp_dir("git-e2e");
    let repo = tmp.path();
    let _initial_head = init_git_repo(repo, "feat/phase2-e2e");

    let profile_dir = repo.join(".acdd/profiles");
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("e2e.yaml"),
        r#"version: 2
inherit: default
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output:
      require_counts: false
  build_check:
    command: "true"
  verify_all:
    command: "true"
  ok_cmd:
    command: "printf '1 passed; 0 failed\n'"
    output:
      counts:
        passed: '(\d+)\s+passed'
        failed: '(\d+)\s+failed'
  test_cmd:
    command: "sh -c 'printf \"test t1_tests ... FAILED\\n0 passed; 1 failed\\n\"; exit 101' -- {test_filter}"
    test_filter: "--filter {test}"
    output:
      counts:
        passed: '(\d+)\s+passed'
        failed: '(\d+)\s+failed'
      tests:
        passed: '^test (\S+) \.\.\. ok$'
        failed: '^test (\S+) \.\.\. FAILED$'
git:
  isolation: worktree
  worktrees: managed
  commit_mode: plumbing
  integrate: ff-only
gates:
  - id: pre_verify
    proof: command
    command: ok_cmd
    checks:
      - check: http.headers
    call: http.dispatch
    with:
      target: prod-api
    context:
      - http.endpoint_summary
    save:
      post: snapshot
  - tasks:
      gates:
        - id: build
          role: builder
          proof: contract
          command: test_cmd
          expected_failure:
            exit_codes: [101]
          save:
            post: snapshot
            immutable: true
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          independent_from: [build]
          need:
            - build
          contours: [claims]
          receipt: true
          save:
            post: commit
          fail: build
  - id: handoff
    proof: none
    need: [pre_verify]
    receipt: true
    save:
      post: commit
      archive: true
      prune: true
"#,
    )
    .unwrap();

    fs::create_dir_all(repo.join("docs/milestones")).unwrap();
    fs::write(
        repo.join("docs/milestones/AC-020-e2e.md"),
        r#"---
id: AC-020
title: Full Phase 2 E2E Run
status: planned
---
### task: t1

```yaml
task_ref: t1
scope: [src/lib.rs]
acceptance_criteria:
  - Function returns 42
```
"#,
    )
    .unwrap();

    let cfg = base_config(repo);
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();

    let StateCommandOutput::Start(start_resp) = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-020-e2e.md".to_owned()),
            profile: Some(".acdd/profiles/e2e.yaml".to_owned()),
            worktree: true,
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let rid = start_resp.run.run_id.to_string();
    assert_eq!(start_resp.ready_nodes, vec!["pre_verify".to_owned()]);

    let w_pre = WorkerId::new("worker-pre").unwrap();
    let StateCommandOutput::Claim(pre_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("pre_verify".to_owned()),
            worker_id: w_pre.clone(),
            session_fingerprint: Some("host-a:999999:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    assert!(pre_claim.context_lenses.contains_key("http.endpoint_summary"));

    let StateCommandOutput::Claim(pre_reclaim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("pre_verify".to_owned()),
            worker_id: w_pre.clone(),
            session_fingerprint: Some("host-a:999999:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    assert_eq!(pre_reclaim.claim_revision, pre_claim.claim_revision);

    let steal_err = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("pre_verify".to_owned()),
            worker_id: WorkerId::new("thief").unwrap(),
            session_fingerprint: Some("host-b:111:1".to_owned()),
            force: true,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(steal_err.code, ErrorCode::TaskAlreadyClaimed);

    let StateCommandOutput::Verify(pre_verify_resp) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            worker_id: Some(w_pre.clone()),
            test_target: None,
            claim_revision: Some(pre_claim.claim_revision),
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(pre_verify_resp.passed);
    assert_eq!(pre_verify_resp.tests_passed, 1);
    assert_eq!(pre_verify_resp.check_results.len(), 1);
    assert_eq!(mock.action_calls(), 1);

    let pre_snap = engine
        .backend()
        .load_run_graph(&start_resp.run.run_id)
        .unwrap()
        .unwrap();
    let pre_node = pre_snap
        .nodes
        .iter()
        .find(|n| n.node_id == "root:pre_verify")
        .unwrap()
        .clone();
    let idem_key = format!(
        "{}:{}:{}:{}",
        start_resp.run.run_id,
        pre_node.node_id,
        pre_node.claim_revision,
        pre_verify_resp.execution_digest
    );
    engine
        .backend()
        .exec_tx(|tx| {
            tx.start_trace(
                &crate::machine::graph::build_preconditions(
                    &pre_snap,
                    &[pre_node.node_id.as_str()],
                    Some(&pre_node),
                ),
                &TraceStartInput {
                    run_id: start_resp.run.run_id.clone(),
                    node_id: pre_node.node_id.clone(),
                    claim_revision: pre_node.claim_revision,
                    contract_revision: pre_node.contract_revision,
                    execution_digest: pre_verify_resp.execution_digest.clone(),
                    idempotency_key: Some(idem_key),
                    command_id: Some("ok_cmd".to_owned()),
                    command_line: None,
                    action_id: Some("http.dispatch".to_owned()),
                    test_target: None,
                    candidate_tree_before: pre_node.claim_tree.clone().unwrap(),
                    log_relpath: "logs/crash.log".to_owned(),
                    started_at: "2026-11-17T10:02:00.000Z".to_owned(),
                },
            )?;
            Ok(())
        })
        .unwrap();

    let action_before_recovery = mock.action_calls();
    let StateCommandOutput::Verify(recovered_verify) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            worker_id: Some(w_pre.clone()),
            test_target: None,
            claim_revision: Some(pre_claim.claim_revision),
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(recovered_verify.passed);
    assert_eq!(mock.action_calls(), action_before_recovery);
    assert_eq!(mock.lookup_calls(), 1);

    let StateCommandOutput::Submit(pre_sub) = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_pre,
            claim_revision: Some(pre_claim.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("pre verified".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert!(pre_sub.snapshot_ref.is_some());

    let w_build = WorkerId::new("builder-1").unwrap();
    let StateCommandOutput::Claim(t1_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: Some("host-a:200:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp for t1.build");
    };
    let task_wt = std::path::PathBuf::from(&t1_claim.worktree);
    assert!(task_wt.is_dir());

    fs::write(task_wt.join("README.md"), "# Out of scope edit\n").unwrap();
    let oos_err = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: Some("t1_tests".to_owned()),
            claim_revision: Some(t1_claim.claim_revision),
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(oos_err.code, ErrorCode::OutOfScopeChanges);

    fs::write(task_wt.join("README.md"), "# Test Repo\n").unwrap();
    let StateCommandOutput::Snapshot(snap_resp) = engine
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.clone()),
            label: Some("clean-base".to_owned()),
            worktree: Some(task_wt.clone()),
        }))
        .unwrap()
    else {
        panic!("expected SnapshotResp");
    };
    fs::write(task_wt.join("src/lib.rs"), "pub fn broken() -> i32 { 0 }\n").unwrap();
    let StateCommandOutput::Rollback(rb_resp) = engine
        .execute(StateCommand::Rollback(RollbackReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            target: snap_resp.snapshot_ref,
            worktree: Some(task_wt.clone()),
        }))
        .unwrap()
    else {
        panic!("expected RollbackResp");
    };
    assert_eq!(rb_resp.target_tree, snap_resp.tree_oid);
    assert!(rb_resp.restored_paths.iter().any(|p| p == "src/lib.rs"));
    assert!(fs::read_to_string(task_wt.join("src/lib.rs"))
        .unwrap()
        .contains("pub fn base()"));

    let StateCommandOutput::Claim(t1_claim2) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: Some("host-a:200:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected re-claim ClaimResp");
    };

    let unverified_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(t1_claim2.claim_revision),
            test_target: Some("t1_tests".to_owned()),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap_err();
    assert_eq!(unverified_err.code, ErrorCode::TaskProofUnobserved);

    // Verify on base tree, then edit in-scope file AFTER claim -> immutable gate triggers TASK_CANDIDATE_MISMATCH on submit!
    let StateCommandOutput::Verify(v_pre_edit) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: Some("t1_tests".to_owned()),
            claim_revision: Some(t1_claim2.claim_revision),
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(v_pre_edit.passed);

    fs::write(task_wt.join("src/lib.rs"), "pub fn feat() -> i32 { 42 }\n").unwrap();
    let immut_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(t1_claim2.claim_revision),
            test_target: Some("t1_tests".to_owned()),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap_err();
    assert_eq!(immut_err.code, ErrorCode::TaskCandidateMismatch);

    let StateCommandOutput::Claim(t1_claim3) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: Some("host-a:200:2".to_owned()),
            force: true,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    assert!(t1_claim3.claim_revision > t1_claim2.claim_revision);
    let StateCommandOutput::Verify(v_ok) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: Some("t1_tests".to_owned()),
            claim_revision: Some(t1_claim3.claim_revision),
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(v_ok.passed);

    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(t1_claim3.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("build contract verified".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // Independent review (`independent_from: [build]`): submitting with same worker `w_build` fails with TaskReviewNotIndependent.
    let StateCommandOutput::Claim(same_rev_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: Some("host-a:200:2".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let mut same_contours = BTreeMap::new();
    same_contours.insert(
        "claims".to_owned(),
        ReviewContourResult {
            applicable: true,
            evidence: "self review".to_owned(),
        },
    );
    let same_worker_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build,
            claim_revision: Some(same_rev_claim.claim_revision),
            test_target: None,
            review_evidence: Some(ReviewEvidence {
                decision: SubmitAction::Pass,
                contours: same_contours,
                summary: "self pass".to_owned(),
                notes: None,
            }),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap_err();
    assert_eq!(same_worker_err.code, ErrorCode::TaskReviewNotIndependent);

    let w_rev = WorkerId::new("reviewer-1").unwrap();
    let StateCommandOutput::Claim(rev_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: Some("host-a:201:1".to_owned()),
            force: true,
            steal_remote: true,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected review ClaimResp");
    };

    let rej_out = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Reject,
            worker_id: w_rev.clone(),
            claim_revision: Some(rev_claim.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: Some(RejectionFindings {
                summary: "need cleaner docs comment".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Medium,
                    path: Some("src/lib.rs".to_owned()),
                    message: "missing doc comment".to_owned(),
                    remediation: "add /// doc comment".to_owned(),
                }],
            }),
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();
    assert!(matches!(rej_out, StateCommandOutput::Submit(_)));

    let w_build2 = WorkerId::new("builder-1").unwrap();
    let StateCommandOutput::Claim(b2) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build2.clone(),
            session_fingerprint: Some("host-a:200:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build2.clone()),
            test_target: Some("t1_tests".to_owned()),
            claim_revision: Some(b2.claim_revision),
            worktree: None,
        }))
        .unwrap();
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build2,
            claim_revision: Some(b2.claim_revision),
            test_target: Some("t1_tests".to_owned()),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let StateCommandOutput::Claim(r2) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: Some("host-a:201:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let mut contours = BTreeMap::new();
    contours.insert(
        "claims".to_owned(),
        ReviewContourResult {
            applicable: true,
            evidence: "verified feat() returns 42".to_owned(),
        },
    );
    let StateCommandOutput::Submit(rev_pass) = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev,
            claim_revision: Some(r2.claim_revision),
            test_target: None,
            review_evidence: Some(ReviewEvidence {
                decision: SubmitAction::Pass,
                contours,
                summary: "LGTM".to_owned(),
                notes: None,
            }),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("review passed".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert!(rev_pass.committed_oid.is_some());

    let w_hand = WorkerId::new("handoff-1").unwrap();
    let not_int_err = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("handoff".to_owned()),
            worker_id: w_hand.clone(),
            session_fingerprint: Some("host-a:202:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(not_int_err.code, ErrorCode::TaskNotIntegrated);

    let StateCommandOutput::Integrate(int_resp) = engine
        .execute(StateCommand::Integrate(IntegrateReq {
            run: Some(rid.clone()),
            task_ref: None,
            strategy: None,
            allow_main: false,
        }))
        .unwrap()
    else {
        panic!("expected IntegrateResp");
    };
    assert_eq!(int_resp.integrated_tasks, vec![TaskRef::new("t1").unwrap()]);

    let StateCommandOutput::Claim(h_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("handoff".to_owned()),
            worker_id: w_hand.clone(),
            session_fingerprint: Some("host-a:202:1".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp for handoff");
    };
    // Because `handoff` has `need: [pre_verify]` and `integrate` updated the run worktree tree,
    // submitting `handoff` marks `pre_verify` `Stale` in `StateTx` before returning `TaskCandidateMismatch`.
    let need_drift_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "handoff".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_hand.clone(),
            claim_revision: Some(h_claim.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("should detect pre_verify drift".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap_err();
    assert_eq!(need_drift_err.code, ErrorCode::TaskCandidateMismatch);

    let after_drift = engine
        .backend()
        .load_run_graph(&start_resp.run.run_id)
        .unwrap()
        .unwrap();
    let pre_after_drift = after_drift
        .nodes
        .iter()
        .find(|n| n.node_id == "root:pre_verify")
        .unwrap();
    assert_eq!(pre_after_drift.status, NodeStatus::Stale);

    // Reopen stale `pre_verify`, re-claim, re-verify, and pass on the integrated tree, then finish `handoff`.
    engine
        .execute(StateCommand::Reopen(ReopenReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            cascade: false,
            reason: Some("reopen stale pre_verify after integration".to_owned()),
        }))
        .unwrap();
    let w_pre2 = WorkerId::new("worker-pre").unwrap();
    let StateCommandOutput::Claim(pre2_claim) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("pre_verify".to_owned()),
            worker_id: w_pre2.clone(),
            session_fingerprint: Some("host-a:999999:2".to_owned()),
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected re-claim ClaimResp for pre_verify");
    };
    engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            worker_id: Some(w_pre2.clone()),
            test_target: None,
            claim_revision: Some(pre2_claim.claim_revision),
            worktree: None,
        }))
        .unwrap();
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "pre_verify".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_pre2,
            claim_revision: Some(pre2_claim.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("re-verified on integrated tree".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let StateCommandOutput::Submit(h_sub) = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "handoff".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_hand,
            claim_revision: Some(h_claim.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("all tasks shipped".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp for handoff");
    };
    assert_eq!(h_sub.run_status, RunStatus::Completed);
    assert_eq!(
        h_sub.archive_path.as_deref(),
        Some("docs/milestones/archive/AC-020-e2e.md")
    );
    let closed_oid = h_sub.committed_oid.expect("handoff committed_oid");
    let run_wt = std::path::PathBuf::from(start_resp.run.worktree.as_deref().unwrap());
    assert!(!run_wt.exists(), "post-commit cleanup should remove managed run worktree");
    assert!(
        repo.join("docs/milestones/archive/AC-020-e2e.md").is_file(),
        "archive should be synced to primary workspace after managed run worktree cleanup"
    );
    assert_eq!(
        git_ok(repo, &["rev-parse", "--verify", "refs/heads/run/AC-020"]),
        closed_oid.as_str(),
        "run/AC-020 branch should remain pointing at closed_commit"
    );

    let list_out = engine
        .query(StateQuery::List(ListReq {
            include_tombstones: true,
            ..ListReq::default()
        }))
        .unwrap();
    match list_out {
        StateQueryOutput::List(l) => {
            assert_eq!(l.page.total_runs, 0);
            assert_eq!(l.page.total_tombstones, 1);
            assert_eq!(l.page.tombstones[0].spec_id.as_deref(), Some("AC-020"));
            assert_eq!(
                l.page.tombstones[0].terminal_status,
                TerminalStatus::Completed
            );
            assert!(!l.page.tombstones[0].cleanup_pending);
        }
        other => panic!("unexpected List output: {other:?}"),
    }

    assert!(repo.join(format!(".acdd/run/{rid}/traces.jsonl")).is_file());
}
