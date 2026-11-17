use super::support::{base_engine_config, git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{TaskRef, WorkerId};
use acdd_state::types::{
    FindingItem, FindingSeverity, IntegrateStrategy, NodeStatus, RejectionFindings,
    ReviewContourResult, ReviewEvidence, RunStatus, SubmitAction,
};
use acdd_state::usecases::dto::{
    ClaimReq, IntegrateReq, ListReq, RollbackReq, SnapshotReq, StartReq, SubmitReq, VerifyReq,
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
          save: snapshot
          pass: build
        - id: build
          role: builder
          proof: command
          command: test_build
          save:
            pre: snapshot
            post: snapshot
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
          save: snapshot
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

    // 2. Build gate: out-of-scope blocked -> snapshot/rollback restores clean -> immutable drift caught.
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

    // Snapshot clean, mutate src/lib.rs, Rollback restores it.
    let StateCommandOutput::Snapshot(snap) = engine
        .execute(StateCommand::Snapshot(SnapshotReq {
            run: Some(rid.clone()),
            label: Some("before-edit".to_owned()),
            worktree: Some(task_wt.clone()),
        }))
        .unwrap()
    else {
        panic!("expected SnapshotResp");
    };
    fs::write(task_wt.join("src/lib.rs"), "pub fn broken() -> i32 { 0 }\n").unwrap();
    let StateCommandOutput::Rollback(rb) = engine
        .execute(StateCommand::Rollback(RollbackReq {
            run: Some(rid.clone()),
            target: snap.snapshot_ref,
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
