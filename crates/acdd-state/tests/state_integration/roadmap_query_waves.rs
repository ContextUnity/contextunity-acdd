use super::common::*;
use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::ids::{RunName, WorkerId};
use acdd_core::ports::CompositeDomainProvider;
use acdd_state::store::traits::{StateBackend, StateReader};
use acdd_state::types::SubmitAction;
use acdd_state::usecases::dto::{NextReq, RoadmapResp, StartReq, SubmitReq};
use acdd_state::usecases::StateEngineConfig;
use acdd_state::usecases::{
    RoadmapReq, StateCommand, StateEngine, StateQuery, StateQueryEngine, StateQueryOutput,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tempfile::TempDir;

fn plans_collection_cfg() -> SpecConfig {
    SpecConfig {
        path: None,
        paths: vec!["docs/plans".to_owned()],
        prefixes: vec!["PLAN".to_owned()],
        digits: 3,
        workflow: None,
    }
}

fn ext_collection_cfg(path: &str) -> SpecConfig {
    SpecConfig {
        path: Some(path.to_owned()),
        paths: Vec::new(),
        prefixes: Vec::new(),
        digits: 3,
        workflow: None,
    }
}

fn with_ext_collection(cfg: &mut StateEngineConfig, glob: &str) {
    cfg.spec_collections
        .insert("ext".to_owned(), ext_collection_cfg(glob));
}

fn write_spec(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn git_commit_all(root: &Path, message: &str) {
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "-m", message]);
}

fn roadmap_query(engine: &StateQueryEngine, req: RoadmapReq) -> RoadmapResp {
    match engine.query(StateQuery::Roadmap(req)).unwrap() {
        StateQueryOutput::Roadmap(r) => *r,
        other => panic!("expected Roadmap, got {other:?}"),
    }
}

fn roadmap_query_err(engine: &StateQueryEngine, req: RoadmapReq) -> AcddError {
    engine.query(StateQuery::Roadmap(req)).unwrap_err()
}

fn submit_pass(engine: &StateEngine, run_id: &str, node: &str, worker: &str) {
    claim_node(engine, run_id, node, worker, None, false, false, None).unwrap();
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(run_id.to_owned()),
            node: node.to_owned(),
            action: SubmitAction::Pass,
            worker_id: WorkerId::new(worker).unwrap(),
            claim_revision: None,
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
}

fn complete_task_build_deliver(engine: &StateEngine, run_id: &str, worker: &str) {
    for gate in ["T-01.build", "T-01.deliver"] {
        submit_pass(engine, run_id, gate, worker);
    }
}

fn mock_provider() -> Arc<CompositeDomainProvider> {
    let (_, p) = build_provider_with_mock(
        &base_config(make_temp_dir("roadmap-mock-provider").path()).modules,
    );
    p
}

fn roadmap_engine(root: &Path, provider: Arc<CompositeDomainProvider>) -> StateQueryEngine {
    StateQueryEngine::open_read_only(base_config(root), provider).unwrap()
}

fn git_index_nonutf8_archive(root: &Path, relpath: &[u8]) {
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    let _ = init_git_repo(root, "main");
    let blob_out = std::process::Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"# bad\n").unwrap();
            child.wait_with_output()
        })
        .unwrap();
    assert!(blob_out.status.success());
    let blob_oid = String::from_utf8(blob_out.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mut idx_child = std::process::Command::new("git")
        .args(["update-index", "--index-info"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut line = format!("100644 {blob_oid}\t").into_bytes();
        line.extend_from_slice(relpath);
        line.push(b'\n');
        idx_child.stdin.take().unwrap().write_all(&line).unwrap();
    }
    assert!(idx_child.wait().unwrap().success());
    git_ok(root, &["commit", "-m", "non-utf8 archive path"]);
}
#[test]
fn seam_roadmap_query_waves_missing_db_base_waves_overlay_detail() {
    let tmp = make_temp_dir("roadmap-seam");
    let root = tmp.path();
    write_spec(root, "docs/milestones/archive/AC-000-arch.md",
    "---\nid: AC-000\ntitle: Archived Base\nstatus: completed\ndepends_on: [AC-900, \"AC-900:\"]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/arch/**]\nstatus: completed\nacceptance_criteria: [done]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\nscope: [crates/arch2/**]\nstatus: cancelled\nacceptance_criteria: [dropped]\n```\n",
);
    fs::write(
    root.join("docs/milestones/AC-001-core.md"),
    "---\nid: AC-001\ntitle: Core Ready\nstatus: planned\ndepends_on: [AC-000]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/shared/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    root.join("docs/milestones/AC-002-overlap.md"),
    "---\nid: AC-002\ntitle: Overlap Ready\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/shared/sub/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    write_spec(root, "docs/plans/PLAN-001-mid.md",
    "---\nid: PLAN-001\ntitle: Mid Plan\ndoc_type: plan\nstatus: planned\ndepends_on: [AC-001]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/plan/**]\nacceptance_criteria: [ok]\n```\n",
);
    fs::write(
    root.join("docs/milestones/AC-003-leaf.md"),
    "---\nid: AC-003\ntitle: Leaf Spec\nstatus: planned\ndepends_on: [PLAN-001]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: [\"PLAN-001:T-01\"]\nscope: [crates/leaf/**]\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\ndepends_on: [\"PLAN-001:T-01\"]\nscope: [crates/leaf/sub/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();

    use acdd_state::usecases::RoadmapReq;
    let mut cfg = base_config(root);
    cfg.spec_collections
        .insert("plans".to_owned(), plans_collection_cfg());
    let (_, provider) = build_provider_with_mock(&cfg.modules);

    // 1. Missing state.sqlite: query succeeds read-only and never creates state.sqlite.
    let ro_engine = StateQueryEngine::open_read_only(cfg.clone(), provider.clone()).unwrap();
    let db_path = cfg.resolved_state_db_path().0;
    assert!(!db_path.exists());

    let base_resp = roadmap_query(&ro_engine, RoadmapReq::default());
    assert!(
        !db_path.exists(),
        "Roadmap query must never create state.sqlite"
    );
    assert!(!base_resp.warnings.iter().any(|w| w.contains("AC-900:")));
    assert_eq!(base_resp.archived_count, 1);
    assert!(base_resp.archived.is_empty());
    assert_eq!(base_resp.waves.len(), 3);
    assert_eq!(base_resp.waves[0].specs.len(), 2);
    // AC-001 unblocks PLAN-001 and AC-003 -> ranked before AC-002 (0 unblocks).
    assert_eq!(base_resp.waves[0].specs[0].spec_id, "AC-001");
    assert_eq!(base_resp.waves[0].specs[0].unblocks_specs, 2);
    assert!(!base_resp.waves[0].specs[0].parallel_safe);
    assert_eq!(base_resp.waves[0].specs[0].scope_overlaps, vec!["AC-002"]);
    assert_eq!(base_resp.waves[0].specs[0].depends_on[0].status, "archived");
    assert!(base_resp.waves[0].specs[0].depends_on[0].satisfied);
    assert_eq!(base_resp.waves[0].specs[1].spec_id, "AC-002");
    assert_eq!(base_resp.waves[1].specs[0].spec_id, "PLAN-001");
    assert_eq!(base_resp.waves[1].specs[0].spec, "plans");
    assert_eq!(base_resp.waves[2].specs[0].spec_id, "AC-003");

    // 2. Overlay an active run for AC-004 and run table-driven filter matrix.
    fs::write(
    root.join("docs/milestones/AC-004-active.md"),
    "---\nid: AC-004\ntitle: Active Spec\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/active/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    let bound_p = write_profile(
    root,
    "roadmap-bound.yaml",
    "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
);
    let rw_engine = StateEngine::open(cfg.clone(), provider.clone()).unwrap();
    let started = start_run(
        &rw_engine,
        StartReq {
            spec_path: Some("docs/milestones/AC-004-active.md".to_owned()),
            workflow: Some(bound_p),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();

    struct RoadmapCase<'a> {
        label: &'a str,
        req: RoadmapReq,
        expect_in_progress: usize,
        expect_waves: usize,
        expect_archived: usize,
    }
    let cases = [
        RoadmapCase {
            label: "include_archived",
            req: RoadmapReq {
                archived: true,
                ..RoadmapReq::default()
            },
            expect_in_progress: 1,
            expect_waves: 3,
            expect_archived: 1,
        },
        RoadmapCase {
            label: "ready_only",
            req: RoadmapReq {
                ready_only: true,
                ..RoadmapReq::default()
            },
            expect_in_progress: 0,
            expect_waves: 1,
            expect_archived: 0,
        },
        RoadmapCase {
            label: "active_only",
            req: RoadmapReq {
                active_only: true,
                ..RoadmapReq::default()
            },
            expect_in_progress: 1,
            expect_waves: 0,
            expect_archived: 0,
        },
        RoadmapCase {
            label: "collection_plans",
            req: RoadmapReq {
                spec: Some("plans".to_owned()),
                ..RoadmapReq::default()
            },
            expect_in_progress: 0,
            expect_waves: 1,
            expect_archived: 0,
        },
        RoadmapCase {
            label: "archived_spec_detail",
            req: RoadmapReq {
                spec_id: Some("AC-000".to_owned()),
                ..RoadmapReq::default()
            },
            expect_in_progress: 0,
            expect_waves: 0,
            expect_archived: 1,
        },
    ];
    for case in cases {
        let resp = match ro_engine.query(StateQuery::Roadmap(case.req)).unwrap() {
            StateQueryOutput::Roadmap(r) => r,
            other => panic!("{}: unexpected {other:?}", case.label),
        };
        assert_eq!(
            resp.in_progress.len(),
            case.expect_in_progress,
            "{}",
            case.label
        );
        assert_eq!(resp.waves.len(), case.expect_waves, "{}", case.label);
        assert_eq!(resp.archived.len(), case.expect_archived, "{}", case.label);
        assert_eq!(resp.archived_count, 1, "{}", case.label);
    }

    // 3. Task drill-down on AC-003 verifies cross-spec deps + intra-wave scope overlaps.
    let detail = roadmap_query(
        &ro_engine,
        RoadmapReq {
            spec_id: Some("AC-003".to_owned()),
            ..RoadmapReq::default()
        },
    );
    let ac3 = &detail.waves[0].specs[0];
    assert_eq!(ac3.tasks.len(), 2);
    assert_eq!(ac3.tasks[0].depends_on[0].raw, "PLAN-001:T-01");
    assert_eq!(ac3.tasks[0].depends_on[0].status, "planned");
    assert_eq!(ac3.tasks[0].scope_overlaps, vec!["T-02"]);
    assert_eq!(ac3.tasks[1].scope_overlaps, vec!["T-01"]);
    clear_run(&rw_engine, started.run.run_id.as_str());

    // 3b. Archived spec detail preserves cancelled task and real depends_on.
    let arch_detail = roadmap_query(
        &ro_engine,
        RoadmapReq {
            spec_id: Some("AC-000".to_owned()),
            ..RoadmapReq::default()
        },
    );
    assert!(arch_detail.warnings.iter().any(|w| w.contains("AC-900:")));
    let ac0 = &arch_detail.archived[0];
    assert_eq!(ac0.depends_on[0].raw, "AC-900");
    assert_eq!(ac0.depends_on[0].status, "missing");
    assert_eq!(ac0.tasks[0].status, "completed");
    assert_eq!(ac0.tasks[1].status, "cancelled");
}

#[test]
fn seam_roadmap_query_waves_cross_worktree_dedup() {
    // 4. Cross-worktree discovery + silent identical dedup + divergence warnings + current_only.
    let wt_tmp = make_temp_dir("roadmap-worktrees");
    let main_root = wt_tmp.path();
    fs::create_dir_all(main_root.join("docs/milestones")).unwrap();
    fs::write(
    main_root.join("docs/milestones/AC-001-shared.md"),
    "---\nid: AC-001\ntitle: Shared Main\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/a/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    main_root.join("docs/milestones/AC-011-identical.md"),
    "---\nid: AC-011\ntitle: Identical\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/b/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    let _ = init_git_repo(main_root, "main");
    git_commit_all(main_root, "init");
    let (_, provider) = build_provider_with_mock(&base_config(main_root).modules);
    let feat_wt = main_root.join(".worktrees/feat-x");
    git_ok(
        main_root,
        &["worktree", "add", "-b", "feat-x", feat_wt.to_str().unwrap()],
    );
    fs::write(
    feat_wt.join("docs/milestones/AC-010-sec.md"),
    "---\nid: AC-010\ntitle: Secondary Only\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/c/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    feat_wt.join("docs/milestones/AC-014-sec-active.md"),
    "---\nid: AC-014\ntitle: Secondary Active\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/c/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    feat_wt.join("docs/milestones/AC-001-shared.md"),
    "---\nid: AC-001\ntitle: Diverged In Feat\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/a/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::create_dir_all(feat_wt.join("docs/milestones/archive")).unwrap();
    fs::write(
    feat_wt.join("docs/milestones/archive/AC-012-sib-arch.md"),
    "---\nid: AC-012\ntitle: Sibling Only Archive\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/s/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    main_root.join("docs/milestones/AC-013-uses-sib-arch.md"),
    "---\nid: AC-013\ntitle: Depends On Sibling Archive\nstatus: planned\ndepends_on: [AC-012]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/s2/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
        main_root.join("docs/milestones/AC-015-uses-sibling.md"),
        "---\nid: AC-015\ntitle: Sibling dependency\nstatus: planned\ndepends_on: [AC-010]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/x/**]\nacceptance_criteria: [ok]\n```\n",
    ).unwrap();

    let wt_cfg = base_config(main_root);
    let wt_engine = StateQueryEngine::open_read_only(wt_cfg, provider.clone()).unwrap();
    let query_cross_worktree = || {
        roadmap_query(
            &wt_engine,
            RoadmapReq {
                archived: true,
                ..RoadmapReq::default()
            },
        )
    };
    let uncommitted = query_cross_worktree();
    let sibling_dep = uncommitted
        .waves
        .iter()
        .flat_map(|wave| &wave.specs)
        .find(|spec| spec.spec_id == "AC-015")
        .unwrap();
    assert_eq!(sibling_dep.depends_on[0].status, "planned");
    assert!(uncommitted
        .archived
        .iter()
        .any(|spec| spec.spec_id == "AC-012"));
    let blocked = uncommitted
        .waves
        .iter()
        .find(|wave| wave.wave == 1)
        .unwrap()
        .specs
        .iter()
        .find(|spec| spec.spec_id == "AC-013")
        .unwrap();
    assert_eq!(blocked.depends_on[0].status, "missing");
    assert!(!blocked.depends_on[0].satisfied);
    git_ok(
        &feat_wt,
        &["add", "docs/milestones/archive/AC-012-sib-arch.md"],
    );
    git_ok(&feat_wt, &["commit", "-m", "Commit sibling archive"]);
    // Archived copies with different digests must report divergence.
    fs::create_dir_all(main_root.join("docs/milestones/archive")).unwrap();
    fs::write(
        main_root.join("docs/milestones/archive/AC-012-sib-arch.md"),
        "---\nid: AC-012\ntitle: Sibling Archive In Main With Different Digest\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/s/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    git_ok(
        main_root,
        &["add", "docs/milestones/archive/AC-012-sib-arch.md"],
    );
    git_ok(main_root, &["commit", "-m", "Commit archive in main"]);
    fs::write(
        feat_wt.join("docs/milestones/AC-012-planned.md"),
        "---\nid: AC-012\ntitle: Uncommitted sibling copy\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/s/**]\nacceptance_criteria: [ok]\n```\n",
    ).unwrap();
    let cross_resp = query_cross_worktree();
    let all_w0: Vec<_> = cross_resp.waves[0].specs.iter().collect();
    assert_eq!(all_w0.iter().filter(|s| s.spec_id == "AC-011").count(), 1);
    assert!(
        !all_w0.iter().any(|s| s.spec_id == "AC-010"),
        "Inactive planned spec in secondary worktree must not overlay onto main"
    );
    let ac14 = all_w0
        .iter()
        .find(|s| s.spec_id == "AC-014")
        .expect("AC-014 active in secondary worktree visible from main");
    assert_eq!(ac14.source_worktree.as_deref(), Some(".worktrees/feat-x"));
    let ac01 = all_w0.iter().find(|s| s.spec_id == "AC-001").unwrap();
    assert_eq!(ac01.divergence.len(), 2);
    assert!(cross_resp.warnings.iter().any(|w| w.contains("AC-001")));
    assert!(cross_resp
        .warnings
        .iter()
        .any(|w| w.contains("AC-012") && w.contains("diverges")));
    let ac13 = all_w0
        .iter()
        .find(|s| s.spec_id == "AC-013")
        .expect("AC-013 unblocked by sibling-only archive AC-012");
    assert_eq!(ac13.depends_on[0].status, "archived");
    assert!(ac13.depends_on[0].satisfied);

    let cur_resp = roadmap_query(
        &wt_engine,
        RoadmapReq {
            current_only: true,
            ..RoadmapReq::default()
        },
    );
    assert!(!cur_resp.waves[0]
        .specs
        .iter()
        .any(|s| s.spec_id == "AC-010"));
    assert!(!cur_resp.waves[0]
        .specs
        .iter()
        .any(|s| s.spec_id == "AC-014"));
    assert!(cur_resp.warnings.is_empty());

    let writer_cfg = base_config(main_root);
    let _writer = StateEngine::open(writer_cfg.clone(), provider).unwrap();
    let mut graph = build_rich_graph(
        main_root,
        "run-cleared-archive",
        acdd_state::types::IsolationMode::InPlace,
    );
    graph.run.worktree = None;
    graph.run.run_name = RunName::new("cleared-archive").unwrap();
    let tombstone = sample_tombstone(
        "run-cleared-archive",
        "cleared-archive",
        Some("AC-012"),
        acdd_state::types::TerminalStatus::Cleared,
        None,
    );
    backend_for(&writer_cfg)
        .exec_tx(|tx| {
            tx.insert_run_graph(&graph)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &tombstone)
        })
        .unwrap();
    let cleared = query_cross_worktree();
    let dependency = cleared
        .waves
        .iter()
        .flat_map(|wave| &wave.specs)
        .find(|spec| spec.spec_id == "AC-013")
        .unwrap();
    assert_eq!(dependency.depends_on[0].status, "cleared");
    assert!(!dependency.depends_on[0].satisfied);
}

#[test]
fn seam_roadmap_query_waves_active_run_cycles_blockers() {
    // 5. Empty Wave 0, active-run completed task unblocking, missing task blocker, filtered overlap,
    //    disjoint SCC cycles vs downstream blocked spec, and confinement fail-closed.
    let adv_tmp = make_temp_dir("roadmap-adv");
    let adv_root = adv_tmp.path();
    fs::create_dir_all(adv_root.join("docs/milestones")).unwrap();
    fs::create_dir_all(adv_root.join("docs/plans")).unwrap();
    fs::write(
    adv_root.join("docs/milestones/AC-100-active.md"),
    "---\nid: AC-100\ntitle: Active Run\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/a/**]\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\ndepends_on: [T-01]\nscope: [crates/b/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    adv_root.join("docs/plans/PLAN-101-unblocked.md"),
    "---\nid: PLAN-101\ntitle: Unblocked By Active Task\ndoc_type: plan\nstatus: planned\nrepositories: [{name: selfws, path: .}]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: ['selfws/AC-100:T-01']\nscope: [crates/shared/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    adv_root.join("docs/milestones/AC-102-overlap.md"),
    "---\nid: AC-102\ntitle: Overlap Cross Collection\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/shared/sub/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    adv_root.join("docs/milestones/AC-103-missing-dep.md"),
    "---\nid: AC-103\ntitle: Missing Task Blocker\nstatus: planned\ndepends_on: [\"AC-999:T-01\"]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: [\"AC-999:T-01\"]\nscope: [crates/m/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    adv_root.join("docs/milestones/AC-105-cancelled-dep.md"),
    "---\nid: AC-105\ntitle: Cancelled Target Blocker\nstatus: planned\ndepends_on: [AC-106]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/c1/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    adv_root.join("docs/milestones/AC-106-cancelled.md"),
    "---\nid: AC-106\ntitle: Cancelled Spec\nstatus: cancelled\ncancellation_reason: obsolete\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/c2/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    // Disjoint cycles (CYC-1 <-> CYC-2, CYC-3 <-> CYC-4) + downstream AC-104 -> CYC-1.
    for (id, dep) in [
        ("CYC-1", "CYC-2"),
        ("CYC-2", "CYC-1"),
        ("CYC-3", "CYC-4"),
        ("CYC-4", "CYC-3"),
        ("AC-104", "CYC-1"),
    ] {
        fs::write(
        adv_root.join(format!("docs/milestones/{id}.md")),
        format!("---\nid: {id}\ntitle: {id}\nstatus: planned\ndepends_on: [{dep}]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/{id}/**]\nacceptance_criteria: [ok]\n```\n"),
    )
    .unwrap();
    }

    let mut adv_cfg = base_config(adv_root);
    adv_cfg
        .spec_collections
        .insert("plans".to_owned(), plans_collection_cfg());
    let (_, adv_provider) = build_provider_with_mock(&adv_cfg.modules);
    let adv_bound = write_profile(
    adv_root,
    "adv-bound.yaml",
    "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n          pass: deliver\n        - id: deliver\n          proof: none\n          need: [build]\n          receipt: true\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
);
    let adv_rw = StateEngine::open(adv_cfg.clone(), adv_provider.clone()).unwrap();
    let _ = init_git_repo(adv_root, "main");
    git_commit_all(adv_root, "init");
    let adv_run = start_run(
        &adv_rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-100-active.md".to_owned()),
            workflow: Some(adv_bound),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    // Complete T-01.build + T-01.deliver (proof: none) via real claim + submit so task:T-01 is Completed in state.sqlite.
    complete_task_build_deliver(&adv_rw, adv_run.run.run_id.as_str(), "w-adv");

    let adv_ro = StateQueryEngine::open_read_only(adv_cfg.clone(), adv_provider.clone()).unwrap();
    let filtered_plans = roadmap_query(
        &adv_ro,
        RoadmapReq {
            spec: Some("plans".to_owned()),
            ..RoadmapReq::default()
        },
    );
    // Overlap detection ran BEFORE --collection filter: PLAN-101 overlaps AC-102 in Wave 0.
    assert_eq!(filtered_plans.waves[0].wave, 0);
    assert_eq!(filtered_plans.waves[0].specs[0].spec_id, "PLAN-101");
    assert!(!filtered_plans.waves[0].specs[0].parallel_safe);
    assert_eq!(
        filtered_plans.waves[0].specs[0].scope_overlaps,
        vec!["AC-102"]
    );
    // Disjoint SCCs only (AC-104 downstream of CYC-1 is NOT lumped into cycles).
    assert_eq!(
        filtered_plans.cycles,
        vec![
            vec!["CYC-1".to_owned(), "CYC-2".to_owned()],
            vec!["CYC-3".to_owned(), "CYC-4".to_owned()],
        ]
    );

    // AC-103 (missing task blocker) and AC-105 (cancelled target) must be in Wave 1 (NOT Wave 0).
    for blocked_id in ["AC-103", "AC-105"] {
        let blocked_only = roadmap_query(
            &adv_ro,
            RoadmapReq {
                spec_id: Some(blocked_id.to_owned()),
                ..RoadmapReq::default()
            },
        );
        assert_eq!(blocked_only.waves.len(), 1, "{blocked_id}");
        assert_eq!(blocked_only.waves[0].wave, 1, "{blocked_id}");
        assert_eq!(
            blocked_only.waves[0].specs[0].status, "planned",
            "{blocked_id}"
        );
        let blocked_ready = roadmap_query(
            &adv_ro,
            RoadmapReq {
                spec_id: Some(blocked_id.to_owned()),
                ready_only: true,
                ..RoadmapReq::default()
            },
        );
        assert!(blocked_ready.waves.is_empty(), "{blocked_id}");
    }
    // Equal-priority tie-break: AC-105 and AC-103 both have unblocks=(0,0) in Wave 1 -> AC-105 sorts before AC-103 (spec_id DESC).
    let adv_all = roadmap_query(
        &adv_ro,
        RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(
        adv_all.in_progress[0].tasks[0].blocks,
        ["PLAN-101:T-01", "T-02"]
    );
    let w1_ids: Vec<&str> = adv_all
        .waves
        .iter()
        .find(|w| w.wave == 1)
        .unwrap()
        .specs
        .iter()
        .map(|s| s.spec_id.as_str())
        .collect();
    assert_eq!(w1_ids, vec!["AC-105", "AC-104", "AC-103"]);
    assert!(!adv_all.cycles.iter().flatten().any(|id| id == "AC-104"));
    let ac104_detail = roadmap_query(
        &adv_ro,
        RoadmapReq {
            spec_id: Some("AC-104".to_owned()),
            ..RoadmapReq::default()
        },
    );
    assert_eq!(ac104_detail.waves.len(), 1);
    assert_eq!(ac104_detail.waves[0].wave, 1);
    assert_eq!(ac104_detail.waves[0].specs[0].depends_on[0].raw, "CYC-1");
    assert!(!ac104_detail.waves[0].specs[0].depends_on[0].satisfied);
    let ac104_ready = roadmap_query(
        &adv_ro,
        RoadmapReq {
            spec_id: Some("AC-104".to_owned()),
            ready_only: true,
            ..RoadmapReq::default()
        },
    );
    assert!(ac104_ready.waves.is_empty());

    clear_run(&adv_rw, adv_run.run.run_id.as_str());
}

#[test]
#[cfg(unix)]
fn seam_roadmap_query_waves_confinement_and_escapes() {
    let adv_provider = mock_provider();
    let adv_tmp = make_temp_dir("roadmap-confinement");
    let adv_root = adv_tmp.path();
    fs::create_dir_all(adv_root.join("docs/milestones")).unwrap();
    write_spec(
        adv_root,
        "docs/milestones/AC-CONF.md",
        "---\nid: AC-CONF\ntitle: Confinement Base\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    );
    let adv_ro =
        StateQueryEngine::open_read_only(base_config(adv_root), adv_provider.clone()).unwrap();
    // Confinement fail-closed on symlink escaping workspace root in docs/milestones/ AND in archive/.
    let outside = make_temp_dir("outside-escape");
    fs::write(outside.path().join("secret.md"), "# escaped\n").unwrap();
    let active_link = adv_root.join("docs/milestones/escape.md");
    std::os::unix::fs::symlink(outside.path().join("secret.md"), &active_link).unwrap();
    let err = roadmap_query_err(&adv_ro, RoadmapReq::default());
    assert_eq!(err.code, ErrorCode::SpecSchemaInvalid);
    fs::remove_file(&active_link).unwrap();

    // Also test symlinked archive .md file in a non-Git workspace.
    let arch_tmp = make_temp_dir("archive-symlink");
    let arch_root = arch_tmp.path();
    fs::create_dir_all(arch_root.join("docs/milestones/archive")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        arch_root.join("docs/milestones/archive/AC-998-escape.md"),
    )
    .unwrap();
    let arch_ro =
        StateQueryEngine::open_read_only(base_config(arch_root), adv_provider.clone()).unwrap();
    let arch_err = roadmap_query_err(&arch_ro, RoadmapReq::default());
    assert_eq!(arch_err.code, ErrorCode::SpecSchemaInvalid);

    // Committed Git mode 120000 symlink in archive/ must also fail closed.
    let git_arch_tmp = make_temp_dir("git-archive-symlink");
    let git_arch_root = git_arch_tmp.path();
    fs::create_dir_all(git_arch_root.join("docs/milestones/archive")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        git_arch_root.join("docs/milestones/archive/AC-997-escape.md"),
    )
    .unwrap();
    let _ = init_git_repo(git_arch_root, "main");
    git_commit_all(git_arch_root, "committed symlink");
    let git_arch_ro = roadmap_engine(git_arch_root, adv_provider.clone());
    let git_arch_err = roadmap_query_err(&git_arch_ro, RoadmapReq::default());
    assert_eq!(git_arch_err.code, ErrorCode::SpecSchemaInvalid);

    // Untracked symlink under docs/milestones/archive/escape.md in a primary Git repo must also fail closed.
    let git_untracked_tmp = make_temp_dir("git-untracked-archive-symlink");
    let git_untracked_root = git_untracked_tmp.path();
    fs::create_dir_all(git_untracked_root.join("docs/milestones/archive")).unwrap();
    fs::write(
    git_untracked_root.join("docs/milestones/AC-995.md"),
    "---\nid: AC-995\ntitle: Ok\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    let _ = init_git_repo(git_untracked_root, "main");
    git_commit_all(git_untracked_root, "base");
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        git_untracked_root.join("docs/milestones/archive/escape.md"),
    )
    .unwrap();
    let git_untracked_ro = roadmap_engine(git_untracked_root, adv_provider.clone());
    let git_untracked_err = roadmap_query_err(&git_untracked_ro, RoadmapReq::default());
    assert_eq!(git_untracked_err.code, ErrorCode::SpecSchemaInvalid);
    fs::remove_file(git_untracked_root.join("docs/milestones/archive/escape.md")).unwrap();

    // Direct .md symlink containing literal '\' in filename inside archive/ must fail closed (disk).
    let backslash_link = git_untracked_root.join("docs/milestones/archive/bad\\name.md");
    std::os::unix::fs::symlink(outside.path().join("secret.md"), &backslash_link).unwrap();
    let backslash_err = roadmap_query_err(&git_untracked_ro, RoadmapReq::default());
    assert_eq!(backslash_err.code, ErrorCode::SpecSchemaInvalid);
    assert!(
        backslash_err.message.contains("bad\\name.md"),
        "expected bad\\name.md in error, got: {}",
        backslash_err.message
    );
    fs::remove_file(&backslash_link).unwrap();

    // Committed-only (removed from disk) Git mode 120000 symlink named archive/bad\name.md must also fail closed.
    let committed_bs_tmp = make_temp_dir("git-committed-bs-symlink");
    let committed_bs_root = committed_bs_tmp.path();
    fs::create_dir_all(committed_bs_root.join("docs/milestones/archive")).unwrap();
    let committed_bs_link = committed_bs_root.join("docs/milestones/archive/bad\\name.md");
    std::os::unix::fs::symlink(outside.path().join("secret.md"), &committed_bs_link).unwrap();
    let _ = init_git_repo(committed_bs_root, "main");
    git_ok(committed_bs_root, &["add", "-A"]);
    git_ok(
        committed_bs_root,
        &["commit", "-m", "committed backslash symlink"],
    );
    fs::remove_file(&committed_bs_link).unwrap();
    let committed_bs_ro = roadmap_engine(committed_bs_root, adv_provider.clone());
    let committed_bs_err = roadmap_query_err(&committed_bs_ro, RoadmapReq::default());
    assert_eq!(committed_bs_err.code, ErrorCode::SpecSchemaInvalid);
    assert!(
        committed_bs_err.message.contains("bad\\name.md"),
        "expected bad\\name.md in committed error, got: {}",
        committed_bs_err.message
    );

    // Unreadable glob prefix dir surfaces non-NotFound error in SpecConfig::resolve_dirs.
    use std::os::unix::fs::PermissionsExt;
    let locked_dir = git_untracked_root.join("extensions/locked");
    fs::create_dir_all(locked_dir.join("sub/docs")).unwrap();
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o000)).unwrap();
    let deny_effective = fs::read_dir(&locked_dir).is_err();
    let mut glob_cfg = base_config(git_untracked_root);
    glob_cfg.spec_collections.insert(
        "ext".to_owned(),
        acdd_core::config::SpecConfig {
            path: Some("extensions/locked/*/docs".to_owned()),
            paths: Vec::new(),
            prefixes: Vec::new(),
            digits: 3,
            workflow: None,
        },
    );
    let glob_ro = StateQueryEngine::open_read_only(glob_cfg, adv_provider.clone()).unwrap();
    let glob_res = glob_ro.query(StateQuery::Roadmap(RoadmapReq::default()));
    let _ = fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755));
    if deny_effective {
        let err = glob_res.unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid);
    }
}

#[test]
fn seam_roadmap_query_waves_malformed_linked_deps() {
    let adv_provider = mock_provider();

    // Malformed depends_on emits warning & blocks spec without aborting roadmap; repeated linked-ws deps reuse cache.
    let mal_tmp = make_temp_dir("roadmap-malformed-linked");
    let mal_root = mal_tmp.path();
    let link_tmp = make_temp_dir("roadmap-linked-target");
    let link_root = link_tmp.path();
    fs::create_dir_all(mal_root.join("docs/milestones")).unwrap();
    fs::create_dir_all(link_root.join("docs/milestones")).unwrap();
    fs::write(
    link_root.join("acdd.yaml"),
    "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [EXT]\n    digits: 3\n",
)
.unwrap();
    fs::write(
    link_root.join("docs/milestones/EXT-001.md"),
    "---\nid: EXT-001\ntitle: External\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    mal_root.join("docs/milestones/AC-200-bad-dep.md"),
    "---\nid: AC-200\ntitle: Malformed Dep\nstatus: planned\ndepends_on: [\"AC-100:\"]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/x/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    fs::write(
    mal_root.join("docs/milestones/AC-201-linked.md"),
    "---\nid: AC-201\ntitle: Linked Consumer\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: []\nscope: [crates/y1/**]\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\ndepends_on: [\"ext/EXT-001:T-01\"]\nscope: [crates/y2/**]\nacceptance_criteria: [ok]\n```\n",
)
.unwrap();
    let mut mal_cfg = base_config(mal_root);
    mal_cfg
        .linked_workspaces
        .insert("ext".to_owned(), link_root.to_path_buf());
    let mal_ro = StateQueryEngine::open_read_only(mal_cfg, adv_provider.clone()).unwrap();
    let mal_resp = roadmap_query(
        &mal_ro,
        RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert!(mal_resp
        .warnings
        .iter()
        .any(|w| w.contains("AC-200") && w.contains("malformed depends_on")));
    assert_eq!(mal_resp.waves.len(), 2);
    assert_eq!(mal_resp.waves[0].wave, 0);
    assert_eq!(mal_resp.waves[0].specs.len(), 1);
    let ac201 = &mal_resp.waves[0].specs[0];
    assert_eq!(ac201.spec_id, "AC-201");
    assert_eq!(ac201.tasks[0].task_ref, "T-01");
    assert_eq!(ac201.tasks[0].status, "ready");
    assert_eq!(ac201.tasks[0].wave, 0);
    assert_eq!(ac201.tasks[1].task_ref, "T-02");
    assert_eq!(ac201.tasks[1].status, "planned");
    assert_eq!(ac201.tasks[1].wave, 1);
    assert_eq!(ac201.tasks[1].depends_on[0].status, "planned");
    assert_eq!(mal_resp.waves[1].wave, 1);
    assert_eq!(mal_resp.waves[1].specs.len(), 1);
    let ac200 = &mal_resp.waves[1].specs[0];
    assert_eq!(ac200.spec_id, "AC-200");
    assert_eq!(ac200.depends_on[0].status, "invalid");
    let mal_ready_resp = roadmap_query(
        &mal_ro,
        RoadmapReq {
            ready_only: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(mal_ready_resp.waves.len(), 1);
    assert_eq!(mal_ready_resp.waves[0].wave, 0);
    assert_eq!(mal_ready_resp.waves[0].specs.len(), 1);
    assert_eq!(mal_ready_resp.waves[0].specs[0].spec_id, "AC-201");
}

#[test]
#[cfg(unix)]
fn seam_roadmap_query_waves_archive_symlink_identity_and_non_utf8() {
    let adv_provider = mock_provider();

    let ws_tmp = make_temp_dir("roadmap-r7-git-identity");
    let ws_root = ws_tmp.path();
    let outside = make_temp_dir("roadmap-r7-outside");
    fs::write(outside.path().join("secret.md"), "# outside\n").unwrap();
    fs::create_dir_all(ws_root.join("docs/milestones/archive")).unwrap();
    fs::create_dir_all(ws_root.join("extensions/pkg/docs")).unwrap();
    fs::create_dir_all(ws_root.join("extensions/pkg/examples/archive")).unwrap();
    fs::write(
        ws_root.join("docs/milestones/archive/done.md"),
        "---\nid: AC-DONE\ntitle: Done\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        ws_root.join("docs/milestones/archive/ done.md"),
        "---\nid: AC-LEAD\ntitle: Leading Space\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    // Trailing space ("done.md ") is not a .md file and must NOT alias "done.md".
    fs::write(
        ws_root.join("docs/milestones/archive/done.md "),
        "not valid yaml frontmatter\n",
    )
    .unwrap();
    // Legacy/unparseable frontmatter in archive must NOT emit schema warnings in roadmap.
    fs::write(
        ws_root.join("docs/milestones/archive/legacy_invalid.md"),
        "---\nid: AC-LEGACY\ntitle: Old Milestone\nstatus: completed\nrelated_plans: [docs/plans/old.md]\nstarted_at: 2026-01-01\n---\n",
    )
    .unwrap();
    // Unrelated symlink outside extensions/*/docs/archive/ must be ignored cleanly.
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        ws_root.join("extensions/pkg/examples/archive/escape.md"),
    )
    .unwrap();
    let _ = init_git_repo(ws_root, "main");
    git_commit_all(ws_root, "r7");

    let mut r7_cfg = base_config(ws_root);
    with_ext_collection(&mut r7_cfg, "extensions/*/docs");
    let r7_ro = StateQueryEngine::open_read_only(r7_cfg, adv_provider.clone()).unwrap();
    let r7_resp = roadmap_query(
        &r7_ro,
        RoadmapReq {
            archived: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(r7_resp.archived_count, 2);
    let arch_ids: Vec<&str> = r7_resp
        .archived
        .iter()
        .map(|s| s.spec_id.as_str())
        .collect();
    assert_eq!(arch_ids, vec!["AC-DONE", "AC-LEAD"]);
    assert!(
        r7_resp.warnings.is_empty(),
        "unexpected warnings: {:?}",
        r7_resp.warnings
    );
    fs::write(
        ws_root.join("docs/milestones/archive/invalid-utf8.md"),
        [0xff],
    )
    .unwrap();
    git_commit_all(ws_root, "invalid utf8 archive");
    let mixed = roadmap_query(
        &r7_ro,
        RoadmapReq {
            archived: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(mixed.archived_count, 2);
    assert!(mixed
        .warnings
        .iter()
        .any(|warning| warning.contains("invalid-utf8.md")));

    // Round 8a: Committed-only symlink AT `docs/milestones/archive` itself fails closed.
    let arch_sym_tmp = make_temp_dir("roadmap-r8-archive-dir-sym");
    let arch_sym_root = arch_sym_tmp.path();
    fs::create_dir_all(arch_sym_root.join("docs/milestones")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        arch_sym_root.join("docs/milestones/archive"),
    )
    .unwrap();
    let _ = init_git_repo(arch_sym_root, "main");
    git_commit_all(arch_sym_root, "archive dir symlink");
    fs::remove_file(arch_sym_root.join("docs/milestones/archive")).unwrap();
    let arch_sym_ro = roadmap_engine(arch_sym_root, adv_provider.clone());
    let arch_sym_err = roadmap_query_err(&arch_sym_ro, RoadmapReq::default());
    assert_eq!(arch_sym_err.code, ErrorCode::SpecSchemaInvalid);
    assert!(
        arch_sym_err.message.contains("archive"),
        "expected 'archive' in error, got: {}",
        arch_sym_err.message
    );

    // Round 8c: Non-UTF-8 byte under constrained glob fails closed.
    let nonutf8_tmp = make_temp_dir("roadmap-r8-nonutf8");
    let nonutf8_root = nonutf8_tmp.path();
    git_index_nonutf8_archive(nonutf8_root, b"extensions/pkg\xff/docs/archive/bad.md");
    let mut nonutf8_cfg = base_config(nonutf8_root);
    with_ext_collection(&mut nonutf8_cfg, "extensions/pkg*/docs");
    assert_eq!(
        roadmap_query_err(
            &StateQueryEngine::open_read_only(nonutf8_cfg, adv_provider.clone()).unwrap(),
            RoadmapReq::default(),
        )
        .code,
        ErrorCode::SpecSchemaInvalid
    );
}

#[test]
fn seam_roadmap_query_waves_glob_roots_and_worktree_trailing_whitespace() {
    let adv_provider = mock_provider();

    // Round 8b: Whitespace-bearing glob root (`extensions/pkg /docs/archive/AC-SPACE.md`) works.
    let space_tmp = make_temp_dir("roadmap-r8-space-dir");
    let space_root = space_tmp.path();
    fs::create_dir_all(space_root.join("extensions/pkg /docs/archive")).unwrap();
    fs::write(
        space_root.join("extensions/pkg /docs/archive/AC-SPACE.md"),
        "---\nid: AC-SPACE\ntitle: Space Root\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::create_dir_all(space_root.join("docs/milestones")).unwrap();
    fs::write(
        space_root.join("docs/milestones/AC-USES-SPACE.md"),
        "---\nid: AC-USES-SPACE\ntitle: Uses Space Archive\nstatus: planned\ndepends_on: [AC-SPACE]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(space_root, "main");
    git_commit_all(space_root, "space root");
    let mut space_cfg = base_config(space_root);
    with_ext_collection(&mut space_cfg, "extensions/*/docs");
    let space_ro = StateQueryEngine::open_read_only(space_cfg, adv_provider.clone()).unwrap();
    let space_resp = roadmap_query(
        &space_ro,
        RoadmapReq {
            archived: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(space_resp.archived_count, 1);
    assert_eq!(space_resp.waves[0].specs[0].spec_id, "AC-USES-SPACE");
    assert!(space_resp.waves[0].specs[0].depends_on[0].satisfied);

    // Round 9a: `literal_git_prefix` trailing whitespace (`extensions/pkg /*/docs` ->
    // `"extensions/pkg "`) is preserved by `list_git_tree_entries` for committed archives.
    let pfx_tmp = make_temp_dir("roadmap-r9-prefix-space");
    let pfx_root = pfx_tmp.path();
    fs::create_dir_all(pfx_root.join("docs/milestones")).unwrap();
    fs::create_dir_all(pfx_root.join("extensions/pkg /team/docs/archive")).unwrap();
    fs::write(
        pfx_root.join("extensions/pkg /team/docs/archive/AC-PREFIX-SPACE.md"),
        "---\nid: AC-PREFIX-SPACE\ntitle: Prefix Space Archived\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        pfx_root.join("docs/milestones/AC-USES-PREFIX-SPACE.md"),
        "---\nid: AC-USES-PREFIX-SPACE\ntitle: Uses Prefix Space\nstatus: planned\ndepends_on: [\"AC-PREFIX-SPACE\"]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(pfx_root, "main");
    git_ok(pfx_root, &["add", "-A"]);
    git_ok(
        pfx_root,
        &["commit", "-m", "committed prefix-space archive"],
    );
    fs::remove_dir_all(pfx_root.join("extensions")).unwrap();

    let mut pfx_cfg = base_config(pfx_root);
    with_ext_collection(&mut pfx_cfg, "extensions/pkg /*/docs");
    let pfx_ro = StateQueryEngine::open_read_only(pfx_cfg, adv_provider.clone()).unwrap();
    let pfx_resp = roadmap_query(&pfx_ro, RoadmapReq::default());
    assert_eq!(pfx_resp.archived_count, 1);
    assert_eq!(pfx_resp.waves[0].specs[0].spec_id, "AC-USES-PREFIX-SPACE");
    assert!(pfx_resp.waves[0].specs[0].depends_on[0].satisfied);

    // Round 11: Primary repo directory ending with `\n` (`owner\n`) and `\r` (`owner\r`)
    // preserves its trailing control byte in `git_rev_parse_path_exact` when resolved from a linked checkout.
    for (suffix, spec_id) in [("\n", "LF-001"), ("\r", "CR-001")] {
        let nl_tmp = make_temp_dir("roadmap-r11-nl");
        let nl_pri = nl_tmp.path().join(format!("owner{suffix}"));
        let nl_linked = nl_tmp.path().join("linked-nl");
        fs::create_dir_all(nl_pri.join("docs/milestones")).unwrap();
        let _ = init_git_repo(&nl_pri, "main");
        git_ok(&nl_pri, &["commit", "--allow-empty", "-m", "init"]);
        git_ok(
            &nl_pri,
            &[
                "worktree",
                "add",
                "-b",
                "feat-nl",
                nl_linked.to_str().unwrap(),
            ],
        );
        fs::write(
            nl_pri.join(format!("docs/milestones/{spec_id}.md")),
            format!("---\nid: {spec_id}\ntitle: Primary NL\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/nl/**]\nacceptance_criteria: [ok]\n```\n"),
        )
        .unwrap();
        let resolved_nl = acdd_core::git::primary_worktree_root(&nl_linked).unwrap();
        assert_eq!(resolved_nl, nl_pri.canonicalize().unwrap());
        let nl_ro = roadmap_engine(&nl_linked, adv_provider.clone());
        let nl_resp = roadmap_query(&nl_ro, RoadmapReq::default());
        let nl_ids: Vec<&str> = nl_resp
            .waves
            .iter()
            .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
            .collect();
        assert!(
            nl_ids.contains(&spec_id),
            "expected {spec_id} from primary worktree with trailing control byte, got: {nl_ids:?}"
        );
    }
}

#[test]
fn seam_roadmap_query_waves_stale_and_same_basename_sibling_checkouts() {
    let adv_provider = mock_provider();

    // Round 9b: `enumerate_worktree_roots` preserves trailing whitespace on worktree paths
    // (`wt-space `) via `-z` and does not conflate with unrelated sibling `wt-space`.
    let wt_tmp = make_temp_dir("roadmap-r9-wt-space");
    let wt_pri = wt_tmp.path().join("repo");
    let wt_space_dir = wt_tmp.path().join("wt-space ");
    let wt_sibling_dir = wt_tmp.path().join("wt-space");
    fs::create_dir_all(wt_pri.join("docs/milestones")).unwrap();
    fs::write(
        wt_pri.join("docs/milestones/AC-PRI.md"),
        "---\nid: AC-PRI\ntitle: Primary\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(&wt_pri, "main");
    git_commit_all(&wt_pri, "init");
    git_ok(
        &wt_pri,
        &[
            "worktree",
            "add",
            "-b",
            "feat-space",
            wt_space_dir.to_str().unwrap(),
        ],
    );
    fs::write(
        wt_space_dir.join("docs/milestones/AC-WT-SPACE.md"),
        "---\nid: AC-WT-SPACE\ntitle: Trailing Space WT\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/space/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    // Unrelated sibling without trailing space must NOT be discovered.
    fs::create_dir_all(wt_sibling_dir.join("docs/milestones")).unwrap();
    fs::write(
        wt_sibling_dir.join("docs/milestones/AC-SIBLING.md"),
        "---\nid: AC-SIBLING\ntitle: Sibling\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/sib/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();

    let wt_ro =
        StateQueryEngine::open_read_only(base_config(&wt_pri), adv_provider.clone()).unwrap();
    let wt_resp = roadmap_query(&wt_ro, RoadmapReq::default());
    let discovered_ids: Vec<&str> = wt_resp
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert!(
        discovered_ids.contains(&"AC-WT-SPACE"),
        "expected AC-WT-SPACE from trailing-space worktree, got: {discovered_ids:?}"
    );
    assert!(
        !discovered_ids.contains(&"AC-SIBLING"),
        "sibling wt-space must not be conflated with wt-space , got: {discovered_ids:?}"
    );

    // Round 10a: Stale registered worktree replaced by a separate independent repository
    // at the same path (`wt-space `) must be rejected by `list_owned_worktrees`.
    let wt_moved_dir = wt_tmp.path().join("wt-space-moved");
    fs::rename(&wt_space_dir, &wt_moved_dir).unwrap();
    fs::create_dir_all(wt_space_dir.join("docs/milestones")).unwrap();
    fs::write(
        wt_space_dir.join("docs/milestones/ALIEN-001.md"),
        "---\nid: ALIEN-001\ntitle: Alien Repo\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/alien/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(&wt_space_dir, "main");
    git_commit_all(&wt_space_dir, "alien");
    let stale_resp = roadmap_query(&wt_ro, RoadmapReq::default());
    let stale_ids: Vec<&str> = stale_resp
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert!(
        !stale_ids.contains(&"ALIEN-001"),
        "foreign repo at stale worktree path must not be scanned, got: {stale_ids:?}"
    );
    fs::remove_dir_all(&wt_moved_dir).unwrap();

    // Round 10b: Linked checkout whose primary root has trailing space (`owner `)
    // resolves `owner ` (not unrelated sibling `owner`) and never scans `OTHER-001`.
    let own_tmp = make_temp_dir("roadmap-r10-owner-space");
    let own_pri = own_tmp.path().join("owner ");
    let own_sibling = own_tmp.path().join("owner");
    let own_linked = own_tmp.path().join("linked-wt");
    fs::create_dir_all(own_pri.join("docs/milestones")).unwrap();
    fs::write(
        own_pri.join("docs/milestones/AC-OWNER.md"),
        "---\nid: AC-OWNER\ntitle: Owner Space\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/owner/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(&own_pri, "main");
    git_commit_all(&own_pri, "owner");
    git_ok(
        &own_pri,
        &[
            "worktree",
            "add",
            "-b",
            "feat-linked",
            own_linked.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(own_sibling.join("docs/milestones")).unwrap();
    fs::write(
        own_sibling.join("docs/milestones/OTHER-001.md"),
        "---\nid: OTHER-001\ntitle: Sibling Owner\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/other/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let _ = init_git_repo(&own_sibling, "main");

    let resolved_pri = acdd_core::git::primary_worktree_root(&own_linked).unwrap();
    assert_eq!(resolved_pri, own_pri.canonicalize().unwrap());
    let linked_ro = roadmap_engine(&own_linked, adv_provider.clone());
    let linked_resp = roadmap_query(&linked_ro, RoadmapReq::default());
    let linked_ids: Vec<&str> = linked_resp
        .waves
        .iter()
        .flat_map(|w| w.specs.iter().map(|s| s.spec_id.as_str()))
        .collect();
    assert!(
        linked_ids.contains(&"AC-OWNER"),
        "expected AC-OWNER from primary worktree 'owner ', got: {linked_ids:?}"
    );
    assert!(
        !linked_ids.contains(&"OTHER-001"),
        "sibling repo 'owner' must not be scanned, got: {linked_ids:?}"
    );

    // Round 12: Same-basename sibling checkouts (`left/checkout` and `right/checkout`)
    // with divergent copies of a spec (absent from primary) disambiguate labels,
    // preserve both copies in `divergence`, mark exactly one `canonical: true`,
    // and emit a `diverges across worktrees` warning.
    let sib_tmp = make_temp_dir("roadmap-r12-same-basename");
    let sib_pri = sib_tmp.path().join("repo");
    let sib_left = sib_tmp.path().join("left/checkout");
    let sib_right = sib_tmp.path().join("right/checkout");
    fs::create_dir_all(sib_pri.join("docs/milestones")).unwrap();
    let _ = init_git_repo(&sib_pri, "main");
    git_ok(&sib_pri, &["commit", "--allow-empty", "-m", "init"]);
    git_ok(
        &sib_pri,
        &[
            "worktree",
            "add",
            "-b",
            "feat-left",
            sib_left.to_str().unwrap(),
        ],
    );
    git_ok(
        &sib_pri,
        &[
            "worktree",
            "add",
            "-b",
            "feat-right",
            sib_right.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(sib_left.join("docs/milestones")).unwrap();
    fs::create_dir_all(sib_right.join("docs/milestones")).unwrap();
    fs::write(
        sib_left.join("docs/milestones/AC-DIV.md"),
        "---\nid: AC-DIV\ntitle: Left Copy\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/left/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    fs::write(
        sib_right.join("docs/milestones/AC-DIV.md"),
        "---\nid: AC-DIV\ntitle: Right Copy\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/right/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let sib_ro =
        StateQueryEngine::open_read_only(base_config(&sib_pri), adv_provider.clone()).unwrap();
    let sib_resp = roadmap_query(&sib_ro, RoadmapReq::default());
    let div_spec = sib_resp
        .waves
        .iter()
        .flat_map(|w| w.specs.iter())
        .find(|s| s.spec_id == "AC-DIV")
        .unwrap();
    assert_eq!(div_spec.divergence.len(), 2);
    assert_ne!(
        div_spec.divergence[0].worktree,
        div_spec.divergence[1].worktree
    );
    assert_eq!(
        div_spec.divergence.iter().filter(|d| d.canonical).count(),
        1
    );
    assert!(sib_resp
        .warnings
        .iter()
        .any(|w| w.contains("AC-DIV") && w.contains("diverges across worktrees")));
}

#[test]
fn seam_roadmap_query_waves_pre_task_barrier_and_worktree_scope_overlap() {
    // Round 13: Active run with unfinished pre-task `blueprint_synthesis` barrier keeps
    // dependency-free tasks as `"planned"` until the barrier clears, then projects `"ready"`.
    let bp_tmp = make_temp_dir("roadmap-r13-pretask-barrier");
    let bp_root = bp_tmp.path();
    fs::create_dir_all(bp_root.join("docs/milestones")).unwrap();
    fs::write(
        bp_root.join("docs/milestones/AC-BP-001.md"),
        "---\nid: AC-BP-001\ntitle: Barrier Spec\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/bp/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let bp_profile = write_profile(
        bp_root,
        "bp-barrier.yaml",
        "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - id: blueprint_synthesis\n    gates:\n      - id: synthesize\n        proof: none\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
    );
    let bp_cfg = base_config(bp_root);
    let (_, bp_provider) = build_provider_with_mock(&bp_cfg.modules);
    let _ = init_git_repo(bp_root, "main");
    git_commit_all(bp_root, "init");
    let bp_rw = StateEngine::open(bp_cfg.clone(), bp_provider.clone()).unwrap();
    let bp_started = start_run(
        &bp_rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-BP-001.md".to_owned()),
            workflow: Some(bp_profile),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();

    let bp_ro = StateQueryEngine::open_read_only(bp_cfg, bp_provider).unwrap();
    let before_resp = roadmap_query(
        &bp_ro,
        RoadmapReq {
            spec_id: Some("AC-BP-001".to_owned()),
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(before_resp.in_progress[0].tasks[0].task_ref, "T-01");
    assert_eq!(before_resp.in_progress[0].tasks[0].status, "planned");

    {
        let (node, worker) = ("blueprint_synthesis.synthesize", "arch");
        claim_node(
            &bp_rw,
            bp_started.run.run_id.as_str(),
            node,
            worker,
            None,
            false,
            false,
            None,
        )
        .unwrap();
        bp_rw
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(bp_started.run.run_id.as_str().to_owned()),
                node: node.to_owned(),
                action: SubmitAction::Pass,
                worker_id: WorkerId::new(worker).unwrap(),
                claim_revision: None,
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
    }

    let after_resp = roadmap_query(
        &bp_ro,
        RoadmapReq {
            spec_id: Some("AC-BP-001".to_owned()),
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(after_resp.in_progress[0].tasks[0].task_ref, "T-01");
    assert_eq!(after_resp.in_progress[0].tasks[0].status, "ready");

    // Round 14: In Worktree isolation, claiming `T-01` (`crates/ov/**`) marks overlapping
    // dependency-free `T-02` (`crates/ov/sub/**`) as `"planned"` until `T-01` completes.
    let ov_tmp = make_temp_dir("roadmap-r14-claimed-overlap");
    let ov_root = ov_tmp.path();
    fs::create_dir_all(ov_root.join("docs/milestones")).unwrap();
    fs::write(
        ov_root.join("docs/milestones/AC-OV-001.md"),
        "---\nid: AC-OV-001\ntitle: Overlap Claimed\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/ov/**]\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\nscope: [crates/ov/sub/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let ov_profile = write_profile(
        ov_root,
        "ov-worktree.yaml",
        "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n          pass: deliver\n        - id: deliver\n          proof: none\n          need: [build]\n          receipt: true\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
    );
    let ov_cfg = base_config(ov_root);
    let (_, ov_provider) = build_provider_with_mock(&ov_cfg.modules);
    let _ = init_git_repo(ov_root, "main");
    git_commit_all(ov_root, "init");
    let ov_rw = StateEngine::open(ov_cfg.clone(), ov_provider.clone()).unwrap();
    let ov_started = start_run(
        &ov_rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-OV-001.md".to_owned()),
            workflow: Some(ov_profile),
            in_place: false,
            ..StartReq::default()
        },
    )
    .unwrap();
    let ov_run_id = ov_started.run.run_id.as_str();
    let ov_wt = ov_root.join(".worktrees/ov-t1");
    git_ok(
        ov_root,
        &["worktree", "add", "-b", "ov-t1", ov_wt.to_str().unwrap()],
    );
    claim_node(
        &ov_rw,
        ov_run_id,
        "T-01.build",
        "w-ov",
        None,
        false,
        false,
        Some(ov_wt.clone()),
    )
    .unwrap();

    let ov_ro = StateQueryEngine::open_read_only(ov_cfg, ov_provider).unwrap();
    let claimed_resp = roadmap_query(
        &ov_ro,
        RoadmapReq {
            spec_id: Some("AC-OV-001".to_owned()),
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(claimed_resp.in_progress[0].tasks[0].task_ref, "T-01");
    assert_eq!(claimed_resp.in_progress[0].tasks[0].status, "in_progress");
    assert_eq!(claimed_resp.in_progress[0].tasks[1].task_ref, "T-02");
    assert_eq!(claimed_resp.in_progress[0].tasks[1].status, "planned");

    ov_rw
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(ov_run_id.to_owned()),
            node: "T-01.build".to_owned(),
            action: SubmitAction::Pass,
            worker_id: WorkerId::new("w-ov").unwrap(),
            claim_revision: None,
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: Some(ov_wt.clone()),
        })))
        .unwrap();
    claim_node(
        &ov_rw,
        ov_run_id,
        "T-01.deliver",
        "w-ov",
        None,
        false,
        false,
        Some(ov_wt.clone()),
    )
    .unwrap();
    ov_rw
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            run: Some(ov_run_id.to_owned()),
            node: "T-01.deliver".to_owned(),
            action: SubmitAction::Pass,
            worker_id: WorkerId::new("w-ov").unwrap(),
            claim_revision: None,
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: None,
            allow_main: false,
            worktree: Some(ov_wt),
        })))
        .unwrap();

    let done_resp = roadmap_query(
        &ov_ro,
        RoadmapReq {
            spec_id: Some("AC-OV-001".to_owned()),
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert_eq!(done_resp.in_progress[0].tasks[0].status, "completed");
    assert_eq!(done_resp.in_progress[0].tasks[1].status, "ready");
}

#[test]
fn seam_roadmap_query_waves_live_dependency_sync_and_reverse_blockers() {
    // Round 15: Active-run task readiness obeys live `graph.dependencies` in SQLite,
    // NOT unsynced disk edits; removing `T-02`'s dependency on disk keeps `T-02`
    // `"planned"` until `StateCommand::Sync` updates the run graph.
    let sync_tmp = make_temp_dir("roadmap-r15-live-deps-sync");
    let sync_root = sync_tmp.path();
    fs::create_dir_all(sync_root.join("docs/milestones")).unwrap();
    let sync_spec = sync_root.join("docs/milestones/AC-SYNC-001.md");
    let producer_path = "docs/milestones/AC-SYNC-000.md";
    fs::write(
        sync_root.join(producer_path),
        "---\nid: AC-SYNC-000\ntitle: Producer\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let producer_source = fs::read_to_string(sync_root.join(producer_path)).unwrap();
    fs::create_dir(sync_root.join("docs/plans")).unwrap();
    fs::write(
        sync_root.join("docs/plans/PLAN-NEXT.md"),
        producer_source.replace("AC-SYNC-000", "PLAN-NEXT"),
    )
    .unwrap();
    fs::create_dir(sync_root.join("docs/milestones/archive")).unwrap();
    fs::write(
        sync_root.join("docs/milestones/archive/AC-PREREQ.md"),
        producer_source
            .replace("AC-SYNC-000", "AC-PREREQ")
            .replace("status: planned", "status: completed"),
    )
    .unwrap();
    let consumer_source = producer_source
        .replace("AC-SYNC-000", "AC-SYNC-001")
        .replace("title: Producer", "title: Live Dep Sync")
        .replace(
            "status: planned",
            "status: planned\ndepends_on: [AC-PREREQ]",
        );
    let consumer_source = format!(
        "{consumer_source}### task: T-02\n{fence}yaml\ntask_ref: T-02\ndepends_on: [T-01, 'AC-SYNC-000:T-01']\nscope: [crates/s2/**]\nacceptance_criteria: [ok]\n{fence}\n",
        fence = "```",
    );
    fs::write(&sync_spec, &consumer_source).unwrap();
    let sync_profile = write_profile(
        sync_root,
        "sync-p.yaml",
        "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
    );
    let mut sync_cfg = base_config(sync_root);
    sync_cfg.spec_collections = acdd_core::config::default_specs();
    sync_cfg.state_db_override = Some(sync_root.join(".acdd/custom-state.sqlite"));
    let (_, sync_provider) = build_provider_with_mock(&sync_cfg.modules);
    let _ = init_git_repo(sync_root, "main");
    git_commit_all(sync_root, "init");
    let sync_rw = StateEngine::open(sync_cfg.clone(), sync_provider.clone()).unwrap();
    let producer = start_run(
        &sync_rw,
        StartReq {
            spec_path: Some(producer_path.to_owned()),
            workflow: Some(sync_profile.clone()),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let sync_started = start_run(
        &sync_rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-SYNC-001.md".to_owned()),
            workflow: Some(sync_profile),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let sync_run_id = sync_started.run.run_id.as_str().to_owned();

    let removed_edges =
        consumer_source.replace("depends_on: [T-01, 'AC-SYNC-000:T-01']", "depends_on: []");
    fs::write(
        &sync_spec,
        removed_edges
            .replace(
                "status: planned",
                "status: planned\nrepositories: [{name: invalid, path: missing}]",
            )
            .replace(
                "depends_on: [AC-PREREQ]",
                "depends_on: [AC-PREREQ, PLAN-NEXT, invalid/AC-999]",
            )
            .replace(
                "scope: [src/**]",
                "depends_on: ['AC-SYNC-000:T-01', 'invalid/AC-999:T-01']\nscope: [src/**]",
            ),
    )
    .unwrap();

    let sync_ro = StateQueryEngine::open_read_only(sync_cfg, sync_provider.clone()).unwrap();
    let unsynced_resp = roadmap_query(
        &sync_ro,
        RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    let consumer = unsynced_resp
        .in_progress
        .iter()
        .find(|s| s.spec_id == "AC-SYNC-001")
        .unwrap();
    let producer_row = unsynced_resp
        .in_progress
        .iter()
        .find(|s| s.spec_id == "AC-SYNC-000")
        .unwrap();
    assert_eq!(consumer.total_tasks, 2);
    assert_eq!(consumer.depends_on[0].raw, "AC-PREREQ");
    assert!(consumer.depends_on[0].satisfied);
    assert_eq!(consumer.depends_on[1].raw, "PLAN-NEXT");
    assert_eq!(consumer.depends_on[1].status, "planned");
    assert!(!consumer.depends_on[1].satisfied);
    assert_eq!(consumer.depends_on[2].status, "invalid");
    assert!(unsynced_resp
        .warnings
        .iter()
        .any(|warning| { warning.contains(&sync_run_id) && warning.contains("invalid/AC-999") }));
    assert_eq!(consumer.tasks[0].blocks, vec!["T-02"]);
    assert!(consumer.tasks[0].depends_on.is_empty());
    assert_eq!(consumer.tasks[1].status, "planned");
    assert_eq!(consumer.tasks[1].wave, 1);
    assert!(consumer.tasks[1]
        .depends_on
        .iter()
        .any(|dep| dep.raw == "T-01"));
    assert!(consumer.tasks[1].depends_on.iter().any(|dep| {
        dep.raw.ends_with(&format!("{}:T-01", producer.run.run_id)) && !dep.satisfied
    }));
    assert_eq!(producer_row.tasks[0].blocks, ["AC-SYNC-001:T-02"]);
    assert_eq!(producer_row.unblocks_specs, 1);

    fs::write(&sync_spec, removed_edges).unwrap();
    sync_rw
        .execute(StateCommand::Sync(acdd_state::usecases::SyncReq {
            run: Some(sync_run_id),
            rebind_workflow: false,
        }))
        .unwrap();
    let synced_resp = roadmap_query(
        &sync_ro,
        RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    let consumer = synced_resp
        .in_progress
        .iter()
        .find(|s| s.spec_id == "AC-SYNC-001")
        .unwrap();
    let producer_row = synced_resp
        .in_progress
        .iter()
        .find(|s| s.spec_id == "AC-SYNC-000")
        .unwrap();
    assert!(consumer.tasks[0].blocks.is_empty());
    assert_eq!(consumer.depends_on.len(), 1);
    assert!(consumer.depends_on[0].satisfied);
    assert_eq!(consumer.tasks[1].status, "ready");
    assert_eq!(consumer.tasks[1].wave, 0);
    assert!(consumer.tasks[1].depends_on.is_empty());
    assert!(producer_row.tasks[0].blocks.is_empty());
    assert_eq!(producer_row.unblocks_specs, 0);

    // The linked checkout lacks the producer task on disk. Its shared DB still provides it.
    let lw_tmp = make_temp_dir("roadmap-r16-linked-wt");
    let lw_ext_pri = lw_tmp.path().join("ext-pri");
    let lw_ext_sec = lw_tmp.path().join("ext-sec");
    let lw_main = lw_tmp.path().join("main-ws");
    fs::create_dir_all(lw_ext_pri.join("docs/milestones")).unwrap();
    fs::write(
        lw_ext_pri.join("acdd.yaml"),
        "roots: [.]\nmodules:\n  code: false\n  docs: false\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [EXT]\n    digits: 3\n",
    )
    .unwrap();
    fs::write(
        lw_ext_pri.join("docs/milestones/EXT-010.md"),
        "---\nid: EXT-010\ntitle: External\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    )
    .unwrap();
    let ext_prof = write_profile(
        &lw_ext_pri,
        "ext-p.yaml",
        "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n          pass: deliver\n        - id: deliver\n          proof: none\n          need: [build]\n          receipt: true\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n",
    );
    let _ = init_git_repo(&lw_ext_pri, "main");
    git_commit_all(&lw_ext_pri, "ext init");
    git_ok(
        &lw_ext_pri,
        &[
            "worktree",
            "add",
            "-b",
            "ext-sec",
            lw_ext_sec.to_str().unwrap(),
        ],
    );
    let ext_cfg = base_config(&lw_ext_pri);
    let (_, ext_provider) = build_provider_with_mock(&ext_cfg.modules);
    let ext_rw = StateEngine::open(ext_cfg.clone(), ext_provider).unwrap();
    let ext_started = start_run(
        &ext_rw,
        StartReq {
            spec_path: Some("docs/milestones/EXT-010.md".to_owned()),
            workflow: Some(ext_prof),
            in_place: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let ext_run_id = ext_started.run.run_id.as_str().to_owned();
    for gate in ["T-01.build", "T-01.deliver"] {
        claim_node(
            &ext_rw,
            &ext_run_id,
            gate,
            "w-ext",
            None,
            false,
            false,
            None,
        )
        .unwrap();
        ext_rw
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(ext_run_id.clone()),
                node: gate.to_owned(),
                action: SubmitAction::Pass,
                worker_id: WorkerId::new("w-ext").unwrap(),
                claim_revision: None,
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
    }

    fs::create_dir_all(lw_main.join("docs/milestones")).unwrap();
    let ext_copy = lw_ext_sec.join("docs/milestones/EXT-010.md");
    fs::write(
        &ext_copy,
        fs::read_to_string(&ext_copy)
            .unwrap()
            .replace("T-01", "T-02"),
    )
    .unwrap();
    let linked_source = "---\nid: MAIN-001\ntitle: Main\nstatus: planned\nrepositories: [{name: ext, path: ../ext-sec}]\ndepends_on: ['ext/EXT-010:T-01']\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: ['ext/EXT-010:T-01']\nscope: [src/m/**]\nacceptance_criteria: [ok]\n```\n";
    fs::write(lw_main.join("docs/milestones/MAIN-001.md"), linked_source).unwrap();
    let linked_doc = acdd_spec::parse_spec(linked_source).unwrap();
    let issues = acdd_state::usecases::overlay::validate_spec_dependencies(
        &lw_main,
        &acdd_core::config::load_adapter(&lw_main, None).unwrap(),
        None,
        None,
        &linked_doc,
        std::slice::from_ref(&linked_doc),
    );
    assert!(issues.is_empty(), "{issues:?}");
    let archive = "docs/milestones/archive/EXT-011.md";
    let archived_doc = "---\nid: EXT-011\ntitle: Closed\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\nscope: [src/**]\nstatus: planned\nacceptance_criteria: [ok]\n```\n";
    write_spec(&lw_ext_pri, archive, archived_doc);
    git_commit_all(&lw_ext_pri, "closed archive");
    let closed = git_ok(&lw_ext_pri, &["rev-parse", "HEAD"]);
    let mut graph = build_rich_graph(
        &lw_ext_pri,
        "run-ext-closed",
        acdd_state::types::IsolationMode::InPlace,
    );
    graph.run.run_name = RunName::new("ext-closed").unwrap();
    graph.run.worktree = None;
    let mut tomb = sample_tombstone(
        "run-ext-closed",
        "ext-closed",
        Some("EXT-011"),
        acdd_state::types::TerminalStatus::Completed,
        Some(archive),
    );
    tomb.closed_commit = Some(acdd_core::ids::CommitOid::new(closed.trim()).unwrap());
    backend_for(&ext_cfg)
        .exec_tx(|tx| {
            tx.insert_run_graph(&graph)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &tomb)
        })
        .unwrap();
    write_spec(
        &lw_ext_pri,
        archive,
        &archived_doc.replace(
            "status: completed\nacceptance_criteria",
            "status: planned\nacceptance_criteria",
        ),
    );
    git_commit_all(&lw_ext_pri, "change archive after closure");
    for (task, expected_ok) in [("T-01", true), ("T-02", false), ("missing", false)] {
        let source = linked_source.replace("EXT-010:T-01", &format!("EXT-011:{task}"));
        let doc = acdd_spec::parse_spec(&source).unwrap();
        let issues = acdd_state::usecases::overlay::validate_spec_dependencies(
            &lw_main,
            &acdd_core::config::load_adapter(&lw_main, None).unwrap(),
            None,
            None,
            &doc,
            std::slice::from_ref(&doc),
        );
        assert_eq!(issues.is_empty(), expected_ok, "{task}: {issues:?}");
    }
    fs::write(
        lw_ext_sec.join("docs/milestones/EXT-bad.md"),
        "# Missing frontmatter",
    )
    .unwrap();
    let lw_main_cfg = base_config(&lw_main);
    let lw_ro =
        StateQueryEngine::open_read_only(lw_main_cfg.clone(), sync_provider.clone()).unwrap();
    let lw_resp = roadmap_query(&lw_ro, RoadmapReq::default());
    assert_eq!(lw_resp.waves[0].wave, 0);
    assert_eq!(lw_resp.waves[0].specs[0].spec_id, "MAIN-001");
    assert!(lw_resp.waves[0].specs[0].depends_on[0].satisfied);
    assert!(lw_resp
        .warnings
        .iter()
        .any(|warning| warning.contains("EXT-bad.md")));

    let outside_spec = lw_tmp.path().join("outside.md");
    fs::write(&outside_spec, "# Outside\n").unwrap();
    #[cfg(unix)]
    {
        for (checkout, link, target, diagnostic) in [
            (
                "ext-sec",
                lw_ext_sec.join("docs/milestones/EXT-999.md"),
                outside_spec,
                "must not be a symlink",
            ),
            (
                "ext-dangling",
                lw_tmp.path().join("ext-dangling"),
                lw_tmp.path().join("absent"),
                "invalid or confined-check failed",
            ),
        ] {
            std::os::unix::fs::symlink(target, &link).unwrap();
            fs::write(
                lw_main.join("docs/milestones/MAIN-001.md"),
                linked_source.replace("../ext-sec", &format!("../{checkout}")),
            )
            .unwrap();
            let error = roadmap_query_err(&lw_ro, RoadmapReq::default());
            assert_eq!(error.code, ErrorCode::SpecSchemaInvalid);
            assert!(error.message.contains(diagnostic), "{error}");
            fs::remove_file(link).unwrap();
        }
    }
}

const MULTI_RUN_SOURCE: &str = "---\nid: AC-MULTI\ntitle: Multi Run\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ntarget: Edit M\ndepends_on: ['AC-ARC:T-01']\nscope: [crates/m/**]\nacceptance_criteria: [ok]\n```\n";
const MULTI_ARC_SOURCE: &str = "---\nid: AC-ARC\ntitle: Archive producer\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n";
const MULTI_PROFILE_YAML: &str = "version: 2\ninherit: base\ncommands:\n  test: { command: \"true\", test_filter: \"--filter {test}\" }\n  build_check: { command: \"true\" }\n  verify_all: { command: \"true\" }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          proof: none\n  - id: handoff\n    proof: none\n    save: { archive: true, prune: true }\n";

struct MultiRunFixture {
    _tmp: TempDir,
    root: PathBuf,
    prof: String,
    cfg: StateEngineConfig,
    rw: StateEngine,
    ro: StateQueryEngine,
}

impl MultiRunFixture {
    fn new(label: &str) -> Self {
        let tmp = make_temp_dir(label);
        let root = acdd_state::git_ops::worktree::canonicalize_macos_var(tmp.path());
        fs::create_dir_all(root.join("docs/milestones")).unwrap();
        fs::write(root.join("docs/milestones/AC-MULTI.md"), MULTI_RUN_SOURCE).unwrap();
        fs::write(root.join("docs/milestones/AC-ARC.md"), MULTI_ARC_SOURCE).unwrap();
        let prof = write_profile(&root, "multi-p.yaml", MULTI_PROFILE_YAML);
        let mut cfg = base_config(&root);
        cfg.state_db_override = Some(root.join(".acdd/custom-state.sqlite"));
        let (_, provider) = build_provider_with_mock(&cfg.modules);
        let _ = init_git_repo(&root, "main");
        git_commit_all(&root, "init");
        fs::create_dir(root.join(".acdd/state.sqlite")).unwrap();
        let rw = StateEngine::open(cfg.clone(), provider.clone()).unwrap();
        let ro = StateQueryEngine::open_read_only(cfg.clone(), provider).unwrap();
        Self {
            _tmp: tmp,
            root,
            prof,
            cfg,
            rw,
            ro,
        }
    }

    fn start_sibling_run(&self) -> (acdd_state::usecases::StartResp, PathBuf) {
        let run_b = start_run(
            &self.rw,
            StartReq {
                spec_path: Some("docs/milestones/AC-MULTI.md".to_owned()),
                run_name: Some(RunName::new("run-b").unwrap()),
                workflow: Some(self.prof.clone()),
                in_place: false,
                worktree: true,
                ..StartReq::default()
            },
        )
        .unwrap();
        let wt_b = PathBuf::from(run_b.run.worktree.as_deref().unwrap());
        fs::create_dir_all(wt_b.join("custom/specs/archive")).unwrap();
        fs::write(
            wt_b.join("acdd.yaml"),
            "roots: [.]\nspecs:\n  sibling:\n    path: custom/specs\n",
        )
        .unwrap();
        fs::write(
            wt_b.join("custom/specs/AC-MULTI.md"),
            MULTI_RUN_SOURCE
                .replace("Multi Run", "Sibling Run")
                .replace("Edit M", "Edit N")
                .replace("crates/m/**", "crates/n/**")
                .replace(
                    "status: planned",
                    "status: planned\nrepositories: [{name: sibling, path: .}]\ndepends_on: [sibling/AC-ARC, sibling/AC-ABSENT]",
                ),
        )
        .unwrap();
        (run_b, wt_b)
    }

    fn start_wrong_run(&self, id: &str, in_place: bool) -> acdd_state::usecases::StartResp {
        let started = start_run(
            &self.rw,
            StartReq {
                spec_path: Some(format!("docs/milestones/{id}.md")),
                run_name: Some(RunName::new(format!("wrong-{id}")).unwrap()),
                workflow: Some(self.prof.clone()),
                in_place,
                worktree: !in_place,
                ..StartReq::default()
            },
        )
        .unwrap();
        let run_id = started.run.run_id.as_str();
        claim_node(
            &self.rw,
            run_id,
            "T-01.build",
            "wrong-worker",
            None,
            false,
            false,
            None,
        )
        .unwrap();
        self.rw
            .execute(StateCommand::Submit(Box::new(SubmitReq {
                run: Some(run_id.to_owned()),
                node: "T-01.build".to_owned(),
                action: SubmitAction::Pass,
                worker_id: WorkerId::new("wrong-worker").unwrap(),
                claim_revision: None,
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
        started
    }
}

#[test]
fn seam_roadmap_query_waves_same_id_runs_and_wrong_checkout_isolation() {
    let fx = MultiRunFixture::new("roadmap-r16-same-id");
    let multi_root = &fx.root;
    let multi_path = "docs/milestones/AC-MULTI.md";
    let sibling_path = "custom/specs/AC-MULTI.md";
    let run_a = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some(multi_path.to_owned()),
            run_name: Some(RunName::new("run-a").unwrap()),
            workflow: Some(fx.prof.clone()),
            in_place: false,
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let (run_b, wt_b) = fx.start_sibling_run();
    fs::write(
        wt_b.join("custom/specs/archive/AC-ARC.md"),
        MULTI_ARC_SOURCE
            .replace("status: planned", "status: completed")
            .replace("task_ref: T-01", "task_ref: T-01\nstatus: completed"),
    )
    .unwrap();
    let query_multi = || {
        roadmap_query(
            &fx.ro,
            RoadmapReq {
                spec_id: Some("AC-MULTI".to_owned()),
                ..RoadmapReq::default()
            },
        )
    };
    let uncommitted = query_multi();
    let sibling = uncommitted
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(run_b.run.run_id.as_str()))
        .unwrap();
    assert_eq!(sibling.depends_on[0].status, "missing");
    assert!(!sibling.depends_on[0].satisfied);
    assert_eq!(sibling.tasks[0].status, "planned");
    git_ok(&wt_b, &["add", "custom/specs"]);
    git_ok(
        &wt_b,
        &["commit", "-m", "Archive producer in sibling collection"],
    );
    fs::remove_file(wt_b.join("custom/specs/archive/AC-ARC.md")).unwrap();
    fs::write(
        multi_root.join("docs/milestones/AC-ABSENT.md"),
        MULTI_ARC_SOURCE.replace("AC-ARC", "AC-ABSENT"),
    )
    .unwrap();
    let sibling_rel = wt_b.strip_prefix(multi_root).unwrap().display();
    let planned_deps = "['sibling/AC-ARC:T-01', 'sibling/AC-ABSENT:T-01']";
    fs::write(
        multi_root.join("docs/milestones/AC-CHECKOUT.md"),
        MULTI_ARC_SOURCE
            .replace("AC-ARC", "AC-CHECKOUT")
            .replace(
                "status: planned",
                &format!("status: planned\nrepositories: [{{name: sibling, path: {sibling_rel}}}]\ndepends_on: {planned_deps}"),
            )
            .replace("task_ref: T-01", &format!("task_ref: T-01\ndepends_on: {planned_deps}")),
    )
    .unwrap();
    let multi_resp = query_multi();
    assert_eq!(multi_resp.in_progress.len(), 2);
    assert!(multi_resp.in_progress[0].run_id < multi_resp.in_progress[1].run_id);
    for (run, title, path, task_target, satisfied) in [
        (&run_a, "Multi Run", multi_path, "Edit M", false),
        (&run_b, "Sibling Run", sibling_path, "Edit N", true),
    ] {
        let row = multi_resp
            .in_progress
            .iter()
            .find(|entry| entry.run_id.as_deref() == Some(run.run.run_id.as_str()))
            .unwrap();
        assert_eq!((&*row.title, &*row.path), (title, path));
        assert_eq!(row.worktree, run.run.worktree);
        assert!(row.parallel_safe);
        assert!(row.scope_overlaps.is_empty());
        assert_eq!(row.tasks[0].target.as_deref(), Some(task_target));
        assert_eq!(row.tasks[0].depends_on[0].satisfied, satisfied);
        assert_eq!(
            row.tasks[0].status,
            if satisfied { "ready" } else { "planned" }
        );
        let StateQueryOutput::Next(next) = fx
            .ro
            .query(StateQuery::Next(NextReq {
                run: Some(run.run.run_id.to_string()),
                ..NextReq::default()
            }))
            .unwrap()
        else {
            panic!("unexpected next");
        };
        assert_eq!(
            next.ready_cursors.contains(&row.tasks[0].task_ref),
            row.tasks[0].status == "ready"
        );
        let annotations: Vec<_> = row
            .depends_on
            .iter()
            .map(|dep| (dep.raw.as_str(), dep.status.as_str(), dep.satisfied))
            .collect();
        if satisfied {
            assert_eq!(
                annotations,
                [
                    ("sibling/AC-ARC", "archived", true),
                    ("sibling/AC-ABSENT", "missing", false)
                ]
            );
        } else {
            assert!(annotations.is_empty());
        }
    }
    let StateQueryOutput::Roadmap(checkouts) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    let planned = |id| {
        checkouts
            .waves
            .iter()
            .flat_map(|wave| &wave.specs)
            .find(|spec| spec.spec_id == id)
            .unwrap()
    };
    let consumer = planned("AC-CHECKOUT");
    for deps in [&consumer.depends_on, &consumer.tasks[0].depends_on] {
        let outcomes: Vec<_> = deps
            .iter()
            .map(|dep| (dep.status.as_str(), dep.satisfied))
            .collect();
        assert_eq!(outcomes, [("archived", true), ("missing", false)]);
    }
    for id in ["AC-ARC", "AC-ABSENT"] {
        let stale = planned(id);
        for blocks in [&stale.blocks, &stale.tasks[0].blocks] {
            assert!(blocks.iter().all(|label| !label.starts_with("AC-CHECKOUT")));
        }
    }
    assert_eq!(planned("AC-ABSENT").unblocks_specs, 0);
    let wrong_runs = [
        fx.start_wrong_run("AC-ABSENT", true),
        fx.start_wrong_run("AC-ARC", false),
    ];
    let after_wrong_runs = query_multi();
    let sibling = after_wrong_runs
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(run_b.run.run_id.as_str()))
        .unwrap();
    assert_eq!(
        sibling
            .depends_on
            .iter()
            .map(|dep| (dep.status.as_str(), dep.satisfied))
            .collect::<Vec<_>>(),
        [("archived", true), ("missing", false)]
    );
    let StateQueryOutput::Roadmap(after_wrong_runs) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    let consumer = after_wrong_runs
        .waves
        .iter()
        .flat_map(|wave| &wave.specs)
        .find(|spec| spec.spec_id == "AC-CHECKOUT")
        .unwrap();
    for deps in [&consumer.depends_on, &consumer.tasks[0].depends_on] {
        let outcomes: Vec<_> = deps
            .iter()
            .map(|dep| (dep.status.as_str(), dep.satisfied))
            .collect();
        assert_eq!(outcomes, [("archived", true), ("missing", false)]);
    }
    for run in &wrong_runs {
        let entry = after_wrong_runs
            .in_progress
            .iter()
            .find(|entry| entry.run_id.as_deref() == Some(run.run.run_id.as_str()))
            .unwrap();
        assert!(entry
            .blocks
            .iter()
            .all(|label| !label.starts_with("AC-CHECKOUT")));
        assert!(entry.tasks[0]
            .blocks
            .iter()
            .all(|label| !label.starts_with("AC-CHECKOUT")));
    }
    let run_b_id = run_b.run.run_id.as_str();
    rusqlite::Connection::open(multi_root.join(".acdd/custom-state.sqlite"))
        .unwrap()
        .execute("UPDATE gate_nodes SET status = 'completed' WHERE run_id = ?1 AND node_id = 'task:T-01'", [run_b_id])
        .unwrap();
    let checkout_path = multi_root.join("docs/milestones/AC-CHECKOUT.md");
    let checkout_source = fs::read_to_string(&checkout_path).unwrap();
    fs::write(
        checkout_path,
        checkout_source.replace(
            planned_deps,
            "['sibling/AC-ARC:T-01', 'sibling/AC-ABSENT:T-01', 'sibling/AC-MULTI:T-01']",
        ),
    )
    .unwrap();
    let StateQueryOutput::Roadmap(duplicates) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            spec_id: Some("AC-CHECKOUT".to_owned()),
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    let consumer = duplicates
        .waves
        .iter()
        .flat_map(|wave| &wave.specs)
        .next()
        .unwrap();
    for deps in [&consumer.depends_on, &consumer.tasks[0].depends_on] {
        assert_eq!(
            (deps[2].status.as_str(), deps[2].satisfied),
            ("satisfied", true)
        );
    }
    let tomb_rel = "docs/milestones/archive/AC-TOMB.md";
    let tomb_doc = "---\nid: AC-TOMB\ntitle: Checkout Tomb\nstatus: completed\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/t/**]\nstatus: completed\nacceptance_criteria: [ok]\n```\n";
    write_spec(multi_root, tomb_rel, tomb_doc);
    git_commit_all(multi_root, "root tombstone archive completed");
    write_spec(
        &wt_b,
        tomb_rel,
        &tomb_doc.replace(
            "status: completed\nacceptance_criteria",
            "status: planned\nacceptance_criteria",
        ),
    );
    let mut tomb_graph = build_rich_graph(
        multi_root,
        "run-tomb-local",
        acdd_state::types::IsolationMode::InPlace,
    );
    tomb_graph.run.run_name = RunName::new("tomb-local").unwrap();
    tomb_graph.run.spec_id = Some("AC-TOMB".to_owned());
    tomb_graph.run.worktree = None;
    let tomb_record = sample_tombstone(
        "run-tomb-local",
        "tomb-local",
        Some("AC-TOMB"),
        acdd_state::types::TerminalStatus::Completed,
        Some(tomb_rel),
    );
    backend_for(&fx.cfg)
        .exec_tx(|tx| {
            tx.insert_run_graph(&tomb_graph)?;
            tx.archive_and_prune_run(&empty_preconditions(1), &tomb_record)
        })
        .unwrap();
    let orphan_wt = fx._tmp.path().join("orphan-wt");
    git_ok(
        multi_root,
        &[
            "worktree",
            "add",
            "--orphan",
            "-b",
            "orphan-wt",
            orphan_wt.to_str().unwrap(),
        ],
    );
    write_spec(
        &orphan_wt,
        "docs/milestones/AC-ORPHAN.md",
        "---\nid: AC-ORPHAN\ntitle: Orphan\nstatus: active\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/orphan/**]\nacceptance_criteria: [ok]\n```\n",
    );
    fs::write(
        multi_root.join("docs/milestones/AC-CHECKOUT.md"),
        MULTI_ARC_SOURCE
            .replace("AC-ARC", "AC-CHECKOUT")
            .replace(
                "status: planned",
                &format!("status: planned\nrepositories: [{{name: sibling, path: {sibling_rel}}}]\ndepends_on: ['sibling/AC-TOMB:T-01']"),
            )
            .replace("task_ref: T-01", "task_ref: T-01\ndepends_on: ['sibling/AC-TOMB:T-01']"),
    )
    .unwrap();
    let tomb_isolation = roadmap_query(
        &fx.ro,
        RoadmapReq {
            archived: true,
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    assert!(tomb_isolation
        .waves
        .iter()
        .flat_map(|w| &w.specs)
        .any(|s| s.spec_id == "AC-ORPHAN"));
    let consumer_tomb = tomb_isolation
        .waves
        .iter()
        .flat_map(|w| &w.specs)
        .find(|s| s.spec_id == "AC-CHECKOUT")
        .unwrap();
    assert_eq!(
        (
            consumer_tomb.depends_on[0].status.as_str(),
            consumer_tomb.depends_on[0].satisfied
        ),
        ("missing", false)
    );
    assert_eq!(
        (
            consumer_tomb.tasks[0].depends_on[0].status.as_str(),
            consumer_tomb.tasks[0].depends_on[0].satisfied
        ),
        ("missing", false)
    );
    fs::write(wt_b.join("acdd.yaml"), "specs: [invalid").unwrap();
    let invalid_cfg = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            spec_id: Some("AC-MULTI".to_owned()),
            current_only: true,
            ..RoadmapReq::default()
        }))
        .unwrap_err();
    assert_eq!(invalid_cfg.code, ErrorCode::WorkflowSchemaInvalid);
    assert!(invalid_cfg.message.contains("acdd.yaml"));
}

#[test]
fn seam_roadmap_query_waves_checkout_binding_provenance_and_sync() {
    let fx = MultiRunFixture::new("roadmap-r16-bind");
    let multi_root = &fx.root;
    let run_a = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-MULTI.md".to_owned()),
            run_name: Some(RunName::new("run-a").unwrap()),
            workflow: Some(fx.prof.clone()),
            in_place: true,
            worktree: false,
            ..StartReq::default()
        },
    )
    .unwrap();
    let (run_b, wt_b) = fx.start_sibling_run();
    git_ok(&wt_b, &["add", "custom/specs"]);
    git_ok(&wt_b, &["commit", "-m", "Sibling AC-MULTI"]);
    let run_b_id = run_b.run.run_id.as_str();
    let db = rusqlite::Connection::open(multi_root.join(".acdd/custom-state.sqlite")).unwrap();
    db.execute(
        "UPDATE gate_nodes SET status = 'completed' WHERE run_id = ?1 AND node_id = 'task:T-01'",
        [run_b_id],
    )
    .unwrap();
    let sibling_rel = wt_b.strip_prefix(multi_root).unwrap().display();
    fs::write(
        multi_root.join("docs/milestones/AC-BIND.md"),
        format!("---\nid: AC-BIND\ntitle: Checkout binding\nstatus: planned\nrepositories: [{{name: sibling, path: {sibling_rel}}}, {{name: primary_alias, path: .}}]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: ['sibling/AC-MULTI:T-01']\nscope: [src/a/**]\nacceptance_criteria: [ok]\n```\n### task: T-02\n```yaml\ntask_ref: T-02\ndepends_on: ['primary_alias/AC-MULTI:T-01']\nscope: [src/b/**]\nacceptance_criteria: [ok]\n```\n### task: T-03\n```yaml\ntask_ref: T-03\ndepends_on: ['AC-MULTI:T-01']\nscope: [src/c/**]\nacceptance_criteria: [ok]\n```\n"),
    ).unwrap();
    let binding = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-BIND.md".to_owned()),
            run_name: Some(RunName::new("checkout-binding").unwrap()),
            workflow: Some(fx.prof.clone()),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let graph = reader_for(&fx.cfg)
        .load_run_graph(&binding.run.run_id)
        .unwrap()
        .unwrap();
    let dep = |task: &str| {
        graph
            .dependencies
            .iter()
            .find(|dep| dep.task_ref.as_str() == task)
            .unwrap()
    };
    assert_eq!(dep("T-01").depends_on_run_id, run_b.run.run_id);
    assert_eq!(dep("T-01").depends_on_workspace, wt_b.to_string_lossy());
    assert_eq!(
        dep("T-02").depends_on_workspace,
        multi_root.to_string_lossy()
    );
    assert_eq!(dep("T-03").depends_on_workspace, ".");
    for (task, run) in [("T-01", &run_a), ("T-02", &run_b), ("T-03", &run_b)] {
        db.execute(
            "UPDATE task_dependencies SET depends_on_run_id = ?1, satisfied = CASE WHEN task_ref = 'T-02' THEN 1 ELSE 0 END WHERE run_id = ?2 AND task_ref = ?3",
            [run.run.run_id.as_str(), binding.run.run_id.as_str(), task],
        ).unwrap();
    }
    let StateQueryOutput::Next(next) = fx
        .ro
        .query(StateQuery::Next(NextReq {
            run: Some(binding.run.run_id.to_string()),
            ..NextReq::default()
        }))
        .unwrap()
    else {
        panic!("expected next");
    };
    let StateQueryOutput::Roadmap(binding_roadmap) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    let binding_row = binding_roadmap
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(binding.run.run_id.as_str()))
        .unwrap();
    for (task, ready) in [("T-01", true), ("T-02", false), ("T-03", true)] {
        let row = binding_row
            .tasks
            .iter()
            .find(|row| row.task_ref == task)
            .unwrap();
        assert_eq!(row.depends_on[0].satisfied, ready, "{task}");
        assert_eq!(
            next.ready_cursors.contains(&format!("{task}.build")),
            ready,
            "{task}: {next:?}"
        );
    }
    let sibling_row = binding_roadmap
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(run_b_id))
        .unwrap();
    let blocks = &sibling_row.tasks[0].blocks;
    assert!(blocks.contains(&"AC-BIND:T-03".to_owned()));
    assert!(!blocks.contains(&"AC-BIND:T-02".to_owned()));
    let legacy_bind = "AC-BIND:T-01";
    for (run, expected) in [(&run_b, true), (&run_a, false)] {
        let row = binding_roadmap
            .in_progress
            .iter()
            .find(|entry| entry.run_id.as_deref() == Some(run.run.run_id.as_str()))
            .unwrap();
        assert_eq!(
            row.blocks.contains(&legacy_bind.to_owned()),
            expected,
            "spec block before sync"
        );
        assert_eq!(
            row.tasks[0].blocks.contains(&legacy_bind.to_owned()),
            expected,
            "task block before sync"
        );
    }
    let producer = binding_roadmap
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(run_a.run.run_id.as_str()))
        .unwrap();
    assert_eq!(
        producer.unblocks_specs, 1,
        "stale satisfied bit still fans out"
    );
    assert_eq!(
        producer.unblocks_tasks, 3,
        "stale satisfied bit still fans out"
    );
    fx.rw
        .execute(StateCommand::Sync(acdd_state::usecases::SyncReq {
            run: Some(binding.run.run_id.to_string()),
            rebind_workflow: false,
        }))
        .unwrap();
    let StateQueryOutput::Roadmap(binding_after_sync) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    for (run, expected) in [(&run_b, true), (&run_a, false)] {
        let row = binding_after_sync
            .in_progress
            .iter()
            .find(|entry| entry.run_id.as_deref() == Some(run.run.run_id.as_str()))
            .unwrap();
        assert_eq!(
            row.blocks.contains(&legacy_bind.to_owned()),
            expected,
            "spec block after sync"
        );
        assert_eq!(
            row.tasks[0].blocks.contains(&legacy_bind.to_owned()),
            expected,
            "task block after sync"
        );
    }
    let planned_wt = multi_root.join("planned-sibling");
    git_ok(
        multi_root,
        &[
            "worktree",
            "add",
            "-b",
            "planned-sibling",
            planned_wt.to_str().unwrap(),
        ],
    );
    let planned_canon = acdd_core::fs::canonical_root(&planned_wt).unwrap();
    db.execute(
        "UPDATE task_dependencies SET depends_on_run_id = ?1, depends_on_workspace = ?2, satisfied = 0 WHERE run_id = ?3 AND task_ref = 'T-01'",
        [run_a.run.run_id.as_str(), planned_canon.to_str().unwrap(), binding.run.run_id.as_str()],
    )
    .unwrap();
    let StateQueryOutput::Roadmap(planned_roadmap) = fx
        .ro
        .query(StateQuery::Roadmap(RoadmapReq {
            include_tasks: true,
            ..RoadmapReq::default()
        }))
        .unwrap()
    else {
        panic!("expected roadmap");
    };
    let planned_target = planned_roadmap
        .waves
        .iter()
        .flat_map(|wave| &wave.specs)
        .find(|spec| {
            spec.spec_id == "AC-MULTI" && spec.blocks.iter().any(|label| label == legacy_bind)
        })
        .expect("planned sibling target");
    assert!(planned_target.tasks.iter().any(|task| {
        task.task_ref == "T-01" && task.blocks.iter().any(|label| label == legacy_bind)
    }));
    let primary = planned_roadmap
        .in_progress
        .iter()
        .find(|entry| entry.run_id.as_deref() == Some(run_a.run.run_id.as_str()))
        .unwrap();
    assert!(!primary.blocks.iter().any(|label| label == legacy_bind));
    assert!(!primary.tasks[0]
        .blocks
        .iter()
        .any(|label| label == legacy_bind));
}

#[test]
fn seam_roadmap_query_waves_late_producer_checkout_sync() {
    let fx = MultiRunFixture::new("roadmap-r16-late");
    let multi_root = &fx.root;
    fs::write(
        multi_root.join("docs/milestones/AC-ABSENT.md"),
        MULTI_ARC_SOURCE.replace("AC-ARC", "AC-ABSENT"),
    )
    .unwrap();
    let wrong_absent = fx.start_wrong_run("AC-ABSENT", true);
    let late_checkout = multi_root.join("late-target");
    git_ok(
        multi_root,
        &[
            "worktree",
            "add",
            "-b",
            "late-target",
            late_checkout.to_str().unwrap(),
        ],
    );
    let _ = write_profile(&late_checkout, "multi-p.yaml", MULTI_PROFILE_YAML);
    fs::write(
        late_checkout.join("docs/milestones/AC-ABSENT.md"),
        MULTI_ARC_SOURCE.replace("AC-ARC", "AC-ABSENT"),
    )
    .unwrap();
    fs::write(
        multi_root.join("docs/milestones/AC-LATE.md"),
        "---\nid: AC-LATE\ntitle: Late consumer\nstatus: planned\nrepositories: [{name: late, path: late-target}]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: ['late/AC-ABSENT:T-01']\nscope: [src/late/**]\nacceptance_criteria: [ok]\n```\n",
    ).unwrap();
    let late_consumer = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-LATE.md".to_owned()),
            run_name: Some(RunName::new("late-consumer").unwrap()),
            workflow: Some(fx.prof.clone()),
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let late_graph = reader_for(&fx.cfg)
        .load_run_graph(&late_consumer.run.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        late_graph.dependencies[0].depends_on_run_id.as_str(),
        "AC-ABSENT"
    );
    let late_producer = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-ABSENT.md".to_owned()),
            run_name: Some(RunName::new("late-producer").unwrap()),
            workflow: Some(fx.prof.clone()),
            worktree_path: Some(late_checkout),
            ..StartReq::default()
        },
    )
    .unwrap();
    for sync in [false, true] {
        if sync {
            fx.rw
                .execute(StateCommand::Sync(acdd_state::usecases::SyncReq {
                    run: Some(late_consumer.run.run_id.to_string()),
                    rebind_workflow: false,
                }))
                .unwrap();
            let graph = reader_for(&fx.cfg)
                .load_run_graph(&late_consumer.run.run_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                graph.dependencies[0].depends_on_run_id,
                late_producer.run.run_id
            );
        }
        let StateQueryOutput::Roadmap(roadmap) = fx
            .ro
            .query(StateQuery::Roadmap(RoadmapReq {
                include_tasks: true,
                ..RoadmapReq::default()
            }))
            .unwrap()
        else {
            panic!("expected roadmap");
        };
        for (run, expected) in [(&late_producer, true), (&wrong_absent, false)] {
            let row = roadmap
                .in_progress
                .iter()
                .find(|row| row.run_id.as_deref() == Some(run.run.run_id.as_str()))
                .unwrap();
            assert_eq!(
                row.blocks.contains(&"AC-LATE:T-01".to_owned()),
                expected,
                "spec block, sync={sync}"
            );
            assert_eq!(
                row.tasks[0].blocks.contains(&"AC-LATE:T-01".to_owned()),
                expected,
                "task block, sync={sync}"
            );
        }
    }
}

#[test]
fn seam_roadmap_lazy_overlay_and_current_only_malformed_worktree_isolation() {
    let fx = MultiRunFixture::new("roadmap-lazy-overlay");
    let multi_root = &fx.root;
    fs::write(
        multi_root.join("docs/milestones/AC-UNREL.md"),
        MULTI_ARC_SOURCE.replace("AC-ARC", "AC-UNREL"),
    )
    .unwrap();
    git_commit_all(multi_root, "add AC-UNREL before sibling checkout");

    let run_unrel = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-UNREL.md".to_owned()),
            run_name: Some(RunName::new("run-unrel").unwrap()),
            workflow: Some(fx.prof.clone()),
            in_place: false,
            worktree: true,
            ..StartReq::default()
        },
    )
    .unwrap();
    let wt_unrel = PathBuf::from(run_unrel.run.worktree.as_deref().unwrap());

    // Remove AC-UNREL from primary root so it exists only on the sibling worktree,
    // and keep a dependency-free local active run on primary root.
    fs::remove_file(multi_root.join("docs/milestones/AC-UNREL.md")).unwrap();
    git_commit_all(multi_root, "remove AC-UNREL from primary root");

    let run_local = start_run(
        &fx.rw,
        StartReq {
            spec_path: Some("docs/milestones/AC-ARC.md".to_owned()),
            run_name: Some(RunName::new("run-local").unwrap()),
            workflow: Some(fx.prof.clone()),
            in_place: true,
            worktree: false,
            ..StartReq::default()
        },
    )
    .unwrap();

    // Corrupt acdd.yaml on the unrelated sibling worktree.
    fs::write(wt_unrel.join("acdd.yaml"), "specs: [unclosed").unwrap();

    // Unscoped roadmap fails closed on the sibling worktree's malformed acdd.yaml,
    // whereas --current-only isolates discovery + overlay and succeeds cleanly.
    let unscoped_err = roadmap_query_err(&fx.ro, RoadmapReq::default());
    assert_eq!(unscoped_err.code, ErrorCode::WorkflowSchemaInvalid);

    let current_only_resp = roadmap_query(
        &fx.ro,
        RoadmapReq {
            current_only: true,
            ..RoadmapReq::default()
        },
    );
    let local_entry = current_only_resp
        .in_progress
        .iter()
        .find(|e| e.run_id.as_deref() == Some(run_local.run.run_id.as_str()))
        .expect("local active run included under --current-only");
    assert!(
        local_entry.tasks.is_empty(),
        "default roadmap without include_tasks skips task projection"
    );

    let with_tasks = roadmap_query(
        &fx.ro,
        RoadmapReq {
            current_only: true,
            include_tasks: true,
            ..RoadmapReq::default()
        },
    );
    let local_with_tasks = with_tasks
        .in_progress
        .iter()
        .find(|e| e.run_id.as_deref() == Some(run_local.run.run_id.as_str()))
        .expect("local active run included with tasks");
    assert_eq!(local_with_tasks.tasks.len(), 1);
    assert_eq!(local_with_tasks.tasks[0].task_ref, "T-01");
}
