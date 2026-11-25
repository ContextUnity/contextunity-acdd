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
        profiles_dirs: adapter.profiles_dirs.clone(),
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
    assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);

    // 2. Host CodeDomainProvider + DocsDomainProvider: task scoping, gate blocking, [ENFORCED], and freshness drift.
    let ws = Workspace::with_files(&[
        (
            ".gitignore",
            ".acdd/code-map.sqlite*\n.acdd/state.sqlite*\n.acdd/extraction-cache/\n",
        ),
        (
            "acdd.yaml",
            r#"roots: [src, docs]
architecture:
  zones:
    core: ["src/core/**"]
    infra: ["src/infra/**"]
specs:
  milestones:
    path: docs/milestones
"#,
        ),
        (
            ".acdd/profile.yaml",
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
            "valid acdd.yaml zone removal",
            "acdd.yaml",
            "roots: [src, docs]\narchitecture:\n  zones:\n    core: [\"src/core/**\"]\nspecs:\n  milestones:\n    path: docs/milestones\n",
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

    // 3. End-to-end `--profile documentation` run using real CodeDomainProvider + DocsDomainProvider.
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
            profile: Some("documentation".to_owned()),
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
}
