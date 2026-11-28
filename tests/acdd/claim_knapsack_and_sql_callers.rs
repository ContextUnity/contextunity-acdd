use acdd_core::config::Adapter;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{CommitOid, RunId, RunName, TaskRef, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{
    CompositeDomainProvider, ContextCapability, DomainActionReq, DomainActionResult,
    DomainCapabilities, DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
};
use acdd_core::response::{pack_knapsack_indices, KnapsackItem};
use acdd_state::context::{ClaimContextRequest, GateContextAssembler};
use acdd_state::workflow::{
    schema::{GateGuidanceRef, GuidanceEntry},
    GateDef, ResolvedWorkflow, RoleDef,
};
use acdd_spec::spec::TaskSpec;
use acdd_spec::{InvariantsState, SpecDocument, SpecStatus};
use acdd_state::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, RunRecord, RunStatus,
};
use contextunity_acdd::state_bridge::build_domain_providers;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

struct StaticLensProvider {
    domain: String,
    lenses: BTreeMap<String, Value>,
}

impl DomainProvider for StaticLensProvider {
    fn domain_id(&self) -> &str {
        &self.domain
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            context: self
                .lenses
                .keys()
                .map(|k| ContextCapability::simple(k.clone()))
                .collect(),
            ..DomainCapabilities::default()
        }
    }

    fn execute_action(
        &self,
        _action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> anyhow::Result<DomainActionResult> {
        anyhow::bail!("unused")
    }

    fn run_check(
        &self,
        _check: &str,
        _params: &Value,
        _req: &DomainCheckReq,
    ) -> anyhow::Result<DomainCheckOutcome> {
        anyhow::bail!("unused")
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
        self.lenses
            .get(lens)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing lens '{lens}'"))
    }
}

fn sample_claim_request(
    workspace: PathBuf,
    context: Vec<String>,
    scope: Vec<String>,
    target: &str,
    response_max_bytes: usize,
) -> ClaimContextRequest {
    let run_id = RunId::new("run-knapsack").unwrap();
    let worker_id = WorkerId::new("w1").unwrap();
    let tree = TreeOid::new("1".repeat(40)).unwrap();
    let commit = CommitOid::new("2".repeat(40)).unwrap();
    let mut profile = ResolvedWorkflow::default();
    profile.paths.insert(
        "instructions".to_owned(),
        vec!["**/AGENTS.md".to_owned(), "TESTS.md".to_owned()],
    );
    profile
        .paths
        .insert("adrs".to_owned(), vec!["docs/adr/**".to_owned()]);
    profile.guidance.insert(
        "g1".to_owned(),
        GuidanceEntry::Text("Follow rules".to_owned()),
    );
    profile
        .roles
        .insert("builder".to_owned(), RoleDef::default());
    let mut gate = GateDef::default();
    gate.id = "build".to_owned();
    gate.role = Some("builder".to_owned());
    gate.guidance = vec![GateGuidanceRef::Slot("g1".to_owned())];
    gate.context = context.into_iter().map(Into::into).collect();
    let ws = workspace.to_string_lossy().into_owned();
    let run = RunRecord {
        run_id: run_id.clone(),
        run_name: RunName::new("knapsack").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: ws.clone(),
        profile_id: "code".to_owned(),
        profile_json: "{}".to_owned(),
        spec_path: Some("docs/milestones/UX-003.md".to_owned()),
        spec_id: None,
        spec_digest: "a".repeat(64),
        attached_specs: Vec::new(),
        is_ephemeral: false,
        isolation: IsolationMode::Worktree,
        target_base: Some("main".to_owned()),
        target_base_commit: Some(commit),
        branch: Some("run/knapsack".to_owned()),
        worktree: Some(ws.clone()),
        trace_id: "3".repeat(32),
        status: RunStatus::Active,
        cancellation_reason: None,
        root_cycle_count: 0,
        generation: 1,
        created_at: "2026-11-25T10:00:00.000Z".to_owned(),
        completed_at: None,
    };
    let node = GateNodeRecord {
        run_id,
        node_id: "task:t1:build".to_owned(),
        parent_id: Some("task:t1".to_owned()),
        kind: NodeKind::TaskGate,
        task_ref: Some(TaskRef::new("t1").unwrap()),
        gate_id: "build".to_owned(),
        seq_order: 1,
        status: NodeStatus::InProgress,
        node_version: 1,
        isolation: IsolationMode::Worktree,
        attempt: 0,
        cycle_count: 0,
        max_attempts: 3,
        claim_revision: 1,
        contract_revision: 1,
        worker_id: Some(worker_id.clone()),
        session_fingerprint: None,
        branch: None,
        worktree: Some(ws),
        baseline_commit: None,
        claim_tree: Some(tree.clone()),
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["default".to_owned()],
        invariants: InvariantsState::default(),
        scope: scope.clone(),
        extended_scope: Vec::new(),
        active_span_id: None,
    };
    let spec = SpecDocument {
        id: "UX-003".to_owned(),
        title: "Knapsack Seam".to_owned(),
        doc_type: "contract".to_owned(),
        workflow: None,
        status: SpecStatus::Active,
        cancellation_reason: None,
        spec: Some("Root".to_owned()),
        goal: None,
        depends_on: Vec::new(),
        owners: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        guidance: None,
        required_skills: Vec::new(),
        repositories: Vec::new(),
        linked_to: Vec::new(),
        closure: None,
        spec_deltas: Vec::new(),
        fields: BTreeMap::new(),
        tasks: vec![TaskSpec {
            task_ref: TaskRef::new("t1").unwrap(),
            target: Some(target.to_owned()),
            spec: None,
            scope,
            extended_scope: Vec::new(),
            depends_on: Vec::new(),
            tags: vec!["default".to_owned()],
            invariants: Vec::new(),
            acceptance_criteria: vec!["Fits budget".to_owned()],
            contract_revision: 1,
            status: SpecStatus::Active,
            subtasks: Vec::new(),
            receipt: None,
        }],
        items: BTreeMap::new(),
        body: String::new(),
        digest: "b".repeat(64),
    };
    ClaimContextRequest {
        run,
        node,
        gate,
        profile,
        spec: Some(spec),
        live_subtasks: Vec::new(),
        claim_revision: 1,
        worker_id,
        worktree: workspace,
        candidate_tree: tree,
        path_policy: WorkspacePathPolicy::default(),
        unresolved_findings: None,
        failed_trace_log_tail: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard_items: Vec::new(),
        context_reset_required: false,
        response_max_bytes,
        hard_max_bytes: None,
        full_detail: response_max_bytes > 10_000,
        proof_pointers: Vec::new(),
    }
}

#[test]
fn knapsack_packs_relevance_within_the_byte_budget() {
    let items = [
        KnapsackItem {
            weight: 11,
            score: 50,
        },
        KnapsackItem {
            weight: 5,
            score: 4,
        },
        KnapsackItem {
            weight: 5,
            score: 5,
        },
    ];
    assert_eq!(pack_knapsack_indices(&items, 10), [1, 2]);
    let ties = [(1, 8), (3, 8), (4, 3), (3, 6), (2, 6), (1, 8)]
        .map(|(weight, score)| KnapsackItem { weight, score });
    assert_eq!(pack_knapsack_indices(&ties, 9), [0, 1, 3, 5]);
    let unequal_cardinality = [
        (2, 2),
        (4, 0),
        (1, 1),
        (1, 1),
        (1, 1),
        (4, 0),
        (4, 0),
        (4, 0),
    ]
    .map(|(weight, score)| KnapsackItem { weight, score });
    assert_eq!(pack_knapsack_indices(&unequal_cardinality, 3), [0, 2]);
    let many = vec![
        KnapsackItem {
            weight: 1,
            score: 1
        };
        32_768
    ];
    assert_eq!(pack_knapsack_indices(&many, 1), [0]);
    assert_eq!(pack_knapsack_indices(&many, 2), [0, 1]);
    assert_eq!(
        pack_knapsack_indices(&many, 16_384),
        (0..16_384).collect::<Vec<_>>()
    );
    assert_eq!(
        pack_knapsack_indices(&many, many.len()),
        (0..many.len()).collect::<Vec<_>>()
    );
    for encoding in 0..4_usize.pow(4) {
        let items: Vec<_> = (0..4)
            .map(|i| KnapsackItem {
                weight: (encoding >> (i * 2)) & 3,
                score: ((encoding + i * 7) % 5) as u64,
            })
            .collect();
        for capacity in 0..8 {
            let chosen = pack_knapsack_indices(&items, capacity);
            let weight: usize = chosen.iter().map(|&i| items[i].weight).sum();
            let score: u64 = chosen.iter().map(|&i| items[i].score).sum();
            let oracle = (0..1 << items.len())
                .filter_map(|mask| {
                    let indices: Vec<usize> =
                        (0..items.len()).filter(|&i| mask & (1 << i) != 0).collect();
                    let (w, s) = indices.iter().fold((0, 0), |(w, s), &i| {
                        (w + items[i].weight, s + items[i].score)
                    });
                    (w <= capacity && indices.iter().all(|&i| items[i].score > 0))
                        .then_some((s, indices))
                })
                .max_by(|(s1, idx1), (s2, idx2)| s1.cmp(s2).then_with(|| idx2.cmp(idx1)))
                .unwrap();
            assert!(weight <= capacity);
            assert_eq!(score, oracle.0, "{items:?}, capacity {capacity}");
            assert_eq!(chosen, oracle.1, "{items:?}, capacity {capacity}");
            assert!(chosen.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}

#[test]
fn claim_context_knapsack_packing_seam_matrix() {
    // 1. Real bridge providers (CodeDomainProvider + DocsDomainProvider) under tight ClaimResp budget:
    //    target-matching compact symbols are packed even when an oversized symbol precedes or competes,
    //    and serialized total_bytes strictly honors response_max_bytes.
    let ws = crate::common::Workspace::new();
    let root = ws.root();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "# Workspace Router\n## Rules\n").unwrap();
    std::fs::write(root.join("src/AGENTS.md"), "# Source Rules\n## Local\n").unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn oversized_unrelated_symbol(
    first_parameter_with_long_name: &str,
    second_parameter_with_long_name: &str,
    third_parameter_with_long_name: usize,
) -> usize {
    first_parameter_with_long_name.len() + second_parameter_with_long_name.len() + third_parameter_with_long_name
}

pub fn pack_target_alpha(x: usize) -> usize { x + 1 }
pub fn pack_target_beta(y: usize) -> usize { y + 2 }
"#,
    )
    .unwrap();

    let adapter = Adapter::default();
    let real_provider = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let baseline_req = sample_claim_request(
        root.to_path_buf(),
        Vec::new(),
        vec!["src/lib.rs".to_owned()],
        "Implement pack_target_alpha and pack_target_beta",
        32_768,
    );
    let bare = GateContextAssembler::assemble(&baseline_req, &real_provider).unwrap();
    let full_req = sample_claim_request(
        root.to_path_buf(),
        vec!["paths.instructions".to_owned(), "code.symbols".to_owned()],
        vec!["src/lib.rs".to_owned()],
        "Implement pack_target_alpha and pack_target_beta",
        32_768,
    );
    let full = GateContextAssembler::assemble(&full_req, &real_provider).unwrap();
    let full_syms = full.context_lenses["code.symbols"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(full_syms.len(), 3);

    // Budget tightly: enough for Tier 1 + paths.instructions + two target symbols, too tight for all 3.
    let tight_budget = full.total_bytes - 100;
    assert!(tight_budget > bare.total_bytes);
    let tight_req = sample_claim_request(
        root.to_path_buf(),
        vec!["paths.instructions".to_owned(), "code.symbols".to_owned()],
        vec!["src/lib.rs".to_owned()],
        "Implement pack_target_alpha and pack_target_beta",
        tight_budget,
    );
    let tight = GateContextAssembler::assemble(&tight_req, &real_provider).unwrap();
    assert_eq!(tight.total_bytes, serde_json::to_vec(&tight).unwrap().len());
    assert!(tight.total_bytes <= tight_budget);
    assert!(tight.context_lenses.contains_key("paths.instructions"));
    let sym_names: Vec<&str> = tight.context_lenses["code.symbols"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["qualname"].as_str())
        .collect();
    assert!(
        sym_names.iter().any(|n| n.ends_with("pack_target_alpha")),
        "{sym_names:?}"
    );
    assert!(
        sym_names.iter().any(|n| n.ends_with("pack_target_beta")),
        "{sym_names:?}"
    );
    assert!(
        !sym_names
            .iter()
            .any(|n| n.ends_with("oversized_unrelated_symbol")),
        "{sym_names:?}"
    );
    assert_eq!(
        tight.context_lenses["code.symbols"]["truncated"],
        json!(true)
    );

    // 2. Tier 6 fallback packing, exhausted-lens omission, non-items blob omission, and fail-closed error:
    let custom = Arc::new(StaticLensProvider {
        domain: "custom".to_owned(),
        lenses: BTreeMap::from([
            (
                "oversized_first".to_owned(),
                json!({
                    "kind": "custom",
                    "items": ["x".repeat(600), "fit-a", "fit-b"],
                    "total": 3,
                    "truncated": false
                }),
            ),
            (
                "exhausted".to_owned(),
                json!({"kind": "exhausted", "items": [], "total": 4, "truncated": true}),
            ),
            ("huge_blob".to_owned(), json!({"blob": "z".repeat(900)})),
        ]),
    });
    let custom_provider = CompositeDomainProvider::with_module_flags(
        vec![custom],
        BTreeMap::from([("custom".to_owned(), true)]),
    )
    .unwrap();
    let tier6_budget = bare.total_bytes + 320;
    let tier6_req = sample_claim_request(
        root.to_path_buf(),
        vec![
            "custom.oversized_first".to_owned(),
            "custom.exhausted".to_owned(),
            "custom.huge_blob".to_owned(),
        ],
        vec!["src/lib.rs".to_owned()],
        "Pack fallback",
        tier6_budget,
    );
    let tier6 = GateContextAssembler::assemble(&tier6_req, &custom_provider).unwrap();
    assert_eq!(tier6.total_bytes, serde_json::to_vec(&tier6).unwrap().len());
    assert!(tier6.total_bytes <= tier6_budget);
    assert_eq!(
        tier6.context_lenses["custom.oversized_first"]["items"],
        json!(["fit-a", "fit-b"])
    );
    assert_eq!(
        tier6.context_lenses["custom.oversized_first"]["truncated"],
        json!(true)
    );
    assert!(!tier6.context_lenses.contains_key("custom.exhausted"));
    assert!(!tier6.context_lenses.contains_key("custom.huge_blob"));
    assert_eq!(
        tier6.omitted_sections,
        ["context:custom.exhausted", "context:custom.huge_blob"]
    );

    // Missing lens still fails closed with DomainCapabilityUnavailable.
    let missing_req = sample_claim_request(
        root.to_path_buf(),
        vec!["custom.nonexistent".to_owned()],
        vec!["src/lib.rs".to_owned()],
        "Missing lens",
        tier6_budget,
    );
    let err = GateContextAssembler::assemble(&missing_req, &custom_provider).unwrap_err();
    assert_eq!(err.code, ErrorCode::DomainCapabilityUnavailable);
}

#[test]
fn sql_table_external_callers_and_signature_scope_seam_matrix() {
    let ws = crate::common::Workspace::new();
    let root = ws.root();
    let run_git = |args: &[&str]| {
        let out = acdd_core::git::git_command(root, args).unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init"]);
    run_git(&["config", "commit.gpgsign", "false"]);
    run_git(&["config", "user.email", "test@example.com"]);
    run_git(&["config", "user.name", "Test"]);

    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("migrations/001_orders.sql"),
        "CREATE TABLE orders (id INT PRIMARY KEY, total INT);\nCREATE TABLE audit_log (id INT PRIMARY KEY);\nCREATE TABLE \"Camel.Dot\" (id INT PRIMARY KEY);\n",
    )
    .unwrap();
    std::fs::write(
        root.join("migrations/002_items.sql"),
        "CREATE TABLE order_items (id INT PRIMARY KEY, order_id INT REFERENCES orders(id));\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/reader.rs"),
        "pub fn read_orders() -> &'static str { \"SELECT id FROM orders\" }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_audit() { let _ = \"SELECT id FROM audit_log\"; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/writer.rs"),
        "pub fn write_order() -> &'static str { \"INSERT INTO orders (id, total) VALUES (1, 10)\" }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/bootstrap.py"),
        "def init_schema(db):\n    db.execute(\"CREATE TABLE orders (id INT)\")\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/unrelated.rs"),
        "pub fn orders() -> i32 { 1 }\npub fn call_orders() -> i32 { orders() }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/orders_test.rs"),
        "#[test]\nfn test_orders_sql() { let _ = \"SELECT * FROM orders\"; }\n",
    )
    .unwrap();
    for (path, name) in [
        ("src/case_match.sql", r#""Camel.Dot""#),
        ("src/case_mismatch.sql", r#""camel.dot""#),
        ("src/qualified_mismatch.sql", "camel.dot"),
    ] {
        std::fs::write(root.join(path), format!("SELECT id FROM {name};\n")).unwrap();
    }
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "init"]);
    let head_oid = String::from_utf8(
        acdd_core::git::git_command(root, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let head_commit = CommitOid::new(head_oid).unwrap();

    let adapter = Adapter::default();
    let provider = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let expected_callers = json!([
        "migrations/002_items.sql",
        "src/bootstrap.py",
        "src/reader.rs",
        "src/writer.rs"
    ]);
    let expected_indexed_callers = json!([
        "migrations/002_items.sql",
        "src/bootstrap.py",
        "src/index_only.sql",
        "src/reader.rs",
        "src/writer.rs"
    ]);
    let assert_quoted_callers = |items: &[Value]| {
        let quoted = items
            .iter()
            .find(|item| {
                item["symbol"]
                    .as_str()
                    .is_some_and(|name| name.contains("Camel.Dot"))
            })
            .unwrap_or_else(|| panic!("missing quoted table in {items:?}"));
        assert_eq!(quoted["external_callers"], json!(["src/case_match.sql"]));
    };
    let assemble_sql_callers = || {
        let mut req = sample_claim_request(
            root.to_path_buf(),
            vec!["code.external_callers".to_owned()],
            vec!["migrations/001_orders.sql".to_owned()],
            "Update orders SQL table schema",
            32_768,
        );
        req.run.target_base_commit = Some(head_commit.clone());
        req.node.baseline_commit = Some(head_commit.clone());
        let resp = GateContextAssembler::assemble(&req, &provider).unwrap();
        resp.context_lenses
            .get("code.external_callers")
            .and_then(|v| v["items"].as_array())
            .cloned()
            .unwrap_or_default()
    };
    let check_signature_scope = || {
        provider
            .run_check(
                "code.signature_scope",
                &json!({}),
                &DomainCheckReq {
                    run_id: RunId::new("run-sql").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["migrations/001_orders.sql".to_owned()],
                    changed_paths: vec!["migrations/001_orders.sql".to_owned()],
                    test_target: None,
                    path_policy: WorkspacePathPolicy::default(),
                    baseline_ref: None,
                    node_tags: vec![],
                },
            )
            .unwrap()
    };

    // 1. Unindexed baseline fallback (no code-map.sqlite).
    let unindexed = assemble_sql_callers();
    assert_eq!(unindexed.len(), 2, "{unindexed:?}");
    let orders = unindexed
        .iter()
        .find(|item| item["symbol"] == "orders")
        .unwrap();
    assert_eq!(orders["signature_changed"], false);
    assert_eq!(orders["external_callers"], expected_callers);
    assert_quoted_callers(&unindexed);

    // 2. Live indexed code-map.sqlite across all 4 SQL table edge kinds, plus unchanged signature_scope.
    std::fs::write(root.join("src/index_only.sql"), "SELECT id FROM orders;\n").unwrap();
    let db_path = root.join(".acdd/code-map.sqlite");
    contextunity_acdd::engine::pipeline::build(root, &db_path, None).unwrap();
    let indexed = assemble_sql_callers();
    assert_eq!(indexed.len(), 2, "{indexed:?}");
    let orders = indexed
        .iter()
        .find(|item| item["symbol"] == "orders")
        .unwrap();
    assert_eq!(orders["signature_changed"], false);
    assert_eq!(orders["external_callers"], expected_indexed_callers);
    assert_quoted_callers(&indexed);

    // Schema column edit must NOT fail code.signature_scope.
    std::fs::write(
        root.join("migrations/001_orders.sql"),
        "CREATE TABLE orders (id INT PRIMARY KEY, total INT, currency TEXT);\nCREATE TABLE audit_log (id INT PRIMARY KEY);\nCREATE TABLE \"Camel.Dot\" (id INT PRIMARY KEY);\n",
    )
    .unwrap();
    assert!(matches!(
        check_signature_scope(),
        DomainCheckOutcome::Passed { .. }
    ));

    // 3. Deleted .sql table: stale index still surfaces consumers, and refreshed index falls back to baseline.
    std::fs::remove_file(root.join("migrations/001_orders.sql")).unwrap();
    for label in ["deleted-stale-index", "deleted-refreshed-index"] {
        if label == "deleted-refreshed-index" {
            contextunity_acdd::engine::pipeline::build(root, &db_path, None).unwrap();
        }
        let items = assemble_sql_callers();
        let orders = items
            .iter()
            .find(|item| item["symbol"] == json!("orders"))
            .unwrap_or_else(|| panic!("{label}: missing orders in {items:?}"));
        assert_eq!(orders["signature_changed"], json!(true), "{label}");
        let expected = if label == "deleted-stale-index" {
            &expected_indexed_callers
        } else {
            &expected_callers
        };
        assert_eq!(&orders["external_callers"], expected, "{label}");
        assert_quoted_callers(&items);
        let audit = items
            .iter()
            .find(|item| item["symbol"] == json!("audit_log"))
            .unwrap_or_else(|| panic!("{label}: missing audit_log in {items:?}"));
        assert_eq!(audit["external_callers"], json!([]), "{label}");
        assert!(
            matches!(check_signature_scope(), DomainCheckOutcome::Passed { .. }),
            "{label}: signature_scope must stay callable-only"
        );
    }
}

#[test]
fn compact_claim_budget_detail_and_lens_compaction_matrix() {
    use acdd_core::response::Detail;
    use acdd_state::context::budget::{MAX_BLACKBOARD_CHARS, MAX_BLACKBOARD_ITEMS};
    use acdd_state::context::render_claim_prompt_markdown;
    use acdd_state::types::{
        BlackboardRecord, BlackboardStatus, BlackboardTopic, FindingItem, FindingSeverity,
        RejectionFindings, RejectionKind,
    };
    use acdd_state::usecases::{ClaimReq, StartReq, StateCommand, StateCommandOutput, StateEngine};
    const COMPACT_CLAIM_MAX_BYTES: usize = 10_000;

    let ws = crate::common::Workspace::new();
    let root = ws.root();
    let run_git = |args: &[&str]| {
        let out = acdd_core::git::git_command(root, args).unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init"]);
    run_git(&["config", "commit.gpgsign", "false"]);
    run_git(&["config", "user.email", "test@example.com"]);
    run_git(&["config", "user.name", "Test"]);

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::create_dir_all(root.join("docs/milestones")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"compact_demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        "# Workspace Rules\n## Authority\n## Routes\n## Workflows\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/AGENTS.md"),
        "# Source Rules\n## Modules\n## Tests\n",
    )
    .unwrap();

    // Rust derive struct + 7 public functions called from 7 external callers.
    let mut api_src = String::from(
        "#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]\npub struct DerivedModel {\n    pub id: u64,\n}\n\npub fn hot_api() -> u64 { 1 }\n",
    );
    for i in 0..6 {
        api_src.push_str(&format!("pub fn ext_api_{i}() -> u64 {{ {i} }}\n"));
    }
    std::fs::write(root.join("src/api.rs"), &api_src).unwrap();
    let mut lib_src = String::from("pub mod api;\n");
    for i in 0..7 {
        lib_src.push_str(&format!("pub mod caller_{i};\n"));
    }
    std::fs::write(root.join("src/lib.rs"), &lib_src).unwrap();

    for i in 0..7 {
        let mut caller_src = String::from("use crate::api::hot_api;\n");
        if i < 6 {
            caller_src.push_str(&format!("use crate::api::ext_api_{i};\n"));
        }
        caller_src.push_str(&format!("pub fn caller_{i}() -> u64 {{ hot_api()"));
        if i < 6 {
            caller_src.push_str(&format!(" + ext_api_{i}()"));
        }
        caller_src.push_str(" }\n");
        std::fs::write(root.join(format!("src/caller_{i}.rs")), caller_src).unwrap();
    }

    // 10 test files, with multiple tests in test_00.rs to verify path deduplication + top-8 cap.
    std::fs::write(
        root.join("tests/test_00.rs"),
        "use compact_demo::api::hot_api;\n#[test]\nfn t_a() { assert_eq!(hot_api(), 1); }\n#[test]\nfn t_b() { assert_eq!(hot_api(), 1); }\n#[test]\nfn t_c() { assert_eq!(hot_api(), 1); }\n",
    )
    .unwrap();
    for i in 1..10 {
        std::fs::write(
            root.join(format!("tests/test_{i:02}.rs")),
            format!(
                "use compact_demo::api::hot_api;\n#[test]\nfn t_{i}() {{ assert_eq!(hot_api(), 1); }}\n"
            ),
        )
        .unwrap();
    }

    std::fs::write(
        root.join("docs/milestones/AC-900.md"),
        "---\nid: AC-900\ntitle: Compact Claim\ndoc_type: contract\nstatus: planned\n---\n\n### task: t1\n\n```yaml\ntask_ref: t1\ntarget: \"Verify compact claim\"\nscope:\n  - src/api.rs\nstatus: planned\n```\n",
    )
    .unwrap();
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "init"]);

    contextunity_acdd::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None)
        .unwrap();
    let adapter = Adapter::default();
    let provider = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();

    // 1. Direct context lenses: no derive noise, no headings TOC, deduped top-8 tests, top-5 external callers + overflow.
    let mut direct_req = sample_claim_request(
        root.to_path_buf(),
        vec![
            "paths.instructions".to_owned(),
            "code.symbols".to_owned(),
            "code.external_callers".to_owned(),
        ],
        vec!["src/api.rs".to_owned()],
        "Verify compact claim",
        32_768,
    );
    direct_req.unresolved_findings = Some(RejectionFindings {
        kind: RejectionKind::Defect,
        summary: "Preserve unresolved findings".to_owned(),
        blackboard_message_id: Some(11),
        items: vec![FindingItem {
            severity: FindingSeverity::High,
            path: Some("src/lib.rs".to_owned()),
            message: "Must remain intact".to_owned(),
            remediation: "Keep Tier 2 intact".to_owned(),
        }],
    });
    direct_req.blackboard_items = (1..=10)
        .map(|i| BlackboardRecord {
            id: Some(i),
            run_id: RunId::new("run-1").unwrap(),
            node_id: Some("task:t1".to_owned()),
            recipient_node_id: None,
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "builder".to_owned(),
            topic: BlackboardTopic::Notes,
            status: BlackboardStatus::Open,
            payload: format!("bb-{i}: {}", "x".repeat(300)),
            created_at: "2026-11-26T10:00:00.000Z".to_owned(),
        })
        .collect();

    let assembled = GateContextAssembler::assemble(&direct_req, &provider).unwrap();
    let md = render_claim_prompt_markdown(&assembled, None);

    // paths.instructions omits headings TOC.
    for item in assembled.context_lenses["paths.instructions"]["items"]
        .as_array()
        .unwrap()
    {
        assert!(
            item.get("headings").is_none(),
            "headings must be stripped: {item}"
        );
    }
    assert!(!md.contains("headings:"), "{md}");

    // code.symbols filters generated derive methods (.ne, .le, .ge, .eq, .clone, .fmt).
    let symbol_qns: Vec<&str> = assembled.context_lenses["code.symbols"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["qualname"].as_str())
        .collect();
    assert!(
        symbol_qns.iter().any(|q| q.ends_with("hot_api")),
        "{symbol_qns:?}"
    );
    for noise in [
        ".ne",
        ".le",
        ".ge",
        ".eq",
        ".clone",
        ".fmt",
        ".cmp",
        ".partial_cmp",
    ] {
        assert!(
            !symbol_qns.iter().any(|q| q.ends_with(noise)),
            "generated derive noise '{noise}' leaked into code.symbols: {symbol_qns:?}"
        );
    }

    // code.tests deduplicates paths and caps at top 8.
    let tests_req = sample_claim_request(
        root.to_path_buf(),
        vec!["code.tests".to_owned()],
        vec!["src/api.rs".to_owned(), "tests/**".to_owned()],
        "Verify compact claim",
        32_768,
    );
    let tests_assembled = GateContextAssembler::assemble(&tests_req, &provider).unwrap();
    let test_items = tests_assembled.context_lenses["code.tests"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(test_items.len(), 8, "{}", test_items.len());
    let test_paths: std::collections::BTreeSet<&str> = test_items
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(test_paths.len(), test_items.len(), "{test_items:?}");

    // code.external_callers caps at top-5 items + top-5 callers per item with overflow count.
    let ext_lens = &assembled.context_lenses["code.external_callers"];
    let ext_items = ext_lens["items"].as_array().unwrap();
    assert_eq!(ext_items.len(), 5, "{ext_lens}");
    assert_eq!(ext_lens["total"], 7, "{ext_lens}");
    let hot_entry = ext_items
        .iter()
        .find(|v| v["symbol"].as_str().is_some_and(|s| s.ends_with("hot_api")))
        .expect("hot_api ranked first by caller count");
    assert_eq!(hot_entry["external_callers"].as_array().unwrap().len(), 5);
    assert_eq!(hot_entry["external_callers_total"], 7);
    assert!(md.contains("(+2 more)"), "{md}");
    assert!(md.contains("... (+2 more)"), "{md}");

    // Blackboard bounded at 6 x 220 chars while keeping Tier 2 findings intact,
    // and omission count recorded when shown < relevant_total.
    assert_eq!(assembled.blackboard.len(), MAX_BLACKBOARD_ITEMS);
    for item in &assembled.blackboard {
        assert!(
            item["payload"].as_str().unwrap().chars().count() <= MAX_BLACKBOARD_CHARS,
            "{item}"
        );
    }
    assert!(assembled
        .omitted_sections
        .contains(&"blackboard (4 of 10 omitted)".to_owned()));
    assert!(assembled.unresolved_findings.is_some());

    // 2. StateEngine ClaimReq::detail: default Compact <= 10,000 bytes vs opt-in Full <= 32,768 bytes.
    let repo_profile_src =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".acdd/workflows/default.yaml");
    let profile_text = std::fs::read_to_string(&repo_profile_src).unwrap();
    assert!(
        !profile_text.contains("code.tools:"),
        "duplicate code.tools override must be removed from .acdd/workflows/default.yaml"
    );
    std::fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    std::fs::write(root.join(".acdd/workflows/default.yaml"), &profile_text).unwrap();
    let resolved_repo_profile = acdd_state::workflow::resolve_workflow_ref_with_dirs(
        ".acdd/workflows/default.yaml",
        Some(root),
        None,
        Some(&[".acdd/workflows".to_owned()]),
    )
    .expect("repo .acdd/workflows/default.yaml must resolve cleanly after code.tools dedup");
    assert!(resolved_repo_profile.guidance.contains_key("code.tools"));
    assert!(resolved_repo_profile
        .guidance
        .contains_key("code.principles"));
    assert!(resolved_repo_profile.guidance.contains_key("repo.tests"));

    std::fs::write(
        root.join(".acdd/workflows/compact_test.yaml"),
        "version: 2\ninherit: code\ngit:\n  isolation: in_place\n  worktrees: manual\ncommands:\n  build_check:\n    command: \"true\"\n  verify_all:\n    command: \"true\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          command: build_check\n          proof: command\n          guidance: [task.code, core.execution, code.principles, code.tools, hitl.pro]\n          context: [paths.instructions, paths.adrs, docs.invariants, code.symbols, code.tests, code.external_callers]\n          pass: review\n        - id: review\n          role: independent_reviewer\n          proof: none\n          independent_from: [build]\n          need: [build]\n          pass: deliver\n        - id: deliver\n          role: independent_reviewer\n          independent_from: [build]\n          need: [build, review]\n          receipt: true\n",
    )
    .unwrap();
    let mut cfg = crate::acdd::support::base_engine_config(root);
    cfg.code_db_path = Some(root.join(".acdd/code-map.sqlite"));
    let engine = StateEngine::open(cfg, provider.clone()).unwrap();
    let started = match engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-900.md".to_owned()),
            workflow: Some(".acdd/workflows/compact_test.yaml".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    {
        StateCommandOutput::Start(resp) => *resp,
        other => panic!("unexpected start: {other:?}"),
    };
    let rid = started.run.run_id.to_string();
    for i in 1..=8 {
        engine
            .execute(StateCommand::BlackboardPost(
                acdd_state::usecases::BlackboardPostReq {
                    run: Some(rid.clone()),
                    node: Some("t1.build".to_owned()),
                    recipient_node: None,
                    subtask_ref: None,
                    gate: Some("build".to_owned()),
                    author: "coord".to_owned(),
                    topic: BlackboardTopic::Decisions,
                    payload: format!("decision-{i}: {}", "d".repeat(260)),
                },
            ))
            .unwrap();
    }
    let compact_claim = match engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: WorkerId::new("w-compact").unwrap(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(resp) => *resp,
        other => panic!("unexpected compact claim: {other:?}"),
    };
    assert!(
        compact_claim.total_bytes <= COMPACT_CLAIM_MAX_BYTES,
        "compact claim exceeded {COMPACT_CLAIM_MAX_BYTES}: {}",
        compact_claim.total_bytes
    );
    let compact_md = render_claim_prompt_markdown(&compact_claim, None);
    assert!(
        compact_md.len() <= COMPACT_CLAIM_MAX_BYTES,
        "compact claim markdown exceeded {COMPACT_CLAIM_MAX_BYTES}: {}",
        compact_md.len()
    );
    assert!(
        compact_claim
            .omitted_sections
            .contains(&"blackboard (2 of 8 omitted)".to_owned()),
        "{:?}",
        compact_claim.omitted_sections
    );
    assert!(
        compact_md.contains("acdd blackboard --action read --limit 20"),
        "{compact_md}"
    );

    // Oversized compact Tier 1 (> 10,000, within 32,768 hard cap) degrades gracefully instead of GuidanceBudgetExceeded.
    let mut soft_req = direct_req.clone();
    soft_req.full_detail = false;
    soft_req.response_max_bytes = COMPACT_CLAIM_MAX_BYTES;
    soft_req.hard_max_bytes = Some(32_768);
    soft_req
        .gate
        .guidance
        .push(GateGuidanceRef::Inline(acdd_state::workflow::schema::InlineGuidance {
            inline: "x".repeat(10_400),
        }));
    let degraded = GateContextAssembler::assemble(&soft_req, &provider).unwrap();
    assert!(degraded.total_bytes > COMPACT_CLAIM_MAX_BYTES && degraded.total_bytes <= 32_768);
    assert!(degraded
        .omitted_sections
        .contains(&"unresolved_findings".to_owned()));
    let degraded_md = render_claim_prompt_markdown(&degraded, None);
    assert!(
        degraded_md.contains("acdd show t1.build")
            && degraded_md.contains("acdd claim t1.build --detail full"),
        "{degraded_md}"
    );

    let rid_cli = rid.clone();
    let full_claim = match engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid),
            node: Some("t1.build".to_owned()),
            worker_id: WorkerId::new("w-compact").unwrap(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Full,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(resp) => *resp,
        other => panic!("unexpected full claim: {other:?}"),
    };
    assert!(full_claim.total_bytes <= 32_768);
    assert!(full_claim.total_bytes >= compact_claim.total_bytes);
    drop(engine);

    // 3. CLI `acdd claim` parity: default (`compact`) <= 10,000 bytes, explicit `--detail compact|full`,
    // and invalid `--detail` rejected.
    let cli_default = crate::common::cli::run_cli_json(
        &ws,
        &[
            "claim",
            "t1.build",
            "--run",
            &rid_cli,
            "--worker",
            "w-cli",
            "--force",
            "--steal-remote",
        ],
    );
    assert!(
        cli_default.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_default.stderr)
    );
    let cli_default_val: Value = serde_json::from_slice(&cli_default.stdout).unwrap();
    let cli_default_bytes = cli_default_val["Claim"]["total_bytes"]
        .as_u64()
        .expect("CLI claim payload must contain Claim.total_bytes")
        as usize;
    assert!(cli_default_bytes <= COMPACT_CLAIM_MAX_BYTES);

    let cli_compact = crate::common::cli::run_cli_json(
        &ws,
        &[
            "claim", "t1.build", "--run", &rid_cli, "--worker", "w-cli", "--force", "--detail",
            "compact",
        ],
    );
    assert!(cli_compact.status.success());
    let cli_compact_val: Value = serde_json::from_slice(&cli_compact.stdout).unwrap();
    assert_eq!(
        cli_compact_val["Claim"]["total_bytes"],
        cli_default_val["Claim"]["total_bytes"]
    );

    let cli_full = crate::common::cli::run_cli_json(
        &ws,
        &[
            "claim", "t1.build", "--run", &rid_cli, "--worker", "w-cli", "--force", "--detail",
            "full",
        ],
    );
    assert!(
        cli_full.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_full.stderr)
    );
    let cli_full_val: Value = serde_json::from_slice(&cli_full.stdout).unwrap();
    let cli_full_bytes = cli_full_val["Claim"]["total_bytes"]
        .as_u64()
        .expect("CLI full claim payload must contain Claim.total_bytes")
        as usize;
    assert!(cli_full_bytes <= 32_768);
    assert!(cli_full_bytes >= cli_default_bytes);

    let cli_bad_detail = crate::common::cli::run_cli_json(
        &ws,
        &[
            "claim", "t1.build", "--run", &rid_cli, "--worker", "w-cli", "--force", "--detail",
            "bogus",
        ],
    );
    assert!(!cli_bad_detail.status.success());
}
