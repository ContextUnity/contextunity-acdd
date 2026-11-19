//! Phase 2 use-case & seam matrix tests (`v7` §6.1 Phase 2).

use super::{
    execute_on, queries, StateCommand, StateCommandOutput, StateEngine, StateEngineConfig,
    StateQuery, StateQueryEngine, StateQueryOutput,
};
use crate::context::ClaimResp;
use crate::git_ops::commit::current_head_commit;
use crate::harness::RunHarness;
use crate::profile::{resolve_profile_ref, CommandDef, ResolvedGateProfile};
use crate::spec::materialize_ephemeral_goal_spec;
use crate::store::traits::{StateBackend, StateReader};
use crate::types::{
    ArchiveCleanupEntry, BlackboardTopic, ClaimOutcome, FindingItem, FindingSeverity,
    GateClaimRecord, GateNodeRecord, InvariantsState, IsolationMode, NodeKind, NodeStatus,
    NodeStatusPatch, NodeVersionExpectation, RejectionFindings, RejectionKind, ReviewContourResult,
    ReviewEvidence, RunGraphSnapshot, RunRecord, RunStatus, RunTombstoneRecord,
    StoragePreconditions, SubmitAction, SubtaskRecord, SubtaskStatus, TaskDependencyRecord,
    TerminalStatus, TraceFinishInput, TraceStartInput, TraceStatus, TransitionPlan,
    WorktreeLifecycle,
};
use crate::usecases::dto::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, ClaimReq, ClearReq,
    DraftReq, ExtendScopeReq, IntegrateReq, ListReq, NextReq, ObservabilityReq, ReopenReq,
    RollbackReq, RunReq, ShowReq, StartReq, StartResp, StatusReq, SubmitReq,
    SubmitResp, SubtaskReq, SyncReq, VerifyReq, VerifyResp,
};
use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::git::git_command;
use acdd_core::ids::{CommitOid, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_core::ports::{
    ActionCapability, CheckCapability, CompositeDomainProvider, DomainActionReq, DomainActionResult,
    DomainCapabilities, DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
    DomainToolDef, DomainToolOutput, DomainToolReq,
};
use acdd_core::response::ResponsePolicy;
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use acdd_core::subsystems::SubsystemRegistry;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

struct MockHttpProvider {
    tool_calls: AtomicUsize,
    action_calls: AtomicUsize,
    lookup_calls: AtomicUsize,
    unsalted_check: AtomicBool,
    fail_check: AtomicBool,
}

impl MockHttpProvider {
    fn new() -> Self {
        Self {
            tool_calls: AtomicUsize::new(0),
            action_calls: AtomicUsize::new(0),
            lookup_calls: AtomicUsize::new(0),
            unsalted_check: AtomicBool::new(false),
            fail_check: AtomicBool::new(false),
        }
    }

    fn set_unsalted_check(&self, v: bool) { self.unsalted_check.store(v, Ordering::SeqCst); }
    fn set_fail_check(&self, v: bool) { self.fail_check.store(v, Ordering::SeqCst); }
    fn tool_calls(&self) -> usize { self.tool_calls.load(Ordering::SeqCst) }
    fn action_calls(&self) -> usize { self.action_calls.load(Ordering::SeqCst) }
    fn lookup_calls(&self) -> usize { self.lookup_calls.load(Ordering::SeqCst) }
}

impl DomainProvider for MockHttpProvider {
    fn domain_id(&self) -> &str { "http" }

    fn capabilities(&self) -> DomainCapabilities {
        let mut probe_schema = SchemaDef::object();
        probe_schema.properties.insert("url".to_owned(), SchemaDef::string());
        probe_schema.required = vec!["url".to_owned()];
        probe_schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

        let mut dispatch_schema = SchemaDef::object();
        dispatch_schema.properties.insert("target".to_owned(), SchemaDef::string());
        dispatch_schema.required = vec!["target".to_owned()];

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
            context_lenses: BTreeSet::from(["endpoint_summary".to_owned()]),
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
            output: json!({ "idempotency_key": idempotency_key, "params": params }),
        })
    }

    fn lookup_action_result(&self, action: &str, idempotency_key: &str) -> Result<Option<DomainActionResult>> {
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

    fn run_check(&self, _check: &str, _params: &Value, _req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        if self.fail_check.load(Ordering::SeqCst) {
            return Ok(DomainCheckOutcome::Failed {
                summary: "headers check failed".to_owned(),
                diagnostics: vec!["live endpoint unhealthy".to_owned()],
            });
        }
        let salt = (!self.unsalted_check.load(Ordering::SeqCst)).then(|| "http-salt-v1".to_owned());
        Ok(DomainCheckOutcome::Passed { summary: "headers valid".to_owned(), execution_digest_salt: salt })
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> Result<Value> {
        Ok(json!({ "lens": lens, "status": "ok" }))
    }

    fn call_tool(&self, tool: &str, args: &Value, _req: &DomainToolReq) -> Result<DomainToolOutput> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        Ok(DomainToolOutput::Json(json!({
            "tool": tool,
            "url": args.get("url").cloned().unwrap_or(Value::Null),
            "status": 200,
        })))
    }
}

fn make_temp_dir(label: &str) -> TempDir {
    tempfile::Builder::new().prefix(&format!("acdd-usecases-{label}-")).tempdir().expect("create tempdir")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git_command(dir, args).expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in '{}' failed:\nstdout: {}\nstderr: {}",
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
    BTreeMap::from([(
        "milestones".to_owned(),
        SpecCollectionConfig {
            path: "docs/milestones".to_owned(),
            profile: Some("workspace".to_owned()),
            prefixes: vec!["AC".to_owned(), "M".to_owned()],
            digits: 3,
        },
    )])
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

fn empty_preconditions(expected_run_generation: u64) -> StoragePreconditions {
    StoragePreconditions {
        expected_run_generation,
        expected_node_versions: Vec::new(),
        expected_claim_revision: None,
        expected_contract_revision: None,
        expected_worker_id: None,
        required_predecessor_trees: Vec::new(),
    }
}

fn sample_tombstone(
    run_id: &str,
    run_name: &str,
    spec_id: Option<&str>,
    status: TerminalStatus,
    archive_path: Option<&str>,
) -> RunTombstoneRecord {
    RunTombstoneRecord {
        run_id: RunId::new(run_id).unwrap(),
        run_name: RunName::new(run_name).unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "default".to_owned(),
        spec_id: spec_id.map(str::to_owned),
        title: format!("{run_name} title"),
        profile_id: "default".to_owned(),
        is_ephemeral: false,
        generation: 1,
        terminal_status: status,
        target_base: None,
        target_base_commit: None,
        archive_path: archive_path.map(str::to_owned),
        closed_commit: None,
        cleanup_pending: false,
        cleanup_manifest: Vec::new(),
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-17T10:00:00.000Z".to_owned(),
    }
}

fn start_run(eng: &StateEngine, req: StartReq) -> Result<StartResp, StateError> {
    match eng.execute(StateCommand::Start(req))? {
        StateCommandOutput::Start(resp) => Ok(*resp),
        other => panic!("expected StartResp, got {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn claim_node(
    eng: &StateEngine,
    rid: &str,
    node: &str,
    worker: &str,
    fp: Option<&str>,
    force: bool,
    steal_remote: bool,
    wt: Option<PathBuf>,
) -> Result<ClaimResp, StateError> {
    match eng.execute(StateCommand::Claim(ClaimReq {
        run: Some(rid.to_owned()),
        node: Some(node.to_owned()),
        worker_id: WorkerId::new(worker).unwrap(),
        session_fingerprint: fp.map(str::to_owned),
        force,
        steal_remote,
        worktree: wt,
    }))? {
        StateCommandOutput::Claim(resp) => Ok(*resp),
        other => panic!("expected ClaimResp, got {other:?}"),
    }
}

fn verify_node(
    eng: &StateEngine,
    rid: &str,
    node: &str,
    worker: &str,
    test_target: Option<&str>,
    claim_revision: Option<u32>,
) -> Result<VerifyResp, StateError> {
    match eng.execute(StateCommand::Verify(VerifyReq {
        run: Some(rid.to_owned()),
        node: node.to_owned(),
        worker_id: Some(WorkerId::new(worker).unwrap()),
        test_target: test_target.map(str::to_owned),
        claim_revision,
        worktree: None,
    }))? {
        StateCommandOutput::Verify(resp) => Ok(resp),
        other => panic!("expected VerifyResp, got {other:?}"),
    }
}

fn pass_req(rid: &str, node: &str, worker: &str, rev: Option<u32>, allow_main: bool) -> SubmitReq {
    SubmitReq {
        run: Some(rid.to_owned()),
        node: node.to_owned(),
        action: SubmitAction::Pass,
        worker_id: WorkerId::new(worker).unwrap(),
        claim_revision: rev,
        test_target: None,
        review_evidence: None,
        rejection_findings: None,
        custom_evidence: None,
        verified_invariants: Vec::new(),
        summary: None,
        allow_main,
        worktree: None,
    }
}

fn submit_req(eng: &StateEngine, req: SubmitReq) -> Result<SubmitResp, StateError> {
    match eng.execute(StateCommand::Submit(Box::new(req)))? {
        StateCommandOutput::Submit(resp) => Ok(*resp),
        other => panic!("expected SubmitResp, got {other:?}"),
    }
}

fn clear_run(eng: &StateEngine, rid: &str) {
    eng.execute(StateCommand::Clear(ClearReq {
        run: Some(rid.to_owned()),
        reason: None,
        force: true,
    }))
    .unwrap();
}

fn write_profile(root: &Path, name: &str, yaml: &str) -> String {
    let dir = root.join(".acdd/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), yaml).unwrap();
    format!(".acdd/profiles/{name}")
}

fn build_rich_graph(root: &Path, run_id_str: &str, isolation: IsolationMode) -> RunGraphSnapshot {
    let mut profile = resolve_profile_ref("default", None, None).expect("resolve default profile");
    for cmd in ["test", "build_check", "verify_all"] {
        profile.commands.insert(cmd.to_owned(), CommandDef::Script("true".into()));
    }
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
        profile_id: profile.profile_id.clone(),
        profile_json: serde_json::to_string(&profile).expect("serialize profile"),
        spec_path: Some(spec_rel.to_owned()),
        spec_id: None,
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
        make_node("tasks#0", None, NodeKind::Container, None, "tasks#0", 0, NodeStatus::Ready, Vec::new()),
        make_node("task:t1", Some("tasks#0"), NodeKind::Task, Some(t1.clone()), "__task__", 1, NodeStatus::Ready, vec!["crates/a/**".to_owned()]),
        make_node("task:t1:build", Some("task:t1"), NodeKind::TaskGate, Some(t1.clone()), "build", 2, NodeStatus::Ready, vec!["crates/a/**".to_owned()]),
        make_node("task:t1:review", Some("task:t1"), NodeKind::TaskGate, Some(t1), "review", 3, NodeStatus::Pending, vec!["crates/a/**".to_owned()]),
        make_node("task:t2", Some("tasks#0"), NodeKind::Task, Some(t2.clone()), "__task__", 4, NodeStatus::Pending, vec!["crates/b/**".to_owned()]),
        make_node("task:t2:build", Some("task:t2"), NodeKind::TaskGate, Some(t2.clone()), "build", 5, NodeStatus::Pending, vec!["crates/b/**".to_owned()]),
        make_node("root:handoff", None, NodeKind::RootGate, None, "handoff", 6, NodeStatus::Pending, Vec::new()),
    ];

    RunGraphSnapshot {
        run,
        nodes,
        dependencies: vec![TaskDependencyRecord {
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

    let empty_cases = [
        (StateQuery::Status(StatusReq::default()), false),
        (StateQuery::List(ListReq::default()), false),
        (StateQuery::Show(ShowReq { run: None, node: None }), true),
        (StateQuery::Next(NextReq::default()), false),
        (StateQuery::BlackboardRead(BlackboardReadReq::default()), true),
        (StateQuery::Observability(ObservabilityReq { include_prometheus: true }), false),
    ];
    for (idx, (query, expect_ambiguous)) in empty_cases.into_iter().enumerate() {
        let res = ro.query(query);
        if expect_ambiguous {
            assert_eq!(res.unwrap_err().code, ErrorCode::RunSelectorAmbiguous, "query #{idx}");
        } else {
            assert!(res.is_ok(), "query #{idx} failed: {res:?}");
        }
    }
    assert!(!empty_tmp.path().join(".acdd/state.sqlite").exists());
    assert!(!empty_tmp.path().join(".acdd/state.bak.sqlite").exists());

    let harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
    let ws = harness.workspace_root().unwrap().to_path_buf();
    let mut cfg_pop = base_config(&ws);
    cfg_pop.state_db_override = Some(harness.db_path().unwrap().to_path_buf());
    harness.seed_run(&build_rich_graph(&ws, "run-ro-001", IsolationMode::InPlace)).unwrap();

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
    match ro_pop.query(StateQuery::Observability(ObservabilityReq { include_prometheus: true })).unwrap() {
        StateQueryOutput::Observability(o) => {
            assert_eq!(o.snapshot.active_runs, 1);
            assert!(o.prometheus_text.as_deref().is_some_and(|t| t.contains("acdd_runs_active 1")));
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

    assert_eq!(StateEngine::open(cfg.clone(), provider.clone()).err().unwrap().code, ErrorCode::FeatureDisabled);
    assert!(!tmp.path().join(".acdd/state.sqlite").exists());

    let ro = StateQueryEngine::open_read_only(cfg.clone(), provider.clone()).unwrap();
    for q in [
        StateQuery::Status(StatusReq::default()),
        StateQuery::List(ListReq::default()),
        StateQuery::Next(NextReq::default()),
        StateQuery::BlackboardRead(BlackboardReadReq::default()),
        StateQuery::Observability(ObservabilityReq::default()),
    ] {
        assert_eq!(ro.query(q).unwrap_err().code, ErrorCode::FeatureDisabled);
    }

    let harness = RunHarness::in_memory().unwrap();
    assert_eq!(
        execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(), slug: "blocked".to_owned(), prefix: None, title: None, profile: None,
        })).unwrap_err().code,
        ErrorCode::FeatureDisabled
    );

    // #30: Stateless CallPluginTool remains usable with modules.state == false; run-scoped stays blocked.
    for (run, expect_ok) in [(None, true), (Some("run-x".to_owned()), false)] {
        let res = execute_on(&cfg, &harness, &provider, StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: "http_probe".to_owned(),
            args: json!({ "url": "https://example.invalid/stateless" }),
            run,
            node: None,
            worktree: None,
        }));
        if expect_ok {
            assert!(matches!(res, Ok(StateCommandOutput::CallPluginTool(_))));
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::FeatureDisabled);
        }
    }

    let mut bb_cfg = base_config(tmp.path());
    bb_cfg.modules.disabled.insert("blackboard".to_owned());
    let (_, bb_prov) = build_provider_with_mock(&bb_cfg.modules);
    assert_eq!(queries::execute_query(&bb_cfg, &harness, &bb_prov, StateQuery::BlackboardRead(BlackboardReadReq::default())).unwrap_err().code, ErrorCode::FeatureDisabled);
    assert!(queries::execute_query(&bb_cfg, &harness, &bb_prov, StateQuery::Status(StatusReq::default())).is_ok());
    assert_eq!(
        execute_on(&bb_cfg, &harness, &bb_prov, StateCommand::BlackboardPost(BlackboardPostReq {
            run: None, node: None, recipient_node: None, subtask_ref: None, gate: None,
            author: "builder".to_owned(), topic: BlackboardTopic::Notes, payload: "note".to_owned(),
        })).unwrap_err().code,
        ErrorCode::FeatureDisabled
    );

    let mut no_cd_cfg = base_config(tmp.path());
    no_cd_cfg.modules.disabled.insert("code".to_owned());
    no_cd_cfg.modules.disabled.insert("docs".to_owned());
    let (_, no_cd_prov) = build_provider_with_mock(&no_cd_cfg.modules);
    assert!(!no_cd_prov.is_domain_enabled("code") && !no_cd_prov.is_domain_enabled("docs") && no_cd_prov.is_domain_enabled("http"));
    assert_eq!(no_cd_prov.validate_check_ref("code.graph_fresh", &json!({})).unwrap_err().code, ErrorCode::DomainCapabilityUnavailable);

    let mut no_http_cfg = base_config(tmp.path());
    no_http_cfg.modules.domains.insert("http".to_owned(), false);
    let (_, no_http_prov) = build_provider_with_mock(&no_http_cfg.modules);
    assert_eq!(
        execute_on(&no_http_cfg, &harness, &no_http_prov, StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: "http_probe".to_owned(),
            args: json!({ "url": "https://example.invalid" }),
            run: None, node: None, worktree: None,
        })).unwrap_err().code,
        ErrorCode::DomainCapabilityUnavailable
    );
}

#[test]
fn seam_mock_domain_provider_plugin_tool_and_gate_tools_matrix() {
    let tmp = make_temp_dir("plugin-tool");
    let cfg = base_config(tmp.path());
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();

    for (name, tool_name, args, expect_err, delta) in [
        ("valid_probe", "http_probe", json!({ "url": "https://example.invalid/health" }), None, 1usize),
        ("missing_url", "http_probe", json!({}), Some(ErrorCode::ProfileSchemaInvalid), 0),
        ("extra_prop", "http_probe", json!({ "url": "https://x", "extra": 42 }), Some(ErrorCode::ProfileSchemaInvalid), 0),
        ("unknown_tool", "http_nonexistent", json!({}), Some(ErrorCode::DomainCapabilityUnavailable), 0),
    ] {
        let before = mock.tool_calls();
        let res = engine.execute(StateCommand::CallPluginTool(CallPluginToolReq {
            tool_name: tool_name.to_owned(),
            args,
            run: None,
            node: None,
            worktree: None,
        }));
        assert_eq!(mock.tool_calls() - before, delta, "delta for {name}");
        match expect_err {
            None => match res.unwrap() {
                StateCommandOutput::CallPluginTool(out) => assert_eq!(
                    out.output,
                    DomainToolOutput::Json(json!({ "tool": "probe", "url": "https://example.invalid/health", "status": 200 }))
                ),
                other => panic!("unexpected output for {name}: {other:?}"),
            },
            Some(code) => assert_eq!(res.unwrap_err().code, code, "case {name}"),
        }
    }

    let make_tool_prof = |tool: &str| format!(
        "version: 2\ninherit: default\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {{test}}\"\n    output:\n      require_counts: false\n  check:\n    command: \"true\"\n  lint:\n    command: \"true\"\ngates:\n  - id: probe_gate\n    proof: none\n    tools: [{tool}]\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n"
    );
    let bad_p = write_profile(tmp.path(), "plugin_bad.yaml", &make_tool_prof("http_missing_tool"));
    let ok_p = write_profile(tmp.path(), "plugin_ok.yaml", &make_tool_prof("http_probe"));
    assert_eq!(
        start_run(&engine, StartReq { goal: Some("Bad".to_owned()), profile: Some(bad_p), ..StartReq::default() }).unwrap_err().code,
        ErrorCode::DomainCapabilityUnavailable
    );
    let ok_started = start_run(&engine, StartReq { goal: Some("Ok".to_owned()), profile: Some(ok_p), in_place: true, ..StartReq::default() }).unwrap();
    clear_run(&engine, ok_started.run.run_id.as_str());

    // #1: In-flight mutating call Running trace inspects prev.idempotency_key without abort/re-dispatch,
    // even under out-of-scope drift, and respects recovery_only if finished concurrently.
    let mut_tmp = make_temp_dir("mut-call");
    let _ = init_git_repo(mut_tmp.path(), "main");
    let mut_p = write_profile(mut_tmp.path(), "mut_call.yaml", r#"version: 2
inherit: default
protected_paths: [Cargo.toml]
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output: { require_counts: false }
  check: { command: "true" }
  lint: { command: "true" }
gates:
  - id: deploy_gate
    proof: call
    call: http.dispatch
    with: { target: prod }
  - tasks:
      gates:
        - id: build
          proof: none
"#);
    let mut_cfg = base_config(mut_tmp.path());
    let (mut_mock, mut_prov) = build_provider_with_mock(&mut_cfg.modules);
    let mut_engine = StateEngine::open(mut_cfg, mut_prov).unwrap();
    let mut_start = start_run(&mut_engine, StartReq {
        goal: Some("Mutating call crash recovery".to_owned()),
        profile: Some(mut_p),
        in_place: true,
        ..StartReq::default()
    }).unwrap();
    let mut_rid = mut_start.run.run_id.clone();
    let mut_claim = claim_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, false, false, None).unwrap();
    let before_snap = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap();
    let deploy_node = before_snap.nodes.iter().find(|n| n.node_id == "root:deploy_gate").unwrap();
    let precond = StoragePreconditions {
        expected_run_generation: before_snap.run.generation,
        expected_node_versions: vec![NodeVersionExpectation {
            node_id: deploy_node.node_id.clone(),
            expected_version: deploy_node.node_version,
        }],
        expected_claim_revision: Some(mut_claim.claim_revision),
        expected_contract_revision: Some(deploy_node.contract_revision),
        expected_worker_id: deploy_node.worker_id.clone(),
        required_predecessor_trees: Vec::new(),
    };
    mut_engine.backend().exec_tx(|tx| {
        tx.start_trace(&precond, &TraceStartInput {
            run_id: mut_rid.clone(),
            node_id: deploy_node.node_id.clone(),
            claim_revision: mut_claim.claim_revision,
            contract_revision: deploy_node.contract_revision,
            execution_digest: "deadbeef".to_owned(),
            idempotency_key: Some("prior-crash-key".to_owned()),
            command_id: None,
            command_line: None,
            action_id: Some("http.dispatch".to_owned()),
            test_target: None,
            candidate_tree_before: mut_claim.candidate_tree.clone(),
            log_relpath: "logs/deploy.log".to_owned(),
            started_at: "2026-11-18T00:00:00.000Z".to_owned(),
        }).map(|_| ())
    }).unwrap();

    for (write_drift, expect_code, expect_lookup_delta) in [
        (false, ErrorCode::MutatingActionUncertainOutcome, 1usize),
        (true, ErrorCode::MutatingActionUncertainOutcome, 1),
    ] {
        if write_drift {
            fs::write(mut_tmp.path().join("Cargo.toml"), "[package]\nname = \"drift\"\n").unwrap();
        }
        let lb = mut_mock.lookup_calls();
        let ab = mut_mock.action_calls();
        let err = verify_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, Some(mut_claim.claim_revision)).unwrap_err();
        assert_eq!(err.code, expect_code);
        assert_eq!(mut_mock.lookup_calls() - lb, expect_lookup_delta);
        assert_eq!(mut_mock.action_calls() - ab, 0);
        let tr = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap().traces
            .into_iter().filter(|t| t.node_id == "root:deploy_gate").max_by_key(|t| t.trace_row_id.unwrap_or(0)).unwrap();
        assert_eq!(tr.status, TraceStatus::Running);
    }

    let tr_id = mut_engine.backend().load_run_graph(&mut_rid).unwrap().unwrap().traces
        .into_iter().filter(|t| t.node_id == "root:deploy_gate").max_by_key(|t| t.trace_row_id.unwrap_or(0)).unwrap().trace_row_id.unwrap();
    mut_engine.backend().exec_tx(|tx| tx.finish_trace(&TraceFinishInput {
        trace_row_id: tr_id, status: TraceStatus::Failed, exit_code: None,
        tests_passed: 0, tests_failed: 0, tests_ignored: 0,
        passed_tests: Vec::new(), failed_tests: Vec::new(), check_results: Vec::new(),
        candidate_tree_after: None, finished_at: "2026-11-18T00:01:00.000Z".to_owned(),
    })).unwrap();
    let ab = mut_mock.action_calls();
    assert_eq!(verify_node(&mut_engine, mut_rid.as_str(), "deploy_gate", "w-mut", None, Some(mut_claim.claim_revision)).unwrap_err().code, ErrorCode::OutOfScopeChanges);
    assert_eq!(mut_mock.action_calls() - ab, 0);
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

    match execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
        collection: "milestones".to_owned(), slug: "new-feature".to_owned(), prefix: None, title: Some("New Feature".to_owned()), profile: None,
    })).unwrap() {
        StateCommandOutput::Draft(d) => {
            assert_eq!(d.spec_id, "AC-008");
            assert_eq!(d.spec_path, "docs/milestones/AC-008-new-feature.md");
            assert!(tmp.path().join(&d.spec_path).is_file());
        }
        other => panic!("unexpected draft output: {other:?}"),
    }
    assert_eq!(
        execute_on(&cfg, &harness, &provider, StateCommand::Draft(DraftReq {
            collection: "milestones".to_owned(), slug: "---".to_owned(), prefix: None, title: None, profile: None,
        })).unwrap_err().code,
        ErrorCode::ProfileSchemaInvalid
    );
}

#[test]
fn seam_start_ephemeral_goal_and_cross_workspace_depends_on_matrix() {
    let root_tmp = make_temp_dir("start-main");
    let other_tmp = make_temp_dir("start-other");
    let mut cfg = base_config(root_tmp.path());
    cfg.linked_workspaces.insert("other".to_owned(), other_tmp.path().to_path_buf());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider.clone()).unwrap();

    let bound_p = write_profile(root_tmp.path(), "bound.yaml", "version: 2\ninherit: default\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n    output: { require_counts: false }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\n");
    assert_eq!(
        start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-001.md".to_owned()), goal: Some("Also".to_owned()), ..StartReq::default() }).unwrap_err().code,
        ErrorCode::ProfileSchemaInvalid
    );
    let eph = start_run(&engine, StartReq { goal: Some("Ship ephemeral run".to_owned()), profile: Some(bound_p.clone()), scope: vec!["crates/a/**".to_owned()], in_place: true, ..StartReq::default() }).unwrap();
    assert!(eph.run.is_ephemeral);
    assert!(root_tmp.path().join(eph.materialized_spec_path.unwrap()).is_file());
    assert!(root_tmp.path().join(format!(".acdd/run/{}/profile.resolved.yaml", eph.run.run_id)).is_file());
    clear_run(&engine, eph.run.run_id.as_str());

    fs::create_dir_all(root_tmp.path().join("docs/milestones")).unwrap();
    let dep_spec = "---\nid: AC-010\ntitle: Dependent Run\nstatus: planned\ndepends_on:\n  - \"other/AC-001:t1\"\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - Consumer works\n```\n";
    fs::write(root_tmp.path().join("docs/milestones/AC-010-dep.md"), dep_spec).unwrap();
    fs::write(root_tmp.path().join("docs/milestones/AC-010-bad-task.md"), dep_spec.replace("other/AC-001:t1", "other/AC-001:missing_task")).unwrap();

    let other_engine = StateEngine::open(base_config(other_tmp.path()), provider).unwrap();
    let og1 = build_rich_graph(other_tmp.path(), "run-other-001", IsolationMode::InPlace);
    let tomb1 = sample_tombstone("run-other-001", "other-cleared", Some("AC-001"), TerminalStatus::Cleared, None);
    other_engine.backend().exec_tx(|tx| { tx.insert_run_graph(&og1)?; tx.archive_and_prune_run(&empty_preconditions(1), &tomb1) }).unwrap();
    assert_eq!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()), profile: Some(bound_p.clone()), in_place: true, ..StartReq::default() }).unwrap_err().code, ErrorCode::GateVerificationFailed);

    let mut og2 = build_rich_graph(other_tmp.path(), "run-other-002", IsolationMode::InPlace);
    og2.run.run_name = RunName::new("other-completed").unwrap();
    let mut tomb2 = sample_tombstone("run-other-002", "other-completed", Some("AC-001"), TerminalStatus::Completed, Some("docs/milestones/archive/AC-001.md"));
    tomb2.closed_at = "2026-11-17T10:20:00.000Z".to_owned();
    fs::create_dir_all(other_tmp.path().join("docs/milestones/archive")).unwrap();
    fs::write(other_tmp.path().join("docs/milestones/archive/AC-001.md"), "---\nid: AC-001\ntitle: Producer Completed\nstatus: completed\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Producer task\nscope: [src/]\nstatus: completed\n```\n").unwrap();
    other_engine.backend().exec_tx(|tx| { tx.insert_run_graph(&og2)?; tx.archive_and_prune_run(&empty_preconditions(1), &tomb2) }).unwrap();

    assert_eq!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-bad-task.md".to_owned()), profile: Some(bound_p.clone()), in_place: true, ..StartReq::default() }).unwrap_err().code, ErrorCode::GateVerificationFailed);
    assert!(start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-010-dep.md".to_owned()), profile: Some(bound_p), in_place: true, ..StartReq::default() }).is_ok());
}

#[test]
fn seam_in_memory_lifecycle_subtask_blackboard_extend_scope_reopen_sync_clear() {
    let tmp = make_temp_dir("lifecycle");
    let cfg = base_config(tmp.path());
    let (_, provider) = build_provider_with_mock(&cfg.modules);
    let harness = RunHarness::in_memory().unwrap();
    let mut graph = build_rich_graph(tmp.path(), "run-life-001", IsolationMode::Worktree);

    if let Some(n) = graph.nodes.iter_mut().find(|n| n.node_id == "task:t2:build") {
        n.status = NodeStatus::InProgress;
        n.claim_revision = 1;
        n.worker_id = Some(WorkerId::new("worker-b").unwrap());
    }
    if let Some(n) = graph.nodes.iter_mut().find(|n| n.node_id == "task:t2") {
        n.status = NodeStatus::InProgress;
    }
    graph.claims.push(GateClaimRecord {
        claim_id: None, run_id: RunId::new("run-life-001").unwrap(), node_id: "task:t2:build".to_owned(), claim_revision: 1,
        worker_id: WorkerId::new("worker-b").unwrap(), session_fingerprint: Some("host:1:1".to_owned()),
        worktree: tmp.path().display().to_string(), span_id: "0123456789abcdef".to_owned(),
        claim_tree: TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
        claimed_at: "2026-11-17T10:01:00.000Z".to_owned(), ended_at: None, outcome: ClaimOutcome::Active,
    });
    harness.seed_run(&graph).unwrap();

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::Subtask(SubtaskReq {
        run: Some("run-life-001".to_owned()), task_ref: TaskRef::new("t1").unwrap(), subtask_ref: SubtaskRef::new("s1").unwrap(),
        status: SubtaskStatus::Completed, evidence: Some("   ".to_owned()),
    })).unwrap_err().code, ErrorCode::ProfileSchemaInvalid);
    assert!(matches!(execute_on(&cfg, &harness, &provider, StateCommand::Subtask(SubtaskReq {
        run: Some("run-life-001".to_owned()), task_ref: TaskRef::new("t1").unwrap(), subtask_ref: SubtaskRef::new("s1").unwrap(),
        status: SubtaskStatus::Completed, evidence: Some("implemented in crates/a/src/lib.rs".to_owned()),
    })).unwrap(), StateCommandOutput::Subtask(_)));

    let bb_post_out = execute_on(&cfg, &harness, &provider, StateCommand::BlackboardPost(BlackboardPostReq {
        run: Some("run-life-001".to_owned()), node: Some("t1.build".to_owned()), recipient_node: None, subtask_ref: None,
        gate: Some("build".to_owned()), author: "worker-a".to_owned(), topic: BlackboardTopic::Notes, payload: "shared note context".to_owned(),
    })).unwrap();
    assert_eq!(bb_post_out.into_untagged_json().unwrap()["entry"]["payload"], "shared note context");

    for (author, payload, expected_field) in [(" ", "non-empty", "author"), ("worker-a", " ", "payload")] {
        let err = execute_on(&cfg, &harness, &provider, StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some("run-life-001".to_owned()), node: None, recipient_node: None, subtask_ref: None, gate: None,
            author: author.to_owned(), topic: BlackboardTopic::Notes, payload: payload.to_owned(),
        })).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        assert!(err.message.contains(expected_field));
    }
    let bb_query_out = queries::execute_query(&cfg, &harness, &provider, StateQuery::BlackboardRead(BlackboardReadReq {
        run: Some("run-life-001".to_owned()), topic: Some(BlackboardTopic::Notes), ..BlackboardReadReq::default()
    })).unwrap();
    assert_eq!(bb_query_out.clone().into_untagged_json().unwrap()["total"], 1);
    match bb_query_out {
        StateQueryOutput::BlackboardRead(bb) => assert_eq!(bb.items[0].payload, "shared note context"),
        other => panic!("unexpected BlackboardRead output: {other:?}"),
    }

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::ExtendScope(ExtendScopeReq {
        run: Some("run-life-001".to_owned()), node: "t1".to_owned(), paths: vec!["crates/b/src/main.rs".to_owned()],
        reason: "needs sibling crate".to_owned(), worker_id: None,
    })).unwrap_err().code, ErrorCode::TaskScopeConflict);
    match execute_on(&cfg, &harness, &provider, StateCommand::ExtendScope(ExtendScopeReq {
        run: Some("run-life-001".to_owned()), node: "t1".to_owned(), paths: vec!["crates/c/**".to_owned()],
        reason: "shared test helper".to_owned(), worker_id: None,
    })).unwrap() {
        StateCommandOutput::ExtendScope(e) => {
            assert_eq!(e.contract_revision, 2);
            assert!(e.admitted_scope.contains(&"crates/c/**".to_owned()));
        }
        other => panic!("unexpected ExtendScope output: {other:?}"),
    }

    let snap = harness.load_run_graph(&RunId::new("run-life-001").unwrap()).unwrap().unwrap();
    harness.exec_tx(|tx| {
        let t1_n = snap.nodes.iter().find(|n| n.node_id == "task:t1").unwrap();
        let t1_b = snap.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap();
        let tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
        let mut p1 = NodeStatusPatch::status_only("task:t1", NodeStatus::Completed);
        p1.accepted_tree = Some(Some(tree.clone()));
        let mut p2 = NodeStatusPatch::status_only("task:t1:build", NodeStatus::Completed);
        p2.accepted_tree = Some(Some(tree));
        tx.apply_transition_plan(&snap.run.run_id, &TransitionPlan {
            preconditions: StoragePreconditions {
                expected_run_generation: snap.run.generation,
                expected_node_versions: vec![
                    NodeVersionExpectation { node_id: t1_n.node_id.clone(), expected_version: t1_n.node_version },
                    NodeVersionExpectation { node_id: t1_b.node_id.clone(), expected_version: t1_b.node_version },
                ],
                expected_claim_revision: None, expected_contract_revision: None, expected_worker_id: None, required_predecessor_trees: Vec::new(),
            },
            bump_run_generation: true, new_run_status: None, new_root_cycle_count: None, completed_at: None,
            node_patches: vec![p1, p2], dependency_patches: Vec::new(), claim_patches: Vec::new(),
            new_artifacts: Vec::new(), new_blackboard: Vec::new(), blackboard_patches: Vec::new(), new_subtasks: Vec::new(),
            significant_transition: true,
        })
    }).unwrap();

    assert_eq!(execute_on(&cfg, &harness, &provider, StateCommand::Reopen(ReopenReq {
        run: Some("run-life-001".to_owned()), node: "t1.build".to_owned(), cascade: false, reason: Some("fix".to_owned()),
    })).unwrap_err().code, ErrorCode::DependentTaskCompleted);
    assert!(matches!(execute_on(&cfg, &harness, &provider, StateCommand::Reopen(ReopenReq {
        run: Some("run-life-001".to_owned()), node: "t1.build".to_owned(), cascade: true, reason: Some("cascade".to_owned()),
    })).unwrap(), StateCommandOutput::Reopen(_)));

    fs::write(tmp.path().join("docs/milestones/AC-001-sample.md"), "---\nid: AC-001\ntitle: Sample Updated\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/a/**]\nacceptance_criteria:\n  - One\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [crates/b/**]\ndepends_on: [t1]\nacceptance_criteria:\n  - Two\n```\n### task: t3\n```yaml\ntask_ref: t3\nscope: [crates/d/**]\nacceptance_criteria:\n  - Three\n```\n").unwrap();
    match execute_on(&cfg, &harness, &provider, StateCommand::Sync(SyncReq { run: Some("run-life-001".to_owned()), rebind_profile: false })).unwrap() {
        StateCommandOutput::Sync(s) => assert!(s.added_nodes >= 2),
        other => panic!("unexpected Sync output: {other:?}"),
    }
    match execute_on(&cfg, &harness, &provider, StateCommand::Clear(ClearReq { run: Some("run-life-001".to_owned()), reason: Some("cancelled".to_owned()), force: true })).unwrap() {
        StateCommandOutput::Clear(c) => {
            assert_eq!(c.tombstone.terminal_status, TerminalStatus::Cleared);
            assert!(harness.load_run_graph(&RunId::new("run-life-001").unwrap()).unwrap().is_none());
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
    harness.seed_run(&build_rich_graph(tmp.path(), "run-auto-001", IsolationMode::InPlace)).unwrap();

    match execute_on(&cfg, &harness, &provider, StateCommand::Run(RunReq { run: Some("run-auto-001".to_owned()), jobs: Some(4), allow_main: false, dry_run: true })).unwrap() {
        StateCommandOutput::Run(resp) => {
            assert_eq!(resp.report.waves_executed, 0);
            assert!(resp.report.halted_reason.as_deref().is_some_and(|s| s.contains("jobs=1")));
        }
        other => panic!("unexpected Run dry_run output: {other:?}"),
    }
    assert_eq!(
        execute_on(&cfg, &harness, &provider, StateCommand::Run(RunReq { run: Some("run-auto-001".to_owned()), jobs: Some(1), allow_main: false, dry_run: false })).unwrap_err().code,
        ErrorCode::WorkflowRunnerRequired
    );
    let t1_build = harness.load_run_graph(&RunId::new("run-auto-001").unwrap()).unwrap().unwrap()
        .nodes.into_iter().find(|n| n.node_id == "task:t1:build").unwrap();
    assert_eq!(t1_build.status, NodeStatus::Ready);
    assert_eq!(t1_build.claim_revision, 0);

    // #24 part 1: pruned Completed tombstone resolves post-runner completion cleanly without RunnerNoSubmission.
    let spawn_ok = crate::runner::RoleSpawnOutcome {
        exit_code: Some(0), timed_out: false, agent_session_id: "sess-1".to_owned(), log_relpath: "logs/run.log".to_owned(),
    };
    for (idx, (rid_str, rname, st, expect_ok)) in [
        ("run-pruned-done", "AC-DONE", TerminalStatus::Completed, true),
        ("run-pruned-cleared", "AC-CLR", TerminalStatus::Cleared, false),
    ].into_iter().enumerate() {
        let mut tomb = sample_tombstone(rid_str, rname, None, st, None);
        tomb.workspace = tmp.path().to_string_lossy().into_owned();
        let mut g = build_rich_graph(tmp.path(), rid_str, IsolationMode::InPlace);
        g.run.run_id = tomb.run_id.clone();
        g.run.run_name = tomb.run_name.clone();
        g.run.worktree = Some(format!("{}-{idx}", tmp.path().display()));
        for n in &mut g.nodes { n.run_id = tomb.run_id.clone(); }
        g.dependencies.clear();
        g.subtasks.clear();
        g.artifacts.clear();
        harness.seed_run(&g).unwrap();
        harness.exec_tx(|tx| { tx.archive_and_prune_run(&empty_preconditions(g.run.generation), &tomb) }).unwrap();
        let res = crate::usecases::run::check_post_runner_completion(&harness, &tomb.run_id, "root:handoff", 1, &spawn_ok);
        if expect_ok {
            assert!(res.unwrap());
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::RunnerNoSubmission);
        }
    }

    // #10: managed worktree RoleDef.runner executes inside `claim_resp.worktree`, not primary.
    let wt_tmp = make_temp_dir("run-wt-cwd");
    let wt_root = wt_tmp.path();
    let _ = init_git_repo(wt_root, "main");
    let wt_p = write_profile(wt_root, "wt_runner.yaml", "version: 2\ninherit: default\ngit:\n  isolation: worktree\n  worktrees: managed\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n    output: { require_counts: false }\n  check: { command: \"true\" }\n  lint: { command: \"true\" }\nroles:\n  builder:\n    mode: subagent\n    runner: \"pwd > .acdd_runner_cwd\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n");
    let wt_cfg = base_config(wt_root);
    let (_, wt_prov) = build_provider_with_mock(&wt_cfg.modules);
    let wt_engine = StateEngine::open(wt_cfg, wt_prov).unwrap();
    let wt_start = start_run(&wt_engine, StartReq {
        goal: Some("Run inside managed task worktree".to_owned()), profile: Some(wt_p), worktree: true, ..StartReq::default()
    }).unwrap();
    assert_eq!(
        wt_engine.execute(StateCommand::Run(RunReq { run: Some(wt_start.run.run_id.to_string()), jobs: Some(1), allow_main: false, dry_run: false })).unwrap_err().code,
        ErrorCode::RunnerNoSubmission
    );
    let task_wt_dir = wt_root.join(".worktrees").join(format!("{}-goal", wt_start.run.run_name));
    assert!(task_wt_dir.join(".acdd_runner_cwd").is_file());
    assert!(!wt_root.join(".acdd_runner_cwd").exists());

    // #drift-reset: poisoned builder session_cache MUST be reused on `defect`, rotated on `architectural_drift -> rearchitect -> build`.
    let dr_root = wt_tmp.path().join("drift_reset_repo");
    let dr_sessions_log = wt_tmp.path().join("dr_sessions.log");
    let dr_prompts_log = wt_tmp.path().join("dr_prompts.log");
    init_git_repo(&dr_root, "main");
    let dr_p = write_profile(
        &dr_root,
        "drift_reset.yaml",
        &format!(
            "version: 2\ninherit: default\ngit: {{ isolation: in_place, worktrees: manual }}\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {{test}}\"\n    output: {{ require_counts: false }}\n  build_check: {{ command: \"true\" }}\n  verify_all: {{ command: \"true\" }}\nroles:\n  builder:\n    mode: subagent\n    reuse_on_reject: true\n    runner: \"sh -c 'echo $ACDD_AGENT_SESSION_ID >> {}; cat >> {}'\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n          pass: review\n        - id: review\n          role: independent_reviewer\n          proof: none\n          independent_from: [build]\n          pass: deliver\n          fail: build\n          fail_drift: rearchitect\n        - id: deliver\n          role: independent_reviewer\n          independent_from: [build]\n          need: [build, review]\n          receipt: true\n          fail: build\n        - id: rearchitect\n          role: architect\n          pass: build\n",
            dr_sessions_log.display(),
            dr_prompts_log.display(),
        ),
    );
    fs::create_dir_all(dr_root.join("docs/milestones")).unwrap();
    fs::write(
        dr_root.join("docs/milestones/AC-DR.md"),
        "---\nid: AC-DR\ntitle: Drift Session Reset\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\n```\n",
    )
    .unwrap();
    git_ok(&dr_root, &["add", "."]);
    git_ok(&dr_root, &["commit", "-m", "seed drift reset"]);
    let dr_cfg = base_config(&dr_root);
    let (_, dr_prov) = build_provider_with_mock(&dr_cfg.modules);
    let dr_eng = StateEngine::open(dr_cfg.clone(), dr_prov.clone()).unwrap();
    let dr_start = start_run(
        &dr_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-DR.md".to_owned()),
            profile: Some(dr_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let dr_rid = dr_start.run.run_id.clone();
    let dr_profile = crate::usecases::lifecycle::parse_profile_from_run(&dr_start.run).unwrap();
    let session_cache = std::sync::Mutex::new(std::collections::BTreeMap::new());

    // Helper closure triggers builder execution via `execute_gate_node` (returns RunnerNoSubmission after spawn).
    let run_builder_step = || {
        let snap = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
        let err = crate::usecases::run::execute_gate_node(
            &dr_cfg,
            dr_eng.backend(),
            &dr_prov,
            &dr_profile,
            &snap,
            "task:t1:build",
            true,
            &session_cache,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::RunnerNoSubmission);
    };

    // Pass 1: fresh builder session created & cached.
    run_builder_step();
    let snap_b1 = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
    let rev_b1 = snap_b1.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap().claim_revision;
    submit_req(&dr_eng, pass_req(dr_rid.as_str(), "t1.build", "runner-builder", Some(rev_b1), true)).unwrap();

    // Reject review with `defect` -> cached session MUST be reused on next builder step.
    let rc1 = claim_node(&dr_eng, dr_rid.as_str(), "t1.review", "rev-1", None, false, false, None).unwrap();
    submit_req(
        &dr_eng,
        SubmitReq {
            action: SubmitAction::Reject,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::Defect,
                summary: "minor defect".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Low,
                    path: Some("src/lib.rs".to_owned()),
                    message: "typo".to_owned(),
                    remediation: "fix typo".to_owned(),
                }],
            }),
            ..pass_req(dr_rid.as_str(), "t1.review", "rev-1", Some(rc1.claim_revision), true)
        },
    )
    .unwrap();
    run_builder_step();
    let snap_b2 = dr_eng.backend().load_run_graph(&dr_rid).unwrap().unwrap();
    let rev_b2 = snap_b2.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap().claim_revision;
    submit_req(&dr_eng, pass_req(dr_rid.as_str(), "t1.build", "runner-builder", Some(rev_b2), true)).unwrap();

    // Reject review with `architectural_drift` -> triggers `rearchitect` & invalidates poisoned session.
    let rc2 = claim_node(&dr_eng, dr_rid.as_str(), "t1.review", "rev-2", None, false, false, None).unwrap();
    submit_req(
        &dr_eng,
        SubmitReq {
            action: SubmitAction::Reject,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::ArchitecturalDrift,
                summary: "layer violation".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::High,
                    path: Some("src/lib.rs".to_owned()),
                    message: "bypassed canonical owner".to_owned(),
                    remediation: "route through owner".to_owned(),
                }],
            }),
            ..pass_req(dr_rid.as_str(), "t1.review", "rev-2", Some(rc2.claim_revision), true)
        },
    )
    .unwrap();
    let arch_c = claim_node(&dr_eng, dr_rid.as_str(), "t1.rearchitect", "arch-1", None, false, false, None).unwrap();
    submit_req(&dr_eng, pass_req(dr_rid.as_str(), "t1.rearchitect", "arch-1", Some(arch_c.claim_revision), true)).unwrap();

    run_builder_step();
    let sessions_raw = fs::read_to_string(&dr_sessions_log).unwrap();
    let sessions: Vec<&str> = sessions_raw.lines().collect();
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions[0], sessions[1], "defect rejection must reuse builder session");
    assert_ne!(sessions[1], sessions[2], "architectural_drift must rotate poisoned builder session");
    let prompts_raw = fs::read_to_string(&dr_prompts_log).unwrap();
    assert!(prompts_raw.contains("CONTEXT RESET REQUIRED"));
}

#[test]
fn seam_git_e2e_claim_verify_submit_snapshot_rollback_integrate_and_archive() {
    // 1. Full Git E2E run: pre_verify (call+check+recovery), contract, immutable, reject, integrate, need: stale, archive, landing, depends_on.
    let tmp = make_temp_dir("git-e2e");
    let repo = tmp.path();
    let _initial_head = init_git_repo(repo, "feat/phase2-e2e");
    let e2e_p = write_profile(repo, "e2e.yaml", r#"version: 2
inherit: default
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output: { require_counts: false }
  build_check: { command: "true" }
  verify_all: { command: "true" }
  ok_cmd:
    command: "printf '1 passed; 0 failed\n'"
    output: { counts: { passed: '(\d+)\s+passed', failed: '(\d+)\s+failed' } }
  test_cmd:
    command: "sh -c 'printf \"test t1_tests ... FAILED\\n0 passed; 1 failed\\n\"; exit 101' -- {test_filter}"
    test_filter: "--filter {test}"
    output:
      counts: { passed: '(\d+)\s+passed', failed: '(\d+)\s+failed' }
      tests: { passed: '^test (\S+) \.\.\. ok$', failed: '^test (\S+) \.\.\. FAILED$' }
git:
  isolation: worktree
  worktrees: managed
  commit_mode: plumbing
  integrate: ff-only
gates:
  - id: pre_verify
    proof: command
    command: ok_cmd
    checks: [{ check: http.headers }]
    call: http.dispatch
    with: { target: prod-api }
    context: [http.endpoint_summary]
    save: { post: none }
  - tasks:
      gates:
        - id: build
          role: builder
          proof: contract
          command: test_cmd
          expected_failure: { exit_codes: [101] }
          save: { immutable: true }
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          independent_from: [build]
          need: [build]
          contours: [claims]
          receipt: true
          save: { post: commit }
          fail: build
  - id: handoff
    proof: none
    need: [pre_verify]
    receipt: true
    save: { post: commit, archive: true, prune: true }
"#);
    fs::create_dir_all(repo.join("docs/milestones")).unwrap();
    fs::write(repo.join("docs/milestones/AC-020-e2e.md"), "---\nid: AC-020\ntitle: Full Phase 2 E2E Run\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs]\nacceptance_criteria:\n  - Function returns 42\n```\n").unwrap();

    let cfg = base_config(repo);
    let (mock, provider) = build_provider_with_mock(&cfg.modules);
    let engine = StateEngine::open(cfg, provider).unwrap();
    let start_resp = start_run(&engine, StartReq { spec_path: Some("docs/milestones/AC-020-e2e.md".to_owned()), profile: Some(e2e_p), worktree: true, ..StartReq::default() }).unwrap();
    let rid = start_resp.run.run_id.to_string();
    assert_eq!(start_resp.ready_nodes, vec!["pre_verify".to_owned()]);

    let pre_claim = claim_node(&engine, &rid, "pre_verify", "worker-pre", Some("host-a:999999:1"), false, false, None).unwrap();
    assert!(pre_claim.context_lenses.contains_key("http.endpoint_summary"));
    assert_eq!(claim_node(&engine, &rid, "pre_verify", "worker-pre", Some("host-a:999999:1"), false, false, None).unwrap().claim_revision, pre_claim.claim_revision);
    assert_eq!(claim_node(&engine, &rid, "pre_verify", "thief", Some("host-b:111:1"), true, false, None).unwrap_err().code, ErrorCode::TaskAlreadyClaimed);

    let pre_verify_resp = verify_node(&engine, &rid, "pre_verify", "worker-pre", None, Some(pre_claim.claim_revision)).unwrap();
    assert!(pre_verify_resp.passed && pre_verify_resp.tests_passed == 1 && pre_verify_resp.check_results.len() == 1 && mock.action_calls() == 1);

    // Inject interrupted Running trace and verify idempotency recovery.
    let pre_snap = engine.backend().load_run_graph(&start_resp.run.run_id).unwrap().unwrap();
    let pre_node = pre_snap.nodes.iter().find(|n| n.node_id == "root:pre_verify").unwrap().clone();
    engine.backend().exec_tx(|tx| tx.start_trace(
        &crate::machine::graph::build_preconditions(&pre_snap, &[pre_node.node_id.as_str()], Some(&pre_node)),
        &TraceStartInput {
            run_id: start_resp.run.run_id.clone(), node_id: pre_node.node_id.clone(),
            claim_revision: pre_node.claim_revision, contract_revision: pre_node.contract_revision,
            execution_digest: pre_verify_resp.execution_digest.clone(),
            idempotency_key: Some(format!("{}:{}:{}:{}", start_resp.run.run_id, pre_node.node_id, pre_node.claim_revision, pre_verify_resp.execution_digest)),
            command_id: Some("ok_cmd".to_owned()), command_line: None, action_id: Some("http.dispatch".to_owned()),
            test_target: None, candidate_tree_before: pre_node.claim_tree.clone().unwrap(),
            log_relpath: "logs/crash.log".to_owned(), started_at: "2026-11-17T10:02:00.000Z".to_owned(),
        },
    ).map(|_| ())).unwrap();
    let action_before = mock.action_calls();
    assert!(verify_node(&engine, &rid, "pre_verify", "worker-pre", None, Some(pre_claim.claim_revision)).unwrap().passed);
    assert_eq!((mock.action_calls(), mock.lookup_calls()), (action_before, 1));

    let mut pre_pass = pass_req(&rid, "pre_verify", "worker-pre", Some(pre_claim.claim_revision), false);
    pre_pass.summary = Some("pre verified".to_owned());
    assert!(submit_req(&engine, pre_pass).unwrap().snapshot_ref.is_none());

    let t1_claim = claim_node(&engine, &rid, "t1.build", "builder-1", Some("host-a:200:1"), false, false, None).unwrap();
    let task_wt = PathBuf::from(&t1_claim.worktree);
    assert!(task_wt.is_dir());
    fs::write(task_wt.join("README.md"), "# Out of scope edit\n").unwrap();
    assert_eq!(verify_node(&engine, &rid, "t1.build", "builder-1", Some("t1_tests"), Some(t1_claim.claim_revision)).unwrap_err().code, ErrorCode::OutOfScopeChanges);

    fs::write(task_wt.join("README.md"), "# Test Repo\n").unwrap();
    let clean_head = current_head_commit(&task_wt).unwrap();
    fs::write(task_wt.join("src/lib.rs"), "pub fn broken() -> i32 { 0 }\n").unwrap();
    let StateCommandOutput::Rollback(rb_resp) = engine.execute(StateCommand::Rollback(RollbackReq {
        run: Some(rid.clone()), node: Some("t1.build".to_owned()), target: clean_head.to_string(), worktree: Some(task_wt.clone()),
    })).unwrap() else { panic!("expected RollbackResp") };
    assert_eq!(rb_resp.target_tree, t1_claim.candidate_tree);
    assert!(rb_resp.restored_paths.iter().any(|p| p == "src/lib.rs"));
    assert!(fs::read_to_string(task_wt.join("src/lib.rs")).unwrap().contains("pub fn base()"));

    let t1_c2 = claim_node(&engine, &rid, "t1.build", "builder-1", Some("host-a:200:1"), false, false, None).unwrap();
    let mut unverified_req = pass_req(&rid, "t1.build", "builder-1", Some(t1_c2.claim_revision), false);
    unverified_req.test_target = Some("t1_tests".to_owned());
    assert_eq!(submit_req(&engine, unverified_req).unwrap_err().code, ErrorCode::TaskProofUnobserved);

    assert!(verify_node(&engine, &rid, "t1.build", "builder-1", Some("t1_tests"), Some(t1_c2.claim_revision)).unwrap().passed);
    fs::write(task_wt.join("src/lib.rs"), "pub fn feat() -> i32 { 42 }\n").unwrap();
    let mut immut_req = pass_req(&rid, "t1.build", "builder-1", Some(t1_c2.claim_revision), false);
    immut_req.test_target = Some("t1_tests".to_owned());
    assert_eq!(submit_req(&engine, immut_req).unwrap_err().code, ErrorCode::TaskCandidateMismatch);

    let t1_c3 = claim_node(&engine, &rid, "t1.build", "builder-1", Some("host-a:200:2"), true, false, None).unwrap();
    assert!(t1_c3.claim_revision > t1_c2.claim_revision);
    assert!(verify_node(&engine, &rid, "t1.build", "builder-1", Some("t1_tests"), Some(t1_c3.claim_revision)).unwrap().passed);
    let mut b_pass = pass_req(&rid, "t1.build", "builder-1", Some(t1_c3.claim_revision), false);
    b_pass.summary = Some("build contract verified".to_owned());
    submit_req(&engine, b_pass).unwrap();

    // Independence check -> force steal reject -> rework -> review pass -> integrate -> need drift -> reopen -> handoff archive.
    let same_rev_c = claim_node(&engine, &rid, "t1.review", "builder-1", Some("host-a:200:2"), false, false, None).unwrap();
    let contours = BTreeMap::from([("claims".to_owned(), ReviewContourResult { applicable: true, evidence: "verified feat() returns 42".to_owned() })]);
    let same_worker_req = SubmitReq {
        review_evidence: Some(ReviewEvidence { decision: SubmitAction::Pass, contours: contours.clone(), summary: "self pass".to_owned(), notes: None }),
        ..pass_req(&rid, "t1.review", "builder-1", Some(same_rev_c.claim_revision), false)
    };
    assert_eq!(submit_req(&engine, same_worker_req).unwrap_err().code, ErrorCode::TaskReviewNotIndependent);

    let rev_c = claim_node(&engine, &rid, "t1.review", "reviewer-1", Some("host-a:201:1"), true, true, None).unwrap();
    submit_req(&engine, SubmitReq {
        action: SubmitAction::Reject,
        rejection_findings: Some(RejectionFindings {
            kind: RejectionKind::Defect,
            summary: "need cleaner docs comment".to_owned(),
            blackboard_message_id: None,
            items: vec![FindingItem {
                severity: FindingSeverity::Medium,
                path: Some("src/lib.rs".to_owned()),
                message: "missing doc comment".to_owned(),
                remediation: "add /// doc comment".to_owned(),
            }],
        }),
        ..pass_req(&rid, "t1.review", "reviewer-1", Some(rev_c.claim_revision), false)
    }).unwrap();

    let b2 = claim_node(&engine, &rid, "t1.build", "builder-1", Some("host-a:200:1"), false, false, None).unwrap();
    verify_node(&engine, &rid, "t1.build", "builder-1", Some("t1_tests"), Some(b2.claim_revision)).unwrap();
    let mut b2_pass = pass_req(&rid, "t1.build", "builder-1", Some(b2.claim_revision), false);
    b2_pass.test_target = Some("t1_tests".to_owned());
    submit_req(&engine, b2_pass).unwrap();

    let r2 = claim_node(&engine, &rid, "t1.review", "reviewer-1", Some("host-a:201:1"), false, false, None).unwrap();
    assert_eq!(
        r2.unresolved_findings
            .as_ref()
            .and_then(|f| f["findings"]["summary"].as_str()),
        Some("need cleaner docs comment")
    );
    let rev_pass = submit_req(&engine, SubmitReq {
        review_evidence: Some(ReviewEvidence { decision: SubmitAction::Pass, contours, summary: "LGTM".to_owned(), notes: None }),
        summary: Some("review passed".to_owned()),
        ..pass_req(&rid, "t1.review", "reviewer-1", Some(r2.claim_revision), false)
    }).unwrap();
    assert!(rev_pass.committed_oid.is_some());

    assert_eq!(claim_node(&engine, &rid, "handoff", "handoff-1", Some("host-a:202:1"), false, false, None).unwrap_err().code, ErrorCode::TaskNotIntegrated);
    let StateCommandOutput::Integrate(int_resp) = engine.execute(StateCommand::Integrate(IntegrateReq {
        run: Some(rid.clone()), task_ref: None, strategy: None, allow_main: false,
    })).unwrap() else { panic!("expected IntegrateResp") };
    assert_eq!(int_resp.integrated_tasks, vec![TaskRef::new("t1").unwrap()]);

    let h_claim = claim_node(&engine, &rid, "handoff", "handoff-1", Some("host-a:202:1"), false, false, None).unwrap();
    let mut drift_req = pass_req(&rid, "handoff", "handoff-1", Some(h_claim.claim_revision), false);
    drift_req.summary = Some("should detect pre_verify drift".to_owned());
    assert_eq!(submit_req(&engine, drift_req).unwrap_err().code, ErrorCode::TaskCandidateMismatch);
    assert_eq!(engine.backend().load_run_graph(&start_resp.run.run_id).unwrap().unwrap().nodes.iter().find(|n| n.node_id == "root:pre_verify").unwrap().status, NodeStatus::Stale);

    engine.execute(StateCommand::Reopen(ReopenReq {
        run: Some(rid.clone()), node: "pre_verify".to_owned(), cascade: false, reason: Some("reopen stale".to_owned()),
    })).unwrap();
    let pre2 = claim_node(&engine, &rid, "pre_verify", "worker-pre", Some("host-a:999999:2"), false, false, None).unwrap();
    verify_node(&engine, &rid, "pre_verify", "worker-pre", None, Some(pre2.claim_revision)).unwrap();
    let mut pre2_pass = pass_req(&rid, "pre_verify", "worker-pre", Some(pre2.claim_revision), false);
    pre2_pass.summary = Some("re-verified".to_owned());
    submit_req(&engine, pre2_pass).unwrap();

    let mut h_pass = pass_req(&rid, "handoff", "handoff-1", Some(h_claim.claim_revision), false);
    h_pass.summary = Some("all tasks shipped".to_owned());
    let h_sub = submit_req(&engine, h_pass).unwrap();
    assert_eq!(h_sub.run_status, RunStatus::Completed);
    assert_eq!(h_sub.archive_path.as_deref(), Some("docs/milestones/archive/AC-020-e2e.md"));
    let closed_oid = h_sub.committed_oid.expect("handoff committed_oid");
    assert!(!PathBuf::from(start_resp.run.worktree.as_deref().unwrap()).exists());
    assert!(!repo.join("docs/milestones/archive/AC-020-e2e.md").exists());
    assert_eq!(git_ok(repo, &["rev-parse", "--verify", "refs/heads/run/AC-020"]), closed_oid.as_str());
    assert!(git_ok(repo, &["show", &format!("{}:docs/milestones/archive/AC-020-e2e.md", closed_oid.as_str())]).contains("id: AC-020"));

    let pre_land_doc = crate::spec::parse_spec("---\nid: AC-021\ntitle: Dependent Before Landing\nstatus: planned\ndepends_on: [\"AC-020:t1\"]\n---\n### task: t1\n```yaml\ntarget: consume AC-020\nscope: [src/]\n```\n").unwrap();
    crate::usecases::deps::verify_run_level_depends_on(engine.config(), engine.backend(), &pre_land_doc).expect("closed_commit archive satisfies depends_on before landing");
    git_ok(repo, &["merge", "--ff-only", "refs/heads/run/AC-020"]);
    assert!(repo.join("docs/milestones/archive/AC-020-e2e.md").is_file());
    match engine.query(StateQuery::List(ListReq { include_tombstones: true, ..ListReq::default() })).unwrap() {
        StateQueryOutput::List(l) => {
            assert_eq!((l.page.total_runs, l.page.total_tombstones), (0, 1));
            assert_eq!(l.page.tombstones[0].spec_id.as_deref(), Some("AC-020"));
            assert_eq!(l.page.tombstones[0].terminal_status, TerminalStatus::Completed);
            assert!(!l.page.tombstones[0].cleanup_pending);
        }
        other => panic!("unexpected List output: {other:?}"),
    }
    assert!(repo.join(format!(".acdd/run/{rid}/traces.jsonl")).is_file());

    // 2. Unsalted check + fail-closed no-commit archive recovery matrix.
    let nocommit_tmp = make_temp_dir("nocommit-rec");
    let nroot = nocommit_tmp.path();
    init_git_repo(nroot, "main");
    fs::create_dir_all(nroot.join("docs/milestones/archive")).unwrap();
    let nc_p = write_profile(nroot, "nocommit.yaml", r#"version: 2
inherit: none
git: { isolation: in_place, worktrees: manual }
commands:
  contract_cmd:
    command: "sh -c 'printf \"test %s ... FAILED\\n0 passed; 1 failed\\n\" \"$2\"; exit 101' -- {test_filter}"
    test_filter: "--filter {test}"
    output:
      counts: { passed: '(\d+)\s+passed', failed: '(\d+)\s+failed' }
      tests: { passed: '^test (\S+) \.\.\. ok$', failed: '^test (\S+) \.\.\. FAILED$' }
gates:
  - id: pre
    proof: contract
    command: contract_cmd
    checks: [{ check: http.headers }]
    expected_failure: { exit_codes: [101] }
    save: { post: none }
  - tasks:
      gates:
        - id: prep
          proof: none
          pass: work
        - id: work
          proof: none
          need: [prep]
          receipt: true
  - id: handoff
    proof: none
    need: [pre]
    receipt: true
    save: { post: none, immutable: true, archive: true, prune: true }
"#);
    fs::write(nroot.join("docs/milestones/AC-300.md"), "---\nid: AC-300\ntitle: NoCommit Recovery\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: work\nscope: [src/lib.rs]\n```\n").unwrap();
    git_ok(nroot, &["add", "."]);
    git_ok(nroot, &["commit", "-m", "seed nocommit"]);
    let ncfg = base_config(nroot);
    let (nmock, nprov) = build_provider_with_mock(&ncfg.modules);
    let neng = StateEngine::open(ncfg, nprov).unwrap();
    let nstart = start_run(&neng, StartReq { spec_path: Some("docs/milestones/AC-300.md".to_owned()), profile: Some(nc_p), in_place: true, ..StartReq::default() }).unwrap();
    let nrid = nstart.run.run_id.to_string();

    let pre_c = claim_node(&neng, &nrid, "pre", "w-nc", None, false, false, None).unwrap();
    nmock.set_unsalted_check(true);
    assert!(verify_node(&neng, &nrid, "pre", "w-nc", Some("t_unsalted"), Some(pre_c.claim_revision)).unwrap().passed);
    nmock.set_fail_check(true);
    let mut unsalted_req = pass_req(&nrid, "pre", "w-nc", Some(pre_c.claim_revision), true);
    unsalted_req.test_target = Some("t_unsalted".to_owned());
    assert_eq!(submit_req(&neng, unsalted_req).unwrap_err().code, ErrorCode::TaskProofUnobserved);
    nmock.set_fail_check(false);
    nmock.set_unsalted_check(false);

    verify_node(&neng, &nrid, "pre", "w-nc", Some("t_ok"), Some(pre_c.claim_revision)).unwrap();
    let mut pre_ok_req = pass_req(&nrid, "pre", "w-nc", Some(pre_c.claim_revision), true);
    pre_ok_req.test_target = Some("t_ok".to_owned());
    submit_req(&neng, pre_ok_req).unwrap();
    let t1_p = claim_node(&neng, &nrid, "t1.prep", "w-nc", None, false, false, None).unwrap();
    submit_req(&neng, pass_req(&nrid, "t1.prep", "w-nc", Some(t1_p.claim_revision), true)).unwrap();
    let t1_c = claim_node(&neng, &nrid, "t1.work", "w-nc", None, false, false, None).unwrap();
    submit_req(&neng, pass_req(&nrid, "t1.work", "w-nc", Some(t1_c.claim_revision), true)).unwrap();
    let h_nc = claim_node(&neng, &nrid, "handoff", "w-nc", None, false, false, None).unwrap();
    fs::remove_file(nroot.join("docs/milestones/AC-300.md")).unwrap();

    let base_line = nstart.run.target_base.as_deref().map(|b| format!("  target_base: {b}\n")).unwrap_or_default();
    let commit_line = nstart.run.target_base_commit.as_ref().map(|c| format!("  target_base_commit: {c}\n")).unwrap_or_default();
    let mismatched_md = format!(
        "---\nid: AC-300\ntitle: NoCommit Recovery\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n{base_line}{commit_line}---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: work\nscope: [src/lib.rs]\n```\n"
    );
    for (label, content, drift_lib, expect_code) in [
        ("bogus", "not valid spec frontmatter", false, ErrorCode::ExternalGitDrift),
        ("wrong_id", "---\nid: AC-999\ntitle: Wrong\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n---\n# Wrong\n", false, ErrorCode::ExternalGitDrift),
        ("mismatched_closure", mismatched_md.as_str(), false, ErrorCode::ExternalGitDrift),
        ("immutable_drift", mismatched_md.as_str(), true, ErrorCode::TaskCandidateMismatch),
    ] {
        fs::write(nroot.join("docs/milestones/archive/AC-300.md"), content).unwrap();
        if drift_lib {
            fs::write(nroot.join("src/lib.rs"), "pub fn drifted_after_claim() {}\n").unwrap();
        }
        assert_eq!(submit_req(&neng, pass_req(&nrid, "handoff", "w-nc", Some(h_nc.claim_revision), true)).unwrap_err().code, expect_code, "{label}");
    }

    // 3. P7 seams (#25-state, #13, #15, #7, #16-usecases, #5, #2) on shared Git repo.
    let p7_tmp = make_temp_dir("p7-seams");
    let p7_root = p7_tmp.path();
    let p7_base = init_git_repo(p7_root, "feat/p7");
    fs::write(p7_root.join("src/a.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
    fs::write(p7_root.join("src/b.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
    git_ok(p7_root, &["add", "."]);
    git_ok(p7_root, &["commit", "-m", "add src a and b"]);
    let p7_p = write_profile(p7_root, "p7_base.yaml", "version: 2\ninherit: default\ngit: { isolation: worktree, worktrees: manual, commit_mode: plumbing }\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n    output: { require_counts: false }\n  check: { command: \"true\" }\n  lint: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: command\n          command: test\n          pass: review\n        - id: review\n          proof: none\n          need: [build]\n          save: { immutable: true }\n");
    let p7_managed_p = write_profile(p7_root, "p7_managed.yaml", "version: 2\ninherit: none\ngit: { isolation: worktree, worktrees: managed }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n");
    fs::create_dir_all(p7_root.join("docs/milestones")).unwrap();
    fs::write(p7_root.join("docs/milestones/AC-700-seams.md"), "---\nid: AC-700\ntitle: P7 Seams\nstatus: active\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/a.rs]\n```\n### task: t2\n```yaml\ntask_ref: t2\nscope: [src/b.rs]\n```\n").unwrap();

    let p7_cfg = base_config(p7_root);
    let (_, p7_prov) = build_provider_with_mock(&p7_cfg.modules);
    let p7_eng = StateEngine::open(p7_cfg.clone(), p7_prov.clone()).unwrap();

    let wt_s = start_run(&p7_eng, StartReq { spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()), profile: Some(p7_p.clone()), worktree: true, ..StartReq::default() }).unwrap();
    let disk_prof: ResolvedGateProfile = serde_yaml::from_str(&fs::read_to_string(p7_root.join(".acdd/run").join(wt_s.run.run_id.as_str()).join("profile.resolved.yaml")).unwrap()).unwrap();
    assert_eq!((disk_prof.git.isolation, disk_prof.git.worktrees), (IsolationMode::Worktree, WorktreeLifecycle::Managed));
    assert_eq!(crate::usecases::lifecycle::parse_profile_from_run(&wt_s.run).unwrap().git.worktrees, WorktreeLifecycle::Managed);
    assert!(claim_node(&p7_eng, wt_s.run.run_id.as_str(), "t1.build", "w-wt", None, false, false, None).unwrap().worktree.contains(".worktrees/"));
    clear_run(&p7_eng, wt_s.run.run_id.as_str());

    let ip_s = start_run(&p7_eng, StartReq { spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()), profile: Some(p7_p), ..StartReq::default() }).unwrap();
    let ip_rid = ip_s.run.run_id.clone();

    let mut unbound_cfg = p7_cfg.clone();
    unbound_cfg.worktree = p7_root.join(".worktrees/unbound-checkout");
    fs::create_dir_all(&unbound_cfg.worktree).unwrap();
    assert_eq!(StateQueryEngine::open_read_only(unbound_cfg, p7_prov.clone()).unwrap().query(StateQuery::Status(StatusReq::default())).unwrap_err().code, ErrorCode::RunSelectorAmbiguous);

    let mut tiny_cfg = p7_cfg.clone();
    tiny_cfg.response_policy.max_output_bytes = 64;
    let budget_eng = StateEngine::open(tiny_cfg, p7_prov).unwrap();
    assert_eq!(claim_node(&budget_eng, ip_rid.as_str(), "t1.build", "w-budget", None, false, false, None).unwrap_err().code, ErrorCode::GuidanceBudgetExceeded);
    let t1b_after_budget = p7_eng.backend().load_run_graph(&ip_rid).unwrap().unwrap().nodes.into_iter().find(|n| n.node_id == "task:t1:build").unwrap();
    assert_eq!((t1b_after_budget.status, t1b_after_budget.claim_revision), (NodeStatus::Ready, 0));

    let c1 = claim_node(&p7_eng, ip_rid.as_str(), "t1.build", "w-1", None, false, false, None).unwrap();
    fs::write(p7_root.join("src/a.rs"), "pub fn a() -> i32 { 111 }\n").unwrap();
    fs::write(p7_root.join("src/b.rs"), "pub fn b() -> i32 { 222 }\n").unwrap();
    let rb_head = current_head_commit(p7_root).unwrap();
    let StateCommandOutput::Rollback(rb_one) = p7_eng.execute(StateCommand::Rollback(RollbackReq {
        run: Some(ip_rid.to_string()), node: None, target: rb_head.to_string(), worktree: None,
    })).unwrap() else { panic!("expected Rollback") };
    assert!(rb_one.restored_paths.contains(&"src/a.rs".to_owned()) && !rb_one.restored_paths.contains(&"src/b.rs".to_owned()));
    assert_eq!(fs::read_to_string(p7_root.join("src/b.rs")).unwrap(), "pub fn b() -> i32 { 222 }\n");

    let _ = claim_node(&p7_eng, ip_rid.as_str(), "t2.build", "w-2", None, false, false, None).unwrap();
    assert_eq!(p7_eng.execute(StateCommand::Rollback(RollbackReq { run: Some(ip_rid.to_string()), node: None, target: p7_base.to_string(), worktree: None })).unwrap_err().code, ErrorCode::RunSelectorAmbiguous);
    p7_eng.execute(StateCommand::Rollback(RollbackReq { run: Some(ip_rid.to_string()), node: Some("t2.build".to_owned()), target: rb_head.to_string(), worktree: None })).unwrap();

    submit_req(&p7_eng, pass_req(ip_rid.as_str(), "t1.build", "w-1", Some(c1.claim_revision), true)).unwrap();
    let r1 = claim_node(&p7_eng, ip_rid.as_str(), "t1.review", "w-rev1", None, false, false, None).unwrap();
    fs::write(p7_root.join("src/a.rs"), "pub fn a() -> i32 { 999 }\n").unwrap();
    assert_eq!(submit_req(&p7_eng, pass_req(ip_rid.as_str(), "t1.review", "w-rev1", Some(r1.claim_revision), true)).unwrap_err().code, ErrorCode::TaskCandidateMismatch);
    assert_eq!(p7_eng.backend().load_run_graph(&ip_rid).unwrap().unwrap().nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap().status, NodeStatus::Stale);
    clear_run(&p7_eng, ip_rid.as_str());

    let mgr_s = start_run(&p7_eng, StartReq { spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()), profile: Some(p7_managed_p.clone()), ..StartReq::default() }).unwrap();
    let collide_wt = p7_root.join(format!(".worktrees/{}-t1", mgr_s.run.run_name));
    fs::create_dir_all(&collide_wt).unwrap();
    assert_eq!(claim_node(&p7_eng, mgr_s.run.run_id.as_str(), "t1.build", "w-col", None, false, false, Some(collide_wt)).unwrap_err().code, ErrorCode::ExternalGitDrift);
    clear_run(&p7_eng, mgr_s.run.run_id.as_str());

    let start_col_wt = p7_root.join(".worktrees/start-col-run");
    fs::create_dir_all(&start_col_wt).unwrap();
    assert_eq!(start_run(&p7_eng, StartReq {
        spec_path: Some("docs/milestones/AC-700-seams.md".to_owned()), run_name: Some(RunName::new("start-col-run").unwrap()),
        profile: Some(p7_managed_p), worktree_path: Some(start_col_wt), ..StartReq::default()
    }).unwrap_err().code, ErrorCode::ExternalGitDrift);

    // 4. Commit-mode preflight skip (#3), linked-worktree tombstone spec cleanup, and prune without archive.
    let cskip_tmp = make_temp_dir("commit-preflight-skip");
    let cs_root = cskip_tmp.path();
    init_git_repo(cs_root, "main");
    fs::create_dir_all(cs_root.join("docs/milestones/archive")).unwrap();
    let cskip_p = write_profile(cs_root, "cskip.yaml", "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual, commit_mode: plumbing }\ncommands:\n  fail_cmd: { command: \"false\" }\ngates:\n  - id: handoff\n    proof: command\n    command: fail_cmd\n    save: { post: commit, archive: true, prune: true }\n");
    fs::write(cs_root.join("docs/milestones/AC-310.md"), "---\nid: AC-310\ntitle: Commit Preflight Skip\nstatus: planned\n---\n# Empty\n").unwrap();
    git_ok(cs_root, &["add", "."]);
    git_ok(cs_root, &["commit", "-m", "seed AC-310"]);
    let cs_cfg = base_config(cs_root);
    let (_, cs_prov) = build_provider_with_mock(&cs_cfg.modules);
    let cs_eng = StateEngine::open(cs_cfg.clone(), cs_prov).unwrap();
    let cs_s = start_run(&cs_eng, StartReq { spec_path: Some("docs/milestones/AC-310.md".to_owned()), profile: Some(cskip_p.clone()), in_place: true, ..StartReq::default() }).unwrap();
    let cs_c = claim_node(&cs_eng, cs_s.run.run_id.as_str(), "handoff", "w-cs", None, false, false, None).unwrap();
    fs::write(cs_root.join("docs/milestones/archive/AC-310.md"), "---\nid: AC-310\ntitle: Commit Preflight Skip\nstatus: completed\nclosure:\n  status: completed\n  closed_at: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    assert_eq!(submit_req(&cs_eng, pass_req(cs_s.run.run_id.as_str(), "handoff", "w-cs", Some(cs_c.claim_revision), true)).unwrap_err().code, ErrorCode::GateVerificationFailed);
    clear_run(&cs_eng, cs_s.run.run_id.as_str());

    let linked_spec = cs_root.join("linked-wt/docs/milestones/AC-LNK.md");
    fs::create_dir_all(linked_spec.parent().unwrap()).unwrap();
    fs::write(&linked_spec, "# linked spec\n").unwrap();
    fs::write(cs_root.join("docs/milestones/AC-LNK.md"), "---\nid: AC-LNK\ntitle: Linked Spec Replay\nstatus: planned\n---\n# Empty\n").unwrap();
    let linked_wt_abs = cs_root.join("linked-wt").canonicalize().unwrap();
    let lnk_s = start_run(&cs_eng, StartReq { spec_path: Some("docs/milestones/AC-LNK.md".to_owned()), profile: Some(cskip_p), in_place: true, ..StartReq::default() }).unwrap();
    let mut link_tomb = sample_tombstone(lnk_s.run.run_id.as_str(), lnk_s.run.run_name.as_str(), Some("AC-LNK"), TerminalStatus::Completed, Some("docs/milestones/archive/AC-LNK.md"));
    link_tomb.workspace = linked_wt_abs.to_string_lossy().into_owned();
    link_tomb.generation = lnk_s.run.generation + 1;
    link_tomb.cleanup_pending = true;
    link_tomb.cleanup_manifest = vec![
        ArchiveCleanupEntry { kind: "spec_file".to_owned(), path_or_ref: "docs/milestones/AC-LNK.md".to_owned(), workspace: Some(linked_wt_abs.to_string_lossy().into_owned()) },
        ArchiveCleanupEntry { kind: "spec_file".to_owned(), path_or_ref: "docs/milestones/AC-LNK.md".to_owned(), workspace: Some(cs_root.parent().unwrap().join("removed-wt-gone").to_string_lossy().into_owned()) },
    ];
    cs_eng.backend().exec_tx(|tx| tx.archive_and_prune_run(&empty_preconditions(lnk_s.run.generation), &link_tomb)).unwrap();
    let cleaned_spec = crate::usecases::lifecycle::resume_pending_tombstone_cleanups_dyn(&cs_cfg, cs_eng.backend(), None).unwrap();
    assert!(cleaned_spec.iter().any(|p| p == "docs/milestones/AC-LNK.md"));
    assert!(!linked_spec.exists() && !cs_eng.backend().load_tombstone(&lnk_s.run.run_id).unwrap().unwrap().cleanup_pending);

    let prune_p = write_profile(cs_root, "prune_only.yaml", "version: 2\ninherit: none\ngit: { isolation: in_place, worktrees: manual }\ngates:\n  - id: handoff\n    proof: none\n    save: { post: none, archive: false, prune: true }\n");
    fs::write(cs_root.join("docs/milestones/AC-NP.md"), "---\nid: AC-NP\ntitle: Prune Without Archive\nstatus: planned\n---\n# Keep\n").unwrap();
    let np_linked_wt = cs_root.join("np-linked-wt");
    git_ok(cs_root, &["worktree", "add", "-b", "feat/ac-np", np_linked_wt.to_str().unwrap()]);
    let np_linked_spec = np_linked_wt.join("docs/milestones/AC-NP.md");
    fs::create_dir_all(np_linked_spec.parent().unwrap()).unwrap();
    fs::write(&np_linked_spec, "---\nid: AC-NP\ntitle: Prune Without Archive\nstatus: planned\n---\n# Keep Linked\n").unwrap();
    let np_s = start_run(&cs_eng, StartReq { spec_path: Some("docs/milestones/AC-NP.md".to_owned()), profile: Some(prune_p), in_place: true, ..StartReq::default() }).unwrap();
    let np_c = claim_node(&cs_eng, np_s.run.run_id.as_str(), "handoff", "w-np", None, false, false, Some(np_linked_wt.clone())).unwrap();
    let mut np_pass = pass_req(np_s.run.run_id.as_str(), "handoff", "w-np", Some(np_c.claim_revision), true);
    np_pass.worktree = Some(np_linked_wt);
    submit_req(&cs_eng, np_pass).unwrap();
    let np_tomb = cs_eng.backend().load_tombstone(&np_s.run.run_id).unwrap().unwrap();
    assert!(!np_tomb.cleanup_manifest.iter().any(|e| e.kind == "spec_file"));
    assert!(cs_root.join("docs/milestones/AC-NP.md").exists() && np_linked_spec.exists());

    // 5. Mechanical `max_net_loc` budget check (custom pre-task container, untracked UTF-8 path, explicit override decisions),
    //    `{scope_target}` digest sensitivity, custom builder role drift reset, and legacy `v0.6.0` trace compatibility.
    let loc_tmp = make_temp_dir("max-net-loc");
    let loc_root = loc_tmp.path();
    init_git_repo(loc_root, "main");
    fs::create_dir_all(loc_root.join("tests")).unwrap();
    fs::write(loc_root.join("tests/t1_test.rs"), "#[test] fn t0() {}\n").unwrap();
    fs::create_dir_all(loc_root.join("docs/milestones")).unwrap();
    let loc_p = write_profile(
        loc_root,
        "loc.yaml",
        "version: 2\ninherit: default\ngit: { isolation: in_place, worktrees: manual }\ncommands:\n  test:\n    command: \"true\"\n    test_filter: \"--filter {test}\"\n    output: { require_counts: false }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - id: design\n    gates:\n      - id: synthesize\n        role: architect\n        proof: blueprint\n        pass: review\n      - id: review\n        role: independent_reviewer\n        proof: none\n        independent_from: [synthesize]\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          proof: none\n          pass: review\n        - id: review\n          role: independent_reviewer\n          proof: none\n          independent_from: [build]\n          need: [build]\n          receipt: true\n          fail: build\n",
    );
    fs::write(
        loc_root.join("docs/milestones/AC-LOC.md"),
        "---\nid: AC-LOC\ntitle: Max Net LOC\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\nscope: [src/lib.rs, src/é.rs, tests/t1_test.rs]\n```\n",
    )
    .unwrap();
    git_ok(loc_root, &["add", "."]);
    git_ok(loc_root, &["commit", "-m", "seed loc"]);
    let loc_cfg = base_config(loc_root);
    let (_, loc_prov) = build_provider_with_mock(&loc_cfg.modules);
    let loc_eng = StateEngine::open(loc_cfg.clone(), loc_prov.clone()).unwrap();
    let loc_start = start_run(
        &loc_eng,
        StartReq {
            spec_path: Some("docs/milestones/AC-LOC.md".to_owned()),
            profile: Some(loc_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let loc_rid = loc_start.run.run_id.clone();

    let syn_c = claim_node(&loc_eng, loc_rid.as_str(), "design.synthesize", "arch-1", None, false, false, None).unwrap();
    let mut syn_pass = pass_req(loc_rid.as_str(), "design.synthesize", "arch-1", Some(syn_c.claim_revision), true);
    syn_pass.custom_evidence = Some(json!({
        "cited_invariants": ["INV-SINGLE-OWNER"],
        "cited_symbols": ["base"],
        "global_negative_boundaries": ["none"],
        "blackboard_decision_id": "bb-1",
        "tasks": {
            "t1": {
                "layer": "core",
                "summary": "extend base",
                "exported_interfaces": ["base"],
                "negative_boundaries": ["no shadow helpers"],
                "incoming_seams": [],
                "outgoing_seams": [],
                "canonical_owners": ["src/lib.rs::base"],
                "mechanism_rules": ["keep net prod delta <= 2 lines"],
                "max_net_loc": 2
            }
        }
    }));
    submit_req(&loc_eng, syn_pass).unwrap();
    let syn_r = claim_node(&loc_eng, loc_rid.as_str(), "design.review", "rev-bp", None, false, false, None).unwrap();
    submit_req(&loc_eng, pass_req(loc_rid.as_str(), "design.review", "rev-bp", Some(syn_r.claim_revision), true)).unwrap();

    let b_loc = claim_node(&loc_eng, loc_rid.as_str(), "t1.build", "builder-loc", None, false, false, None).unwrap();
    // Test additions (+10 lines) excluded; +2 tracked (`src/lib.rs`) + +2 untracked UTF-8 (`src/é.rs`) -> fails with +4 vs max_net_loc=2.
    fs::write(loc_root.join("tests/t1_test.rs"), (0..11).map(|i| format!("#[test] fn t{i}() {{}}\n")).collect::<String>()).unwrap();
    fs::write(loc_root.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\npub fn f1() {}\npub fn f2() {}\n").unwrap();
    fs::write(loc_root.join("src/é.rs"), "pub fn u1() {}\npub fn u2() {}\n").unwrap();
    let over_err = submit_req(&loc_eng, pass_req(loc_rid.as_str(), "t1.build", "builder-loc", Some(b_loc.claim_revision), true)).unwrap_err();
    assert_eq!(over_err.code, ErrorCode::GateVerificationFailed);
    assert!(over_err.message.contains("+4") && over_err.message.contains("max_net_loc=2"), "unexpected error: {}", over_err.message);

    // Removing untracked `src/é.rs` lowers net prod delta to +2 -> passes immediately.
    fs::remove_file(loc_root.join("src/é.rs")).unwrap();
    submit_req(&loc_eng, pass_req(loc_rid.as_str(), "t1.build", "builder-loc", Some(b_loc.claim_revision), true)).unwrap();

    // Reject review back to build, exceed again (+4 via `src/é.rs`), verify explicit decision table vs denied/insufficient strings.
    let r_loc = claim_node(&loc_eng, loc_rid.as_str(), "t1.review", "rev-loc", None, false, false, None).unwrap();
    submit_req(
        &loc_eng,
        SubmitReq {
            action: SubmitAction::Reject,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::Defect,
                summary: "expand slightly".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Low,
                    path: Some("src/lib.rs".to_owned()),
                    message: "needs f3/f4".to_owned(),
                    remediation: "add u1/u2 with explicit budget override".to_owned(),
                }],
            }),
            ..pass_req(loc_rid.as_str(), "t1.review", "rev-loc", Some(r_loc.claim_revision), true)
        },
    )
    .unwrap();
    let b_loc2 = claim_node(&loc_eng, loc_rid.as_str(), "t1.build", "builder-loc", None, false, false, None).unwrap();
    fs::write(loc_root.join("src/é.rs"), "pub fn u1() {}\npub fn u2() {}\n").unwrap();
    for (decision_payload, expect_ok) in [
        ("Budget exception denied.", false),
        ("{\"approved_max_net_loc\": 2}", false),
        ("BUDGET_OVERRIDE: expanded error handling", true),
    ] {
        loc_eng
            .execute(StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some(loc_rid.to_string()),
                node: Some("t1.build".to_owned()),
                recipient_node: None,
                subtask_ref: None,
                gate: Some("build".to_owned()),
                author: "builder-loc".to_owned(),
                topic: BlackboardTopic::Decisions,
                payload: decision_payload.to_owned(),
            }))
            .unwrap();
        let res = submit_req(
            &loc_eng,
            pass_req(loc_rid.as_str(), "t1.build", "builder-loc", Some(b_loc2.claim_revision), true),
        );
        if expect_ok {
            res.expect(decision_payload);
        } else {
            assert_eq!(res.unwrap_err().code, ErrorCode::GateVerificationFailed, "payload={decision_payload}");
        }
    }

    // `{scope_target}` changes rendered command lines inside `resolve_commands_and_output_spec` (`execution_digest` sensitivity).
    let mut scope_prof: ResolvedGateProfile = serde_json::from_str(&loc_start.run.profile_json).unwrap();
    scope_prof.commands.insert(
        "scoped".to_owned(),
        CommandDef::Detailed(Box::new(crate::profile::DetailedCommandDef {
            command: "cargo check {scope_target}".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::from([("src/**".to_owned(), "--lib".to_owned())]),
            output: None,
        })),
    );
    let scope_gate = crate::profile::GateDef {
        id: "scoped_gate".to_owned(),
        command: Some("scoped".to_owned()),
        ..Default::default()
    };
    let (cmds_src, _) = crate::usecases::verify::resolve_commands_and_output_spec(
        &scope_prof,
        &scope_gate,
        &["src/lib.rs".to_owned()],
        None,
    )
    .unwrap();
    let (cmds_docs, _) = crate::usecases::verify::resolve_commands_and_output_spec(
        &scope_prof,
        &scope_gate,
        &["docs/readme.md".to_owned()],
        None,
    )
    .unwrap();
    assert_ne!(cmds_src, cmds_docs);

    // Custom non-`builder` role context reset uses persisted `reset_gate_id` when present, falling back to `builder` only when absent.
    let mut reset_snap = loc_eng.backend().load_run_graph(&loc_rid).unwrap().unwrap();
    let impl_node = reset_snap.nodes.iter().find(|n| n.node_id == "task:t1:build").unwrap().clone();
    let mut impl_gate = crate::profile::GateDef {
        id: "build".to_owned(),
        role: Some("implementer".to_owned()),
        ..Default::default()
    };
    reset_snap.artifacts.push(crate::types::GateArtifactRecord {
        run_id: loc_rid.clone(),
        node_id: "task:t1:review".to_owned(),
        claim_revision: 9,
        kind: crate::types::ArtifactKind::Findings,
        payload_json: json!({
            "kind": "architectural_drift",
            "reset_worker_context": true,
            "reset_gate_id": "build",
            "summary": "custom role drift",
            "items": [{ "severity": "high", "message": "drift", "remediation": "rebuild" }]
        })
        .to_string(),
        snapshot_ref: None,
        snapshot_commit: None,
        created_at: "2099-01-01T00:00:00Z".to_owned(),
    });
    assert!(crate::usecases::commands::is_context_reset_required(&reset_snap, &impl_node, &impl_gate));
    reset_snap.artifacts.last_mut().unwrap().payload_json = json!({
        "kind": "architectural_drift",
        "reset_worker_context": true,
        "summary": "legacy without reset_gate_id",
        "items": [{ "severity": "high", "message": "drift", "remediation": "rebuild" }]
    })
    .to_string();
    assert!(!crate::usecases::commands::is_context_reset_required(&reset_snap, &impl_node, &impl_gate));
    impl_gate.role = Some("builder".to_owned());
    assert!(crate::usecases::commands::is_context_reset_required(&reset_snap, &impl_node, &impl_gate));

    // Legacy `v0.6.0` `runs.profile_json` (`save.pre: snapshot`, `save.post: snapshot`) preserves valid pre-upgrade verified contract traces.
    let v60_policy = crate::usecases::lifecycle::build_path_policy(&loc_cfg, loc_eng.backend(), &[]).unwrap();
    let v60_tree = crate::git_ops::snapshot::current_scope_tree(loc_root, None, &v60_policy).unwrap();
    let v60_rid = RunId::new("run-v60-compat").unwrap();
    let mut v60_prof_val = json!({
        "version": 2,
        "roles": {},
        "commands": { "fail_cmd": { "command": "false" } },
        "gates": [{
            "id": "v60_contract",
            "proof": "contract",
            "command": "fail_cmd",
            "scope": ["src/lib.rs"],
            "save": { "pre": "snapshot", "post": "snapshot", "immutable": false, "archive": false, "prune": false }
        }]
    });
    let v60_profile_json = v60_prof_val.to_string();
    let v60_prof: ResolvedGateProfile = serde_json::from_value(v60_prof_val.take()).unwrap();
    assert!(v60_prof.gates[0].save.had_legacy_save_compat);
    let v60_legacy_def_digest = v60_prof.gates[0].legacy_v060_definition_digest().unwrap();
    assert_ne!(
        v60_legacy_def_digest,
        v60_prof.gates[0].definition_digest().unwrap()
    );
    let (v60_cmds, v60_out) = crate::usecases::verify::resolve_commands_and_output_spec(
        &v60_prof,
        &v60_prof.gates[0],
        &["src/lib.rs".to_owned()],
        None,
    )
    .unwrap();
    let v60_exec_digest = crate::machine::verify::compute_execution_digest(&crate::machine::ExecutionDigestInput {
        contract_revision: 1,
        gate_def_digest: v60_legacy_def_digest,
        admitted_scope: vec!["src/lib.rs".to_owned()],
        resolved_commands: v60_cmds,
        test_target: None,
        output_spec: v60_out,
        call_with: serde_json::json!({}),
        checks_params: serde_json::Value::Array(Vec::new()),
        index_generation: None,
        live_files_digest: None,
    })
    .unwrap();
    let v60_head = current_head_commit(loc_root).unwrap();
    let v60_snap = RunGraphSnapshot {
        run: RunRecord {
            run_id: v60_rid.clone(),
            run_name: RunName::new("v60-compat").unwrap(),
            collection: None,
            workspace: loc_root.to_string_lossy().into_owned(),
            profile_id: "default".to_owned(),
            profile_json: v60_profile_json,
            spec_path: None,
            spec_id: None,
            spec_digest: "v60".to_owned(),
            is_ephemeral: false,
            isolation: IsolationMode::InPlace,
            target_base: None,
            target_base_commit: None,
            branch: None,
            worktree: None,
            trace_id: "v60-trace".to_owned(),
            status: RunStatus::Active,
            cancellation_reason: None,
            root_cycle_count: 0,
            generation: 1,
            created_at: "2026-11-17T10:00:00.000Z".to_owned(),
            completed_at: None,
        },
        nodes: vec![GateNodeRecord {
            run_id: v60_rid.clone(),
            node_id: "root:v60_contract".to_owned(),
            parent_id: None,
            kind: NodeKind::RootGate,
            task_ref: None,
            gate_id: "v60_contract".to_owned(),
            seq_order: 1,
            status: NodeStatus::InProgress,
            node_version: 1,
            isolation: IsolationMode::InPlace,
            attempt: 0,
            cycle_count: 0,
            max_attempts: 3,
            claim_revision: 1,
            contract_revision: 1,
            worker_id: Some(WorkerId::new("w-v60").unwrap()),
            session_fingerprint: None,
            branch: None,
            worktree: None,
            baseline_commit: Some(v60_head),
            claim_tree: Some(v60_tree.clone()),
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: Vec::new(),
            invariants: InvariantsState::default(),
            scope: vec!["src/lib.rs".to_owned()],
            extended_scope: Vec::new(),
            active_span_id: None,
        }],
        dependencies: Vec::new(),
        claims: vec![GateClaimRecord {
            claim_id: None,
            run_id: v60_rid.clone(),
            node_id: "root:v60_contract".to_owned(),
            claim_revision: 1,
            worker_id: WorkerId::new("w-v60").unwrap(),
            session_fingerprint: None,
            worktree: loc_root.to_string_lossy().into_owned(),
            span_id: "0123456789abcdef".to_owned(),
            claim_tree: v60_tree.clone(),
            claimed_at: "2026-11-17T10:00:01.000Z".to_owned(),
            ended_at: None,
            outcome: ClaimOutcome::Active,
        }],
        traces: Vec::new(),
        artifacts: Vec::new(),
        subtasks: Vec::new(),
    };
    loc_eng
        .backend()
        .exec_tx(|tx| {
            tx.insert_run_graph(&v60_snap)?;
            let pre = empty_preconditions(1);
            let tid = tx.start_trace(
                &pre,
                &TraceStartInput {
                    run_id: v60_rid.clone(),
                    node_id: "root:v60_contract".to_owned(),
                    claim_revision: 1,
                    contract_revision: 1,
                    execution_digest: v60_exec_digest,
                    idempotency_key: None,
                    command_id: Some("fail_cmd".to_owned()),
                    command_line: None,
                    action_id: None,
                    test_target: None,
                    candidate_tree_before: v60_tree.clone(),
                    log_relpath: "logs/v60.log".to_owned(),
                    started_at: "2026-11-17T10:00:02.000Z".to_owned(),
                },
            )?;
            tx.finish_trace(&TraceFinishInput {
                trace_row_id: tid.trace_row_id.unwrap(),
                status: TraceStatus::ExpectedFailure,
                exit_code: Some(1),
                tests_passed: 0,
                tests_failed: 0,
                tests_ignored: 0,
                passed_tests: Vec::new(),
                failed_tests: Vec::new(),
                check_results: Vec::new(),
                candidate_tree_after: Some(v60_tree),
                finished_at: "2026-11-17T10:00:03.000Z".to_owned(),
            })
        })
        .unwrap();
    let v60_sub = submit_req(&loc_eng, pass_req(v60_rid.as_str(), "v60_contract", "w-v60", Some(1), true)).unwrap();
    assert_eq!(v60_sub.new_node_status, NodeStatus::Completed);
}
