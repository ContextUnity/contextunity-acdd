//! Shared lifecycle infrastructure: lock ordering, path policies, profile/spec resolution,
//! cross-workspace dependency queries, PID liveness checks, and tombstone cleanup recovery.

mod io;
mod runs;
mod specs;
mod workspace;

pub use crate::usecases::deps::{
    collect_external_satisfied_deps, parse_task_dependency, verify_run_level_depends_on,
};
pub(crate) use io::{combine_check_observations, sorted_dir_paths};
pub use io::{
    append_trace_event, current_session_fingerprint, is_local_pid_alive, prune_old_run_trace_dirs,
    read_confined_utf8,
};
pub(super) use runs::cleanup_tombstone;
pub(crate) use runs::{
    build_tombstone_cleanup_manifest_from_snapshot, is_managed_run_branch,
    resume_pending_tombstone_cleanups_dyn,
};
pub use runs::{
    collect_integration_inputs, compute_unintegrated_tasks, generate_run_id, list_all_runs,
    list_all_tombstones, load_active_run_graph, resolve_active_run, resolve_run_optional,
    LoadedRunGraph,
};
pub(crate) use specs::{compute_archive_relpath, reconcile_restored_runs_with_archives};
pub use specs::{
    auto_select_context_cursor, auto_select_ready_cursor, effective_spec_collections,
    match_spec_collection, parse_cursor_for_snapshot, parse_profile_from_run, to_machine_profile,
};
pub(super) use workspace::snapshot_worktree_layout;
pub(crate) use workspace::{
    apply_effective_isolation, with_repo_lock_and_maintenance,
    with_repo_lock_and_maintenance_unchecked,
};
pub use workspace::{
    acquire_repo_lock, build_path_policy, checkout_config, effective_modules_for_profile,
    ensure_blackboard_enabled, ensure_state_enabled, primary_workspace_root,
    resolve_node_worktree, resolve_repo_layout_optional, resolve_source_checkout,
    validate_profile_fail_closed,
};

#[cfg(test)]
mod tests {
    use super::io::current_host_id;
    use super::*;
    use crate::harness::RunHarness;
    use crate::store::traits::StateBackend;
    use crate::types::{ArchiveCleanupEntry, IsolationMode, NodeKind, RunTombstoneRecord};
    use acdd_core::config::{default_specs, ACDD_DIR, WORKTREES_DIR};
    use acdd_core::ids::RunId;
    use acdd_core::response::ResponsePolicy;
    use acdd_core::subsystems::SubsystemRegistry;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn build_path_policy_and_cleanup_manifest_invariants() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let harness = RunHarness::in_memory().expect("in-memory harness");
        let mut graph = RunHarness::build_sample_graph(
            "run-life-1",
            "AC-001",
            root.to_str().unwrap(),
            None,
            &["t1"],
        )
        .expect("build sample graph");
        graph.run.collection = Some("milestones".to_owned());
        graph.run.spec_path = Some("docs/milestones/AC-001.md".to_owned());
        graph.run.branch = Some("feature/user-owned".to_owned());
        for node in &mut graph.nodes {
            if node.kind == NodeKind::Task {
                node.branch = Some("task/AC-001-t1".to_owned());
                node.worktree = Some(
                    root.join(WORKTREES_DIR)
                        .join("AC-001-t1")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        harness
            .exec_tx(|tx| tx.insert_run_graph(&graph))
            .expect("insert graph");

        let cfg = crate::usecases::StateEngineConfig {
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: default_specs(),
            profiles_dirs: Vec::new(),
            linked_workspaces: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            response_policy: ResponsePolicy::default(),
            retain_traces: 10,
        };

        // 1. build_path_policy marks active spec_path and derived archive_path as engine-managed,
        //    isolates sibling worktree runs via zero-SQLite fast path, and scans shared checkouts.
        let policy = build_path_policy(&cfg, &harness, &[], None, None).expect("build_path_policy");
        assert!(!policy.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!policy.is_candidate_included("docs/milestones/archive/AC-001.md"));
        assert!(policy.is_candidate_included("src/lib.rs"));

        let mut sibling_graph = RunHarness::build_sample_graph(
            "run-life-2",
            "AC-002",
            root.to_str().unwrap(),
            None,
            &["t1"],
        )
        .expect("build sibling graph");
        sibling_graph.run.collection = Some("milestones".to_owned());
        sibling_graph.run.spec_path = Some("docs/milestones/AC-002.md".to_owned());
        let wt1 = root.join(WORKTREES_DIR).join("AC-001");
        let wt2 = root.join(WORKTREES_DIR).join("AC-002");
        fs::create_dir_all(&wt1).expect("mkdir wt1");
        fs::create_dir_all(&wt2).expect("mkdir wt2");
        sibling_graph.run.worktree = Some(wt2.to_string_lossy().into_owned());
        harness
            .exec_tx(|tx| tx.insert_run_graph(&sibling_graph))
            .expect("insert sibling graph");

        let scoped_wt1 = build_path_policy(
            &cfg,
            &harness,
            &[],
            Some(&graph.run),
            Some(&wt1),
        )
        .expect("scoped policy wt1");
        assert!(!scoped_wt1.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!scoped_wt1.is_candidate_included("docs/milestones/archive/AC-001.md"));
        assert!(
            scoped_wt1.is_candidate_included("docs/milestones/AC-002.md"),
            "sibling run in another worktree must not alter candidate_tree inclusion"
        );

        let corrupt_db = root.join("not-sqlite.db");
        fs::write(&corrupt_db, b"not a sqlite database").expect("write corrupt db");
        let bad_reader = crate::store::sqlite::SqliteStateReader::new(corrupt_db);
        let fast_wt1 = build_path_policy(
            &cfg,
            &bad_reader,
            &[],
            Some(&graph.run),
            Some(&wt1),
        )
        .expect("isolated worktree fast path must bypass SQLite list_runs");
        assert!(!fast_wt1.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!fast_wt1.is_candidate_included("docs/milestones/archive/AC-001.md"));
        assert!(fast_wt1.is_candidate_included("docs/milestones/AC-002.md"));

        let scoped_shared = build_path_policy(
            &cfg,
            &harness,
            &[],
            Some(&sibling_graph.run),
            Some(root),
        )
        .expect("scoped policy shared root");
        assert!(!scoped_shared.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!scoped_shared.is_candidate_included("docs/milestones/AC-002.md"));

        let mut in_place_a = RunHarness::build_sample_graph(
            "run-life-3",
            "AC-003",
            root.to_str().unwrap(),
            None,
            &["t1"],
        )
        .expect("build in_place_a");
        in_place_a.run.isolation = IsolationMode::InPlace;
        in_place_a.run.collection = Some("milestones".to_owned());
        in_place_a.run.spec_path = Some("docs/milestones/AC-003.md".to_owned());
        in_place_a.run.worktree = Some(wt1.to_string_lossy().into_owned());
        let mut in_place_b = RunHarness::build_sample_graph(
            "run-life-4",
            "AC-004",
            root.to_str().unwrap(),
            None,
            &["t1"],
        )
        .expect("build in_place_b");
        in_place_b.run.isolation = IsolationMode::InPlace;
        in_place_b.run.collection = Some("milestones".to_owned());
        in_place_b.run.spec_path = Some("docs/milestones/AC-004.md".to_owned());
        harness
            .exec_tx(|tx| {
                tx.insert_run_graph(&in_place_a)?;
                tx.insert_run_graph(&in_place_b)
            })
            .expect("insert in_place runs");
        assert!(
            build_path_policy(&cfg, &bad_reader, &[], Some(&in_place_a.run), Some(&wt1)).is_err(),
            "in_place runs must not skip SQLite list_runs fast path"
        );
        let in_place_policy = build_path_policy(
            &cfg,
            &harness,
            &[],
            Some(&in_place_b.run),
            Some(root),
        )
        .expect("shared in_place checkout policy");
        assert!(!in_place_policy.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!in_place_policy.is_candidate_included("docs/milestones/AC-004.md"));
        assert!(in_place_policy.is_candidate_included("docs/milestones/AC-003.md"));
        assert!(in_place_policy.is_candidate_included("docs/milestones/AC-002.md"));

        // 2. Manual worktrees profile never populates worktrees/branches into cleanup_manifest.
        fs::create_dir_all(root.join("docs/milestones")).expect("mkdir");
        fs::write(root.join("docs/milestones/AC-001.md"), "# spec\n").expect("write spec");
        let manual_manifest = build_tombstone_cleanup_manifest_from_snapshot(&cfg, &graph, root);
        assert!(manual_manifest.iter().any(|e| e.kind == "spec_file" && e.path_or_ref == "docs/milestones/AC-001.md"));
        assert!(!manual_manifest.iter().any(|e| matches!(e.kind.as_str(), "worktree" | "branch")));

        // 3. Managed worktrees profile includes ONLY ACDD-managed `.worktrees/<run>-...` and `task/<run>-...`.
        let mut managed_graph = graph.clone();
        managed_graph.run.profile_json = r#"{"version":2,"git":{"worktrees":"managed"}}"#.to_owned();
        managed_graph.run.worktree = Some(
            root.join(WORKTREES_DIR)
                .join("AC-001")
                .to_string_lossy()
                .into_owned(),
        );
        managed_graph.nodes.push(crate::types::GateNodeRecord {
            worktree: Some(root.join("../manual-wt").to_string_lossy().into_owned()),
            branch: Some("feature/manual-branch".to_owned()),
            ..managed_graph
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::Task)
                .unwrap()
                .clone()
        });
        let manifest = build_tombstone_cleanup_manifest_from_snapshot(&cfg, &managed_graph, root);
        assert!(manifest.iter().any(|e| e.kind == "spec_file" && e.path_or_ref == "docs/milestones/AC-001.md"));
        assert!(manifest.iter().any(|e| e.kind == "branch" && e.path_or_ref == "task/AC-001-t1"));
        let expected_t1_wt = format!("{WORKTREES_DIR}/AC-001-t1");
        let expected_run_wt = format!("{WORKTREES_DIR}/AC-001");
        assert!(manifest.iter().any(|e| e.kind == "worktree" && e.path_or_ref.ends_with(&expected_t1_wt)));
        assert!(manifest.iter().any(|e| e.kind == "worktree" && e.path_or_ref.ends_with(&expected_run_wt)));
        assert!(!manifest.iter().any(|e| e.path_or_ref.contains("manual-wt") || e.path_or_ref == "feature/manual-branch"));
        assert!(
            !manifest.iter().any(|e| e.kind == "snapshot_ref"),
            "cleanup_manifest must not emit dead snapshot_ref entries: {manifest:?}"
        );
        assert!(
            !manifest.iter().any(|e| e.path_or_ref == "feature/user-owned"),
            "cleanup_manifest must never include user/run branch: {manifest:?}"
        );
        assert!(
            !manifest.iter().any(|e| e.kind == "run_ref"),
            "cleanup_manifest must not emit dead run_ref entries: {manifest:?}"
        );

        // 4. resume_pending_tombstone_cleanups retains `cleanup_pending == true` when an entry fails to clean.
        let mut fail_tomb = RunTombstoneRecord {
            run_id: RunId::new("run-life-1").expect("rid"),
            run_name: acdd_core::ids::RunName::new("AC-001").expect("rname"),
            collection: Some("milestones".to_owned()),
            workspace: root.to_string_lossy().into_owned(),
            spec_id: Some("AC-001".to_owned()),
            title: "AC-001".to_owned(),
            profile_id: "code".to_owned(),
            is_ephemeral: false,
            generation: 1,
            terminal_status: crate::types::TerminalStatus::Cleared,
            target_base: None,
            target_base_commit: None,
            archive_path: None,
            closed_commit: None,
            cleanup_pending: true,
            cleanup_manifest: vec![ArchiveCleanupEntry {
                kind: "worktree".to_owned(),
                path_or_ref: root.to_string_lossy().into_owned(),
                workspace: None,
                force: false,
            }],
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: "2026-11-17T10:00:00.000Z".to_owned(),
        };
        harness
            .exec_tx(|tx| {
                tx.archive_and_prune_run(
                    &crate::types::StoragePreconditions {
                        expected_run_generation: 1,
                        expected_node_versions: Vec::new(),
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &fail_tomb,
                )
            })
            .expect("archive fail tomb");
        let _ = resume_pending_tombstone_cleanups_dyn(&cfg, &harness, None).expect("resume");
        fail_tomb = list_all_tombstones(&harness)
            .expect("list tombs")
            .into_iter()
            .find(|t| t.run_id.as_str() == "run-life-1")
            .expect("tomb exists");
        assert!(
            fail_tomb.cleanup_pending,
            "failed worktree cleanup must leave cleanup_pending=true for retry"
        );

        // 5. `is_local_pid_alive` guards `pid == 0` and overflowing/negative `pid_t` values.
        let host = current_host_id();
        for bad_fp in [
            format!("{host}:0:0"),
            format!("{host}:{}:0", u32::MAX),
            format!("{host}:{}:0", (i32::MAX as u32) + 1),
        ] {
            assert_eq!(
                is_local_pid_alive(Some(&bad_fp)),
                Some(false),
                "is_local_pid_alive must reject unsafe pid in '{bad_fp}'"
            );
        }

        // 6. `prune_old_run_trace_dirs` preserves active runs & `exempt_run_id` under count & byte
        //    budgets, and fails closed (deletes nothing) when the state DB reader errors.
        let run_root = root.join(ACDD_DIR).join("run");
        let active_dir = run_root.join("run-active-keep");
        let exempt_dir = run_root.join("run-exempt-keep");
        let closed_small = run_root.join("run-closed-small");
        let closed_big = run_root.join("run-closed-big");
        for d in [&active_dir, &exempt_dir, &closed_small, &closed_big] {
            fs::create_dir_all(d).expect("mkdir trace dir");
        }
        fs::write(active_dir.join("traces.jsonl"), vec![b'a'; 64]).unwrap();
        fs::write(exempt_dir.join("traces.jsonl"), vec![b'e'; 64]).unwrap();
        fs::write(closed_big.join("traces.jsonl"), vec![b'b'; 256]).unwrap();
        fs::write(closed_small.join("traces.jsonl"), vec![b's'; 32]).unwrap();
        let active_graph = RunHarness::build_sample_graph(
            "run-active-keep",
            "AC-ACTIVE",
            root.to_str().unwrap(),
            None,
            &[],
        )
        .expect("active graph");
        harness
            .exec_tx(|tx| tx.insert_run_graph(&active_graph))
            .expect("seed active");

        // Budget = 160 bytes (active 64 + exempt 64 = 128 -> closed_small 32 fits, closed_big 256 pruned!).
        prune_old_run_trace_dirs(root, &harness, 10, Some(160), Some("run-exempt-keep"))
            .expect("prune with budget");
        assert!(active_dir.is_dir(), "active run dir must never be pruned");
        assert!(exempt_dir.is_dir(), "exempt_run_id dir must never be pruned");
        assert!(!closed_big.exists(), "closed dir exceeding byte budget must be pruned");
        assert!(closed_small.is_dir(), "closed dir within count & byte budget kept");

        // Fail closed when the state DB cannot be read: preserve existing closed dirs.
        let corrupt_db = root.join(ACDD_DIR).join("corrupt-state.sqlite");
        fs::write(&corrupt_db, b"not-a-valid-sqlite-database").unwrap();
        let corrupt_reader = crate::store::sqlite::SqliteStateReader::new(corrupt_db);
        assert!(prune_old_run_trace_dirs(root, &corrupt_reader, 0, Some(0), None).is_err());
        assert!(closed_small.is_dir(), "corrupt reader must fail closed without deleting trace dirs");
    }
}
