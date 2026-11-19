use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::StateError;
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
use acdd_state::context::ClaimResp;
use acdd_state::git_ops::commit::current_head_commit;
use acdd_state::profile::{resolve_optional_profile_ref_with_dirs, CommandDef};
use acdd_state::spec::materialize_ephemeral_goal_spec;
use acdd_state::types::{
    GateNodeRecord, InvariantsState, IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot,
    RunRecord, RunStatus, RunTombstoneRecord, StoragePreconditions, SubmitAction, SubtaskRecord,
    SubtaskStatus, TaskDependencyRecord, TerminalStatus,
};
use acdd_state::usecases::dto::{
    ClaimReq, ClearReq, StartReq, StartResp, SubmitReq, SubmitResp, VerifyReq, VerifyResp,
};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine, StateEngineConfig};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

pub struct MockHttpProvider {
    tool_calls: AtomicUsize,
    action_calls: AtomicUsize,
    lookup_calls: AtomicUsize,
    unsalted_check: AtomicBool,
    fail_check: AtomicBool,
    index_gen: AtomicUsize,
}

impl MockHttpProvider {
    pub fn new() -> Self {
        Self {
            tool_calls: AtomicUsize::new(0),
            action_calls: AtomicUsize::new(0),
            lookup_calls: AtomicUsize::new(0),
            unsalted_check: AtomicBool::new(false),
            fail_check: AtomicBool::new(false),
            index_gen: AtomicUsize::new(0),
        }
    }

    pub fn set_unsalted_check(&self, v: bool) { self.unsalted_check.store(v, Ordering::SeqCst); }
    pub fn set_fail_check(&self, v: bool) { self.fail_check.store(v, Ordering::SeqCst); }
    pub fn set_index_gen(&self, v: usize) { self.index_gen.store(v, Ordering::SeqCst); }
    pub fn tool_calls(&self) -> usize { self.tool_calls.load(Ordering::SeqCst) }
    pub fn action_calls(&self) -> usize { self.action_calls.load(Ordering::SeqCst) }
    pub fn lookup_calls(&self) -> usize { self.lookup_calls.load(Ordering::SeqCst) }
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

    fn observe_freshness(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<acdd_core::ports::FreshnessObservation> {
        let idx = self.index_gen.load(Ordering::SeqCst);
        let index_generation = (idx > 0).then(|| format!("gen-{idx}"));
        match self.run_check(check, params, req)? {
            DomainCheckOutcome::Passed { execution_digest_salt, .. } => {
                Ok(acdd_core::ports::FreshnessObservation::Passed {
                    index_generation,
                    execution_digest_salt,
                })
            }
            DomainCheckOutcome::Failed { summary, .. } => {
                Ok(acdd_core::ports::FreshnessObservation::Failed { summary })
            }
        }
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> Result<Value> {
        Ok(json!({ "lens": lens, "status": "ok" }))
    }

    fn call_tool(&self, tool: &str, args: &Value, req: &DomainToolReq) -> Result<DomainToolOutput> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        let mut obj = serde_json::Map::new();
        obj.insert("tool".to_owned(), json!(tool));
        obj.insert("url".to_owned(), args.get("url").cloned().unwrap_or(Value::Null));
        obj.insert("status".to_owned(), json!(200));
        if let Some(ref wt) = req.worktree {
            obj.insert("worktree".to_owned(), json!(wt.to_string_lossy().into_owned()));
        }
        Ok(DomainToolOutput::Json(Value::Object(obj)))
    }
}

pub fn make_temp_dir(label: &str) -> TempDir {
    tempfile::Builder::new().prefix(&format!("acdd-usecases-{label}-")).tempdir().expect("create tempdir")
}

pub fn git_ok(dir: &Path, args: &[&str]) -> String {
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

pub fn init_git_repo(dir: &Path, branch: &str) -> CommitOid {
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

pub fn default_collections() -> BTreeMap<String, SpecCollectionConfig> {
    BTreeMap::from([(
        "milestones".to_owned(),
        SpecCollectionConfig {
            path: "docs/milestones".to_owned(),
            profile: None,
            prefixes: vec!["AC".to_owned(), "M".to_owned()],
            digits: 3,
        },
    )])
}

pub fn base_config(root: &Path) -> StateEngineConfig {
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

pub fn build_provider_with_mock(
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

pub fn empty_preconditions(expected_run_generation: u64) -> StoragePreconditions {
    StoragePreconditions {
        expected_run_generation,
        expected_node_versions: Vec::new(),
        expected_claim_revision: None,
        expected_contract_revision: None,
        expected_worker_id: None,
        required_predecessor_trees: Vec::new(),
    }
}

pub fn sample_tombstone(
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

pub fn start_run(eng: &StateEngine, req: StartReq) -> Result<StartResp, StateError> {
    match eng.execute(StateCommand::Start(req))? {
        StateCommandOutput::Start(resp) => Ok(*resp),
        other => panic!("expected StartResp, got {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn claim_node(
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

pub fn verify_node(
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

pub fn pass_req(rid: &str, node: &str, worker: &str, rev: Option<u32>, allow_main: bool) -> SubmitReq {
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

pub fn submit_req(eng: &StateEngine, req: SubmitReq) -> Result<SubmitResp, StateError> {
    match eng.execute(StateCommand::Submit(Box::new(req)))? {
        StateCommandOutput::Submit(resp) => Ok(*resp),
        other => panic!("expected SubmitResp, got {other:?}"),
    }
}

pub fn clear_run(eng: &StateEngine, rid: &str) {
    eng.execute(StateCommand::Clear(ClearReq {
        run: Some(rid.to_owned()),
        reason: None,
        force: true,
    }))
    .unwrap();
}

pub fn write_profile(root: &Path, name: &str, yaml: &str) -> String {
    let dir = root.join(".acdd/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), yaml).unwrap();
    format!(".acdd/profiles/{name}")
}

pub fn build_rich_graph(root: &Path, run_id_str: &str, isolation: IsolationMode) -> RunGraphSnapshot {
    let mut non_domain_modules = SubsystemRegistry::default();
    non_domain_modules.disabled.insert("code".to_owned());
    non_domain_modules.disabled.insert("docs".to_owned());
    let mut profile =
        resolve_optional_profile_ref_with_dirs(None, None, Some(&non_domain_modules), None)
            .expect("resolve default profile");
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

