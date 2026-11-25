use super::*;
use crate::profile::{
    resolve_profile_ref_with_dirs,
    schema::{GateGuidanceRef, GuidanceEntry, InlineGuidance},
    GateDef, ResolvedGateProfile, RoleDef,
};
use crate::spec::{Receipt, SpecDocument, SpecStatus, TaskSpec};
use crate::types::{
    BlackboardRecord, BlackboardStatus, BlackboardTopic, FindingItem, FindingSeverity,
    GateNodeRecord, InvariantsState, IsolationMode, NodeKind, NodeStatus, RejectionFindings,
    RejectionKind, RunRecord, RunStatus,
};
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, RunName, TaskRef, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{
    CompositeDomainProvider, DomainActionReq, DomainActionResult, DomainCapabilities,
    DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

struct LensMockProvider {
    domain: String,
    lenses: BTreeMap<String, Value>,
}

impl DomainProvider for LensMockProvider {
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

fn base_request() -> ClaimContextRequest {
    let run_id = RunId::new("run-1").unwrap();
    let worker_id = WorkerId::new("w1").unwrap();
    let tree = TreeOid::new("1".repeat(40)).unwrap();
    let commit = CommitOid::new("2".repeat(40)).unwrap();
    let mut profile = ResolvedGateProfile::default();
    profile.guidance.insert(
        "g1".to_owned(),
        GuidanceEntry::Text("Follow rules".to_owned()),
    );
    profile
        .roles
        .insert("builder".to_owned(), RoleDef::default());

    let gate = GateDef {
        id: "build".to_owned(),
        role: Some("builder".to_owned()),
        guidance: vec![GateGuidanceRef::Slot("g1".to_owned())],
        context: vec!["code.symbols".to_owned(), "code.tests".to_owned()],
        ..Default::default()
    };

    let run = RunRecord {
        run_id: run_id.clone(),
        run_name: RunName::new("r1").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "/tmp/ws".to_owned(),
        profile_id: "code".to_owned(),
        profile_json: "{}".to_owned(),
        spec_path: Some("docs/milestones/AC-001.md".to_owned()),
        spec_id: None,
        spec_digest: "a".repeat(64),
        is_ephemeral: false,
        isolation: IsolationMode::Worktree,
        target_base: Some("main".to_owned()),
        target_base_commit: Some(commit),
        branch: Some("milestone/r1".to_owned()),
        worktree: Some("/tmp/ws".to_owned()),
        trace_id: "3".repeat(32),
        status: RunStatus::Active,
        cancellation_reason: None,
        root_cycle_count: 0,
        generation: 1,
        created_at: "2026-11-17T10:00:00.000Z".to_owned(),
        completed_at: None,
    };

    let node = GateNodeRecord {
        run_id: run_id.clone(),
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
        worktree: Some("/tmp/ws".to_owned()),
        baseline_commit: None,
        claim_tree: Some(tree.clone()),
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["default".to_owned()],
        invariants: InvariantsState::default(),
        scope: vec!["src/**".to_owned()],
        extended_scope: Vec::new(),
        active_span_id: None,
    };

    let spec = SpecDocument {
        id: "AC-001".to_owned(),
        title: "Test Spec".to_owned(),
        doc_type: "contract".to_owned(),
        profile: None,
        status: SpecStatus::Active,
        cancellation_reason: None,
        spec: Some("Root spec".to_owned()),
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
            target: Some("Implement feature".to_owned()),
            spec: Some("Details".to_owned()),
            scope: vec!["src/**".to_owned()],
            extended_scope: Vec::new(),
            depends_on: Vec::new(),
            tags: vec!["default".to_owned()],
            invariants: Vec::new(),
            acceptance_criteria: vec!["Works".to_owned()],
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
        worktree: PathBuf::from("/tmp/ws"),
        candidate_tree: tree,
        path_policy: WorkspacePathPolicy::default(),
        unresolved_findings: None,
        failed_trace_log_tail: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard_items: Vec::new(),
        context_reset_required: false,
        response_max_bytes: CLAIM_DEFAULT_MAX_BYTES,
    }
}

#[test]
fn context_six_tier_budget_and_guidance_fail_closed_matrix() {
    let empty_provider = CompositeDomainProvider::default();

    // Case 1: Guidance overflow fails closed with GuidanceBudgetExceeded (never silently truncated).
    let mut overflow_req = base_request();
    overflow_req.profile.limits.max_guidance_bytes = 128;
    overflow_req
        .gate
        .guidance
        .push(GateGuidanceRef::Inline(InlineGuidance {
            inline: "x".repeat(512),
        }));
    let err = build_claim_context(&overflow_req, &empty_provider).expect_err("must fail closed");
    assert_eq!(err.code, ErrorCode::GuidanceBudgetExceeded);

    // Case 2: Tier priority ordering drops lower-priority context lenses first, keeps higher tiers,
    // caps blackboard at 15 x 500 chars, and preserves YAML lens declaration order.
    let mut tiered_req = base_request();
    tiered_req.unresolved_findings = Some(RejectionFindings {
        kind: RejectionKind::Defect,
        summary: "Fix defect".to_owned(),
        blackboard_message_id: Some(7),
        items: vec![FindingItem {
            severity: FindingSeverity::High,
            path: Some("src/lib.rs".to_owned()),
            message: "Missing guard".to_owned(),
            remediation: "Add guard".to_owned(),
        }],
    });
    tiered_req.failed_trace_log_tail = Some("stderr line\n".repeat(10));
    tiered_req.pre_task_slice = Some(json!({ "layer": "core", "summary": "slice" }));
    tiered_req.dependency_receipts = vec![Receipt {
        run_id: RunId::new("run-1").unwrap(),
        node_cursor: NodeCursor::task(
            TaskRef::new("t0").unwrap(),
            vec![GateId::new("deliver").unwrap()],
        ),
        gate_id: GateId::new("deliver").unwrap(),
        worker_id: WorkerId::new("w0").unwrap(),
        claim_revision: 1,
        contract_revision: 1,
        candidate_tree: TreeOid::new("4".repeat(40)).unwrap(),
        committed_oid: None,
        accepted_at: "2026-11-17T10:00:00.000Z".to_owned(),
        need_proofs: BTreeMap::new(),
        rollup: None,
    }];
    tiered_req.blackboard_items = (1..=20)
        .map(|i| BlackboardRecord {
            id: Some(i),
            run_id: RunId::new("run-1").unwrap(),
            node_id: Some("task:t1".to_owned()),
            recipient_node_id: None,
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "w1".to_owned(),
            topic: BlackboardTopic::Notes,
            status: BlackboardStatus::Open,
            payload: format!("note-{i}:{}", "z".repeat(600)),
            created_at: "2026-11-17T10:00:00.000Z".to_owned(),
        })
        .collect();

    let mut lenses = BTreeMap::new();
    lenses.insert("zebra".to_owned(), json!({ "items": ["fn_z"] }));
    lenses.insert("symbols".to_owned(), json!({ "items": ["fn_a", "fn_b"] }));
    lenses.insert("tests".to_owned(), json!({ "blob": "t".repeat(1500) }));
    let code_provider = Arc::new(LensMockProvider {
        domain: "code".to_owned(),
        lenses,
    });
    let provider =
        CompositeDomainProvider::with_module_flags(vec![code_provider], BTreeMap::new()).unwrap();

    tiered_req.gate.context = vec![
        "code.zebra".to_owned(),
        "code.symbols".to_owned(),
        "code.tests".to_owned(),
    ];

    let full = build_claim_context(&tiered_req, &provider).unwrap();
    assert_eq!(full.blackboard.len(), 15);
    for item in &full.blackboard {
        assert!(item["payload"].as_str().unwrap().chars().count() <= 500);
    }
    let lens_keys: Vec<&str> = full.context_lenses.keys().map(String::as_str).collect();
    assert_eq!(lens_keys, vec!["code.zebra", "code.symbols", "code.tests"]);

    // Tight budget: fits Tier 1..5, `code.zebra`, `code.symbols`, omits `code.tests` without overflow.
    tiered_req.response_max_bytes = full.total_bytes - 500;
    let trimmed = build_claim_context(&tiered_req, &provider).unwrap();
    assert!(trimmed.total_bytes <= tiered_req.response_max_bytes);
    assert!(trimmed.unresolved_findings.is_some());
    assert!(trimmed.pre_task_slice.is_some());
    assert_eq!(trimmed.dependency_receipts.len(), 1);
    assert!(!trimmed.blackboard.is_empty());
    assert!(trimmed.context_lenses.contains_key("code.symbols"));
    assert!(!trimmed.context_lenses.contains_key("code.tests"));
    assert!(trimmed
        .omitted_sections
        .contains(&"context:code.tests".to_owned()));

    // Razor-tight budget where `omitted_sections` itself cannot fit: total_bytes MUST still not overflow!
    let mut tiny_req = base_request();
    tiny_req.pre_task_slice = Some(json!({ "big": "x".repeat(400) }));
    tiny_req.gate.context.clear();
    let bare = build_claim_context(
        &ClaimContextRequest {
            pre_task_slice: None,
            ..tiny_req.clone()
        },
        &empty_provider,
    )
    .unwrap();
    tiny_req.response_max_bytes = bare.total_bytes + 5;
    let bounded = build_claim_context(&tiny_req, &empty_provider).unwrap();
    assert!(
        bounded.total_bytes <= tiny_req.response_max_bytes,
        "omitted_sections must never overflow response_max_bytes: {} > {}",
        bounded.total_bytes,
        tiny_req.response_max_bytes
    );

    // Disabled/missing provider lens fails closed with DomainCapabilityUnavailable.
    let err_missing = build_claim_context(&tiered_req, &empty_provider).unwrap_err();
    assert_eq!(err_missing.code, ErrorCode::DomainCapabilityUnavailable);

    // Markdown rendering works both with fallback template and custom template.
    let mut grouped_resp = trimmed.clone();
    grouped_resp.guidance = vec![
        json!("### SECTION ONE\n\n- step a"),
        json!("HITL top note"),
        json!("[role:builder] top role"),
    ];
    let md_default = render_claim_prompt_markdown(&grouped_resp, None);
    assert!(md_default.contains("# Gate Claim: `t1.build`"));
    assert!(md_default.contains("## Contract"));
    assert!(md_default.contains("## Guidance"));
    let hitl_pos = md_default.find("- HITL top note").unwrap();
    let role_pos = md_default.find("- [role:builder] top role").unwrap();
    let sec_pos = md_default.find("### SECTION ONE").unwrap();
    assert!(hitl_pos < sec_pos && role_pos < sec_pos, "{md_default}");
    assert!(
        md_default.contains("`t0.deliver`") && !md_default.contains("{\"Task\":"),
        "dependency receipt cursor must render human-readable in markdown: {md_default}"
    );

    let md_custom = render_claim_prompt_markdown(
        &trimmed,
        Some("Run={run_id} Node={node_cursor} Rev={claim_revision}\n{contract}"),
    );
    assert!(md_custom.contains("Run=run-1 Node=t1.build Rev=1"));
    assert!(md_custom.contains("\"gate_id\": \"build\""));

    // `context_reset_required: true` emits reset warning in markdown and keeps exact total_bytes,
    // and non-empty `spec.required_skills` is propagated into `contract.required_skills`.
    let mut reset_req = base_request();
    reset_req.gate.context.clear();
    reset_req.context_reset_required = true;
    if let Some(spec) = &mut reset_req.spec {
        spec.required_skills = vec!["acdd".to_owned(), "close-review-findings".to_owned()];
    }
    let reset_resp = build_claim_context(&reset_req, &empty_provider).unwrap();
    assert!(reset_resp.context_reset_required);
    assert_eq!(
        reset_resp.contract["required_skills"],
        json!(["acdd", "close-review-findings"])
    );
    assert_eq!(
        reset_resp.total_bytes,
        serde_json::to_vec(&reset_resp).unwrap().len()
    );
    let reset_md = render_claim_prompt_markdown(&reset_resp, None);
    assert!(reset_md.contains("CONTEXT RESET REQUIRED"));
    assert!(reset_md.contains("close-review-findings"));

    // Tier 5 Blackboard filters unrelated sibling task notes and keeps only latest BUDGET_OVERRIDE per scope.
    let mut bb_req = base_request();
    bb_req.gate.context.clear();
    let make_bb =
        |id, node_id: Option<&str>, rec: Option<&str>, topic, payload: &str| BlackboardRecord {
            id: Some(id),
            run_id: RunId::new("run-1").unwrap(),
            node_id: node_id.map(str::to_owned),
            recipient_node_id: rec.map(str::to_owned),
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "arch".to_owned(),
            topic,
            status: BlackboardStatus::Open,
            payload: payload.to_owned(),
            created_at: "2026-11-20T10:00:00.000Z".to_owned(),
        };
    bb_req.blackboard_items = vec![
        make_bb(
            1,
            Some("task:t0:build"),
            None,
            BlackboardTopic::Notes,
            "stale sibling task note",
        ),
        make_bb(
            2,
            Some("task:t0:review"),
            Some("task:t1:build"),
            BlackboardTopic::Notes,
            "directed handoff note",
        ),
        make_bb(
            3,
            Some("root:blueprint_synthesis.synthesize"),
            None,
            BlackboardTopic::Decisions,
            "global arch decision",
        ),
        make_bb(
            4,
            Some("task:t1:build"),
            None,
            BlackboardTopic::Decisions,
            "BUDGET_OVERRIDE: old limit 200",
        ),
        make_bb(
            5,
            Some("task:t1:build"),
            None,
            BlackboardTopic::Decisions,
            "BUDGET_OVERRIDE: latest limit 300",
        ),
    ];
    let bb_resp = build_claim_context(&bb_req, &empty_provider).unwrap();
    let bb_payloads: Vec<&str> = bb_resp
        .blackboard
        .iter()
        .filter_map(|v| v["payload"].as_str())
        .collect();
    assert!(!bb_payloads.contains(&"stale sibling task note"));
    assert!(!bb_payloads.contains(&"BUDGET_OVERRIDE: old limit 200"));
    assert!(bb_payloads.contains(&"directed handoff note"));
    assert!(bb_payloads.contains(&"global arch decision"));
    assert!(bb_payloads.contains(&"BUDGET_OVERRIDE: latest limit 300"));

    // `predecessor_proofs` inside `pre_task_slice` renders blueprint tasks cleanly in Markdown.
    let mut pred_resp = base_request();
    pred_resp.gate.context.clear();
    pred_resp.pre_task_slice = Some(json!({
        "predecessor_proofs": {
            "synthesize": {
                "blackboard_decision_id": "#1",
                "cited_invariants": ["INV-SINGLE-OWNER"],
                "tasks": {
                    "t1": { "layer": "core", "summary": "Batch reader", "max_net_loc": 50 }
                }
            }
        }
    }));
    let pred_built = build_claim_context(&pred_resp, &empty_provider).unwrap();
    let pred_md = render_claim_prompt_markdown(&pred_built, None);
    assert!(
        pred_md.contains("### Predecessor Proof: `synthesize`"),
        "{pred_md}"
    );
    assert!(pred_md.contains("#### Task Slice: `t1`"), "{pred_md}");
    assert!(pred_md.contains("Batch reader"), "{pred_md}");
}

#[test]
fn profile_gates_claim_context_window_budget_matrix() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let profile_dirs = vec![".acdd/profiles".to_owned()];
    let provider = CompositeDomainProvider::with_module_flags(
        vec![Arc::new(LensMockProvider {
            domain: "docs".to_owned(),
            lenses: BTreeMap::from([
                (
                    "instructions".to_owned(),
                    json!({
                        "directive": "MANDATORY: read repository rules for respective scope.",
                        "items": [{"path": "AGENTS.md", "headings": ["Instruction Authority", "Routes"]}]
                    }),
                ),
                (
                    "workspace_config".to_owned(),
                    json!({
                        "specs": {
                            "milestones": {
                                "patterns": ["docs/milestones"],
                                "default_draft_dir": "docs/milestones",
                                "prefixes": ["AC"],
                                "digits": 3,
                                "profile": "code",
                                "next_ids": {"AC": "AC-010"}
                            }
                        },
                        "docs_roots": ["docs"],
                        "profiles_dirs": [".acdd/profiles"],
                        "available_profiles": ["code", "documentation", "spec_planning"],
                        "architecture_zones": {"core": ["crates/core/**"]},
                        "limits": {"max_metadata_bytes": 2048},
                        "modules": {"profile": null, "enabled": [], "disabled": [], "domains": {}}
                    }),
                ),
                (
                    "adrs".to_owned(),
                    json!({"items": [{"title": "ADR-0001", "path": "docs/adr/0001.md"}]}),
                ),
                (
                    "invariants".to_owned(),
                    json!({
                        "items": [
                            {"id": "INV-SINGLE-OWNER", "path": "src/AGENTS.md", "rule": "Keep one canonical owner per subsystem."},
                            {"id": "INV-ENFORCED-RULE", "path": "docs/adr/0001.md", "line": 12, "rule": "Core stays leaf.", "enforced": true}
                        ]
                    }),
                ),
            ]),
        }),
        Arc::new(LensMockProvider {
            domain: "code".to_owned(),
            lenses: BTreeMap::from([
                (
                    "symbols".to_owned(),
                    json!({"items": [{"qualname": "crate::foo", "line": 10, "end_line": 20, "signature": "pub fn foo()"}]}),
                ),
                (
                    "tests".to_owned(),
                    json!({"items": [{"qualname": "crate::tests::foo_works", "path": "src/lib.rs", "line": 42}]}),
                ),
                (
                    "external_callers".to_owned(),
                    json!({"items": [{"symbol": "crate::foo", "path": "src/lib.rs", "caller_files": ["src/main.rs"]}]}),
                ),
            ]),
        })],
        BTreeMap::new(),
    )
    .unwrap();

    for (label, profile_ref) in [
        ("default", ".acdd/profile.yaml"),
        ("hotfix", "hotfix"),
        ("documentation", "documentation"),
        ("spec_planning", "spec_planning"),
    ] {
        let profile = resolve_profile_ref_with_dirs(
            profile_ref,
            Some(&workspace_root),
            None,
            Some(&profile_dirs),
        )
        .unwrap_or_else(|e| panic!("failed to resolve {label} profile: {e}"));

        let mut leaf_gates: Vec<(String, GateDef, bool)> = Vec::new();
        for g in &profile.gates {
            if let Some(tb) = &g.tasks {
                for sub in &tb.gates {
                    leaf_gates.push((format!("{}.{}", g.id, sub.id), sub.clone(), true));
                }
            } else if g.gates.is_empty() {
                leaf_gates.push((g.id.clone(), g.clone(), false));
            } else {
                for sub in &g.gates {
                    leaf_gates.push((format!("{}.{}", g.id, sub.id), sub.clone(), false));
                }
            }
        }
        assert!(
            !leaf_gates.is_empty(),
            "{label} profile must declare leaf gates"
        );

        for (gate_label, gate, is_task) in leaf_gates {
            let mut req = base_request();
            req.profile = profile.clone();
            req.gate = gate;
            req.node.kind = if is_task {
                NodeKind::TaskGate
            } else {
                NodeKind::RootGate
            };
            req.node.node_id = if is_task {
                format!("task:t1:{}", req.gate.id)
            } else {
                format!("root:{gate_label}")
            };
            req.response_max_bytes = CLAIM_DEFAULT_MAX_BYTES;
            let resp = build_claim_context(&req, &provider)
                .unwrap_or_else(|e| panic!("{label}/{gate_label} failed: {e}"));
            let md = render_claim_prompt_markdown(&resp, None);
            assert!(
                resp.total_bytes <= CLAIM_DEFAULT_MAX_BYTES,
                "{label}/{gate_label} JSON {} > {}",
                resp.total_bytes,
                CLAIM_DEFAULT_MAX_BYTES
            );
            assert!(
                md.len() <= CLAIM_DEFAULT_MAX_BYTES,
                "{label}/{gate_label} MD {} > {}",
                md.len(),
                CLAIM_DEFAULT_MAX_BYTES
            );
            assert!(
                resp.omitted_sections.is_empty(),
                "{label}/{gate_label} omitted sections: {:?}",
                resp.omitted_sections
            );
            assert!(
                !md.contains("```json"),
                "{label}/{gate_label} must not embed pretty JSON blocks"
            );
            if md.contains("## Workspace Configuration") {
                assert!(
                    md.contains("- spec `milestones`: `docs/milestones` (prefixes: [AC], digits: 3, profile: `code`, next: AC=AC-010)"),
                    "{label}/{gate_label}: unexpected spec rendering:\n{md}"
                );
                assert!(
                    md.contains("- zone `core`: [crates/core/**]"),
                    "{label}/{gate_label}: missing zone rendering:\n{md}"
                );
                assert!(
                    !md.contains("``"),
                    "{label}/{gate_label}: empty backticks:\n{md}"
                );
            }
            if md.contains("## Governing Invariants") {
                assert!(
                    md.contains("- `INV-SINGLE-OWNER` (`src/AGENTS.md`): Keep one canonical owner per subsystem."),
                    "{label}/{gate_label}: missing plain invariant rendering:\n{md}"
                );
                assert!(
                    md.contains("- `INV-ENFORCED-RULE` [ENFORCED] (`docs/adr/0001.md:12`): Core stays leaf."),
                    "{label}/{gate_label}: missing [ENFORCED] invariant rendering:\n{md}"
                );
            }
            if let Some(guidance_md) = md.split("\n## Guidance\n").nth(1) {
                let guidance_section = guidance_md.split("\n## ").next().unwrap_or(guidance_md);
                if let (Some(hitl_pos), Some(sub_pos)) = (
                    guidance_section.find("- HITL"),
                    guidance_section.find("\n### "),
                ) {
                    assert!(
                        hitl_pos < sub_pos,
                        "{label}/{gate_label}: ungrouped HITL must precede ### subsections"
                    );
                }
            }
        }
    }
}
