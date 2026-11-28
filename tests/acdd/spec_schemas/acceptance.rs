//! End-to-end AC-006 universal spec schemas and memory acceptance seam tests (Wave 6).

use acdd_core::config::load_adapter;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::WorkerId;
use acdd_core::response::Detail;
use acdd_spec::overlay::NoopRuntimeOverlay;
use acdd_spec::roadmap::{build_roadmap, RoadmapConfig};
use acdd_spec::types::RoadmapReq;
use acdd_state::types::BlackboardTopic;
use acdd_state::usecases::dto::{BlackboardPostReq, ClaimReq, StartReq, VerifyReq};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use acdd_state::validate::validate_workspace;
use contextunity_acdd::state_bridge::build_domain_providers;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::acdd::support::{base_engine_config, init_git_repo_on_branch};

fn seed_ac006_acceptance_workspace(root: &Path) {
    let files: &[(&str, &str)] = &[
        ("AGENTS.md", "# Acceptance Repo Instructions\n"),
        ("src/billing/mod.rs", "pub fn charge_invoice() {}\n"),
        (
            "acdd.yaml",
            r#"preset: full
roots:
  - src
docs:
  - AGENTS.md
  - docs
specs_dirs:
  - .acdd/specs
lint_dirs:
  - .acdd/lint
workflows_dirs:
  - .acdd/workflows
specs:
  milestones:
    path: docs/milestones
    base: contract
    prefixes: [AC]
    digits: 3
    roadmap: true
    fields:
      priority: { type: string, enum: [high, low] }
      tags: { type: array, items: { type: string } }
  research:
    path: docs/research
    ref: research
    prefixes: [RES]
    digits: 3
    roadmap: true
  memories:
    path: docs/memories
    base: memory
    roadmap: false
roadmap:
  color: never
  badges: [priority, status]
  group_by: wave
state_db: .acdd/state.sqlite
modules:
  code: true
  docs: true
  state: true
  blackboard: true
"#,
        ),
        (
            ".acdd/lint/architecture.yaml",
            r#"unused_public_api: false
zones:
  billing: ["src/billing/**"]
rules: []
"#,
        ),
        (
            ".acdd/specs/research.yaml",
            r#"roadmap: true
fields:
  type: object
  required: [question]
  properties:
    question: { type: string }
items:
  probes:
    item: probe
    fields:
      type: object
      required: [method]
      properties:
        method: { type: string }
"#,
        ),
        (
            ".acdd/workflows/default.yaml",
            r#"version: 2
inherit: documentation
git:
  isolation: in_place
roles:
  builder: {}
  reviewer: {}
gates:
  - id: memory_synthesis
    role: builder
    pass: preflight_review
    scope:
      - specs.memories
    checks:
      - check: docs.specs
        with:
          spec: memories
  - id: preflight_review
    role: reviewer
    proof: review
    contours:
      - architecture
    fail: memory_synthesis
  - tasks:
      gates:
        - id: build
          role: builder
          checks:
            - spec.deltas_applied
          context:
            - paths.instructions
            - specs.memories
"#,
        ),
        (
            "docs/research/RES-001-billing-latency.md",
            r#"---
id: RES-001
title: Billing latency study
spec: research
status: planned
question: How fast is invoice charging?
---

### probe: latency-check
```yaml
method: benchmark
```
"#,
        ),
        (
            "docs/milestones/AC-001-billing.md",
            r#"---
id: AC-001
title: Billing acceptance milestone
status: planned
priority: high
tags: [billing, core]
spec_deltas:
  - target: RES-001
    field: probes
    added: [latency-check]
---

### task: t1
```yaml
target: Implement billing helper in src/billing/mod.rs
scope:
  - src/billing/mod.rs
```
"#,
        ),
    ];

    for (rel, content) in files {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(abs, content).unwrap();
    }
}

#[test]
fn ac006_end_to_end_universal_specs_lenses_roadmap_deltas_and_memory() {
    let dir = ScopedTempDir::create("acdd-ac006-acceptance").unwrap();
    let root = dir.path();
    seed_ac006_acceptance_workspace(root);
    init_git_repo_on_branch(root, "main");

    // 1. Adapter + external .acdd/specs/*.yaml + .acdd/lint/*.yaml + workspace validation.
    let adapter = load_adapter(root, None).unwrap();
    assert!(adapter.specs.contains_key("milestones"));
    assert!(adapter.specs.contains_key("research"));
    assert!(adapter.specs.contains_key("memories"));
    assert!(adapter.architecture.zones.contains_key("billing"));

    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let validation = validate_workspace(root, None, None, providers.as_ref()).unwrap();
    assert!(
        validation.valid,
        "workspace validation must pass: {:?}",
        validation.diagnostics
    );

    // 2. Multi-spec `start` attaching RES-001 alongside primary AC-001.
    let mut cfg = base_engine_config(root);
    cfg.spec_collections = adapter.specs.clone();
    let engine = StateEngine::open(cfg, providers).unwrap();

    let start_resp = match engine
        .execute(StateCommand::Start(StartReq {
            spec_paths: vec![
                "docs/milestones/AC-001-billing.md".to_owned(),
                "docs/research/RES-001-billing-latency.md".to_owned(),
            ],
            workflow: Some("default".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    {
        StateCommandOutput::Start(resp) => resp,
        other => panic!("unexpected start output: {other:?}"),
    };
    let run_id = start_resp.run.run_id.as_str().to_owned();

    // 3. Real rejected-review -> memory_synthesis seam:
    //    pass initial `memory_synthesis` with zero edits (AC-09), independently claim `preflight_review`,
    //    post findings, reject via `SubmitAction::Reject`, follow the route back to `memory_synthesis`,
    //    write `spec: memory` citing that rejected-review finding ID, and verify via `StateCommand::Verify`.
    let worker = WorkerId::new("acceptance-worker").unwrap();
    let _init_mem_claim = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("memory_synthesis".to_owned()),
            worker_id: worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap();
    let _init_mem_verify = engine
        .execute(StateCommand::Verify(VerifyReq {
            node: "memory_synthesis".to_owned(),
            run: Some(run_id.clone()),
            worker_id: Some(worker.clone()),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .unwrap();
    let _init_mem_pass = engine
        .execute(StateCommand::Submit(Box::new(
            acdd_state::usecases::dto::SubmitReq {
                node: "memory_synthesis".to_owned(),
                run: Some(run_id.clone()),
                worker_id: worker.clone(),
                action: acdd_state::types::SubmitAction::Pass,
                summary: Some("No initial memory edits needed".to_owned()),
                test_target: None,
                review_evidence: None,
                custom_evidence: None,
                rejection_findings: None,
                verified_invariants: Vec::new(),
                worktree: None,
                claim_revision: None,
                allow_main: false,
            },
        )))
        .unwrap();

    let reviewer = WorkerId::new("reviewer-1").unwrap();
    let _rev_claim = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("preflight_review".to_owned()),
            worker_id: reviewer.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap();

    let finding_id = match engine
        .execute(StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some(run_id.clone()),
            node: Some("preflight_review".to_owned()),
            recipient_node: Some("memory_synthesis".to_owned()),
            subtask_ref: None,
            gate: Some("preflight_review".to_owned()),
            topic: BlackboardTopic::Findings,
            author: reviewer.as_str().to_owned(),
            payload: serde_json::json!({
                "title": "Validate currency",
                "body": "Always validate invoice currency before charging."
            })
            .to_string(),
        }))
        .unwrap()
    {
        StateCommandOutput::BlackboardPost(resp) => resp.entry.id.expect("blackboard id"),
        other => panic!("unexpected blackboard output: {other:?}"),
    };

    let _reject_rev = engine
        .execute(StateCommand::Submit(Box::new(
            acdd_state::usecases::dto::SubmitReq {
                node: "preflight_review".to_owned(),
                run: Some(run_id.clone()),
                worker_id: reviewer,
                action: acdd_state::types::SubmitAction::Reject,
                summary: Some("Rejected preflight review; route to memory_synthesis".to_owned()),
                test_target: None,
                review_evidence: None,
                custom_evidence: None,
                rejection_findings: Some(acdd_state::types::RejectionFindings {
                    kind: acdd_state::types::RejectionKind::Defect,
                    blackboard_message_id: Some(finding_id),
                    summary: "Validate invoice currency before charging".to_owned(),
                    items: vec![acdd_state::types::FindingItem {
                        severity: acdd_state::types::FindingSeverity::High,
                        path: Some("src/billing/mod.rs".to_owned()),
                        message: "Missing invoice currency validation".to_owned(),
                        remediation: "Record currency validation rule in memory spec".to_owned(),
                    }],
                }),
                verified_invariants: Vec::new(),
                worktree: None,
                claim_revision: None,
                allow_main: false,
            },
        )))
        .unwrap();

    let _mem_claim = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("memory_synthesis".to_owned()),
            worker_id: worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap();

    fs::create_dir_all(root.join("docs/memories")).unwrap();
    fs::write(
        root.join("docs/memories/MEM-001-billing-rules.md"),
        format!(
            r##"---
id: MEM-001
title: Billing memory rules
spec: memory
status: active
---

### rule: R-BILLING-SCOPE
```yaml
statement: Always validate invoice currency before charging
scope:
  - src/billing/**
evidence:
  - "#{finding_id}"
```

### rule: R-GLOBAL-FALLBACK
```yaml
statement: Keep workspace logs structured
scope:
  - "**"
evidence:
  - "#{finding_id}"
```
"##
        ),
    )
    .unwrap();

    let mem_verify = match engine
        .execute(StateCommand::Verify(VerifyReq {
            node: "memory_synthesis".to_owned(),
            run: Some(run_id.clone()),
            worker_id: Some(worker.clone()),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .unwrap()
    {
        StateCommandOutput::Verify(v) => v,
        other => panic!("unexpected verify output: {other:?}"),
    };
    assert!(
        mem_verify.passed,
        "memory_synthesis.synthesize with real blackboard#{finding_id} evidence must pass: {:?}",
        mem_verify.check_results
    );

    // 4. Roadmap filters: default excludes `memories`, `--all` includes it, `--tag`/`--field`/`--group-by` filter cleanly.
    let empty_linked = BTreeMap::new();
    let rm_cfg = RoadmapConfig {
        workspace_root: root,
        worktree: root,
        spec_collections: &adapter.specs,
        linked_workspaces: &empty_linked,
    };
    let default_rm = build_roadmap(&rm_cfg, &NoopRuntimeOverlay, RoadmapReq::default()).unwrap();
    let default_ids: Vec<&str> = default_rm
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert!(default_ids.contains(&"AC-001"));
    assert!(default_ids.contains(&"RES-001"));
    assert!(
        !default_ids.contains(&"MEM-001"),
        "memories (roadmap: false) must be excluded by default"
    );

    let all_rm = build_roadmap(
        &rm_cfg,
        &NoopRuntimeOverlay,
        RoadmapReq {
            all: true,
            ..RoadmapReq::default()
        },
    )
    .unwrap();
    let all_ids: Vec<&str> = all_rm
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert!(all_ids.contains(&"MEM-001"), "--all must include memories");

    let filtered_rm = build_roadmap(
        &rm_cfg,
        &NoopRuntimeOverlay,
        RoadmapReq {
            tags: vec!["billing".to_owned()],
            fields: vec!["priority=high".to_owned()],
            group_by: Some("priority".to_owned()),
            badges: adapter.roadmap.badges.clone(),
            ..RoadmapReq::default()
        },
    )
    .unwrap();
    let filtered_ids: Vec<&str> = filtered_rm
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert_eq!(filtered_ids, vec!["AC-001"]);
    assert_eq!(filtered_rm.groups.len(), 1);
    assert_eq!(filtered_rm.groups[0].group, "high");
    let ac001_badges: Vec<(&str, &str)> = filtered_rm.groups[0].specs[0]
        .badges
        .iter()
        .map(|b| (b.name.as_str(), b.value.as_str()))
        .collect();
    assert!(ac001_badges.contains(&("priority", "high")));
    assert!(ac001_badges.contains(&("status", "ready")));
    let md = contextunity_acdd::format::render_roadmap_markdown_with_options(
        &filtered_rm,
        false,
        &contextunity_acdd::format::RoadmapRenderOptions {
            ansi_color: false,
            group_by: Some("priority".to_owned()),
        },
    );
    assert!(!md.contains('\x1b'), "color: never must emit zero ANSI escapes");
    assert!(md.contains("AC-001"));

    // 5. Pass memory_synthesis -> claim `t1.build` -> verify `attached_specs`, `paths.instructions`,
    //    `specs.memories` 4-tier ranking, and `spec.deltas_applied` via StateCommand::Verify.
    let _submit_mem = engine
        .execute(StateCommand::Submit(Box::new(
            acdd_state::usecases::dto::SubmitReq {
                node: "memory_synthesis".to_owned(),
                run: Some(run_id.clone()),
                worker_id: worker.clone(),
                action: acdd_state::types::SubmitAction::Pass,
                summary: Some("Recorded billing memory rules".to_owned()),
                test_target: None,
                review_evidence: None,
                custom_evidence: None,
                rejection_findings: None,
                verified_invariants: Vec::new(),
                worktree: None,
                claim_revision: None,
                allow_main: false,
            },
        )))
        .unwrap();

    // Re-claim and pass `preflight_review` now that `memory_synthesis` recorded the rule,
    // unlocking `t1.build` to receive `specs.memories`.
    let reviewer_pass = WorkerId::new("reviewer-1").unwrap();
    let _rev_reclaim = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("preflight_review".to_owned()),
            worker_id: reviewer_pass.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap();
    let _rev_pass = engine
        .execute(StateCommand::Submit(Box::new(
            acdd_state::usecases::dto::SubmitReq {
                node: "preflight_review".to_owned(),
                run: Some(run_id.clone()),
                worker_id: reviewer_pass,
                action: acdd_state::types::SubmitAction::Pass,
                summary: Some("Preflight review passed after memory synthesis".to_owned()),
                test_target: None,
                review_evidence: Some(acdd_state::types::ReviewEvidence {
                    decision: acdd_state::types::SubmitAction::Pass,
                    contours: BTreeMap::from([(
                        "architecture".to_owned(),
                        acdd_state::types::ReviewContourResult {
                            applicable: true,
                            evidence: "docs/memories/MEM-001-billing-rules.md recorded".to_owned(),
                        },
                    )]),
                    summary: "Memory rule recorded".to_owned(),
                    notes: None,
                }),
                custom_evidence: None,
                rejection_findings: None,
                verified_invariants: Vec::new(),
                worktree: None,
                claim_revision: None,
                allow_main: false,
            },
        )))
        .unwrap();

    let claim = match engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Detail::Compact,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(resp) => resp,
        other => panic!("unexpected claim output: {other:?}"),
    };

    let attached = claim.contract["attached_specs"]
        .as_array()
        .expect("contract.attached_specs must be an array");
    assert_eq!(attached.len(), 2);
    let res_attached = attached
        .iter()
        .find(|s| s["spec_id"].as_str() == Some("RES-001"))
        .expect("attached_specs must include RES-001");
    assert_eq!(
        res_attached["fields"]["question"].as_str(),
        Some("How fast is invoice charging?"),
        "attached_specs must carry schema-backed fields summary: {res_attached:?}"
    );
    let ac_attached = attached
        .iter()
        .find(|s| s["spec_id"].as_str() == Some("AC-001"))
        .expect("attached_specs must include AC-001");
    assert_eq!(
        ac_attached["fields"]["priority"].as_str(),
        Some("high"),
        "primary attached_spec must carry schema-backed fields summary: {ac_attached:?}"
    );

    assert!(
        claim.context_lenses.contains_key("paths.instructions"),
        "paths.instructions lens must be delivered"
    );

    let memories_lens = claim
        .context_lenses
        .get("specs.memories")
        .expect("specs.memories lens must be delivered");
    let items = memories_lens["items"]
        .as_array()
        .expect("specs.memories must contain items array");
    assert!(items.len() >= 2);
    let first_ref = items[0]["item_ref"]
        .as_str()
        .or_else(|| items[0]["ref"].as_str())
        .unwrap_or_default();
    assert_eq!(
        first_ref, "R-BILLING-SCOPE",
        "Tier 2 scope-overlapping memory rule must rank before Tier 4 global fallback"
    );

    // 6. Real `SpecDomainProvider` (`spec.deltas_applied`) via StateCommand::Verify.
    let build_verify = match engine
        .execute(StateCommand::Verify(VerifyReq {
            node: "t1.build".to_owned(),
            run: Some(run_id),
            worker_id: Some(worker),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .unwrap()
    {
        StateCommandOutput::Verify(v) => v,
        other => panic!("unexpected verify output: {other:?}"),
    };
    assert!(
        build_verify.passed,
        "t1.build spec.deltas_applied check must pass through SpecDomainProvider: {:?}",
        build_verify.check_results
    );
}
