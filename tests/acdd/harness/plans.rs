use crate::acdd::support::{git_command, git_head, init_git, Workspace};
use clap::Parser;
use contextunity_acdd::{core::models::DocSection, engine::docs};

/// Canonical plan documents extracted through the production Markdown indexer.
/// Extraction stays in memory; execution fixtures explicitly materialize Git state.
pub(crate) struct PlanHarness {
    pub path: &'static str,
    pub sections: Vec<DocSection>,
    source: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) enum PlanFixture {
    UnknownDependency,
    Cycle,
    UnsafeScope,
    CancelledDependency,
    PendingSubtask,
    MissingSubtaskEvidence,
    CompletedSubtask,
}

impl PlanHarness {
    fn run(&self, root: &Workspace) -> anyhow::Result<serde_json::Value> {
        let cli = contextunity_acdd::cli::Cli::try_parse_from([
            "forge",
            "--root",
            root.root().to_str().unwrap(),
            "--format",
            "json",
            "plan",
            "run",
            "linear",
            "--in-place",
        ])?;
        let mut output = Vec::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(cli.run_with_writer(&mut output))?;
        Ok(serde_json::from_slice(&output)?)
    }

    pub fn fixture(case: PlanFixture) -> Self {
        let source = match case {
            PlanFixture::PendingSubtask => include_str!("../fixtures/plans/pending-subtask.md"),
            PlanFixture::MissingSubtaskEvidence => {
                include_str!("../fixtures/plans/missing-subtask-evidence.md")
            }
            PlanFixture::CompletedSubtask => include_str!("../fixtures/plans/completed-subtask.md"),
            PlanFixture::UnknownDependency => {
                include_str!("../fixtures/plans/unknown-dependency.md")
            }
            PlanFixture::Cycle => include_str!("../fixtures/plans/cycle.md"),
            PlanFixture::UnsafeScope => include_str!("../fixtures/plans/unsafe-scope.md"),
            PlanFixture::CancelledDependency => {
                include_str!("../fixtures/plans/cancelled-dependency.md")
            }
        };
        Self::document("docs/plans/invalid.md", source)
    }
    pub fn linear() -> Self {
        Self::document(
            "docs/plans/linear.md",
            include_str!("../fixtures/plans/linear.md"),
        )
    }

    pub fn dag() -> Self {
        Self::document(
            "docs/plans/dag.md",
            include_str!("../fixtures/plans/dag.md"),
        )
    }

    pub fn materialize(&self, path: &str) -> Workspace {
        let root = Workspace::new();
        root.write(path, self.source);
        root.write("acdd.yaml", include_str!("../fixtures/plans/acdd.yaml"));
        root.write(
            "configs/custom.yaml",
            include_str!("../fixtures/plans/custom-config.yaml"),
        );
        root.write(
            "profiles/plan.yaml",
            include_str!("../fixtures/plans/snapshot.yaml"),
        );
        root.write(
            "profiles/rejected.yaml",
            include_str!("../fixtures/plans/rejected-profile.yaml"),
        );
        root.write(
            "profiles/commit.yaml",
            include_str!("../fixtures/plans/commit.yaml"),
        );
        root.write(
            "profiles/immutable-drift.yaml",
            include_str!("../fixtures/plans/immutable-drift.yaml"),
        );
        root.write("src/first.rs", "// initial plan scope\n");
        init_git(root.root());
        root
    }

    pub fn snapshots(&self, root: &Workspace) -> Vec<String> {
        let output = git_command(
            root.root(),
            &[
                "for-each-ref",
                "--format=%(objecttype) %(objectname) %(refname)",
                "refs/forge/",
            ],
        )
        .output()
        .expect("read plan snapshot refs");
        if !output.status.success() {
            panic!(
                "could not read plan snapshot refs: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout)
            .expect("Git ref output is UTF-8")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn document(path: &'static str, source: &'static str) -> Self {
        let sections = docs::extract_with_details(path, source, 0.0)
            .expect("canonical plan Markdown must extract")
            .into_iter()
            .map(|(section, _)| section)
            .collect();
        Self {
            path,
            sections,
            source,
        }
    }
}

#[test]
fn plans_exclude_configured_database_from_each_snapshot() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "acdd.yaml",
        "acdd_profile: profiles/plan.yaml\ntasks_db: runtime/tasks.sqlite\nplans: [docs/plans]\n",
    );
    root.write(
        "profiles/plan.yaml",
        include_str!("../fixtures/plans/configured-snapshot.yaml"),
    );
    root.write("unrelated.txt", "preserved\n");
    std::fs::create_dir_all(root.path("runtime")).unwrap();
    drop(acdd_tasks::store::TasksStore::open(&root.path("runtime/tasks.sqlite")).unwrap());
    let db = rusqlite::Connection::open(root.path("runtime/tasks.sqlite")).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; INSERT INTO meta(key, value) VALUES ('activity', 1);",
    )
    .unwrap();
    for suffix in ["", "-wal", "-shm"] {
        assert!(root.path(format!("runtime/tasks.sqlite{suffix}")).exists());
    }
    drop(db);
    root.write("runtime/tasks.sqlite-journal", "journal");
    root.write("runtime/tasks.sqlite.lock", "lock");
    let head = git_head(root.root());
    let result = harness.run(&root).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(git_head(root.root()), head);
    let refs = harness
        .snapshots(&root)
        .into_iter()
        .filter(|reference| reference.contains(" refs/forge/snapshots/"))
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 6, "{refs:?}");
    let mut completed = 0;
    let mut implemented = 0;
    for reference in refs {
        let name = reference.split_whitespace().last().unwrap();
        let output = git_command(root.root(), &["ls-tree", "-r", "--name-only", name])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree = String::from_utf8(output.stdout).unwrap();
        for suffix in ["", "-wal", "-shm", "-journal", ".lock"] {
            assert!(
                !tree
                    .lines()
                    .any(|p| p == format!("runtime/tasks.sqlite{suffix}")),
                "{name}: {tree}"
            );
        }
        assert!(root.path("unrelated.txt").exists());
        if name.starts_with("refs/forge/snapshots/forge-mcp/") {
            assert!(!tree.lines().any(|p| p == "unrelated.txt"));
        }
        if tree.lines().any(|path| path == "src/first.rs") {
            let output = git_command(root.root(), &["show", &format!("{name}:src/first.rs")])
                .output()
                .unwrap();
            assert!(output.status.success());
            let source = String::from_utf8(output.stdout).unwrap();
            implemented += usize::from(source.contains("implementation"));
        }
        if tree.lines().any(|path| path == "docs/plans/linear.md") {
            let output = git_command(
                root.root(),
                &["show", &format!("{name}:docs/plans/linear.md")],
            )
            .output()
            .unwrap();
            assert!(output.status.success());
            let plan = String::from_utf8(output.stdout).unwrap();
            if plan.contains("**Status**: completed") {
                assert!(plan.contains("**Evidence**:"));
                completed += 1;
            }
        }
    }
    assert!(implemented >= 4);
    assert!(completed >= 2);
    assert_eq!(
        std::fs::read_to_string(root.path("unrelated.txt")).unwrap(),
        "preserved\n"
    );
}

#[test]
fn plans_reject_source_drift_and_external_database_configuration() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "profiles/plan.yaml",
        include_str!("../fixtures/plans/drift-snapshot.yaml"),
    );
    let head = git_head(root.root());
    let error = harness.run(&root).unwrap_err();
    assert!(
        format!("{error:#}").contains("PLAN_SOURCE_DRIFT"),
        "{error:#}"
    );
    assert_eq!(git_head(root.root()), head);
    root.write(harness.path, harness.source);
    root.write("acdd.yaml", "acdd_profile: profiles/plan.yaml\ntasks_db: ../external-tasks.sqlite\nplans: [docs/plans]\n");
    let error = harness.run(&root).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_SCOPE_INVALID"),
        "{error:#}"
    );
}

#[test]
fn canonical_plans_reach_document_indexing_with_step_text_intact() {
    for (harness, titles, dependency) in [
        (
            PlanHarness::linear(),
            vec![
                "Canonical Linear Plan",
                "1. Step 1: First Action",
                "2. Step 2: Second Action",
            ],
            "Step 1: First Action",
        ),
        (
            PlanHarness::dag(),
            vec![
                "Canonical DAG Plan",
                "1. Step A: Independent Core",
                "2. Step B: Extension Beta",
                "3. Step C: Extension Gamma",
            ],
            "Step A: Independent Core",
        ),
    ] {
        assert_eq!(
            harness
                .sections
                .iter()
                .map(|s| s.section_title.as_str())
                .collect::<Vec<_>>(),
            titles
        );
        for section in &harness.sections {
            assert_eq!(section.path, harness.path);
            assert_eq!(section.doc_type, "plan");
        }
        for step in &harness.sections[2..] {
            assert!(step
                .content
                .contains(&format!("**Depends On**: [\"{dependency}\"]")));
        }
    }
}
