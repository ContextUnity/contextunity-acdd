use super::support::{base_engine_config, git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{TaskRef, WorkerId};
use acdd_state::types::{
    BlackboardTopic, FindingItem, FindingSeverity, IntegrateStrategy, NodeStatus, RejectionFindings,
    RejectionKind, ReviewContourResult, ReviewEvidence, RunStatus, SubmitAction,
};
use acdd_state::usecases::dto::{
    BlackboardPostReq, ClaimReq, IntegrateReq, ListReq, RollbackReq, RunReq, StartReq, SubmitReq,
    VerifyReq,
};
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateQuery, StateQueryOutput,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const GIT_E2E_PROFILE: &str = r#"version: 2
inherit: none
git:
  isolation: worktree
  worktrees: managed
  commit_mode: plumbing
  integrate: manual
save: commit
contours:
  claims:
    description: "Verify claims"
    criteria: ["Verify claims"]
roles:
  contract_author: {}
  builder: {}
  independent_reviewer:
    reuse_on_reject: false
commands:
  test_contract:
    command: ": {test_filter} && if test -f .acdd/green; then printf 'test result: ok. 1 passed; 0 failed\nRunning target/debug/deps/seam\ntest seam_red ... ok\n'; else printf 'assertion failed\nRunning target/debug/deps/seam\ntest seam_red ... FAILED\n' >&2; exit 101; fi"
    test_filter: "--exact {test}"
    output:
      require_counts: false
      tests:
        passed: '^test (\S+) \.\.\. ok$'
        failed: '^test (\S+) \.\.\. FAILED$'
  test_build:
    command: "printf 'test result: ok. 1 passed; 0 failed\nRunning target/debug/deps/seam\ntest seam_red ... ok\n'"
    output:
      require_counts: true
      counts: { passed: '(\d+) passed', failed: '(\d+) failed' }
      tests:
        passed: '^test (\S+) \.\.\. ok$'
        failed: '^test (\S+) \.\.\. FAILED$'
gates:
  - tasks:
      gates:
        - id: contract
          role: contract_author
          proof: contract
          command: test_contract
          expected_failure:
            exit_codes: [101]
            stderr_contains: "assertion failed"
          save: none
          pass: build
        - id: build
          role: builder
          proof: command
          command: test_build
          save:
            post: none
            immutable: true
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          independent_from: [build]
          contours: [claims]
          pass: deliver
          fail: build
          save: commit
        - id: deliver
          need: [build, review]
          receipt: true
          save: none
  - id: handoff
    need: [tasks.deliver]
    receipt: true
    save:
      post: commit
      archive: true
      prune: true
"#;

#[test]
fn git_verify_submit_commit_integrate_rollback_and_archive_e2e() {
    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "main");
    ws.write(".acdd/profiles/e2e.yaml", GIT_E2E_PROFILE);
    ws.write(
        "docs/milestones/AC-010-e2e.md",
        r#"---
id: AC-010
title: E2E Git Flow
status: planned
---

# E2E Git Flow

### task: t1
```yaml
task_ref: t1
target: Implement t1
scope: [src/lib.rs]
```
"#,
    );
    git_cmd(ws.root(), &["add", ".acdd/profiles/e2e.yaml", "docs/milestones/AC-010-e2e.md"]);
    git_cmd(ws.root(), &["commit", "-m", "add e2e spec and profile"]);

    let cfg = base_engine_config(ws.root());
    let engine = StateEngine::open(cfg, TestProvider::full_composite()).unwrap();

    // Start managed worktree run off `main`.
    let StateCommandOutput::Start(started) = engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-010-e2e.md".to_owned()),
            profile: Some(".acdd/profiles/e2e.yaml".to_owned()),
            worktree: true,
            ..StartReq::default()
        }))
        .unwrap()
    else {
        panic!("expected StartResp");
    };
    let rid = started.run.run_id.to_string();

    let w_contract = WorkerId::new("author-1").unwrap();
    let w_build = WorkerId::new("builder-1").unwrap();
    let w_rev = WorkerId::new("reviewer-1").unwrap();

    // 1. Contract gate: runtime-red proof required.
    let StateCommandOutput::Claim(c_contract) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.contract".to_owned()),
            worker_id: w_contract.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let task_wt = PathBuf::from(&c_contract.worktree);
    let StateCommandOutput::Verify(v_contract) = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.contract".to_owned(),
            worker_id: Some(w_contract.clone()),
            test_target: Some("seam_red".to_owned()),
            claim_revision: Some(c_contract.claim_revision),
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected VerifyResp");
    };
    assert!(v_contract.passed);
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.contract".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_contract,
            claim_revision: Some(c_contract.claim_revision),
            test_target: Some("seam_red".to_owned()),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // 2. Build gate: out-of-scope blocked -> commit rollback restores clean -> immutable drift caught.
    let StateCommandOutput::Claim(c_build) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    fs::write(task_wt.join("README.md"), "# out of scope\n").unwrap();
    let oos_err = engine
        .execute(StateCommand::Verify(VerifyReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build.claim_revision),
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(oos_err.code, ErrorCode::OutOfScopeChanges);
    fs::write(task_wt.join("README.md"), "# ACDD Fixture Repo\n").unwrap();

    // Mutate src/lib.rs, Rollback against baseline commit restores it.
    let baseline_commit = git_cmd(&task_wt, &["rev-parse", "HEAD"]);
    fs::write(task_wt.join("src/lib.rs"), "pub fn broken() -> i32 { 0 }\n").unwrap();
    let StateCommandOutput::Rollback(rb) = engine
        .execute(StateCommand::Rollback(RollbackReq {
            run: Some(rid.clone()),
            target: baseline_commit,
            node: Some("t1.build".to_owned()),
            worktree: Some(task_wt.clone()),
        }))
        .unwrap()
    else {
        panic!("expected RollbackResp");
    };
    assert!(rb.restored_paths.contains(&"src/lib.rs".to_owned()));
    assert!(fs::read_to_string(task_wt.join("src/lib.rs"))
        .unwrap()
        .contains("pub fn base()"));

    // Verify, then drift in-scope file -> immutable gate returns TaskCandidateMismatch.
    let StateCommandOutput::Claim(c_build2) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: None,
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
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build2.claim_revision),
            worktree: None,
        }))
        .unwrap();
    fs::write(task_wt.join("src/lib.rs"), "pub fn feat() -> i32 { 42 }\n").unwrap();
    let immut_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_build2.claim_revision),
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
    assert_eq!(immut_err.code, ErrorCode::TaskCandidateMismatch);

    // Re-claim with a fresh session fingerprint (`--force` updates `claim_tree`), verify, and pass build.
    let StateCommandOutput::Claim(c_build3) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: Some("s-reclaim-2".to_owned()),
            force: true,
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
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build3.claim_revision),
            worktree: None,
        }))
        .unwrap();
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_build3.claim_revision),
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

    // 3. Review gate: same worker rejected (TaskReviewNotIndependent), distinct reviewer Reject -> Pass.
    let StateCommandOutput::Claim(c_rev_self) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: None,
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
            evidence: "verified feat".to_owned(),
        },
    );
    let self_err = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_rev_self.claim_revision),
            test_target: None,
            review_evidence: Some(ReviewEvidence {
                decision: SubmitAction::Pass,
                contours: contours.clone(),
                summary: "self".to_owned(),
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
    assert_eq!(self_err.code, ErrorCode::TaskReviewNotIndependent);

    let StateCommandOutput::Claim(c_rev1) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: None,
            force: true,
            steal_remote: true,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Reject,
            worker_id: w_rev.clone(),
            claim_revision: Some(c_rev1.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::Defect,
                summary: "add doc comment".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::Low,
                    path: Some("src/lib.rs".to_owned()),
                    message: "missing docs".to_owned(),
                    remediation: "add ///".to_owned(),
                }],
            }),
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // Pass build again -> pass review (plumbing commit) -> pass deliver.
    let StateCommandOutput::Claim(c_build4) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.build".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: None,
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
            worker_id: Some(w_build.clone()),
            test_target: None,
            claim_revision: Some(c_build4.claim_revision),
            worktree: None,
        }))
        .unwrap();
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build.clone(),
            claim_revision: Some(c_build4.claim_revision),
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

    let StateCommandOutput::Claim(c_rev2) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.review".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let StateCommandOutput::Submit(rev_pass) = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.review".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev.clone(),
            claim_revision: Some(c_rev2.claim_revision),
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
            summary: Some("review ok".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert!(rev_pass.committed_oid.is_some());

    let StateCommandOutput::Claim(c_del) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("t1.deliver".to_owned()),
            worker_id: w_build.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid.clone()),
            node: "t1.deliver".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_build,
            claim_revision: Some(c_del.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("delivered".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap();

    // 4. Before `integrate`, claiming `handoff` fails with TaskNotIntegrated!
    let not_int_err = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("handoff".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap_err();
    assert_eq!(not_int_err.code, ErrorCode::TaskNotIntegrated);

    // 5. Integrate via rebase-ff -> claim & submit handoff -> verify archive, tombstone, backup!
    let StateCommandOutput::Integrate(int_resp) = engine
        .execute(StateCommand::Integrate(IntegrateReq {
            run: Some(rid.clone()),
            task_ref: Some(TaskRef::new("t1").unwrap()),
            strategy: Some(IntegrateStrategy::RebaseFf),
            allow_main: false,
        }))
        .unwrap()
    else {
        panic!("expected IntegrateResp");
    };
    assert_eq!(int_resp.integrated_tasks, vec![TaskRef::new("t1").unwrap()]);

    let StateCommandOutput::Claim(c_hand) = engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(rid.clone()),
            node: Some("handoff".to_owned()),
            worker_id: w_rev.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        }))
        .unwrap()
    else {
        panic!("expected ClaimResp");
    };
    let StateCommandOutput::Submit(h_sub) = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(rid),
            node: "handoff".to_owned(),
            action: SubmitAction::Pass,
            worker_id: w_rev,
            claim_revision: Some(c_hand.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some("milestone closed".to_owned()),
            allow_main: false,
            worktree: None,
        })))
        .unwrap()
    else {
        panic!("expected SubmitResp");
    };
    assert_eq!(h_sub.new_node_status, NodeStatus::Completed);
    assert_eq!(h_sub.run_status, RunStatus::Completed);
    assert!(
        !ws.path("docs/milestones/archive/AC-010-e2e.md").exists(),
        "committed archive must stay in run/AC-010 until --ff-only landing"
    );
    git_cmd(ws.root(), &["merge", "--ff-only", "run/AC-010"]);
    assert!(ws.path("docs/milestones/archive/AC-010-e2e.md").is_file());

    let StateQueryOutput::List(listed) = engine
        .query(StateQuery::List(ListReq {
            include_tombstones: true,
            ..ListReq::default()
        }))
        .unwrap()
    else {
        panic!("expected ListResp");
    };
    assert!(listed.page.runs.is_empty());
    assert_eq!(listed.page.tombstones.len(), 1);
    assert_eq!(listed.page.tombstones[0].spec_id.as_deref(), Some("AC-010"));
    assert!(ws.path(".git/acdd/state.bak.sqlite").is_file());
}

#[test]
fn blueprint_max_net_loc_and_architectural_drift_runner_seams() {
    let providers = TestProvider::full_composite();

    // (a) `blueprint.max_net_loc` budget overrun fails closed on `tasks.build` unless escalated in `decisions`,
    // and `tasks.review` rejecting with `ArchitecturalDrift` routes through `tasks.rearchitect` -> `pass: build`
    // setting `ClaimResp.context_reset_required == true`.
    {
        let ws = Workspace::new();
        init_git_repo_on_branch(ws.root(), "dev");
        ws.write(".gitignore", ".acdd/\n");
        ws.write("tests/t1_seam.rs", "#[test] fn seam_ok() {}\n");
        ws.write(
            "docs/milestones/AC-DRIFT.md",
            "---\nid: AC-DRIFT\ntitle: Drift Seam\nstatus: planned\n---\n# Drift\n\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Exercise max_net_loc and architectural_drift seams\nscope: [src/lib.rs, tests/t1_seam.rs]\n```\n",
        );
        ws.write(
            ".acdd/profiles/drift.yaml",
            r#"version: 2
inherit: default
git: { isolation: in_place, worktrees: manual }
commands:
  test:
    command: "true"
    test_filter: "--filter {test}"
    output: { require_counts: false }
  build_check: { command: "true" }
  verify_all: { command: "true" }
gates:
  - id: blueprint_synthesis
    gates:
      - id: synthesize
        role: architect
        proof: blueprint
        pass: review
      - id: review
        role: independent_reviewer
        proof: none
        independent_from: [synthesize]
  - tasks:
      gates:
        - id: build
          role: builder
          proof: none
          pass: review
        - id: review
          role: independent_reviewer
          proof: none
          independent_from: [build]
          need: [build]
          pass: deliver
          fail: build
          fail_drift: rearchitect
        - id: rearchitect
          role: architect
          proof: none
          pass: build
        - id: deliver
          need: [build, review]
          receipt: true
  - id: finish
    need: [tasks.deliver]
    receipt: true
"#,
        );
        git_cmd(ws.root(), &["add", "."]);
        git_cmd(ws.root(), &["commit", "-m", "seed drift spec"]);

        let engine = StateEngine::open(base_engine_config(ws.root()), providers.clone()).unwrap();
        let StateCommandOutput::Start(start) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/AC-DRIFT.md".to_owned()),
                profile: Some(".acdd/profiles/drift.yaml".to_owned()),
                in_place: true,
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let rid = start.run.run_id.to_string();

        let w_arch = WorkerId::new("w-arch").unwrap();
        let w_rev = WorkerId::new("w-rev").unwrap();
        let w_build = WorkerId::new("w-build").unwrap();

        let StateCommandOutput::Claim(c_syn) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("blueprint_synthesis.synthesize".to_owned()),
                worker_id: w_arch.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp");
        };
        engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "blueprint_synthesis.synthesize".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_arch.clone(),
                claim_revision: Some(c_syn.claim_revision),
                test_target: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: Some(serde_json::json!({
                    "cited_invariants": ["INV-SINGLE-OWNER"],
                    "cited_symbols": ["base"],
                    "global_negative_boundaries": ["none"],
                    "blackboard_decision_id": "bb-01",
                    "tasks": {
                        "t1": {
                            "layer": "core",
                            "summary": "extend base cleanly",
                            "exported_interfaces": ["base"],
                            "negative_boundaries": ["no shadow helpers"],
                            "canonical_owners": ["src/lib.rs::base"],
                            "mechanism_rules": ["stay within max_net_loc unless escalated"],
                            "max_net_loc": 2,
                            "incoming_seams": [],
                            "outgoing_seams": []
                        }
                    }
                })),
                verified_invariants: Vec::new(),
                summary: None,
                allow_main: false,
                worktree: None,
            })))
            .unwrap();
        let StateCommandOutput::Claim(c_syn_rev) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("blueprint_synthesis.review".to_owned()),
                worker_id: w_rev.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp");
        };
        engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "blueprint_synthesis.review".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_rev.clone(),
                claim_revision: Some(c_syn_rev.claim_revision),
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

        let StateCommandOutput::Claim(c_b1) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("t1.build".to_owned()),
                worker_id: w_build.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp");
        };
        // Add +4 prod LOC (plus large test additions which must not trip production LOC budget).
        ws.write(
            "tests/t1_seam.rs",
            &(0..8)
                .map(|i| format!("#[test] fn seam_{i}() {{}}\n"))
                .collect::<String>(),
        );
        ws.write(
            "src/lib.rs",
            "pub fn base() -> i32 { 1 }\npub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\n",
        );
        let budget_err = engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "t1.build".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_build.clone(),
                claim_revision: Some(c_b1.claim_revision),
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
        assert_eq!(budget_err.code, ErrorCode::GateVerificationFailed);
        assert!(
            budget_err.message.contains("max_net_loc"),
            "unexpected budget error: {budget_err}"
        );

        engine
            .execute(StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some(rid.clone()),
                node: Some("t1.build".to_owned()),
                recipient_node: None,
                subtask_ref: None,
                gate: Some("build".to_owned()),
                author: "w-build".to_owned(),
                topic: BlackboardTopic::Decisions,
                payload: "Budget exception denied.".to_owned(),
            }))
            .unwrap();
        assert_eq!(
            engine
                .execute(StateCommand::Submit(Box::new(SubmitReq {
                    run: Some(rid.clone()),
                    node: "t1.build".to_owned(),
                    action: SubmitAction::Pass,
                    worker_id: w_build.clone(),
                    claim_revision: Some(c_b1.claim_revision),
                    test_target: None,
                    review_evidence: None,
                    rejection_findings: None,
                    custom_evidence: None,
                    verified_invariants: Vec::new(),
                    summary: None,
                    allow_main: false,
                    worktree: None,
                })))
                .unwrap_err()
                .code,
            ErrorCode::GateVerificationFailed
        );
        engine
            .execute(StateCommand::BlackboardPost(BlackboardPostReq {
                run: Some(rid.clone()),
                node: Some("t1.build".to_owned()),
                recipient_node: None,
                subtask_ref: None,
                gate: Some("build".to_owned()),
                author: "w-build".to_owned(),
                topic: BlackboardTopic::Decisions,
                payload: "BUDGET_OVERRIDE: justified helper expansion".to_owned(),
            }))
            .unwrap();
        engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "t1.build".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_build.clone(),
                claim_revision: Some(c_b1.claim_revision),
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

        let StateCommandOutput::Claim(c_rev) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("t1.review".to_owned()),
                worker_id: w_rev.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp");
        };
        engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "t1.review".to_owned(),
                action: SubmitAction::Reject,
                worker_id: w_rev,
                claim_revision: Some(c_rev.claim_revision),
                test_target: None,
                review_evidence: None,
                rejection_findings: Some(RejectionFindings {
                    kind: RejectionKind::ArchitecturalDrift,
                    summary: "shadow helpers bypass canonical owner".to_owned(),
                    blackboard_message_id: None,
                    items: vec![FindingItem {
                        severity: FindingSeverity::High,
                        path: Some("src/lib.rs".to_owned()),
                        message: "duplicated helper".to_owned(),
                        remediation: "route via rearchitect".to_owned(),
                    }],
                }),
                custom_evidence: None,
                verified_invariants: Vec::new(),
                summary: None,
                allow_main: false,
                worktree: None,
            })))
            .unwrap();

        let StateCommandOutput::Claim(c_rearch) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid.clone()),
                node: Some("t1.rearchitect".to_owned()),
                worker_id: w_arch.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp on t1.rearchitect");
        };
        engine
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(rid.clone()),
                node: "t1.rearchitect".to_owned(),
                action: SubmitAction::Pass,
                worker_id: w_arch,
                claim_revision: Some(c_rearch.claim_revision),
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

        let StateCommandOutput::Claim(c_b2) = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(rid),
                node: Some("t1.build".to_owned()),
                worker_id: w_build,
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            }))
            .unwrap()
        else {
            panic!("expected ClaimResp on t1.build after rearchitect");
        };
        assert!(
            c_b2.context_reset_required,
            "t1.build claim after rearchitect must require builder context reset"
        );
    }

    // (b) Driven via `StateCommand::Run` + `RoleDef.runner`, `ArchitecturalDrift` -> `rearchitect`
    // rotates `ACDD_AGENT_SESSION_ID` for the builder role and injects the CONTEXT RESET banner on stdin.
    {
        let ws = Workspace::new();
        init_git_repo_on_branch(ws.root(), "dev");
        ws.write(".gitignore", ".acdd/\n");
        ws.write(
            "docs/milestones/AC-RUN-DRIFT.md",
            "---\nid: AC-RUN-DRIFT\ntitle: Runner Drift Rotation\nstatus: planned\n---\n# Run\n\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Rotate builder session ID on architectural_drift\nscope: [src/lib.rs]\n```\n",
        );
        let acdd_bin = env!("CARGO_BIN_EXE_acdd");
        ws.write(
            ".acdd/runner-builder.sh",
            &format!(
                "#!/bin/sh\nset -eu\nPROMPT=$(cat)\nRESET=no\ncase \"$PROMPT\" in\n  *\"CONTEXT RESET REQUIRED\"*) RESET=yes ;;\nesac\nprintf 'build:%s:%s\\n' \"$ACDD_AGENT_SESSION_ID\" \"$RESET\" >> .acdd/role-sessions.log\nexec '{acdd_bin}' submit \"$ACDD_NODE_CURSOR\" --run \"$ACDD_RUN_ID\" --action pass --worker \"$ACDD_WORKER_ID\" --claim-revision \"$ACDD_CLAIM_REVISION\"\n"
            ),
        );
        ws.write(
            ".acdd/runner-review.sh",
            &format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\nif [ ! -f .acdd/drift-once ]; then\n  : > .acdd/drift-once\n  exec '{acdd_bin}' submit \"$ACDD_NODE_CURSOR\" --run \"$ACDD_RUN_ID\" --action reject --worker \"$ACDD_WORKER_ID\" --claim-revision \"$ACDD_CLAIM_REVISION\" --findings '{{\"kind\":\"architectural_drift\",\"summary\":\"drift\",\"items\":[{{\"severity\":\"high\",\"path\":\"src/lib.rs\",\"message\":\"drift\",\"remediation\":\"rearchitect\"}}]}}'\nfi\nexec '{acdd_bin}' submit \"$ACDD_NODE_CURSOR\" --run \"$ACDD_RUN_ID\" --action pass --worker \"$ACDD_WORKER_ID\" --claim-revision \"$ACDD_CLAIM_REVISION\"\n"
            ),
        );
        ws.write(
            ".acdd/runner-arch.sh",
            &format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf 'arch:%s\\n' \"$ACDD_AGENT_SESSION_ID\" >> .acdd/role-sessions.log\nexec '{acdd_bin}' submit \"$ACDD_NODE_CURSOR\" --run \"$ACDD_RUN_ID\" --action pass --worker \"$ACDD_WORKER_ID\" --claim-revision \"$ACDD_CLAIM_REVISION\"\n"
            ),
        );
        ws.write(
            ".acdd/profiles/drift-run.yaml",
            r#"version: 2
inherit: none
git: { isolation: in_place, worktrees: manual }
roles:
  builder:
    mode: subagent
    reuse_on_reject: true
    runner: "sh .acdd/runner-builder.sh"
  independent_reviewer:
    mode: subagent
    reuse_on_reject: false
    runner: "sh .acdd/runner-review.sh"
  architect:
    mode: subagent
    runner: "sh .acdd/runner-arch.sh"
gates:
  - tasks:
      gates:
        - id: build
          role: builder
          proof: none
          pass: review
        - id: review
          role: independent_reviewer
          proof: none
          independent_from: [build]
          need: [build]
          pass: deliver
          fail: build
          fail_drift: rearchitect
        - id: rearchitect
          role: architect
          proof: none
          pass: build
        - id: deliver
          need: [build, review]
          receipt: true
  - id: finish
    need: [tasks.deliver]
    receipt: true
"#,
        );
        git_cmd(ws.root(), &["add", "."]);
        git_cmd(ws.root(), &["commit", "-m", "seed runner drift spec"]);

        let engine = StateEngine::open(base_engine_config(ws.root()), providers).unwrap();
        let StateCommandOutput::Start(start) = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: Some("docs/milestones/AC-RUN-DRIFT.md".to_owned()),
                profile: Some(".acdd/profiles/drift-run.yaml".to_owned()),
                in_place: true,
                ..StartReq::default()
            }))
            .unwrap()
        else {
            panic!("expected StartResp");
        };
        let StateCommandOutput::Run(run_resp) = engine
            .execute(StateCommand::Run(RunReq {
                run: Some(start.run.run_id.to_string()),
                ..RunReq::default()
            }))
            .unwrap()
        else {
            panic!("expected RunResp");
        };
        assert_eq!(run_resp.report.halted_reason, None);

        let log = fs::read_to_string(ws.path(".acdd/role-sessions.log")).unwrap();
        let build_entries: Vec<(&str, &str)> = log
            .lines()
            .filter_map(|line| {
                let mut parts = line.split(':');
                match (parts.next(), parts.next(), parts.next()) {
                    (Some("build"), Some(sid), Some(reset)) => Some((sid, reset)),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(
            build_entries.len(),
            2,
            "expected two builder invocations in role-sessions.log:\n{log}"
        );
        assert_eq!(build_entries[0].1, "no", "{log}");
        assert_eq!(build_entries[1].1, "yes", "{log}");
        assert_ne!(
            build_entries[0].0, build_entries[1].0,
            "architectural_drift -> rearchitect must rotate ACDD_AGENT_SESSION_ID:\n{log}"
        );
    }
}
