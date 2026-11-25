use acdd_core::config::Adapter;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{CommitOid, RunId, RunName, TaskRef, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{
    CompositeDomainProvider, DomainActionReq, DomainActionResult, DomainCapabilities,
    DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
};
use acdd_core::{pack_knapsack_indices, KnapsackItem};
use acdd_state::context::{ClaimContextRequest, GateContextAssembler};
use acdd_state::profile::{
    schema::{GateGuidanceRef, GuidanceEntry},
    GateDef, ResolvedGateProfile, RoleDef,
};
use acdd_state::spec::{SpecDocument, SpecStatus, TaskSpec};
use acdd_state::types::{
    GateNodeRecord, InvariantsState, IsolationMode, NodeKind, NodeStatus, RunRecord, RunStatus,
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
            context_lenses: self.lenses.keys().cloned().collect(),
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
    let mut profile = ResolvedGateProfile::default();
    profile
        .guidance
        .insert("g1".to_owned(), GuidanceEntry::Text("Follow rules".to_owned()));
    profile
        .roles
        .insert("builder".to_owned(), RoleDef::default());
    let mut gate = GateDef::default();
    gate.id = "build".to_owned();
    gate.role = Some("builder".to_owned());
    gate.guidance = vec![GateGuidanceRef::Slot("g1".to_owned())];
    gate.context = context;
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
        profile: None,
        status: SpecStatus::Active,
        cancellation_reason: None,
        spec: Some("Root".to_owned()),
        depends_on: Vec::new(),
        owners: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        guidance: None,
        required_skills: Vec::new(),
        repositories: Vec::new(),
        linked_to: Vec::new(),
        closure: None,
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
    let many = vec![KnapsackItem { weight: 1, score: 1 }; 32_768];
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
                    let indices: Vec<usize> = (0..items.len())
                        .filter(|&i| mask & (1 << i) != 0)
                        .collect();
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
        vec!["docs.instructions".to_owned(), "code.symbols".to_owned()],
        vec!["src/lib.rs".to_owned()],
        "Implement pack_target_alpha and pack_target_beta",
        32_768,
    );
    let full = GateContextAssembler::assemble(&full_req, &real_provider).unwrap();
    let full_syms = full.context_lenses["code.symbols"]["items"].as_array().unwrap();
    assert_eq!(full_syms.len(), 3);

    // Budget tightly: enough for Tier 1 + docs.instructions + two target symbols, too tight for all 3.
    let tight_budget = full.total_bytes - 100;
    assert!(tight_budget > bare.total_bytes);
    let tight_req = sample_claim_request(
        root.to_path_buf(),
        vec!["docs.instructions".to_owned(), "code.symbols".to_owned()],
        vec!["src/lib.rs".to_owned()],
        "Implement pack_target_alpha and pack_target_beta",
        tight_budget,
    );
    let tight = GateContextAssembler::assemble(&tight_req, &real_provider).unwrap();
    assert_eq!(tight.total_bytes, serde_json::to_vec(&tight).unwrap().len());
    assert!(tight.total_bytes <= tight_budget);
    assert!(tight.context_lenses.contains_key("docs.instructions"));
    let sym_names: Vec<&str> = tight.context_lenses["code.symbols"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["qualname"].as_str())
        .collect();
    assert!(sym_names.iter().any(|n| n.ends_with("pack_target_alpha")), "{sym_names:?}");
    assert!(sym_names.iter().any(|n| n.ends_with("pack_target_beta")), "{sym_names:?}");
    assert!(!sym_names.iter().any(|n| n.ends_with("oversized_unrelated_symbol")), "{sym_names:?}");
    assert_eq!(tight.context_lenses["code.symbols"]["truncated"], json!(true));

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
    let custom_provider =
        CompositeDomainProvider::with_module_flags(vec![custom], BTreeMap::from([("custom".to_owned(), true)])).unwrap();
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
    assert_eq!(tier6.context_lenses["custom.oversized_first"]["truncated"], json!(true));
    assert!(!tier6.context_lenses.contains_key("custom.exhausted"));
    assert!(!tier6.context_lenses.contains_key("custom.huge_blob"));
    assert!(tier6.omitted_sections.contains(&"context:custom.exhausted".to_owned()));
    assert!(tier6.omitted_sections.contains(&"context:custom.huge_blob".to_owned()));

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
