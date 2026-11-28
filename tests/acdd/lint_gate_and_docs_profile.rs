use super::support::{git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::WorkerId;
use acdd_core::ports::{DomainCheckReq, FreshnessObservation};
use acdd_state::types::{NodeStatus, ReviewContourResult, ReviewEvidence, RunStatus, SubmitAction};
use acdd_state::usecases::dto::{ClaimReq, StartReq, SubmitReq, VerifyReq};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use contextunity_acdd::format::render_claim_markdown;
use contextunity_acdd::state_bridge::build_domain_providers;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;

fn init_lint_repo(ws: &Workspace, branch: &str) {
    init_git_repo_on_branch(ws.root(), branch);
    let _ = git_cmd(ws.root(), &["config", "commit.gpgsign", "false"]);
}

fn open_real_engine(ws: &Workspace) -> StateEngine {
    let adapter = acdd_core::config::load_adapter(ws.root(), None).unwrap();
    let providers = build_domain_providers(ws.root(), &adapter, None, Vec::new()).unwrap();
    let cfg = acdd_state::usecases::StateEngineConfig {
        workspace_root: ws.root().to_path_buf(),
        worktree: ws.root().to_path_buf(),
        state_db_override: None,
        code_db_path: Some(acdd_core::config::resolve_code_db_path(ws.root(), None)),
        spec_collections: adapter.specs.clone(),
        workflows_dirs: adapter.workflows_dirs.clone(),
        linked_workspaces: adapter
            .linked_workspaces
            .iter()
            .map(|lw| (lw.name.clone(), lw.path.clone()))
            .collect(),
        modules: adapter.modules.clone(),
        response_policy: adapter.response.clone(),
        retain_traces: adapter.retain_traces,
        max_snapshot_file_bytes: adapter.limits.max_snapshot_file_bytes,
        allowed_binary_extensions: adapter
            .limits
            .normalized_allowed_binary_extensions()
            .unwrap(),
    };
    StateEngine::open(cfg, providers).unwrap()
}

fn claim_node(
    engine: &StateEngine,
    rid: &str,
    node: &str,
    worker: &WorkerId,
) -> acdd_state::context::ClaimResp {
    let StateCommandOutput::Claim(resp) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.to_owned()),
            node: Some(node.to_owned()),
            worker_id: worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Default::default(),
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    *resp
}

fn pass_review_evidence(contours: &[&str], summary: &str) -> ReviewEvidence {
    let map: BTreeMap<String, ReviewContourResult> = contours
        .iter()
        .map(|c| {
            (
                (*c).to_owned(),
                ReviewContourResult {
                    applicable: true,
                    evidence: summary.to_owned(),
                },
            )
        })
        .collect();
    ReviewEvidence {
        decision: SubmitAction::Pass,
        contours: map,
        summary: summary.to_owned(),
        notes: None,
    }
}

#[test]
fn acdd_lint_gate_blocking_scoping_freshness_enforced_and_docs_profile() {
    // 1. Capability registration & schema validation.
    let test_provider = TestProvider::full_composite();
    test_provider
        .validate_check_ref("code.lint", &json!({"categories": ["arch", "docs"]}))
        .expect("valid code.lint params pass");
    let err = test_provider
        .validate_check_ref("code.lint", &json!({"categories": ["invalid"]}))
        .expect_err("invalid category must fail closed");
    assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid);

    // 2. Host CodeDomainProvider + DocsDomainProvider: task scoping, gate blocking, [ENFORCED], and freshness drift.
    let ws = Workspace::with_files(&[
        (
            ".gitignore",
            ".acdd/code-map.sqlite*\n.acdd/state.sqlite*\n.acdd/extraction-cache/\n",
        ),
        (
            "acdd.yaml",
            r#"roots: [src, docs]
specs:
  milestones:
    path: docs/milestones
"#,
        ),
        (
            ".acdd/lint/zones.yaml",
            r#"zones:
  core: ["src/core/**"]
  infra: ["src/infra/**"]
"#,
        ),
        (
            ".acdd/workflows/default.yaml",
            r#"version: 2
inherit: none
git:
  isolation: in_place
  worktrees: manual
save: none
contours:
  claims:
    description: "Verify claims"
    criteria: ["Verify claims"]
roles:
  builder: {}
  independent_reviewer:
    reuse_on_reject: false
commands:
  ok_cmd:
    command: "printf 'test result: ok. 1 passed; 0 failed\n'"
gates:
  - tasks:
      gates:
        - id: build
          role: builder
          proof: command
          command: ok_cmd
          checks:
            - check: code.lint
              with:
                categories: [arch, syntax, docs]
          context: [docs.invariants]
          save: snapshot
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          independent_from: [build]
          need: [build]
          contours: [claims]
          pass: deliver
        - id: deliver
          need: [build, review]
          receipt: true
  - id: verify_all
    checks:
      - check: code.lint
        with:
          categories: [arch, docs]
    pass: handoff
  - id: handoff
    need: [tasks.deliver, verify_all]
    receipt: true
"#,
        ),
        (
            "docs/architecture/core-rule.md",
            r#"---
title: Core Isolation
doc_type: architecture
---
# Core Isolation

> - INV-TEST-CORE: `core` must not depend on `infra`.

```acdd-rule
id: INV-TEST-CORE
kind: forbid_dependency
severity: error
from:
  zones: [core]
to:
  zones: [infra]
message: "core must not import infra"
```
"#,
        ),
        ("src/infra/db.rs", "pub fn db() {}\n"),
        (
            "src/core/scoped.rs",
            "use crate::infra::db::db;\npub fn bad() { db(); }\n",
        ),
        (
            "src/core/other.rs",
            "use crate::infra::db::db;\npub fn out_of_scope_bad() { db(); }\n",
        ),
        (
            "docs/milestones/AR-901-lint-gate.md",
            r#"---
id: AR-901
title: Lint Gate Test
status: active
---
# Lint Gate Test

### task: t1
```yaml
task_ref: t1
target: Implement scoped core fn
scope: [src/core/scoped.rs]
```
"#,
        ),
    ]);
    init_lint_repo(&ws, "main");
    ws.build();

    let adapter = acdd_core::config::load_adapter(ws.root(), None).unwrap();
    let real_providers = build_domain_providers(ws.root(), &adapter, None, Vec::new()).unwrap();
    real_providers
        .validate_check_ref("code.lint", &json!({"categories": ["arch", "docs"]}))
        .expect("real CodeDomainProvider registers code.lint");

    let engine = open_real_engine(&ws);
    let StateCommandOutput::Start(started) = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AR-901-lint-gate.md".to_owned()),
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let rid = started.run.run_id.to_string();
    let w_build = WorkerId::new("b-1").unwrap();
    let w_rev = WorkerId::new("r-1").unwrap();

    // Claim t1.build -> DocsDomainProvider stamps enforced: true and ClaimResp renders [ENFORCED].
    let c_build = claim_node(&engine, &rid, "t1.build", &w_build);
    let inv_items = c_build.context_lenses["docs.invariants"]["items"]
        .as_array()
        .expect("docs.invariants items");
    let core_inv = inv_items
        .iter()
        .find(|i| i["id"] == "INV-TEST-CORE")
        .expect("INV-TEST-CORE in docs.invariants");
    assert_eq!(core_inv["enforced"], true);
    let rendered_claim = render_claim_markdown(&c_build);
    assert!(
        rendered_claim.contains("INV-TEST-CORE` [ENFORCED]"),
        "expected [ENFORCED] badge in ClaimResp markdown:\n{rendered_claim}"
    );

    // Verify fails while scoped file violates INV-TEST-CORE; t1.review remains blocked.
    let verify_fail = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build.claim_revision),
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(verify_fail.code, ErrorCode::GateVerificationFailed);
    let _rev_blocked = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: Default::default(),
        }))
        .unwrap_err();

    // Fix ONLY scoped.rs (leaving out-of-scope other.rs violating) -> task gate Verify passes!
    ws.write("src/core/scoped.rs", "pub fn good() -> i32 { 1 }\n");
    engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build.claim_revision),
            worktree: None,
        }))
        .unwrap();

    // Freshness observation is strictly read-only (does not mutate SQLite bytes).
    let db_bytes_before = fs::read(ws.db()).unwrap();
    let check_req = DomainCheckReq {
        run_id: started.run.run_id.clone(),
        node_cursor: c_build.node_cursor.clone(),
        gate_id: acdd_core::ids::GateId::new("build").unwrap(),
        workspace_root: ws.root().to_path_buf(),
        worktree: ws.root().to_path_buf(),
        admitted_scope: vec!["src/core/scoped.rs".to_owned()],
        changed_paths: Vec::new(),
        test_target: None,
        path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
        baseline_ref: None,
        node_tags: vec![],
    };
    let FreshnessObservation::Passed {
        index_generation: Some(_),
        execution_digest_salt: Some(baseline_salt),
    } = real_providers
        .observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "syntax", "docs"]}),
            &check_req,
        )
        .unwrap()
    else {
        panic!("expected FreshnessObservation::Passed with index_generation and salt");
    };
    assert_eq!(fs::read(ws.db()).unwrap(), db_bytes_before);

    // Linked-worktree freshness isolation (never falls back to primary ws.db()) + gitignored docs excluded from digest.
    let child_wt = ws.root().join(".worktrees/isolated-child");
    fs::create_dir_all(child_wt.join("src/core")).unwrap();
    fs::write(
        child_wt.join("src/core/scoped.rs"),
        "pub fn good() -> i32 { 1 }\n",
    )
    .unwrap();
    let FreshnessObservation::Passed {
        index_generation: Some(child_gen),
        ..
    } = real_providers
        .observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "syntax", "docs"]}),
            &DomainCheckReq {
                worktree: child_wt,
                ..check_req.clone()
            },
        )
        .unwrap()
    else {
        panic!("expected child worktree FreshnessObservation::Passed");
    };
    assert_eq!(child_gen, "no-index");

    let orig_gitignore = fs::read_to_string(ws.path(".gitignore")).unwrap();
    ws.write(
        ".gitignore",
        ".acdd/code-map.sqlite*\n.acdd/state.sqlite*\n.acdd/extraction-cache/\nignored_docs/\n",
    );
    ws.write("ignored_docs/scratch.md", "# Ignored\n");
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(with_ignored_salt),
        ..
    } = real_providers
        .observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "syntax", "docs"]}),
            &check_req,
        )
        .unwrap()
    else {
        panic!("expected FreshnessObservation::Passed with gitignored doc");
    };
    assert_eq!(with_ignored_salt, baseline_salt);
    ws.write(".gitignore", &orig_gitignore);
    fs::remove_dir_all(ws.path("ignored_docs")).unwrap();

    // Task-scoped source mutation after Verify drifts observe_freshness read-only and fails Submit closed!
    let orig_src = fs::read_to_string(ws.path("src/core/scoped.rs")).unwrap();
    ws.write(
        "src/core/scoped.rs",
        "use crate::infra::db::db;\npub fn good() { db(); }\n",
    );
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(drifted_src_salt),
        ..
    } = real_providers
        .observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "syntax", "docs"]}),
            &check_req,
        )
        .unwrap()
    else {
        panic!("expected FreshnessObservation::Passed for source mutation");
    };
    assert_ne!(drifted_src_salt, baseline_salt);
    assert_eq!(fs::read(ws.db()).unwrap(), db_bytes_before);
    let submit_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_build.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap_err();
    assert_eq!(submit_err.code, ErrorCode::TaskProofUnobserved);
    ws.write("src/core/scoped.rs", &orig_src);
    engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build.claim_revision),
            worktree: None,
        }))
        .unwrap();

    // Submit t1.build -> t1.review -> t1.deliver.
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_build.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();
    let c_rev = claim_node(&engine, &rid, "t1.review", &w_rev);
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev.clone(),
            claim_revision: Some(c_rev.claim_revision),
            test_target: None,
            review_evidence: Some(pass_review_evidence(&["claims"], "clean")),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();
    let c_del = claim_node(&engine, &rid, "t1.deliver", &w_rev);
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.deliver".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev.clone(),
            claim_revision: Some(c_del.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // Root gate verify_all runs workspace-wide -> catches src/core/other.rs until fixed!
    let c_verify = claim_node(&engine, &rid, "verify_all", &w_rev);
    let root_err = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "verify_all".to_owned(),
            worker_id: Some(w_rev.clone()),
            test_target: None,
            claim_revision: Some(c_verify.claim_revision),
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(root_err.code, ErrorCode::GateVerificationFailed);

    ws.write("src/core/other.rs", "pub fn fixed() -> i32 { 2 }\n");
    engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "verify_all".to_owned(),
            worker_id: Some(w_rev.clone()),
            test_target: None,
            claim_revision: Some(c_verify.claim_revision),
            worktree: None,
        }))
        .unwrap();

    // Root-gate freshness drift matrix (doc + valid zone-breaking acdd.yaml + malformed acdd.yaml):
    // zero OutOfScopeChanges; observe_freshness drifts/fails closed and Submit rejects closed.
    let root_req = DomainCheckReq {
        node_cursor: "verify_all".parse().unwrap(),
        gate_id: acdd_core::ids::GateId::new("verify_all").unwrap(),
        admitted_scope: Vec::new(),
        ..check_req.clone()
    };
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(root_salt),
        ..
    } = real_providers
        .observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "docs"]}),
            &root_req,
        )
        .unwrap()
    else {
        panic!("expected root FreshnessObservation::Passed");
    };

    let root_drift_cases: &[(&str, &str, &str, bool)] = &[
        (
            "doc mutation",
            "docs/architecture/core-rule.md",
            "---\ntitle: Core Isolation\ndoc_type: architecture\n---\n# Core Isolation\n\n```acdd-rule\ninvalid_yaml: [\n```\n",
            false,
        ),
        (
            "valid lint zone removal",
            ".acdd/lint/zones.yaml",
            "zones:\n  core: [\"src/core/**\"]\n",
            false,
        ),
        (
            "malformed acdd.yaml fail-closed",
            "acdd.yaml",
            "roots: [src, docs\n",
            true,
        ),
    ];
    for &(label, rel, mutated, expect_obs_err) in root_drift_cases {
        let orig = fs::read_to_string(ws.path(rel)).unwrap();
        ws.write(rel, mutated);
        let db_before_obs = fs::read(ws.db()).unwrap();
        let obs = real_providers.observe_freshness(
            "code.lint",
            &json!({"categories": ["arch", "docs"]}),
            &root_req,
        );
        if expect_obs_err {
            assert!(
                obs.is_err(),
                "broken {label} must fail closed, not fall back"
            );
        } else {
            let FreshnessObservation::Passed {
                execution_digest_salt: Some(drifted),
                ..
            } = obs.unwrap()
            else {
                panic!("expected FreshnessObservation::Passed for {label}");
            };
            assert_ne!(drifted, root_salt, "expected salt drift for {label}");
        }
        assert_eq!(fs::read(ws.db()).unwrap(), db_before_obs, "{label}");

        let submit_err = engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "verify_all".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_rev.clone(),
                claim_revision: Some(c_verify.claim_revision),
                test_target: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                summary: None,
                allow_main: false,
                worktree: None,
            })))
            .unwrap_err();
        let _ = expect_obs_err;
        assert_eq!(
            submit_err.code,
            ErrorCode::TaskProofUnobserved,
            "expected fail-closed error (not OutOfScopeChanges) for {label}"
        );

        ws.write(rel, &orig);
        engine
            .execute(StateCommand::Verify(VerifyReq {
                run: Some(rid.clone()),
                node: "verify_all".to_owned(),
                worker_id: Some(w_rev.clone()),
                test_target: None,
                claim_revision: Some(c_verify.claim_revision),
                worktree: None,
            }))
            .unwrap();
    }

    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "verify_all".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev.clone(),
            claim_revision: Some(c_verify.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // 3. End-to-end `--workflow documentation` run using real CodeDomainProvider + DocsDomainProvider.
    let doc_ws = Workspace::with_files(&[
        (
            ".gitignore",
            ".acdd/code-map.sqlite*\n.acdd/state.sqlite*\n.acdd/extraction-cache/\n",
        ),
        (
            "acdd.yaml",
            "roots: [src, docs]\nspecs:\n  milestones:\n    path: docs/milestones\n",
        ),
        ("src/lib.rs", "pub fn helper() -> i32 { 1 }\n"),
        (
            "docs/architecture/guide.md",
            "---\ntitle: Docs Profile Fixture\ndoc_type: architecture\n---\n# Guide\n\n> - INV-DOC-CLEAN: `src/lib.rs` exports [`base`](../../src/lib.rs).\n",
        ),
        (
            "docs/milestones/AR-902-docs-run.md",
            r#"---
id: AR-902
title: Documentation Profile E2E
status: active
---
# Documentation Profile E2E

### task: d1
```yaml
task_ref: d1
target: Update architecture doc
scope: [docs/architecture/guide.md]
```
"#,
        ),
    ]);
    init_lint_repo(&doc_ws, "docs/run-ar902");

    let doc_engine = open_real_engine(&doc_ws);
    let StateCommandOutput::Start(doc_started) = doc_engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AR-902-docs-run.md".to_owned()),
            workflow: Some("documentation".to_owned()),
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp for documentation profile");
    };
    let doc_rid = doc_started.run.run_id.to_string();
    assert_eq!(doc_started.run.profile_id, "documentation");

    let arch = WorkerId::new("doc-arch").unwrap();
    let doc_b = WorkerId::new("doc-builder").unwrap();
    let doc_r = WorkerId::new("doc-reviewer").unwrap();

    let c_syn = claim_node(
        &doc_engine,
        &doc_rid,
        "blueprint_synthesis.synthesize",
        &arch,
    );
    let blueprint_proof: Value = json!({
        "cited_invariants": ["INV-DOC-CLEAN"],
        "cited_symbols": ["src/lib.rs::base"],
        "global_negative_boundaries": ["Do not mutate src/"],
        "blackboard_decision_id": "1",
        "tasks": {
            "d1": {
                "layer": "docs",
                "summary": "Update architecture doc",
                "exported_interfaces": ["docs/architecture/guide.md"],
                "negative_boundaries": ["Do not touch src/"],
                "incoming_seams": [],
                "outgoing_seams": []
            }
        }
    });
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "blueprint_synthesis.synthesize".to_owned(),
            action: SubmitAction::Pass,
            worker_id: arch,
            claim_revision: Some(c_syn.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: Some(blueprint_proof),
            verified_invariants: Vec::new(),
            summary: Some("blueprint ready".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_bp_rev = claim_node(&doc_engine, &doc_rid, "blueprint_synthesis.review", &doc_r);
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "blueprint_synthesis.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r.clone(),
            claim_revision: Some(c_bp_rev.claim_revision),
            test_target: None,
            review_evidence: Some(pass_review_evidence(
                &["paths", "claims", "architecture", "administration"],
                "blueprint approved",
            )),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_d1 = claim_node(&doc_engine, &doc_rid, "d1.build", &doc_b);
    doc_engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(doc_rid.clone()),
            node: "d1.build".to_owned(),
            worker_id: Some(doc_b.clone()),
            test_target: None,
            claim_revision: Some(c_d1.claim_revision),
            worktree: None,
        }))
        .unwrap();
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "d1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_b.clone(),
            claim_revision: Some(c_d1.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_d1_rev = claim_node(&doc_engine, &doc_rid, "d1.review", &doc_r);
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "d1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r.clone(),
            claim_revision: Some(c_d1_rev.claim_revision),
            test_target: None,
            review_evidence: Some(pass_review_evidence(
                &["paths", "claims", "architecture", "administration"],
                "d1 verified",
            )),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_d1_del = claim_node(&doc_engine, &doc_rid, "d1.deliver", &doc_r);
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "d1.deliver".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r.clone(),
            claim_revision: Some(c_d1_del.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // `verify_docs` runs `docs.links` + `code.lint` ([arch, docs]).
    let c_vdocs = claim_node(&doc_engine, &doc_rid, "verify_docs", &doc_r);
    doc_engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(doc_rid.clone()),
            node: "verify_docs".to_owned(),
            worker_id: Some(doc_r.clone()),
            test_target: None,
            claim_revision: Some(c_vdocs.claim_revision),
            worktree: None,
        }))
        .unwrap();
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "verify_docs".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r.clone(),
            claim_revision: Some(c_vdocs.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_int = claim_node(&doc_engine, &doc_rid, "integration_review", &doc_r);
    doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid.clone()),
            node: "integration_review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r.clone(),
            claim_revision: Some(c_int.claim_revision),
            test_target: None,
            review_evidence: Some(pass_review_evidence(
                &["paths", "claims", "architecture", "administration"],
                "integration docs clean",
            )),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    let c_hand = claim_node(&doc_engine, &doc_rid, "handoff", &doc_r);
    let StateCommandOutput::Submit(hand_resp) = doc_engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(doc_rid),
            node: "handoff".to_owned(),
            action: SubmitAction::Pass,
            worker_id: doc_r,
            claim_revision: Some(c_hand.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("docs profile handoff".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert_eq!(hand_resp.new_node_status, NodeStatus::Completed);
    assert_eq!(hand_resp.run_status, RunStatus::Completed);

    // Consolidated diff-duplicates, baseline callable identity, real indexed impl trait resolution,
    // graph-ranked reuse_hint, and pre-truncation batched cochange matrix.
    {
        use acdd_core::ports::DomainCheckOutcome;
        use acdd_index::db::analyze::collect_untouched_cochange_siblings;
        use acdd_index::db::reader;

        let ws = Workspace::new();
        let root = ws.root();
        fs::create_dir_all(root.join("src")).unwrap();

        let base_owner = "\
pub fn shared_target(v: i32) -> i32 { v + 1 }
pub fn shared_helper(v: i32) -> i32 { v * 2 }
pub fn owner_fn(x: i32) -> i32 {
    let a = x + 10;
    let b = a * 20;
    let c = b - 30;
    let d = c + 40;
    let e = d * 50;
    let f = e - 60;
    shared_target(f) + shared_helper(f)
}
pub fn owner_caller_one(x: i32) -> i32 { owner_fn(x) + 1 }
pub fn owner_caller_two(x: i32) -> i32 { owner_fn(x) + 2 }
pub fn pre_existing_clone(x: i32) -> i32 {
    let a = x + 10;
    let b = a * 20;
    let c = b - 30;
    let d = c + 40;
    let e = d * 50;
    let f = e - 60;
    shared_target(f) + shared_helper(f)
}
pub struct Original;
impl Original {
    pub fn same_name(&self, x: i32) -> i32 {
        let p = x + 11;
        let q = p * 22;
        let r = q - 33;
        let s = r + 44;
        let t = s * 55;
        t - 66
    }
}
pub struct Alpha(pub u32);
impl From<u32> for Alpha {
    fn from(v: u32) -> Self {
        let a = v.wrapping_add(1);
        let b = a.wrapping_mul(2);
        let c = b.wrapping_add(3);
        let d = c.wrapping_mul(4);
        let e = d.wrapping_add(5);
        Self(e)
    }
}
pub trait Display {
    fn render(&self, x: i32) -> i32;
}
pub struct CustomA;
impl Display for CustomA {
    fn render(&self, x: i32) -> i32 {
        let a = x + 10;
        let b = a * 2;
        let c = b + 3;
        let d = c * 4;
        let e = d + 5;
        e
    }
}
pub struct OwnA;
impl OwnA {
    pub fn from(v: u32) -> u32 {
        let a = v.wrapping_add(10);
        let b = a.wrapping_mul(20);
        let c = b.wrapping_add(30);
        let d = c.wrapping_mul(40);
        let e = d.wrapping_add(50);
        e
    }
}
pub fn free_from_a(v: u32) -> u32 {
    let a = v.wrapping_add(100);
    let b = a.wrapping_mul(200);
    let c = b.wrapping_add(300);
    let d = c.wrapping_mul(400);
    let e = d.wrapping_add(500);
    e
}
";
        fs::write(root.join("src/baseline.rs"), base_owner).unwrap();
        // 9 changed siblings + 4 untouched disallowed siblings each share 2 callees
        // (`shared_target` + `shared_helper`) with baseline.rs, strictly outranking
        // `eligible_ninth.rs` (1 shared callee) if truncation happened before filtering.
        for i in 0..9 {
            fs::write(
                root.join(format!("src/changed_{i}.rs")),
                format!(
                    "use crate::baseline::{{shared_helper, shared_target}};\npub fn c_{i}(x: i32) -> i32 {{ shared_target(x + {i}) + shared_helper(x) }}\n"
                ),
            )
            .unwrap();
        }
        for i in 0..4 {
            fs::write(
                root.join(format!("src/disallowed_{i}.rs")),
                format!(
                    "use crate::baseline::{{shared_helper, shared_target}};\npub fn d_{i}(x: i32) -> i32 {{ shared_target(x + {i}) + shared_helper(x) }}\n"
                ),
            )
            .unwrap();
        }
        fs::write(
            root.join("src/eligible_ninth.rs"),
            "use crate::baseline::shared_target;\npub fn ninth(x: i32) -> i32 { shared_target(x + 99) }\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let mut lib_rs = String::from("pub mod baseline;\npub mod eligible_ninth;\n");
        for i in 0..9 {
            lib_rs.push_str(&format!("pub mod changed_{i};\n"));
        }
        for i in 0..4 {
            lib_rs.push_str(&format!("pub mod disallowed_{i};\n"));
        }
        fs::write(root.join("src/lib.rs"), lib_rs).unwrap();

        init_lint_repo(&ws, "main");
        ws.build();
        let adapter = acdd_core::config::Adapter::default();
        let db_path = root.join(".acdd/code-map.sqlite");
        let providers = build_domain_providers(root, &adapter, Some(&db_path), Vec::new()).unwrap();

        // 1. Comment + line-shift edit on baseline.rs: pre-existing exact clones & Original::same_name stay Warn (Passed),
        //    AND higher-count changed_0..8 and exposure-disallowed disallowed_0..3 (2 shared callees each)
        //    must NEVER leak and must NOT crowd out eligible_ninth.rs (1 shared callee).
        fs::write(
            root.join("src/baseline.rs"),
            format!("// leading line shift comment\n// second comment line\n{base_owner}"),
        )
        .unwrap();
        let mut many_changed: Vec<String> = (0..9).map(|i| format!("src/changed_{i}.rs")).collect();
        many_changed.push("src/baseline.rs".to_owned());
        let disallowed_paths: Vec<String> =
            (0..4).map(|i| format!("src/disallowed_{i}.rs")).collect();
        let exposure_policy = acdd_core::path_policy::WorkspacePathPolicy::from_workspace_config(
            ".acdd/state.sqlite",
            ".acdd/code-map.sqlite",
            disallowed_paths.clone(),
        );
        let check_req = DomainCheckReq {
            run_id: "xray-check".parse().unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/baseline.rs".to_owned()],
            changed_paths: many_changed.clone(),
            test_target: None,
            path_policy: exposure_policy.clone(),
            baseline_ref: Some("HEAD".to_owned()),
            node_tags: vec![],
        };
        let outcome = providers
            .run_check("code.lint", &json!({ "categories": ["duplicates"] }), &check_req)
            .unwrap();
        match outcome {
            DomainCheckOutcome::Passed { diagnostics, .. } => {
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.starts_with("[xray.cochange]") && d.contains("src/eligible_ninth.rs")),
                    "expected [xray.cochange] for eligible_ninth.rs in passing diagnostics: {diagnostics:?}"
                );
                assert!(
                    diagnostics
                        .iter()
                        .all(|d| !disallowed_paths.iter().any(|p| d.contains(p))),
                    "exposure-disallowed siblings must never leak into [xray.cochange]: {diagnostics:?}"
                );
            }
            other => panic!("comment/line-shift edit must pass: {other:?}"),
        }
        let conn = reader::open_locked(&db_path, root).unwrap();
        let siblings = collect_untouched_cochange_siblings(
            &conn,
            root,
            &many_changed,
            Some(&exposure_policy),
            3,
        )
        .unwrap();
        assert!(
            siblings.iter().any(|(_, sib, _)| sib == "src/eligible_ninth.rs"),
            "higher-count changed/disallowed siblings must not suppress eligible_ninth.rs: {siblings:?}"
        );
        assert!(
            siblings
                .iter()
                .all(|(_, sib, _)| !disallowed_paths.contains(sib)),
            "exposure-disallowed siblings must not leak from collect_untouched_cochange_siblings: {siblings:?}"
        );
        drop(conn);

        // 2. Canonical callable structural identity: adding Introduced::same_name in the same file
        //    as Original::same_name MUST promote to duplicates.exact Error.
        fs::write(
            root.join("src/baseline.rs"),
            format!(
                "{base_owner}\npub struct Introduced;\nimpl Introduced {{\n    pub fn same_name(&self, x: i32) -> i32 {{\n        let p = x + 11;\n        let q = p * 22;\n        let r = q - 33;\n        let s = r + 44;\n        let t = s * 55;\n        t - 66\n    }}\n}}\n"
            ),
        )
        .unwrap();
        let same_name_outcome = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/baseline.rs".to_owned()],
                    changed_paths: vec!["src/baseline.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(
                same_name_outcome,
                DomainCheckOutcome::Failed { ref diagnostics, .. }
                    if diagnostics.iter().any(|d| d.contains("duplicates.exact"))
            ),
            "Introduced::same_name in same file as Original::same_name must fail with duplicates.exact: {same_name_outcome:?}"
        );
        fs::write(root.join("src/baseline.rs"), base_owner).unwrap();

        // 3. Real indexed impl trait resolution matrix:
        //    (a) std `impl From<u32> for Beta` (<=6 stmts, different literals) -> Passed (exempted!)
        //    (b) local custom `impl Display for CustomB` -> Failed (`duplicates.similar`, NOT exempted!)
        //    (c) inherent `impl OwnB::from` -> Failed (`duplicates.similar`, NOT exempted!)
        //    (d) free function `free_from_b` -> Failed (`duplicates.similar`, NOT exempted!)
        for (rel_path, code, expect_pass) in [
            (
                "src/std_from.rs",
                "pub struct Beta(pub u32);\nimpl From<u32> for Beta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(91);\n        let b = a.wrapping_mul(92);\n        let c = b.wrapping_add(93);\n        let d = c.wrapping_mul(94);\n        let e = d.wrapping_add(95);\n        Self(e)\n    }\n}\n",
                true,
            ),
            (
                "src/custom_display.rs",
                "use crate::baseline::Display;\npub struct CustomB;\nimpl Display for CustomB {\n    fn render(&self, x: i32) -> i32 {\n        let a = x + 71;\n        let b = a * 72;\n        let c = b + 73;\n        let d = c * 74;\n        let e = d + 75;\n        e\n    }\n}\n",
                false,
            ),
            (
                "src/inherent_from.rs",
                "pub struct OwnB;\nimpl OwnB {\n    pub fn from(v: u32) -> u32 {\n        let a = v.wrapping_add(61);\n        let b = a.wrapping_mul(62);\n        let c = b.wrapping_add(63);\n        let d = c.wrapping_mul(64);\n        let e = d.wrapping_add(65);\n        e\n    }\n}\n",
                false,
            ),
            (
                "src/free_from.rs",
                "pub fn free_from_b(v: u32) -> u32 {\n    let a = v.wrapping_add(81);\n    let b = a.wrapping_mul(82);\n    let c = b.wrapping_add(83);\n    let d = c.wrapping_mul(84);\n    let e = d.wrapping_add(85);\n    e\n}\n",
                false,
            ),
            (
                "src/macro_local_from.rs",
                "macro_rules! define_local_from {\n    () => {\n        pub trait From<T> {\n            fn from(v: T) -> Self;\n        }\n    };\n}\ndefine_local_from!();\npub struct MacroAlpha(pub u32);\nimpl From<u32> for MacroAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(11);\n        let b = a.wrapping_mul(12);\n        let c = b.wrapping_add(13);\n        let d = c.wrapping_mul(14);\n        let e = d.wrapping_add(15);\n        Self(e)\n    }\n}\npub struct MacroBeta(pub u32);\nimpl From<u32> for MacroBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(21);\n        let b = a.wrapping_mul(22);\n        let c = b.wrapping_add(23);\n        let d = c.wrapping_mul(24);\n        let e = d.wrapping_add(25);\n        Self(e)\n    }\n}\n",
                false,
            ),
            (
                "src/local_from_control.rs",
                "pub trait LocalFrom<T> {\n    fn from(v: T) -> Self;\n}\npub struct LocalAlpha(pub u32);\nimpl LocalFrom<u32> for LocalAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(31);\n        let b = a.wrapping_mul(32);\n        let c = b.wrapping_add(33);\n        let d = c.wrapping_mul(34);\n        let e = d.wrapping_add(35);\n        Self(e)\n    }\n}\npub struct LocalBeta(pub u32);\nimpl LocalFrom<u32> for LocalBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(41);\n        let b = a.wrapping_mul(42);\n        let c = b.wrapping_add(43);\n        let d = c.wrapping_mul(44);\n        let e = d.wrapping_add(45);\n        Self(e)\n    }\n}\n",
                false,
            ),
            (
                "src/macro_local_qualified_std_from.rs",
                "#![no_std]\nmacro_rules! local_std {\n    () => {\n        pub mod std {\n            pub mod convert {\n                pub trait From<T> {\n                    fn from(v: T) -> Self;\n                }\n            }\n        }\n    };\n}\nlocal_std!();\npub struct QualMacroAlpha(pub u32);\nimpl std::convert::From<u32> for QualMacroAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(11);\n        let b = a.wrapping_mul(12);\n        let c = b.wrapping_add(13);\n        let d = c.wrapping_mul(14);\n        let e = d.wrapping_add(15);\n        Self(e)\n    }\n}\npub struct QualMacroBeta(pub u32);\nimpl std::convert::From<u32> for QualMacroBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(21);\n        let b = a.wrapping_mul(22);\n        let c = b.wrapping_add(23);\n        let d = c.wrapping_mul(24);\n        let e = d.wrapping_add(25);\n        Self(e)\n    }\n}\n",
                false,
            ),
            (
                "src/std_qualified_from.rs",
                "pub struct QualStdAlpha(pub u32);\nimpl std::convert::From<u32> for QualStdAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(51);\n        let b = a.wrapping_mul(52);\n        let c = b.wrapping_add(53);\n        let d = c.wrapping_mul(54);\n        let e = d.wrapping_add(55);\n        Self(e)\n    }\n}\npub struct QualStdBeta(pub u32);\nimpl std::convert::From<u32> for QualStdBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(61);\n        let b = a.wrapping_mul(62);\n        let c = b.wrapping_add(63);\n        let d = c.wrapping_mul(64);\n        let e = d.wrapping_add(65);\n        Self(e)\n    }\n}\n",
                true,
            ),
            (
                "src/local_mod_std_from.rs",
                "#![no_std]\npub mod std {\n    pub mod convert {\n        pub trait From<T> {\n            fn from(v: T) -> Self;\n        }\n    }\n}\npub struct LocalModAlpha(pub u32);\nimpl std::convert::From<u32> for LocalModAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(71);\n        let b = a.wrapping_mul(72);\n        let c = b.wrapping_add(73);\n        let d = c.wrapping_mul(74);\n        let e = d.wrapping_add(75);\n        Self(e)\n    }\n}\npub struct LocalModBeta(pub u32);\nimpl std::convert::From<u32> for LocalModBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(81);\n        let b = a.wrapping_mul(82);\n        let c = b.wrapping_add(83);\n        let d = c.wrapping_mul(84);\n        let e = d.wrapping_add(85);\n        Self(e)\n    }\n}\n",
                false,
            ),
            (
                "src/abs_std_qualified_from.rs",
                "pub struct AbsStdAlpha(pub u32);\nimpl ::std::convert::From<u32> for AbsStdAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(91);\n        let b = a.wrapping_mul(92);\n        let c = b.wrapping_add(93);\n        let d = c.wrapping_mul(94);\n        let e = d.wrapping_add(95);\n        Self(e)\n    }\n}\npub struct AbsStdBeta(pub u32);\nimpl ::std::convert::From<u32> for AbsStdBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(101);\n        let b = a.wrapping_mul(102);\n        let c = b.wrapping_add(103);\n        let d = c.wrapping_mul(104);\n        let e = d.wrapping_add(105);\n        Self(e)\n    }\n}\n",
                true,
            ),
            (
                "src/abs_core_qualified_from.rs",
                "pub struct AbsCoreAlpha(pub u32);\nimpl ::core::convert::From<u32> for AbsCoreAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(111);\n        let b = a.wrapping_mul(112);\n        let c = b.wrapping_add(113);\n        let d = c.wrapping_mul(114);\n        let e = d.wrapping_add(115);\n        Self(e)\n    }\n}\npub struct AbsCoreBeta(pub u32);\nimpl ::core::convert::From<u32> for AbsCoreBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(121);\n        let b = a.wrapping_mul(122);\n        let c = b.wrapping_add(123);\n        let d = c.wrapping_mul(124);\n        let e = d.wrapping_add(125);\n        Self(e)\n    }\n}\n",
                true,
            ),
            (
                "src/abs_alias_std_from.rs",
                "#![no_std]\nextern crate self as std;\npub mod convert { pub trait From<T> { fn from(v: T) -> Self; } }\npub struct AbsAliasAlpha(pub u32);\nimpl ::std::convert::From<u32> for AbsAliasAlpha {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(131);\n        let b = a.wrapping_mul(132);\n        let c = b.wrapping_add(133);\n        let d = c.wrapping_mul(134);\n        let e = d.wrapping_add(135);\n        Self(e)\n    }\n}\npub struct AbsAliasBeta(pub u32);\nimpl ::std::convert::From<u32> for AbsAliasBeta {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(141);\n        let b = a.wrapping_mul(142);\n        let c = b.wrapping_add(143);\n        let d = c.wrapping_mul(144);\n        let e = d.wrapping_add(145);\n        Self(e)\n    }\n}\n",
                false,
            ),
        ] {
            fs::write(root.join(rel_path), code).unwrap();
            let res = providers
                .run_check(
                    "code.lint",
                    &json!({ "categories": ["duplicates"] }),
                    &DomainCheckReq {
                        admitted_scope: vec![rel_path.to_owned()],
                        changed_paths: vec![rel_path.to_owned()],
                        ..check_req.clone()
                    },
                )
                .unwrap();
            if expect_pass {
                assert!(
                    matches!(res, DomainCheckOutcome::Passed { .. }),
                    "expected recognized std trait mapper in {rel_path} to pass, got {res:?}"
                );
            } else {
                assert!(
                    matches!(
                        res,
                        DomainCheckOutcome::Failed { ref diagnostics, .. }
                            if diagnostics.iter().any(|d| d.contains("duplicates.similar"))
                    ),
                    "expected non-std/custom/inherent/free clone in {rel_path} to fail duplicates.similar, got {res:?}"
                );
            }
            fs::remove_file(root.join(rel_path)).unwrap();
        }

        // 3b. Cross-file trait-name isolation & crate-root extern crate alias shadowing (#716):
        //     (a) Unrelated `pub trait Display` in `src/local_fmt.rs` must NOT block `::std::fmt::Display` in `src/std_display.rs`.
        //     (b) Crate-root `extern crate self as std;` + re-export alias `From` in `src/lib.rs` MUST shadow `::std::convert::From` in `src/probe.rs`.
        fs::write(
            root.join("src/local_fmt.rs"),
            "pub trait Display { fn fmt(&self) -> u32; }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/std_display.rs"),
            "pub struct StdDispA(pub u32);\nimpl ::std::fmt::Display for StdDispA {\n    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {\n        let a = self.0.wrapping_add(1);\n        let b = a.wrapping_mul(2);\n        let c = b.wrapping_add(3);\n        let d = c.wrapping_mul(4);\n        write!(f, \"{}\", d)\n    }\n}\npub struct StdDispB(pub u32);\nimpl ::std::fmt::Display for StdDispB {\n    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {\n        let a = self.0.wrapping_add(11);\n        let b = a.wrapping_mul(12);\n        let c = b.wrapping_add(13);\n        let d = c.wrapping_mul(14);\n        write!(f, \"{}\", d)\n    }\n}\n",
        )
        .unwrap();
        let cross_file_std = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/std_display.rs".to_owned()],
                    changed_paths: vec!["src/std_display.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(cross_file_std, DomainCheckOutcome::Passed { .. }),
            "unrelated local Display trait must not block ::std::fmt::Display: {cross_file_std:?}"
        );
        fs::remove_file(root.join("src/local_fmt.rs")).unwrap();
        fs::remove_file(root.join("src/std_display.rs")).unwrap();

        fs::write(
            root.join("src/lib.rs"),
            "#![no_std]\nextern crate self as std;\npub mod other { pub trait MyFrom<T> { fn from(v: T) -> Self; } }\npub mod convert { pub use crate::other::MyFrom as From; }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/probe.rs"),
            "pub struct ProbeA(pub u32);\nimpl ::std::convert::From<u32> for ProbeA {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(151);\n        let b = a.wrapping_mul(152);\n        let c = b.wrapping_add(153);\n        let d = c.wrapping_mul(154);\n        let e = d.wrapping_add(155);\n        Self(e)\n    }\n}\npub struct ProbeB(pub u32);\nimpl ::std::convert::From<u32> for ProbeB {\n    fn from(v: u32) -> Self {\n        let a = v.wrapping_add(161);\n        let b = a.wrapping_mul(162);\n        let c = b.wrapping_add(163);\n        let d = c.wrapping_mul(164);\n        let e = d.wrapping_add(165);\n        Self(e)\n    }\n}\n",
        )
        .unwrap();
        let crate_alias_shadowed = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/probe.rs".to_owned()],
                    changed_paths: vec!["src/probe.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(
                crate_alias_shadowed,
                DomainCheckOutcome::Failed { ref diagnostics, .. }
                    if diagnostics.iter().any(|d| d.contains("duplicates.similar"))
            ),
            "crate-root extern crate self as std must shadow ::std::convert::From in src/probe.rs: {crate_alias_shadowed:?}"
        );
        fs::remove_file(root.join("src/lib.rs")).unwrap();
        fs::remove_file(root.join("src/probe.rs")).unwrap();

        // (c) `#![no_std]` with genuine `extern crate alloc;` must NOT shadow `::alloc::string::ToString`.
        fs::write(
            root.join("src/alloc_no_std.rs"),
            "#![no_std]\nextern crate alloc;\npub struct AllocA(pub u32);\nimpl ::alloc::string::ToString for AllocA {\n    fn to_string(&self) -> ::alloc::string::String {\n        let a = self.0.wrapping_add(1);\n        let b = a.wrapping_mul(2);\n        let c = b.wrapping_add(3);\n        let d = c.wrapping_mul(4);\n        ::alloc::format!(\"{}\", d)\n    }\n}\npub struct AllocB(pub u32);\nimpl ::alloc::string::ToString for AllocB {\n    fn to_string(&self) -> ::alloc::string::String {\n        let a = self.0.wrapping_add(11);\n        let b = a.wrapping_mul(12);\n        let c = b.wrapping_add(13);\n        let d = c.wrapping_mul(14);\n        ::alloc::format!(\"{}\", d)\n    }\n}\n",
        )
        .unwrap();
        let alloc_no_std = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/alloc_no_std.rs".to_owned()],
                    changed_paths: vec!["src/alloc_no_std.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(alloc_no_std, DomainCheckOutcome::Passed { .. }),
            "genuine #![no_std] extern crate alloc; must not shadow ::alloc::string::ToString: {alloc_no_std:?}"
        );
        fs::remove_file(root.join("src/alloc_no_std.rs")).unwrap();

        // 4. Partial-similarity (0.45..0.80) reuse_hint ranked by inbound callers without hardcoded repo names.
        let partial_overlap = "\
use crate::baseline::{shared_helper, shared_target};
pub fn partial_probe(x: i32) -> i32 {
    let a = x + 10;
    let b = a * 20;
    let c = b - 30;
    let d = c + 40;
    let extra = shared_helper(d) + 777;
    let extra2 = extra * 3;
    shared_target(extra2)
}
";
        fs::write(root.join("src/partial_overlap.rs"), partial_overlap).unwrap();
        let hint_outcome = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/partial_overlap.rs".to_owned()],
                    changed_paths: vec!["src/partial_overlap.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        match hint_outcome {
            DomainCheckOutcome::Passed { diagnostics, .. } => {
                let hint = diagnostics
                    .iter()
                    .find(|d| d.starts_with("[xray.reuse_hint]"))
                    .unwrap_or_else(|| panic!("expected [xray.reuse_hint] diagnostic: {diagnostics:?}"));
                assert!(hint.contains("owner_fn"), "expected canonical owner_fn in hint: {hint}");
                assert!(hint.contains("2 inbound callers"), "expected inbound caller rank in hint: {hint}");
                assert!(
                    !hint.contains("acdd-core")
                        && !hint.contains("contextunity-acdd")
                        && !hint.contains("build_manifests.rs"),
                    "reuse hint must not contain hardcoded repo names: {hint}"
                );
            }
            other => panic!("expected Passing outcome with reuse_hint, got {other:?}"),
        }
        fs::remove_file(root.join("src/partial_overlap.rs")).unwrap();

        // 5. Newly introduced 7-stmt literal-masked clone -> Failed (`duplicates.similar`).
        let masked_clone = "\
use crate::baseline::{shared_helper, shared_target};
pub fn new_masked_clone(x: i32) -> i32 {
    let a = x + 101;
    let b = a * 202;
    let c = b - 303;
    let d = c + 404;
    let e = d * 505;
    let f = e - 606;
    shared_target(f) + shared_helper(f)
}
";
        fs::write(root.join("src/new_clone.rs"), masked_clone).unwrap();
        let masked_outcome = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["duplicates"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/new_clone.rs".to_owned()],
                    changed_paths: vec!["src/new_clone.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(
                masked_outcome,
                DomainCheckOutcome::Failed { ref diagnostics, .. }
                    if diagnostics.iter().any(|d| d.contains("duplicates.similar"))
            ),
            "new 7-stmt literal-masked clone must fail duplicates.similar: {masked_outcome:?}"
        );
        fs::remove_file(root.join("src/new_clone.rs")).unwrap();

        // 6. Git-baseline file-monolith ratchet (`max_file_prod_lines: 10` WARN, `max_file_prod_lines_error: 20` ERROR).
        fs::create_dir_all(root.join(".acdd/lint")).unwrap();
        fs::write(root.join("acdd.yaml"), "lint_dirs:\n  - .acdd/lint\n").unwrap();
        fs::write(
            root.join(".acdd/lint/zones.yaml"),
            "complexity:\n  severity: warn\n  max_file_prod_lines: 10\n  max_file_prod_lines_error: 20\nzones:\n  src:\n    - \"src/**\"\n",
        )
        .unwrap();
        let existing_monolith = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["syntax"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/eligible_ninth.rs".to_owned()],
                    changed_paths: vec!["src/eligible_ninth.rs".to_owned()],
                    ..check_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(existing_monolith, DomainCheckOutcome::Passed { .. }),
            "unchanged file above warn threshold must stay non-blocking: {existing_monolith:?}"
        );
        let big_new: String = (0..25)
            .map(|i| format!("pub fn f_{i}() -> i32 {{ {i} }}\n"))
            .collect();
        fs::write(root.join("src/big_new.rs"), big_new).unwrap();
        let new_monolith = providers
            .run_check(
                "code.lint",
                &json!({ "categories": ["syntax"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/big_new.rs".to_owned()],
                    changed_paths: vec!["src/big_new.rs".to_owned()],
                    ..check_req
                },
            )
            .unwrap();
        assert!(
            matches!(
                new_monolith,
                DomainCheckOutcome::Failed { ref diagnostics, .. }
                    if diagnostics.iter().any(|d| d.contains("syntax.file_prod_lines_exceeded") && d.contains("error ratchet 20, new file"))
            ),
            "new file exceeding max_file_prod_lines_error must fail syntax ratchet: {new_monolith:?}"
        );
    }
}
