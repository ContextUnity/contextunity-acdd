//! In-memory and disk test harness for state transitions and storage invariants.

use crate::machine::graph::build_initial_run_graph;
use crate::machine::inputs::{
    BuildGraphInput, MachineGateDef, MachineGitPolicy, MachineGraphItem, MachineProfileInput,
    MachineProofKind, MachineSavePolicy, MachineSpecInput, MachineSubtaskInput, MachineTaskDep,
    MachineTaskSpec, MachineTasksBlockDef,
};
use crate::store::memory::MemoryStateBackend;
use crate::store::sqlite::migrations::SqliteMeta;
use crate::store::sqlite::{SqliteBackendOptions, SqliteStateBackend};
use crate::store::traits::{StateBackend, StateReader, StateTx};
use crate::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, RunStatus,
    SaveAction, TaskDependencyRecord,
};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::{
    CommitOid, GateId, GateSelector, RunId, RunName, SubtaskRef, TaskRef, TreeOid,
};
use acdd_core::schema::SchemaDef;
use acdd_spec::{InvariantsState, SubtaskStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

enum HarnessBackend {
    Memory(MemoryStateBackend),
    Disk {
        backend: Box<SqliteStateBackend>,
        workspace_root: Option<PathBuf>,
        _temp_dir: Option<ScopedTempDir>,
    },
}

/// Unified test harness wrapping [`MemoryStateBackend`] (`:memory:`, `< 5 ms`) or
/// [`SqliteStateBackend`] (disk WAL + optional backup directory).
pub struct RunHarness {
    inner: HarnessBackend,
}

impl RunHarness {
    /// Construct a fresh in-memory `RunHarness` backed by SQLite `:memory:`.
    pub fn in_memory() -> Result<Self, AcddError> {
        Ok(Self {
            inner: HarnessBackend::Memory(MemoryStateBackend::new()?),
        })
    }

    /// Construct a temporary disk-backed `RunHarness` with isolated workspace, `.acdd/state.sqlite`,
    /// and backup directory `<root>/.git/acdd`.
    ///
    /// Owned temporary directories are removed automatically on drop.
    pub fn on_disk(spec_collection_paths: Vec<String>) -> Result<Self, AcddError> {
        let temp_guard = ScopedTempDir::create("acdd-state-harness").map_err(|err| {
            AcddError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed to create RunHarness tempdir: {err}"),
            )
        })?;
        let root = temp_guard.path();
        let workspace_root = root.join("workspace");
        let backup_dir = root.join("git-common/acdd");
        std::fs::create_dir_all(&workspace_root).map_err(|err| {
            AcddError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed to create harness workspace dir: {err}"),
            )
        })?;
        std::fs::create_dir_all(&backup_dir).map_err(|err| {
            AcddError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed to create harness backup dir: {err}"),
            )
        })?;

        let db_path = workspace_root.join(".acdd/state.sqlite");
        let backend = Self::open_sqlite_with_archive_reconciler(SqliteBackendOptions {
            db_path,
            backup_dir: Some(backup_dir),
            workspace_root: Some(workspace_root.clone()),
            spec_collection_paths,
            spec_collections: BTreeMap::new(),
            is_custom_db: false,
        })?;

        Ok(Self {
            inner: HarnessBackend::Disk {
                backend: Box::new(backend),
                workspace_root: Some(workspace_root),
                _temp_dir: Some(temp_guard),
            },
        })
    }

    /// Return a `dyn StateReader` view over this harness backend.
    pub fn reader(&self) -> &dyn StateReader {
        match &self.inner {
            HarnessBackend::Memory(m) => m,
            HarnessBackend::Disk { backend, .. } => backend.as_ref(),
        }
    }

    /// Return the disk database path when running on disk.
    pub fn db_path(&self) -> Option<&Path> {
        match &self.inner {
            HarnessBackend::Memory(_) => None,
            HarnessBackend::Disk { backend, .. } => Some(backend.db_path()),
        }
    }

    /// Return the workspace root path when configured on disk.
    pub fn workspace_root(&self) -> Option<&Path> {
        match &self.inner {
            HarnessBackend::Memory(_) => None,
            HarnessBackend::Disk { workspace_root, .. } => workspace_root.as_deref(),
        }
    }

    /// Read `_sqlite_meta` counters from the underlying SQLite backend.
    pub fn read_meta(&self) -> Result<SqliteMeta, AcddError> {
        match &self.inner {
            HarnessBackend::Memory(m) => m.read_meta(),
            HarnessBackend::Disk { backend, .. } => backend.read_meta(),
        }
    }

    /// Open a [`SqliteStateBackend`] wired with archive restore reconciliation for `options`.
    pub fn open_sqlite_with_archive_reconciler(
        options: SqliteBackendOptions,
    ) -> Result<SqliteStateBackend, AcddError> {
        let workspace_root = options.workspace_root.clone();
        let spec_collection_paths = options.spec_collection_paths.clone();
        let spec_collections = options.spec_collections.clone();
        SqliteStateBackend::open_with_reconciler(options, |runs| {
            crate::usecases::lifecycle::reconcile_restored_runs_with_archives(
                runs,
                workspace_root.as_deref(),
                &spec_collection_paths,
                &spec_collections,
            )
        })
    }

    /// Seed a run graph atomically via `StateTx::insert_run_graph`.
    pub fn seed_run(&self, graph: &RunGraphSnapshot) -> Result<(), AcddError> {
        self.exec_tx(|tx| tx.insert_run_graph(graph))
    }

    fn sample_run_record(
        run_id: RunId,
        run_name: RunName,
        workspace: &str,
        isolation: IsolationMode,
        worktree: Option<&str>,
    ) -> Result<RunRecord, AcddError> {
        let dummy_commit = CommitOid::new("0123456789abcdef0123456789abcdef01234567")?;
        let spec_path = format!("docs/milestones/{run_name}.md");
        let branch = format!("milestone/{run_name}");
        Ok(RunRecord {
            run_id,
            run_name,
            collection: Some("milestones".to_owned()),
            workspace: workspace.to_owned(),
            profile_id: "code".to_owned(),
            profile_json: r#"{"version":2}"#.to_owned(),
            spec_path: Some(spec_path),
            spec_id: None,
            spec_digest: "deadbeef".repeat(8),
            attached_specs: Vec::new(),
            is_ephemeral: false,
            isolation,
            target_base: Some("main".to_owned()),
            target_base_commit: Some(dummy_commit),
            branch: Some(branch),
            worktree: worktree.map(str::to_owned),
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            status: RunStatus::Active,
            cancellation_reason: None,
            root_cycle_count: 0,
            generation: 1,
            created_at: "2026-11-17T10:00:00.000Z".to_owned(),
            completed_at: None,
        })
    }

    /// Build a minimal valid [`RunGraphSnapshot`] with `task_refs` (`task:<ref>` + `build` gate).
    pub fn build_sample_graph(
        run_id: &str,
        run_name: &str,
        workspace: &str,
        worktree: Option<&str>,
        task_refs: &[&str],
    ) -> Result<RunGraphSnapshot, AcddError> {
        let rid = RunId::new(run_id)?;
        let rname = RunName::new(run_name)?;
        let dummy_tree = TreeOid::new("fedcba9876543210fedcba9876543210fedcba98")?;
        let run = Self::sample_run_record(
            rid.clone(),
            rname.clone(),
            workspace,
            IsolationMode::Worktree,
            worktree,
        )?;

        let mut nodes = vec![GateNodeRecord {
            run_id: rid.clone(),
            node_id: "tasks#0".to_owned(),
            parent_id: None,
            kind: NodeKind::Container,
            task_ref: None,
            gate_id: "tasks#0".to_owned(),
            seq_order: 0,
            status: NodeStatus::Ready,
            node_version: 1,
            isolation: IsolationMode::Worktree,
            attempt: 0,
            cycle_count: 0,
            max_attempts: 3,
            claim_revision: 0,
            contract_revision: 1,
            worker_id: None,
            session_fingerprint: None,
            branch: None,
            worktree: None,
            baseline_commit: None,
            claim_tree: None,
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: vec!["default".to_owned()],
            invariants: InvariantsState::default(),
            scope: Vec::new(),
            extended_scope: Vec::new(),
            active_span_id: None,
        }];

        let mut seq = 1u32;
        for &tref in task_refs {
            let task_ref = TaskRef::new(tref)?;
            let task_node_id = format!("task:{tref}");
            nodes.push(GateNodeRecord {
                run_id: rid.clone(),
                node_id: task_node_id.clone(),
                parent_id: Some("tasks#0".to_owned()),
                kind: NodeKind::Task,
                task_ref: Some(task_ref.clone()),
                gate_id: "__task__".to_owned(),
                seq_order: seq,
                status: NodeStatus::Ready,
                node_version: 1,
                isolation: IsolationMode::Worktree,
                attempt: 0,
                cycle_count: 0,
                max_attempts: 3,
                claim_revision: 0,
                contract_revision: 1,
                worker_id: None,
                session_fingerprint: None,
                branch: Some(format!("task/{run_name}-{tref}")),
                worktree: None,
                baseline_commit: None,
                claim_tree: None,
                accepted_tree: None,
                accepted_definition_digest: None,
                tags: vec!["default".to_owned()],
                invariants: InvariantsState::default(),
                scope: vec![format!("src/{tref}.rs")],
                extended_scope: Vec::new(),
                active_span_id: None,
            });
            seq += 1;

            nodes.push(GateNodeRecord {
                run_id: rid.clone(),
                node_id: format!("task:{tref}:build"),
                parent_id: Some(task_node_id),
                kind: NodeKind::TaskGate,
                task_ref: Some(task_ref),
                gate_id: "build".to_owned(),
                seq_order: seq,
                status: NodeStatus::Ready,
                node_version: 1,
                isolation: IsolationMode::Worktree,
                attempt: 0,
                cycle_count: 0,
                max_attempts: 3,
                claim_revision: 0,
                contract_revision: 1,
                worker_id: None,
                session_fingerprint: None,
                branch: Some(format!("task/{run_name}-{tref}")),
                worktree: None,
                baseline_commit: None,
                claim_tree: Some(dummy_tree.clone()),
                accepted_tree: None,
                accepted_definition_digest: None,
                tags: vec!["default".to_owned()],
                invariants: InvariantsState::default(),
                scope: vec![format!("src/{tref}.rs")],
                extended_scope: Vec::new(),
                active_span_id: None,
            });
            seq += 1;
        }

        Ok(RunGraphSnapshot {
            run,
            nodes,
            dependencies: Vec::<TaskDependencyRecord>::new(),
            subtasks: Vec::new(),
            claims: Vec::new(),
            traces: Vec::new(),
            artifacts: Vec::new(),
        })
    }

    /// Construct a canonical [`MachineTaskSpec`] fixture for state-machine tests.
    pub fn make_sample_task(
        name: &str,
        tags: &[&str],
        scope: &[&str],
        deps: &[&str],
        run_id: &RunId,
    ) -> MachineTaskSpec {
        MachineTaskSpec {
            task_ref: TaskRef::new(name).expect("valid task_ref"),
            tags: tags.iter().map(|s| (*s).to_owned()).collect(),
            scope: scope.iter().map(|s| (*s).to_owned()).collect(),
            extended_scope: Vec::new(),
            invariants: vec!["INV-1".to_owned()],
            depends_on: deps
                .iter()
                .map(|d| MachineTaskDep {
                    depends_on_workspace: "/repo".to_owned(),
                    depends_on_run_id: run_id.clone(),
                    depends_on_ref: TaskRef::new(*d).expect("valid dep task_ref"),
                })
                .collect(),
            subtasks: vec![MachineSubtaskInput {
                subtask_ref: SubtaskRef::new("S1").expect("valid subtask_ref"),
                title: "Implement core".to_owned(),
                status: SubtaskStatus::Completed,
                evidence: Some("implemented and tested".to_owned()),
            }],
        }
    }

    /// Build the canonical [`MachineProfileInput`] fixture used by state-machine integration tests.
    pub fn canonical_machine_profile(isolation: IsolationMode) -> MachineProfileInput {
        let gate_id = |s: &str| GateId::new(s).expect("valid gate_id");
        let selector = |s: &str| s.parse::<GateSelector>().expect("valid selector");
        let mut bp_schema = SchemaDef::object();
        bp_schema
            .properties
            .insert("tasks".to_owned(), SchemaDef::object());
        bp_schema.required = vec!["tasks".to_owned()];

        let synthesize = MachineGateDef {
            role: Some("architect".to_owned()),
            proof: MachineProofKind::CustomSchema("blueprint".to_owned()),
            pass: Some("review".to_owned()),
            ..MachineGateDef::minimal(gate_id("synthesize"))
        };
        let bp_review = MachineGateDef {
            role: Some("independent_reviewer".to_owned()),
            proof: MachineProofKind::Review,
            contours: vec![
                "claims".to_owned(),
                "project_isolation".to_owned(),
                "administration".to_owned(),
            ],
            independent_from: vec![selector("synthesize")],
            fail: Some("synthesize".to_owned()),
            save: MachineSavePolicy {
                immutable: true,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("review"))
        };
        let blueprint_synthesis = MachineGateDef {
            children: vec![synthesize, bp_review],
            ..MachineGateDef::minimal(gate_id("blueprint_synthesis"))
        };

        let build = MachineGateDef {
            role: Some("builder".to_owned()),
            proof: MachineProofKind::Command,
            command: Some("build_check".to_owned()),
            pass: Some("review".to_owned()),
            save: MachineSavePolicy {
                post: SaveAction::None,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("build"))
        };
        let task_review = MachineGateDef {
            role: Some("independent_reviewer".to_owned()),
            proof: MachineProofKind::Review,
            contours: vec!["paths".to_owned(), "claims".to_owned()],
            independent_from: vec![selector("build")],
            need: vec![selector("build")],
            pass: Some("deliver".to_owned()),
            fail: Some("build".to_owned()),
            save: MachineSavePolicy {
                immutable: true,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("review"))
        };
        let post_action = if isolation == IsolationMode::InPlace {
            SaveAction::None
        } else {
            SaveAction::Commit
        };
        let deliver = MachineGateDef {
            role: Some("independent_reviewer".to_owned()),
            independent_from: vec![selector("build")],
            need: vec![selector("build"), selector("review")],
            receipt: true,
            fail: Some("build".to_owned()),
            save: MachineSavePolicy {
                immutable: true,
                post: post_action,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("deliver"))
        };

        let verify_root = MachineGateDef {
            proof: MachineProofKind::Command,
            command: Some("verify_all".to_owned()),
            pass: Some("integration_review".to_owned()),
            fail: Some("polish".to_owned()),
            max_attempts: 3,
            ..MachineGateDef::minimal(gate_id("verify"))
        };
        let polish = MachineGateDef {
            role: Some("builder".to_owned()),
            proof: MachineProofKind::Command,
            command: Some("verify_all".to_owned()),
            scope: vec!["docs/**".to_owned(), ".acdd/workflows/**".to_owned()],
            pass: Some("verify".to_owned()),
            max_attempts: 3,
            ..MachineGateDef::minimal(gate_id("polish"))
        };
        let integration_review = MachineGateDef {
            role: Some("independent_reviewer".to_owned()),
            proof: MachineProofKind::Review,
            contours: vec!["paths".to_owned(), "claims".to_owned()],
            independent_from: vec![selector("polish"), selector("tasks.build")],
            need: vec![selector("verify")],
            pass: Some("handoff".to_owned()),
            fail: Some("polish".to_owned()),
            save: MachineSavePolicy {
                immutable: true,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("integration_review"))
        };
        let handoff = MachineGateDef {
            role: Some("independent_reviewer".to_owned()),
            need: vec![selector("verify"), selector("integration_review")],
            receipt: true,
            fail: Some("polish".to_owned()),
            save: MachineSavePolicy {
                immutable: true,
                archive: true,
                prune: true,
                post: post_action,
                ..Default::default()
            },
            ..MachineGateDef::minimal(gate_id("handoff"))
        };

        MachineProfileInput {
            profile_id: "default".to_owned(),
            profile_json: "{}".to_owned(),
            git: MachineGitPolicy {
                isolation,
                ..Default::default()
            },
            default_save: SaveAction::Commit,
            protected_paths: vec!["Cargo.toml".to_owned(), "build.rs".to_owned()],
            tags: BTreeSet::from(["code".to_owned(), "docs".to_owned(), "urgent".to_owned()]),
            proof_schemas: BTreeMap::from([("blueprint".to_owned(), bp_schema)]),
            gates: vec![
                MachineGraphItem::Gate(Box::new(blueprint_synthesis)),
                MachineGraphItem::TasksBlock(MachineTasksBlockDef {
                    when: None,
                    gates: vec![build, task_review, deliver],
                }),
                MachineGraphItem::Gate(Box::new(verify_root)),
                MachineGraphItem::Gate(Box::new(polish)),
                MachineGraphItem::Gate(Box::new(integration_review)),
                MachineGraphItem::Gate(Box::new(handoff)),
            ],
        }
    }

    /// Build a full profile-driven [`RunGraphSnapshot`] (`T1`, `T2 -> T1`, `T3`) for machine tests.
    pub fn build_machine_sample_graph(
        isolation: IsolationMode,
        with_pre_task: bool,
    ) -> (MachineProfileInput, RunGraphSnapshot) {
        let mut profile = Self::canonical_machine_profile(isolation);
        if !with_pre_task {
            profile.gates.remove(0);
        }

        let run_id = RunId::new("run-1").expect("valid run_id");
        let run_name = RunName::new("AC-100").expect("valid run_name");
        let base_run = Self::sample_run_record(
            run_id.clone(),
            run_name.clone(),
            "/repo",
            isolation,
            Some("/repo/.worktrees/AC-100"),
        )
        .expect("valid sample run record");
        let input = BuildGraphInput {
            run_id: run_id.clone(),
            run_name,
            collection: base_run.collection,
            workspace: base_run.workspace,
            spec_path: base_run.spec_path,
            is_ephemeral: base_run.is_ephemeral,
            target_base: base_run.target_base,
            target_base_commit: base_run.target_base_commit,
            branch: base_run.branch,
            worktree: base_run.worktree,
            trace_id: base_run.trace_id,
            created_at: "2026-11-17T09:00:00.000Z".to_owned(),
            spec: MachineSpecInput {
                tasks: vec![
                    Self::make_sample_task("T1", &["code"], &["crates/a/**"], &[], &run_id),
                    Self::make_sample_task("T2", &["code"], &["crates/b/**"], &["T1"], &run_id),
                    Self::make_sample_task("T3", &["docs"], &["docs/guide/**"], &[], &run_id),
                ],
                spec_digest: "spec-sha".to_owned(),
            },
        };
        let snap = build_initial_run_graph(&profile, &input).expect("valid machine sample graph");
        (profile, snap)
    }

    /// Execute a [`StateCommand`] against this harness backend.
    pub fn execute(
        &self,
        config: &crate::usecases::StateEngineConfig,
        provider: &acdd_core::ports::CompositeDomainProvider,
        cmd: crate::usecases::StateCommand,
    ) -> Result<crate::usecases::StateCommandOutput, AcddError> {
        crate::usecases::execute_on(config, self, provider, cmd)
    }
}

crate::store::impl_state_reader_delegate!(RunHarness, |s| s.reader());

impl StateBackend for RunHarness {
    fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), AcddError>,
    ) -> Result<(), AcddError> {
        match &self.inner {
            HarnessBackend::Memory(m) => m.exec_tx_dyn(f),
            HarnessBackend::Disk { backend, .. } => backend.exec_tx_dyn(f),
        }
    }

    fn after_commit_maintenance(&self) -> Result<(), AcddError> {
        match &self.inner {
            HarnessBackend::Memory(m) => m.after_commit_maintenance(),
            HarnessBackend::Disk { backend, .. } => backend.after_commit_maintenance(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::schema::DOMAIN_TABLES;
    use crate::store::sqlite::SqliteStateReader;
    use crate::types::{
        ArtifactKind, BlackboardListReq, BlackboardRecord, BlackboardStatus, BlackboardStatusPatch,
        BlackboardTopic, ClaimInsertInput, ClaimOutcome, ClaimOutcomePatch,
        DependencySatisfiedPatch, GateArtifactRecord, ListRunsReq, NodeStatusPatch,
        NodeVersionExpectation, PredecessorTreeExpectation, RunTombstoneRecord,
        StoragePreconditions, SubtaskRecord, TerminalStatus, TraceFinishInput, TraceStartInput,
        TraceStatus, TransitionPlan,
    };
    use acdd_spec::SubtaskStatus;
    use acdd_core::git::git_command;
    use acdd_core::ids::{SubtaskRef, WorkerId};
    use std::fs;

    fn oid_tree(ch: char) -> TreeOid {
        TreeOid::new(ch.to_string().repeat(40)).unwrap()
    }

    fn oid_commit(ch: char) -> CommitOid {
        CommitOid::new(ch.to_string().repeat(40)).unwrap()
    }

    fn count_contextunity_temp_dirs() -> usize {
        let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("contextunity-test-")
            })
            .count()
    }

    #[test]
    fn ddl_schema_constraints_and_partial_unique_indexes_matrix() {
        let mem = MemoryStateBackend::new().unwrap();
        let tables = mem.table_names().unwrap();
        for domain in DOMAIN_TABLES {
            assert!(
                tables.contains(&domain.to_owned()),
                "missing table {domain}"
            );
        }
        assert!(tables.contains(&"_sqlite_meta".to_owned()));
        assert!(!tables.contains(&"op_intents".to_owned()));
        assert_eq!(
            mem.read_meta().unwrap().schema_version,
            crate::store::sqlite::schema::CURRENT_SCHEMA_VERSION
        );

        let graph = RunHarness::build_sample_graph(
            "run-ddl-1",
            "AC-001-ddl",
            "/repo",
            Some("/repo/.worktrees/AC-001"),
            &["T1", "T2"],
        )
        .unwrap();
        mem.exec_tx(|tx| tx.insert_run_graph(&graph)).unwrap();
        assert_eq!(mem.read_meta().unwrap().write_seq, 1);

        // Table-driven negative DDL constraint and partial unique index checks.
        let negative_sql_cases: &[(&str, &str)] = &[
            (
                "invalid_is_ephemeral",
                "INSERT INTO runs VALUES ('r2','n2',NULL,'/repo','code','{}',NULL,'d',2,'worktree',NULL,NULL,NULL,NULL,'t','active',NULL,0,1,'2026-01-01T00:00:00Z',NULL,NULL,'[]');",
            ),
            (
                "invalid_profile_json",
                "INSERT INTO runs VALUES ('r2','n2',NULL,'/repo','code','{bad',NULL,'d',0,'worktree',NULL,NULL,NULL,NULL,'t','active',NULL,0,1,'2026-01-01T00:00:00Z',NULL,NULL,'[]');",
            ),
            (
                "invalid_attached_specs_json",
                "INSERT INTO runs VALUES ('r2','n2',NULL,'/repo','code','{}',NULL,'d',0,'worktree',NULL,NULL,NULL,NULL,'t','active',NULL,0,1,'2026-01-01T00:00:00Z',NULL,NULL,'{bad');",
            ),
            (
                "uq_runs_active_worktree_collision",
                "INSERT INTO runs VALUES ('r2','n2',NULL,'/repo','code','{}',NULL,'d',0,'worktree',NULL,NULL,NULL,'/repo/.worktrees/AC-001','t','active',NULL,0,1,'2026-01-01T00:00:00Z',NULL,NULL,'[]');",
            ),
            (
                "gate_nodes_task_must_have_dunder_task_gate_id",
                "INSERT INTO gate_nodes (run_id,node_id,parent_id,kind,task_ref,gate_id,seq_order,status,isolation) VALUES ('run-ddl-1','task:T9',NULL,'task','T9','build',9,'ready','worktree');",
            ),
            (
                "gate_nodes_root_gate_must_have_null_task_ref",
                "INSERT INTO gate_nodes (run_id,node_id,parent_id,kind,task_ref,gate_id,seq_order,status,isolation) VALUES ('run-ddl-1','root:v',NULL,'root_gate','T1','v',9,'ready','worktree');",
            ),
            (
                "uq_gate_nodes_task_duplicate_task_ref",
                "INSERT INTO gate_nodes (run_id,node_id,parent_id,kind,task_ref,gate_id,seq_order,status,isolation) VALUES ('run-ddl-1','task:T1:dup',NULL,'task','T1','__task__',9,'ready','worktree');",
            ),
            (
                "task_dependencies_forbid_self_dependency",
                "INSERT INTO task_dependencies VALUES ('run-ddl-1','T1','/repo','run-ddl-1','T1',0);",
            ),
            (
                "subtasks_completed_requires_nonempty_evidence",
                "INSERT INTO subtasks VALUES ('run-ddl-1','task:T1','S1',0,'title','completed','   ','2026-01-01T00:00:00Z');",
            ),
            (
                "gate_claims_active_requires_null_ended_at",
                "INSERT INTO gate_claims (run_id,node_id,claim_revision,worker_id,worktree,span_id,claim_tree,claimed_at,ended_at,outcome) VALUES ('run-ddl-1','task:T1:build',1,'w1','/wt','s1','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-01-01T00:00:00Z','2026-01-01T00:01:00Z','active');",
            ),
            (
                "gate_claims_passed_requires_ended_at",
                "INSERT INTO gate_claims (run_id,node_id,claim_revision,worker_id,worktree,span_id,claim_tree,claimed_at,ended_at,outcome) VALUES ('run-ddl-1','task:T1:build',1,'w1','/wt','s1','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-01-01T00:00:00Z',NULL,'passed');",
            ),
            (
                "gate_traces_running_requires_null_finished_at",
                "INSERT INTO gate_traces (run_id,node_id,claim_revision,contract_revision,execution_digest,status,candidate_tree_before,candidate_tree_after,log_relpath,started_at,finished_at) VALUES ('run-ddl-1','task:T1:build',1,1,'dig','running','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',NULL,'l.log','2026-01-01T00:00:00Z','2026-01-01T00:01:00Z');",
            ),
            (
                "gate_traces_running_requires_null_tree_after",
                "INSERT INTO gate_traces (run_id,node_id,claim_revision,contract_revision,execution_digest,status,candidate_tree_before,candidate_tree_after,log_relpath,started_at,finished_at) VALUES ('run-ddl-1','task:T1:build',1,1,'dig','running','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','l.log','2026-01-01T00:00:00Z',NULL);",
            ),
        ];

        for &(label, sql) in negative_sql_cases {
            assert!(
                mem.execute_raw_sql(sql).is_err(),
                "expected DDL failure for case '{label}'"
            );
        }

        // Completed run can share worktree path, and partial unique indexes allow historical rows.
        mem.execute_raw_sql(
            "INSERT INTO runs VALUES ('r-done','n-done',NULL,'/repo','code','{}',NULL,'d',0,'worktree',NULL,NULL,NULL,'/repo/.worktrees/AC-001','t','completed',NULL,0,1,'2026-01-01T00:00:00Z','2026-01-01T00:05:00Z',NULL,'[]');",
        )
        .unwrap();

        // uq_gate_claims_one_active allows multiple closed claims, rejects second active claim.
        mem.execute_raw_sql(
            "INSERT INTO gate_claims (run_id,node_id,claim_revision,worker_id,worktree,span_id,claim_tree,claimed_at,ended_at,outcome) VALUES ('run-ddl-1','task:T1:build',1,'w1','/wt','s1','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-01-01T00:00:00Z','2026-01-01T00:01:00Z','rejected');
             INSERT INTO gate_claims (run_id,node_id,claim_revision,worker_id,worktree,span_id,claim_tree,claimed_at,ended_at,outcome) VALUES ('run-ddl-1','task:T1:build',2,'w1','/wt','s2','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-01-01T00:02:00Z',NULL,'active');",
        )
        .unwrap();
        assert!(mem
            .execute_raw_sql(
                "INSERT INTO gate_claims (run_id,node_id,claim_revision,worker_id,worktree,span_id,claim_tree,claimed_at,ended_at,outcome) VALUES ('run-ddl-1','task:T1:build',3,'w2','/wt','s3','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-01-01T00:03:00Z',NULL,'active');"
            )
            .is_err());

        // uq_gate_traces_one_running allows multiple finished traces, rejects second running trace.
        mem.execute_raw_sql(
            "INSERT INTO gate_traces (run_id,node_id,claim_revision,contract_revision,execution_digest,status,candidate_tree_before,candidate_tree_after,log_relpath,started_at,finished_at) VALUES ('run-ddl-1','task:T1:build',2,1,'d1','failed','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','l1.log','2026-01-01T00:02:00Z','2026-01-01T00:02:01Z');
             INSERT INTO gate_traces (run_id,node_id,claim_revision,contract_revision,execution_digest,status,candidate_tree_before,candidate_tree_after,log_relpath,started_at,finished_at) VALUES ('run-ddl-1','task:T1:build',2,1,'d2','running','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',NULL,'l2.log','2026-01-01T00:02:05Z',NULL);",
        )
        .unwrap();
        assert!(mem
            .execute_raw_sql(
                "INSERT INTO gate_traces (run_id,node_id,claim_revision,contract_revision,execution_digest,status,candidate_tree_before,candidate_tree_after,log_relpath,started_at,finished_at) VALUES ('run-ddl-1','task:T1:build',2,1,'d3','running','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',NULL,'l3.log','2026-01-01T00:02:06Z',NULL);"
            )
            .is_err());

        // ON DELETE CASCADE cleans all child tables.
        mem.execute_raw_sql("DELETE FROM runs WHERE run_id = 'run-ddl-1';")
            .unwrap();
        assert!(mem
            .load_run_graph(&RunId::new("run-ddl-1").unwrap())
            .unwrap()
            .is_none());

        // Domain newtypes are validated on read: tampered row fails closed.
        mem.execute_raw_sql(
            "INSERT INTO runs VALUES ('bad id with spaces','n-bad',NULL,'/repo','code','{}',NULL,'d',0,'worktree',NULL,NULL,NULL,NULL,'t','active',NULL,0,1,'2026-01-01T00:00:00Z',NULL,NULL,'[]');",
        )
        .unwrap();
        assert!(mem
            .list_runs(&ListRunsReq::default())
            .is_err_and(|e| e.code == ErrorCode::WorkflowSchemaInvalid));
    }

    #[test]
    fn cas_transitions_pagination_and_worktree_resolution_matrix() {
        let harness = RunHarness::in_memory().unwrap();
        let mut graph = RunHarness::build_sample_graph(
            "run-cas-1",
            "AC-002-cas",
            "/repo",
            Some("/repo/.worktrees/AC-002"),
            &["T1", "T2"],
        )
        .unwrap();
        graph.dependencies.push(TaskDependencyRecord {
            run_id: graph.run.run_id.clone(),
            task_ref: TaskRef::new("T2").unwrap(),
            depends_on_workspace: "/repo".to_owned(),
            depends_on_run_id: graph.run.run_id.clone(),
            depends_on_ref: TaskRef::new("T1").unwrap(),
            satisfied: false,
        });
        harness.seed_run(&graph).unwrap();

        let rid = RunId::new("run-cas-1").unwrap();
        let worker1 = WorkerId::new("worker-1").unwrap();
        let worker2 = WorkerId::new("worker-2").unwrap();
        let t1_build = "task:T1:build";

        // Admit claim rev 1
        let claim1 = harness
            .exec_tx(|tx| {
                tx.admit_claim(
                    &rid,
                    &StoragePreconditions {
                        expected_run_generation: 1,
                        expected_node_versions: vec![NodeVersionExpectation {
                            node_id: t1_build.to_owned(),
                            expected_version: 1,
                        }],
                        expected_claim_revision: Some(0),
                        expected_contract_revision: Some(1),
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &ClaimInsertInput {
                        node_id: t1_build.to_owned(),
                        claim_revision: 1,
                        worker_id: worker1.clone(),
                        session_fingerprint: Some("host:10:1".to_owned()),
                        worktree: "/repo/.worktrees/AC-002-T1".to_owned(),
                        branch: None,
                        baseline_commit: Some(oid_commit('1')),
                        span_id: "0123456789abcdef".to_owned(),
                        claim_tree: oid_tree('a'),
                        claimed_at: "2026-11-17T10:01:00.000Z".to_owned(),
                    },
                )
            })
            .unwrap();
        assert_eq!(claim1.claim_revision, 1);

        // Force-supersede active claim rev 1 -> rev 2, then mark_nodes_stale, then re-claim rev 3
        let claim2 = harness
            .exec_tx(|tx| {
                tx.admit_claim(
                    &rid,
                    &StoragePreconditions {
                        expected_run_generation: 2,
                        expected_node_versions: vec![NodeVersionExpectation {
                            node_id: t1_build.to_owned(),
                            expected_version: 2,
                        }],
                        expected_claim_revision: Some(1),
                        expected_contract_revision: Some(1),
                        expected_worker_id: Some(worker1.clone()),
                        required_predecessor_trees: Vec::new(),
                    },
                    &ClaimInsertInput {
                        node_id: t1_build.to_owned(),
                        claim_revision: 2,
                        worker_id: worker2.clone(),
                        session_fingerprint: Some("host:11:1".to_owned()),
                        worktree: "/repo/.worktrees/AC-002-T1".to_owned(),
                        branch: None,
                        baseline_commit: Some(oid_commit('1')),
                        span_id: "1123456789abcdef".to_owned(),
                        claim_tree: oid_tree('a'),
                        claimed_at: "2026-11-17T10:01:10.000Z".to_owned(),
                    },
                )
            })
            .unwrap();
        assert_eq!(claim2.claim_revision, 2);

        harness
            .exec_tx(|tx| {
                tx.mark_nodes_stale(
                    &rid,
                    &StoragePreconditions {
                        expected_run_generation: 3,
                        expected_node_versions: vec![NodeVersionExpectation {
                            node_id: t1_build.to_owned(),
                            expected_version: 3,
                        }],
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &[t1_build.to_owned()],
                    "2026-11-17T10:01:15.000Z",
                )
            })
            .unwrap();
        assert_eq!(
            harness.load_node(&rid, t1_build).unwrap().unwrap().status,
            NodeStatus::Stale
        );
        assert_eq!(
            harness
                .latest_claim(&rid, t1_build)
                .unwrap()
                .unwrap()
                .outcome,
            ClaimOutcome::Reopened
        );

        let claim3 = harness
            .exec_tx(|tx| {
                tx.admit_claim(
                    &rid,
                    &StoragePreconditions {
                        expected_run_generation: 4,
                        expected_node_versions: vec![NodeVersionExpectation {
                            node_id: t1_build.to_owned(),
                            expected_version: 4,
                        }],
                        expected_claim_revision: Some(2),
                        expected_contract_revision: Some(1),
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &ClaimInsertInput {
                        node_id: t1_build.to_owned(),
                        claim_revision: 3,
                        worker_id: worker1.clone(),
                        session_fingerprint: Some("host:10:1".to_owned()),
                        worktree: "/repo/.worktrees/AC-002-T1".to_owned(),
                        branch: None,
                        baseline_commit: Some(oid_commit('1')),
                        span_id: "2123456789abcdef".to_owned(),
                        claim_tree: oid_tree('a'),
                        claimed_at: "2026-11-17T10:01:20.000Z".to_owned(),
                    },
                )
            })
            .unwrap();
        assert_eq!(claim3.claim_revision, 3);

        // Resolve active run via task node worktree as well as run worktree
        assert_eq!(
            harness
                .resolve_active_run_by_worktree("/repo/.worktrees/AC-002")
                .unwrap()
                .map(|r| r.run_id),
            Some(rid.clone())
        );
        assert_eq!(
            harness
                .resolve_active_run_by_worktree("/repo/.worktrees/AC-002-T1")
                .unwrap()
                .map(|r| r.run_id),
            Some(rid.clone())
        );

        // Table-driven CAS conflict matrix for admit_claim, start_trace, apply_transition_plan
        let stale_cases: &[(&str, StoragePreconditions)] = &[
            (
                "stale_run_generation",
                StoragePreconditions {
                    expected_run_generation: 4, // bumped to 5 by claim3
                    expected_node_versions: vec![NodeVersionExpectation {
                        node_id: t1_build.to_owned(),
                        expected_version: 5,
                    }],
                    expected_claim_revision: Some(3),
                    expected_contract_revision: Some(1),
                    expected_worker_id: Some(worker1.clone()),
                    required_predecessor_trees: Vec::new(),
                },
            ),
            (
                "stale_node_version",
                StoragePreconditions {
                    expected_run_generation: 5,
                    expected_node_versions: vec![NodeVersionExpectation {
                        node_id: t1_build.to_owned(),
                        expected_version: 4, // bumped to 5
                    }],
                    expected_claim_revision: Some(3),
                    expected_contract_revision: Some(1),
                    expected_worker_id: Some(worker1.clone()),
                    required_predecessor_trees: Vec::new(),
                },
            ),
            (
                "wrong_worker_id",
                StoragePreconditions {
                    expected_run_generation: 5,
                    expected_node_versions: vec![NodeVersionExpectation {
                        node_id: t1_build.to_owned(),
                        expected_version: 5,
                    }],
                    expected_claim_revision: Some(3),
                    expected_contract_revision: Some(1),
                    expected_worker_id: Some(worker2.clone()),
                    required_predecessor_trees: Vec::new(),
                },
            ),
            (
                "missing_predecessor_tree",
                StoragePreconditions {
                    expected_run_generation: 5,
                    expected_node_versions: vec![NodeVersionExpectation {
                        node_id: t1_build.to_owned(),
                        expected_version: 5,
                    }],
                    expected_claim_revision: Some(3),
                    expected_contract_revision: Some(1),
                    expected_worker_id: Some(worker1.clone()),
                    required_predecessor_trees: vec![PredecessorTreeExpectation {
                        node_id: "task:T1:build".to_owned(),
                        expected_accepted_tree: oid_tree('f'),
                    }],
                },
            ),
        ];

        for (label, pre) in stale_cases {
            let err = harness
                .exec_tx(|tx| {
                    tx.apply_transition_plan(
                        &rid,
                        &TransitionPlan {
                            preconditions: pre.clone(),
                            bump_run_generation: true,
                            new_run_status: None,
                            new_root_cycle_count: None,
                            completed_at: None,
                            node_patches: vec![NodeStatusPatch::status_only(
                                t1_build,
                                NodeStatus::Completed,
                            )],
                            dependency_patches: Vec::new(),
                            claim_patches: Vec::new(),
                            new_artifacts: Vec::new(),
                            new_blackboard: Vec::new(),
                            blackboard_patches: Vec::new(),
                            new_subtasks: Vec::new(),
                            significant_transition: true,
                        },
                    )
                })
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::BackendConflict, "case {label}");
        }

        // Start & finish trace
        let trace = harness
            .exec_tx(|tx| {
                tx.start_trace(
                    &StoragePreconditions {
                        expected_run_generation: 5,
                        expected_node_versions: vec![NodeVersionExpectation {
                            node_id: t1_build.to_owned(),
                            expected_version: 5,
                        }],
                        expected_claim_revision: Some(3),
                        expected_contract_revision: Some(1),
                        expected_worker_id: Some(worker1.clone()),
                        required_predecessor_trees: Vec::new(),
                    },
                    &TraceStartInput {
                        run_id: rid.clone(),
                        node_id: t1_build.to_owned(),
                        claim_revision: 3,
                        contract_revision: 1,
                        execution_digest: "exec1".to_owned(),
                        idempotency_key: None,
                        command_id: Some("build_check".to_owned()),
                        command_line: Some("cargo check".to_owned()),
                        action_id: None,
                        test_target: None,
                        candidate_tree_before: oid_tree('a'),
                        log_relpath: "logs/t1-build-rev3.log".to_owned(),
                        started_at: "2026-11-17T10:02:00.000Z".to_owned(),
                    },
                )
            })
            .unwrap();
        let trace_id = trace.trace_row_id.unwrap();
        harness
            .exec_tx(|tx| {
                tx.finish_trace(&TraceFinishInput {
                    trace_row_id: trace_id,
                    status: TraceStatus::Passed,
                    exit_code: Some(0),
                    passed_tests: vec!["t1".to_owned()],
                    failed_tests: Vec::new(),
                    check_results: Vec::new(),
                    candidate_tree_after: Some(oid_tree('b')),
                    finished_at: "2026-11-17T10:02:05.000Z".to_owned(),
                })
            })
            .unwrap();
        assert_eq!(
            harness
                .latest_trace(&rid, t1_build, 3)
                .unwrap()
                .unwrap()
                .status,
            TraceStatus::Passed
        );

        // Apply valid transition plan (completes node, closes claim, satisfies T1->T2 dep, adds subtask, blackboard, artifact)
        let mut node_patch = NodeStatusPatch::status_only(t1_build, NodeStatus::Completed);
        node_patch.accepted_tree = Some(Some(oid_tree('b')));
        let bb_item = BlackboardRecord {
            id: None,
            run_id: rid.clone(),
            node_id: Some(t1_build.to_owned()),
            recipient_node_id: None,
            subtask_ref: None,
            gate: Some("build".to_owned()),
            author: "worker-1".to_owned(),
            topic: BlackboardTopic::Notes,
            status: BlackboardStatus::Open,
            payload: "compiled cleanly".to_owned(),
            created_at: "2026-11-17T10:02:06.000Z".to_owned(),
        };
        harness
            .exec_tx(|tx| {
                tx.insert_blackboard(&BlackboardRecord {
                    payload: "first note".to_owned(),
                    ..bb_item.clone()
                })?;
                tx.apply_transition_plan(
                    &rid,
                    &TransitionPlan {
                        preconditions: StoragePreconditions {
                            expected_run_generation: 5,
                            expected_node_versions: vec![NodeVersionExpectation {
                                node_id: t1_build.to_owned(),
                                expected_version: 5,
                            }],
                            expected_claim_revision: Some(3),
                            expected_contract_revision: Some(1),
                            expected_worker_id: Some(worker1.clone()),
                            required_predecessor_trees: Vec::new(),
                        },
                        bump_run_generation: true,
                        new_run_status: None,
                        new_root_cycle_count: None,
                        completed_at: None,
                        node_patches: vec![node_patch],
                        dependency_patches: vec![DependencySatisfiedPatch {
                            depends_on_ref: TaskRef::new("T1").unwrap(),
                            dependent_task_ref: Some(TaskRef::new("T2").unwrap()),
                            satisfied: true,
                        }],
                        claim_patches: vec![ClaimOutcomePatch {
                            node_id: t1_build.to_owned(),
                            claim_revision: 3,
                            outcome: ClaimOutcome::Passed,
                            ended_at: "2026-11-17T10:02:06.000Z".to_owned(),
                        }],
                        new_artifacts: vec![GateArtifactRecord {
                            run_id: rid.clone(),
                            node_id: t1_build.to_owned(),
                            claim_revision: 3,
                            kind: ArtifactKind::Proof,
                            payload_json: r#"{"ok":true}"#.to_owned(),
                            snapshot_commit: None,
                            created_at: "2026-11-17T10:02:06.000Z".to_owned(),
                        }],
                        new_blackboard: vec![bb_item],
                        blackboard_patches: vec![BlackboardStatusPatch {
                            id: 1,
                            status: BlackboardStatus::Resolved,
                        }],
                        new_subtasks: vec![SubtaskRecord {
                            run_id: rid.clone(),
                            node_id: "task:T1".to_owned(),
                            subtask_ref: SubtaskRef::new("S1").unwrap(),
                            seq_order: 0,
                            title: "implement store".to_owned(),
                            status: SubtaskStatus::Completed,
                            evidence: Some("verified".to_owned()),
                            updated_at: "2026-11-17T10:02:06.000Z".to_owned(),
                        }],
                        significant_transition: true,
                    },
                )
            })
            .unwrap();

        // Pagination & generation pin checks
        let page = harness
            .list_blackboard(
                &rid,
                &BlackboardListReq {
                    limit: 1,
                    offset: 0,
                    generation: Some(6),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.generation, 6);

        let drifted_bb = harness
            .list_blackboard(
                &rid,
                &BlackboardListReq {
                    generation: Some(5),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(drifted_bb.code, ErrorCode::BackendConflict);

        let runs_page = harness
            .list_runs(&ListRunsReq {
                limit: 500, // clamped to 100
                generation: Some(6),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(runs_page.limit, 100);
        assert_eq!(runs_page.runs.len(), 1);
        assert_eq!(
            harness.max_spec_number("milestones", "AC").unwrap(),
            Some(2)
        );
        let obs = harness.observability_snapshot().unwrap();
        assert_eq!(obs.active_runs, 1);
        assert_eq!(obs.open_blackboard_items, 1);
        assert_eq!(obs.traces_by_status.get("passed"), Some(&1));

        // Archive & prune run -> tombstone recorded, active graph deleted, cleanup cleared
        harness
            .exec_tx(|tx| {
                tx.archive_and_prune_run(
                    &StoragePreconditions {
                        expected_run_generation: 6,
                        expected_node_versions: Vec::new(),
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &RunTombstoneRecord {
                        run_id: rid.clone(),
                        run_name: RunName::new("AC-002-cas").unwrap(),
                        collection: Some("milestones".to_owned()),
                        workspace: "/repo".to_owned(),
                        spec_id: Some("AC-002".to_owned()),
                        title: "CAS".to_owned(),
                        profile_id: "code".to_owned(),
                        is_ephemeral: false,
                        generation: 6,
                        terminal_status: TerminalStatus::Completed,
                        target_base: Some("main".to_owned()),
                        target_base_commit: Some(oid_commit('1')),
                        archive_path: Some("docs/milestones/archive/AC-002-cas.md".to_owned()),
                        closed_commit: Some(oid_commit('2')),
                        cleanup_pending: true,
                        cleanup_manifest: Vec::new(),
                        summary_json: "{}".to_owned(),
                        metrics_json: "{}".to_owned(),
                        closed_at: "2026-11-17T10:03:00.000Z".to_owned(),
                    },
                )?;
                tx.mark_tombstone_cleaned(&rid)
            })
            .unwrap();
        assert!(harness.load_run_graph(&rid).unwrap().is_none());
        assert!(
            !harness
                .load_tombstone(&rid)
                .unwrap()
                .unwrap()
                .cleanup_pending
        );
        assert_eq!(
            harness.max_spec_number("milestones", "AC").unwrap(),
            Some(2)
        );
    }

    #[cfg(unix)]
    #[test]
    fn disk_security_read_only_zero_create_and_backup_reconciliation_matrix() {
        use std::os::unix::fs::symlink;

        let temp_before = count_contextunity_temp_dirs();
        {
            let tmp = tempfile::Builder::new()
                .prefix("acdd-state-disk-matrix-")
                .tempdir()
                .unwrap();
            let root = tmp.path();

            // 1. Read-only zero-create behavior (`modules.state: false` / read-only safety)
            let missing_db = root.join("never-created/.acdd/state.sqlite");
            let ro_missing = SqliteStateReader::new(&missing_db);
            assert!(ro_missing
                .load_run_graph(&RunId::new("run-1").unwrap())
                .unwrap()
                .is_none());
            assert_eq!(
                ro_missing
                    .list_runs(&ListRunsReq::default())
                    .unwrap()
                    .total_runs,
                0
            );
            assert_eq!(ro_missing.observability_snapshot().unwrap().active_runs, 0);
            assert!(!missing_db.exists());
            assert!(!root.join("never-created").exists());

            // 2. SQLITE_OPEN_NOFOLLOW + symlink guard on file and ancestor dir
            let real_dir = root.join("real");
            fs::create_dir_all(&real_dir).unwrap();
            let real_db = real_dir.join("state.sqlite");
            let _seeded = SqliteStateBackend::open(&real_db).unwrap();

            let link_file = root.join("alias.sqlite");
            symlink(&real_db, &link_file).unwrap();
            assert!(SqliteStateReader::new(&link_file)
                .observability_snapshot()
                .is_err());
            assert!(SqliteStateBackend::open(&link_file).is_err());

            let link_dir = root.join("alias_dir");
            symlink(&real_dir, &link_dir).unwrap();
            assert!(SqliteStateReader::new(link_dir.join("state.sqlite"))
                .observability_snapshot()
                .is_err());
            assert!(SqliteStateBackend::open(link_dir.join("state.sqlite")).is_err());

            // 3. Git-backed workspace with backup + archive restore reconciliation
            let ws = root.join("repo");
            fs::create_dir_all(ws.join("docs/milestones/archive")).unwrap();
            assert!(git_command(&ws, &["init"]).unwrap().status.success());
            assert!(
                git_command(&ws, &["config", "user.email", "test@example.com"])
                    .unwrap()
                    .status
                    .success()
            );
            assert!(git_command(&ws, &["config", "user.name", "Test"])
                .unwrap()
                .status
                .success());
            let archived_spec = "docs/milestones/archive/AC-010-done.md";
            fs::write(
                ws.join(archived_spec),
                "---\nid: AC-010\ntitle: Done Run\nstatus: completed\n---\n",
            )
            .unwrap();
            assert!(git_command(&ws, &["add", archived_spec])
                .unwrap()
                .status
                .success());
            assert!(git_command(&ws, &["commit", "-m", "commit archived spec"])
                .unwrap()
                .status
                .success());

            let db_path = ws.join(".acdd/state.sqlite");
            let backup_dir = ws.join(".git/acdd");
            let opts = SqliteBackendOptions {
                db_path: db_path.clone(),
                backup_dir: Some(backup_dir.clone()),
                workspace_root: Some(ws.clone()),
                spec_collection_paths: vec!["docs/milestones".to_owned()],
                spec_collections: BTreeMap::new(),
                is_custom_db: false,
            };

            let backend = SqliteStateBackend::open_with_options(opts.clone()).unwrap();
            // Verify PRAGMAs on both writer and read-only reader
            backend
                .inspect_writer(|conn| {
                    let fk: i64 = conn
                        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                        .unwrap();
                    let jm: String = conn
                        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                        .unwrap();
                    let sync: i64 = conn
                        .query_row("PRAGMA synchronous", [], |r| r.get(0))
                        .unwrap();
                    assert_eq!(fk, 1);
                    assert_eq!(jm.to_ascii_lowercase(), "wal");
                    assert_eq!(sync, 1); // NORMAL
                    Ok(())
                })
                .unwrap();
            backend
                .inspect_read_only(|conn| {
                    let qo: i64 = conn
                        .query_row("PRAGMA query_only", [], |r| r.get(0))
                        .unwrap();
                    let fk: i64 = conn
                        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                        .unwrap();
                    assert_eq!(qo, 1);
                    assert_eq!(fk, 1);
                    Ok(())
                })
                .unwrap();

            let mut g_done = RunHarness::build_sample_graph(
                "run-010",
                "AC-010-done",
                ws.to_str().unwrap(),
                None,
                &["T1"],
            )
            .unwrap();
            g_done.run.spec_path = Some("docs/milestones/AC-010-done.md".to_owned());

            let mut g_active = RunHarness::build_sample_graph(
                "run-011",
                "AC-011-live",
                ws.to_str().unwrap(),
                None,
                &["T1"],
            )
            .unwrap();
            g_active.run.spec_path = Some("docs/milestones/AC-011-live.md".to_owned());

            backend
                .exec_tx(|tx| {
                    tx.insert_run_graph(&g_done)?;
                    tx.insert_run_graph(&g_active)
                })
                .unwrap();
            backend.after_commit_maintenance().unwrap();
            assert!(backup_dir.join("state.bak.sqlite").exists());
            assert!(backup_dir.join("backup.lock").exists());
            let meta_after_backup = backend.read_meta().unwrap();
            assert_eq!(meta_after_backup.write_seq, 2);
            assert_eq!(meta_after_backup.last_backup_seq, 2);

            // Non-significant trace write advances write_seq without forcing backup
            backend
                .exec_tx(|tx| {
                    tx.insert_blackboard(&BlackboardRecord {
                        id: None,
                        run_id: RunId::new("run-011").unwrap(),
                        node_id: None,
                        recipient_node_id: None,
                        subtask_ref: None,
                        gate: None,
                        author: "tester".to_owned(),
                        topic: BlackboardTopic::Notes,
                        status: BlackboardStatus::Open,
                        payload: "ping".to_owned(),
                        created_at: "2026-11-17T10:04:00.000Z".to_owned(),
                    })?;
                    Ok(())
                })
                .unwrap();
            backend.after_commit_maintenance().unwrap();
            let meta_after_note = backend.read_meta().unwrap();
            assert_eq!(meta_after_note.write_seq, 3);
            assert_eq!(meta_after_note.last_backup_seq, 2);
            drop(backend);

            // Simulate state.sqlite loss (including WAL/SHM sidecars), reopen -> restore + archive reconciliation
            let _ = fs::remove_file(&db_path);
            let _ = fs::remove_file(ws.join(".acdd/state.sqlite-wal"));
            let _ = fs::remove_file(ws.join(".acdd/state.sqlite-shm"));

            let restored = RunHarness::open_sqlite_with_archive_reconciler(opts).unwrap();
            let done_id = RunId::new("run-010").unwrap();
            let live_id = RunId::new("run-011").unwrap();
            assert!(restored.load_run_graph(&done_id).unwrap().is_none());
            let tomb = restored.load_tombstone(&done_id).unwrap().unwrap();
            assert_eq!(tomb.terminal_status, TerminalStatus::Completed);
            assert_eq!(tomb.archive_path.as_deref(), Some(archived_spec));
            assert!(restored.load_run_graph(&live_id).unwrap().is_some());
        }
        let temp_after = count_contextunity_temp_dirs();
        assert_eq!(temp_before, temp_after);
    }
}
