//! `spec_deltas` and `spec.deltas_applied` verification seam tests (Wave 4).

use acdd_core::config::load_adapter;
use acdd_core::errors::ErrorCode;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::{GateId, NodeCursor, RunId, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, FreshnessObservation};
use acdd_core::subsystems::SubsystemRegistry;
use acdd_spec::{parse_spec, parse_spec_with_collections, verify_spec_deltas};
use acdd_state::usecases::dto::{ClaimReq, StartReq, SubmitReq, VerifyReq};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use acdd_state::workflow::{
    resolve_workflow_from_str_with_dirs, validate_workflow, WorkflowValidationOptions,
};
use contextunity_acdd::state_bridge::build_domain_providers;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::acdd::support::{base_engine_config, init_git_repo_on_branch};

fn make_check_req(root: &Path, scope: Vec<String>) -> DomainCheckReq {
    DomainCheckReq {
        run_id: RunId::new("run-deltas").unwrap(),
        node_cursor: "t1.build".parse::<NodeCursor>().unwrap(),
        gate_id: GateId::new("build").unwrap(),
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        admitted_scope: scope,
        changed_paths: Vec::new(),
        test_target: None,
        path_policy: WorkspacePathPolicy::default(),
        baseline_ref: None,
        node_tags: Vec::new(),
    }
}

fn write_delta_workspace(root: &Path, target_frontmatter: &str) {
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/api")).unwrap();
    fs::write(
        root.join("acdd.yaml"),
        r#"preset: full
specs:
  milestones:
    path: docs/milestones
    base: contract
  api:
    path: docs/api
    prefixes: [API]
    digits: 3
    roadmap: false
    fields:
      type: object
      additionalProperties: true
    items:
      endpoints:
        item: endpoint
        fields:
          type: object
          additionalProperties: true
"#,
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-010.md"),
        r#"---
id: AC-010
title: Worker Streaming Upgrade
status: active
spec_deltas:
  - target: API-WORKER
    field: endpoints
    added: [CancelSession]
    modified:
      Run:
        previously: blocking
        now: streaming
    removed: [LegacyRun]
---

### task: t1
```yaml
task_ref: t1
scope: [docs/api/**]
acceptance_criteria: [applied]
```
"#,
    )
    .unwrap();
    fs::write(root.join("docs/api/API-WORKER.md"), target_frontmatter).unwrap();
}

#[test]
fn pure_spec_deltas_parsing_validation_and_verification_matrix() {
    // 1. Valid spec_deltas with added / modified (with and without previously) / removed parses cleanly.
    let valid_raw = r#"---
id: AC-010
title: Streaming Cutover
status: planned
spec_deltas:
  - target: API-WORKER
    field: endpoints
    added: [CancelSession]
    modified:
      Run:
        previously: blocking
        now: streaming
      Ping:
        now: pong-v2
    removed: [LegacyRun]
---
"#;
    let src_doc = parse_spec(valid_raw).expect("valid spec_deltas must parse");
    assert_eq!(src_doc.spec_deltas.len(), 1);
    assert_eq!(src_doc.spec_deltas[0].target, "API-WORKER");
    assert_eq!(src_doc.spec_deltas[0].field, "endpoints");
    assert_eq!(src_doc.spec_deltas[0].added, vec!["CancelSession"]);
    assert_eq!(src_doc.spec_deltas[0].removed, vec!["LegacyRun"]);
    assert_eq!(
        src_doc.spec_deltas[0].modified["Run"].previously.as_deref(),
        Some("blocking")
    );
    assert_eq!(src_doc.spec_deltas[0].modified["Run"].now, "streaming");

    // 2. Fail-closed parse/validation matrix for malformed or contradictory spec_deltas.
    let invalid_cases: &[(&str, &str)] = &[
        ("missing_target", "spec_deltas:\n  - field: endpoints\n"),
        ("unsafe_target", "spec_deltas:\n  - target: ../etc\n    field: endpoints\n"),
        ("empty_field", "spec_deltas:\n  - target: API-WORKER\n    field: \"  \"\n"),
        (
            "unknown_delta_key",
            "spec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    extra: 1\n",
        ),
        (
            "missing_now",
            "spec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    modified:\n      Run: { previously: blocking }\n",
        ),
        (
            "empty_now",
            "spec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    modified:\n      Run: { now: \"  \" }\n",
        ),
        (
            "contradictory_added_removed",
            "spec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    added: [CancelSession]\n    removed: [CancelSession]\n",
        ),
        (
            "contradictory_modified_removed",
            "spec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    modified:\n      Run: { now: streaming }\n    removed: [Run]\n",
        ),
    ];
    for (name, block) in invalid_cases {
        let raw = format!("---\nid: AC-BAD\ntitle: Bad\n{block}---\n");
        let err = parse_spec(&raw).expect_err(&format!("case {name} must fail closed"));
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "case {name}: {err}");
    }

    // 3. Target representation matrix (object list, id-keyed map, string list, named item collection, tasks).
    let dir = ScopedTempDir::create("acdd_deltas_pure").unwrap();
    let root = dir.path();
    write_delta_workspace(root, "---\nid: API-WORKER\ntitle: Dummy\nspec: api\n---\n");
    let adapter = load_adapter(root, None).unwrap();

    let ok_targets: &[(&str, &str)] = &[
        (
            "object_array",
            "---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: CancelSession\n    summary: cancels\n  - id: Run\n    mode: streaming\n  - id: Ping\n    note: pong-v2\n---\n",
        ),
        (
            "id_keyed_map",
            "---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  CancelSession: cancels\n  Run:\n    mode: streaming\n  Ping: pong-v2\n---\n",
        ),
        (
            "string_array",
            "---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - CancelSession\n  - \"Run: streaming\"\n  - \"Ping: pong-v2\"\n---\n",
        ),
        (
            "named_item_collection",
            "---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n\n### endpoint: CancelSession\n```yaml\nsummary: cancel\n```\n\n### endpoint: Run\n```yaml\nmode: streaming\n```\n\n### endpoint: Ping\n```yaml\nnote: pong-v2\n```\n",
        ),
    ];
    for (label, target_raw) in ok_targets {
        let target_doc =
            parse_spec_with_collections(target_raw, adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)), &adapter.specs)
                .unwrap_or_else(|e| panic!("target {label} failed parse: {e}"));
        let report = verify_spec_deltas(&[&src_doc], &[&src_doc, &target_doc], None);
        assert!(report.is_ok(), "target {label} failed: {:?}", report.diagnostics);
        assert_eq!(report.specs_checked, 1);
        assert_eq!(report.deltas_checked, 1);
    }

    // 4. Tasks & named item core fields (`status`, `tags`, `depends_on`, `invariants`, `subtasks`) resolve cleanly.
    let task_delta_src = parse_spec(
        "---\nid: AC-TASK-D\ntitle: Task Delta\nspec_deltas:\n  - target: AC-TGT\n    field: tasks\n    added: [t1]\n    modified:\n      t1: { now: INV-CORE }\n---\n",
    )
    .unwrap();
    let task_tgt = parse_spec(
        "---\nid: AC-TGT\ntitle: Target\n---\n\n### task: t1\n```yaml\ntask_ref: t1\ntarget: updated target\nstatus: active\ntags: [core]\ninvariants: [INV-CORE]\nscope: [src/]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    assert!(verify_spec_deltas(&[&task_delta_src], &[&task_delta_src, &task_tgt], None).is_ok());

    let item_core_src = parse_spec(
        "---\nid: AC-ITEM-CORE\ntitle: Item Core\nspec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    modified:\n      Run: { now: deprecated }\n---\n",
    )
    .unwrap();
    let item_core_ok = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n\n### endpoint: Run\n```yaml\ntags: [deprecated]\n```\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    assert!(verify_spec_deltas(&[&item_core_src], &[&item_core_src, &item_core_ok], None).is_ok());
    let item_core_stale = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n\n### endpoint: Run\n```yaml\ntags: [active]\n```\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    assert!(!verify_spec_deltas(&[&item_core_src], &[&item_core_src, &item_core_stale], None).is_ok());

    let item_status_src = parse_spec(
        "---\nid: AC-ITEM-STATUS\ntitle: Item Status\nspec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    modified:\n      Run: { previously: active, now: cancelled }\n---\n",
    )
    .unwrap();
    let item_status_ok = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n\n### endpoint: Run\n```yaml\nstatus: cancelled\n```\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    assert!(verify_spec_deltas(&[&item_status_src], &[&item_status_src, &item_status_ok], None).is_ok());
    let item_status_stale = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n\n### endpoint: Run\n```yaml\nstatus: active\n```\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    assert!(
        !verify_spec_deltas(&[&item_status_src], &[&item_status_src, &item_status_stale], None)
            .is_ok()
    );

    // 5. Substring-id guard, bare-ID string guard & identity-key exclusion guard:
    // - "Cancel" must not match "CancelSession" in string list;
    // - bare-ID string "Run" must not satisfy modified `now: Run`;
    // - modified `now: Run` must not match `id: Run` when payload has `mode: blocking`.
    let guard_src = parse_spec(
        "---\nid: AC-GUARD\ntitle: Guard\nspec_deltas:\n  - target: API-WORKER\n    field: endpoints\n    added: [Cancel]\n    modified:\n      Run: { now: Run }\n---\n",
    )
    .unwrap();
    let guard_tgt = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Guard Tgt\nspec: api\nendpoints:\n  - CancelSession\n  - Run\n---\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    let guard_rep = verify_spec_deltas(&[&guard_src], &[&guard_src, &guard_tgt], None);
    assert!(!guard_rep.is_ok());
    assert!(
        guard_rep.diagnostics.iter().any(|d| d.contains("added entry 'Cancel'")),
        "{:?}",
        guard_rep.diagnostics
    );
    assert!(
        guard_rep.diagnostics.iter().any(|d| d.contains("modified entry 'Run'")),
        "bare-ID string entry must not satisfy modified.now matching its own ID: {:?}",
        guard_rep.diagnostics
    );

    let id_only_tgt = parse_spec_with_collections(
        "---\nid: API-WORKER\ntitle: Guard Tgt 2\nspec: api\nendpoints:\n  - id: Cancel\n  - id: Run\n    mode: blocking\n---\n",
        adapter.specs.get_key_value("api").map(|(k, v)| (k.as_str(), v)),
        &adapter.specs,
    )
    .unwrap();
    let id_rep = verify_spec_deltas(&[&guard_src], &[&guard_src, &id_only_tgt], None);
    assert!(!id_rep.is_ok());
    assert!(
        id_rep.diagnostics.iter().any(|d| d.contains("modified entry 'Run'")),
        "{:?}",
        id_rep.diagnostics
    );

    // 6. Empty deltas pass cleanly.
    let no_deltas = parse_spec("---\nid: AC-EMPTY\ntitle: Empty\n---\n").unwrap();
    let empty_rep = verify_spec_deltas(&[&no_deltas], &[&no_deltas], None);
    assert!(empty_rep.is_ok());
    assert_eq!(empty_rep.deltas_checked, 0);
}

#[test]
fn spec_deltas_applied_provider_workflow_freshness_and_gate_matrix() {
    let dir = ScopedTempDir::create("acdd_deltas_provider").unwrap();
    let root = dir.path().canonicalize().unwrap();
    init_git_repo_on_branch(&root, "main");
    write_delta_workspace(
        &root,
        "---\nid: API-WORKER\ntitle: Worker Spec\nspec: api\nendpoints:\n  - id: CancelSession\n    summary: cancel\n  - id: Run\n    mode: streaming\n---\n",
    );

    let adapter = load_adapter(&root, None).unwrap();
    let providers = build_domain_providers(&root, &adapter, None, Vec::new()).unwrap();
    assert!(providers.is_domain_enabled("spec"));

    // Preset `index` disables `spec` domain both in CompositeDomainProvider and workflow validation.
    let mut index_adapter = adapter.clone();
    index_adapter.modules = SubsystemRegistry {
        preset: acdd_core::subsystems::SubsystemPreset::Index,
        ..SubsystemRegistry::default()
    };
    let index_providers = build_domain_providers(&root, &index_adapter, None, Vec::new()).unwrap();
    assert!(!index_providers.is_domain_enabled("spec"));

    let wf_yaml = r#"version: 2
inherit: base
gates:
  - tasks:
      gates:
        - id: apply
          checks:
            - spec.deltas_applied
          proof: none
"#;
    let resolved_wf =
        resolve_workflow_from_str_with_dirs(wf_yaml, "deltas", None, None, None, None).unwrap();
    validate_workflow(
        &resolved_wf,
        &WorkflowValidationOptions {
            domain_providers: Some(providers.as_ref()),
            acdd_modules: Some(&adapter.modules),
            require_bound_commands: false,
            spec_collections: Some(&adapter.specs),
        },
    )
    .expect("workflow with spec.deltas_applied validates when spec subsystem is enabled");

    let disabled_err = validate_workflow(
        &resolved_wf,
        &WorkflowValidationOptions {
            domain_providers: Some(index_providers.as_ref()),
            acdd_modules: Some(&index_adapter.modules),
            require_bound_commands: false,
            spec_collections: Some(&index_adapter.specs),
        },
    )
    .expect_err("workflow with spec.deltas_applied must fail closed when preset disables spec");
    assert_eq!(disabled_err.code, ErrorCode::DomainCapabilityUnavailable);

    // Parameter schema validation (additionalProperties: false).
    providers
        .validate_check_ref(
            "spec.deltas_applied",
            &json!({"spec": "AC-010", "target": "API-WORKER"}),
        )
        .expect("valid params accepted");
    assert!(
        providers
            .validate_check_ref("spec.deltas_applied", &json!({"bogus": true}))
            .is_err(),
        "unknown params rejected"
    );

    // Passing check + byte-exact observe_freshness digest salt.
    let req = make_check_req(&root, vec!["docs/api/**".to_owned()]);
    let pass_out = providers
        .run_check("spec.deltas_applied", &json!({}), &req)
        .unwrap();
    let DomainCheckOutcome::Passed {
        execution_digest_salt: Some(run_salt),
        ..
    } = pass_out
    else {
        panic!("expected Passed outcome, got {pass_out:?}");
    };
    assert!(run_salt.starts_with("spec_deltas_live="));
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(fresh_salt),
        ..
    } = providers
        .observe_freshness("spec.deltas_applied", &json!({}), &req)
        .unwrap()
    else {
        panic!("expected Passed freshness observation");
    };
    assert_eq!(run_salt, fresh_salt);

    // Negative mutation matrix: missing added, remaining removed, diverged now, missing field, missing target.
    let fail_cases: &[(&str, Option<&str>, &str)] = &[
        (
            "missing_added",
            Some("---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: Run\n    mode: streaming\n---\n"),
            "added entry 'CancelSession'",
        ),
        (
            "removed_still_present",
            Some("---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: CancelSession\n  - id: Run\n    mode: streaming\n  - id: LegacyRun\n---\n"),
            "removed entry 'LegacyRun'",
        ),
        (
            "modified_now_diverged",
            Some("---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: CancelSession\n  - id: Run\n    mode: blocking\n---\n"),
            "does not carry expected now text 'streaming'",
        ),
        (
            "empty_declared_collection_missing_added",
            Some("---\nid: API-WORKER\ntitle: Worker\nspec: api\n---\n"),
            "added entry 'CancelSession'",
        ),
        ("missing_target", None, "target spec 'API-WORKER' not found"),
    ];

    for (name, target_content, expected_substr) in fail_cases {
        let tgt_path = root.join("docs/api/API-WORKER.md");
        match target_content {
            Some(content) => fs::write(&tgt_path, content).unwrap(),
            None => {
                let _ = fs::remove_file(&tgt_path);
            }
        }
        let outcome = providers
            .run_check("spec.deltas_applied", &json!({}), &req)
            .unwrap();
        let DomainCheckOutcome::Failed { diagnostics, .. } = outcome else {
            panic!("case {name} expected Failed, got {outcome:?}");
        };
        assert!(
            diagnostics.iter().any(|d| d.contains(expected_substr)),
            "case {name}: expected '{expected_substr}' in {diagnostics:?}"
        );
        // Editing target spec changes observe_freshness salt.
        let FreshnessObservation::Passed {
            execution_digest_salt: Some(mutated_salt),
            ..
        } = providers
            .observe_freshness("spec.deltas_applied", &json!({}), &req)
            .unwrap()
        else {
            panic!("expected Passed freshness observation");
        };
        assert_ne!(mutated_salt, fresh_salt, "case {name} must change freshness salt");
    }

    // End-to-end StateEngine verify -> fix target -> verify -> submit gate proof!
    fs::write(
        root.join("docs/api/API-WORKER.md"),
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: Run\n    mode: blocking\n---\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(root.join(".acdd/workflows/deltas.yaml"), wf_yaml).unwrap();
    let cfg = base_engine_config(&root);
    let mut cfg = cfg;
    cfg.spec_collections = adapter.specs.clone();
    let engine = StateEngine::open(cfg, providers.clone()).unwrap();
    let StateCommandOutput::Start(started) = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-010.md".to_owned()),
            workflow: Some("deltas".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let run_id = started.run.run_id.as_str().to_owned();
    let worker = WorkerId::new("w-deltas").unwrap();
    engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(run_id.clone()),
            node: Some("t1.apply".to_owned()),
            worker_id: worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Compact,
        }))
        .unwrap();

    // First verify fails closed because target spec has not applied the delta yet.
    let bad_err = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(run_id.clone()),
            node: "t1.apply".to_owned(),
            worker_id: Some(worker.clone()),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .expect_err("stale target must fail gate verify");
    assert_eq!(bad_err.code, ErrorCode::GateVerificationFailed, "{bad_err}");

    // Apply edits to API-WORKER.md, then verify + submit must pass!
    fs::write(
        root.join("docs/api/API-WORKER.md"),
        "---\nid: API-WORKER\ntitle: Worker\nspec: api\nendpoints:\n  - id: CancelSession\n    summary: cancel\n  - id: Run\n    mode: streaming\n---\n",
    )
    .unwrap();
    let StateCommandOutput::Verify(good_verify) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(run_id.clone()),
            node: "t1.apply".to_owned(),
            worker_id: Some(worker.clone()),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(good_verify.passed, "updated target must pass gate verify: {good_verify:?}");
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(run_id),
            node: "t1.apply".to_owned(),
            action: acdd_state::types::SubmitAction::Pass,
            worker_id: worker,
            claim_revision: None,
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: true,
            worktree: None,
        })))
        .expect("submit must pass after fresh spec.deltas_applied verify");

    // Unknown `spec` selector fails closed; disabled `spec` domain rejects run_check.
    let unknown_err = providers
        .run_check("spec.deltas_applied", &json!({"spec": "NO-SUCH-SPEC"}), &req)
        .expect_err("unknown spec selector must fail closed");
    assert!(unknown_err.to_string().contains("NO-SUCH-SPEC"), "{unknown_err}");

    // Blank / unsafe `target` filter fails closed with WorkflowSchemaInvalid.
    for bad_target in ["   ", "../escape"] {
        let bad_target_err = providers
            .run_check("spec.deltas_applied", &json!({"target": bad_target}), &req)
            .expect_err("blank or unsafe target must fail closed");
        assert!(
            bad_target_err.to_string().contains("invalid spec.deltas_applied target"),
            "{bad_target_err}"
        );
    }

    // Removing the final item of a declared named `items:` collection (`endpoints`) passes,
    // empty base-matched collection (`with: { spec: memory }`) passes, and `status: cancelled`
    // source specs with unapplied deltas are skipped in default unscoped runs.
    fs::write(
        root.join("acdd.yaml"),
        format!(
            "{}\n  memories:\n    path: docs/memories\n    base: memory\n",
            fs::read_to_string(root.join("acdd.yaml")).unwrap()
        ),
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-011.md"),
        "---\nid: AC-011\ntitle: Remove Last Endpoint\nstatus: active\nspec_deltas:\n  - target: API-EMPTY\n    field: endpoints\n    removed: [OnlyEndpoint]\n---\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/api/API-EMPTY.md"),
        "---\nid: API-EMPTY\ntitle: Empty Endpoints\nspec: api\n---\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-012.md"),
        "---\nid: AC-012\ntitle: Cancelled Spec\nstatus: cancelled\ncancellation_reason: superseded\nspec_deltas:\n  - target: API-EMPTY\n    field: endpoints\n    added: [NeverApplied]\n---\n",
    )
    .unwrap();
    let empty_mem_out = providers
        .run_check("spec.deltas_applied", &json!({"spec": "memory"}), &req)
        .unwrap();
    assert!(
        matches!(empty_mem_out, DomainCheckOutcome::Passed { .. }),
        "empty base-matched memory collection must pass: {empty_mem_out:?}"
    );
    let final_remove_out = providers
        .run_check("spec.deltas_applied", &json!({}), &req)
        .unwrap();
    assert!(
        matches!(final_remove_out, DomainCheckOutcome::Passed { .. }),
        "removing final item of declared collection + skipping cancelled source must pass: {final_remove_out:?}"
    );

    // Undeclared target field (`nonexistent` on `API-EMPTY`) and undeclared `tasks` on
    // `docs/memories/MEM-001.md` (omitting frontmatter `spec:`) must fail closed, while
    // removing the final `### rule:` under declared `field: rules` passes.
    fs::write(
        root.join("docs/milestones/AC-013.md"),
        "---\nid: AC-013\ntitle: Undeclared Field\nstatus: active\nspec_deltas:\n  - target: API-EMPTY\n    field: nonexistent\n    removed: [Ghost]\n---\n",
    )
    .unwrap();
    let DomainCheckOutcome::Failed {
        diagnostics: undeclared_diags,
        ..
    } = providers
        .run_check("spec.deltas_applied", &json!({"spec": "AC-013"}), &req)
        .unwrap()
    else {
        panic!("undeclared field 'nonexistent' on API-EMPTY must fail closed");
    };
    assert!(
        undeclared_diags
            .iter()
            .any(|d| d.contains("target field 'nonexistent' not found")),
        "expected undeclared field diagnostic, got {undeclared_diags:?}"
    );

    fs::create_dir_all(root.join("docs/memories")).unwrap();
    fs::write(
        root.join("docs/memories/MEM-001.md"),
        "---\nid: MEM-001\ntitle: Memory Without Explicit Spec\n---\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-014.md"),
        "---\nid: AC-014\ntitle: Undeclared Tasks On Memory\nstatus: active\nspec_deltas:\n  - target: MEM-001\n    field: tasks\n    removed: [r1]\n---\n",
    )
    .unwrap();
    let DomainCheckOutcome::Failed {
        diagnostics: mem_tasks_diags,
        ..
    } = providers
        .run_check("spec.deltas_applied", &json!({"spec": "AC-014"}), &req)
        .unwrap()
    else {
        panic!("undeclared field 'tasks' on memory spec omitting spec: must fail closed");
    };
    assert!(
        mem_tasks_diags
            .iter()
            .any(|d| d.contains("target field 'tasks' not found")),
        "expected undeclared tasks diagnostic on MEM-001, got {mem_tasks_diags:?}"
    );

    fs::write(
        root.join("docs/milestones/AC-015.md"),
        "---\nid: AC-015\ntitle: Remove Final Rule On Memory\nstatus: active\nspec_deltas:\n  - target: MEM-001\n    field: rules\n    removed: [r1]\n---\n",
    )
    .unwrap();
    let mem_rules_out = providers
        .run_check("spec.deltas_applied", &json!({"spec": "AC-015"}), &req)
        .unwrap();
    assert!(
        matches!(mem_rules_out, DomainCheckOutcome::Passed { .. }),
        "removing final rule on declared memory field 'rules' must pass: {mem_rules_out:?}"
    );

    let _ = fs::remove_file(root.join("docs/milestones/AC-011.md"));
    let _ = fs::remove_file(root.join("docs/milestones/AC-012.md"));
    let _ = fs::remove_file(root.join("docs/milestones/AC-013.md"));
    let _ = fs::remove_file(root.join("docs/milestones/AC-014.md"));
    let _ = fs::remove_file(root.join("docs/milestones/AC-015.md"));
    let _ = fs::remove_file(root.join("docs/api/API-EMPTY.md"));
    let _ = fs::remove_file(root.join("docs/memories/MEM-001.md"));

    let disabled_check_err = index_providers
        .run_check("spec.deltas_applied", &json!({}), &req)
        .expect_err("disabled spec domain must fail closed");
    assert!(disabled_check_err.to_string().contains("disabled"));

    // Symlink inside spec collection fails closed.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            root.join("docs/api/API-WORKER.md"),
            root.join("docs/api/LINKED.md"),
        )
        .unwrap();
        let symlink_outcome = providers
            .run_check("spec.deltas_applied", &json!({}), &req)
            .unwrap();
        assert!(
            matches!(symlink_outcome, DomainCheckOutcome::Failed { .. }),
            "symlink in spec dir must fail closed: {symlink_outcome:?}"
        );
    }

    let mut explicit_disabled = adapter.clone();
    explicit_disabled.modules = SubsystemRegistry {
        disabled: BTreeSet::from(["spec".to_owned()]),
        ..SubsystemRegistry::default()
    };
    let explicit_disabled_providers =
        build_domain_providers(&root, &explicit_disabled, None, Vec::new()).unwrap();
    assert!(!explicit_disabled_providers.is_domain_enabled("spec"));
}
