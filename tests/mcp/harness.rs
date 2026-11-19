//! MCP-specific workspaces and indexed observations.

pub use crate::common::mcp_harness::{HarnessError, HarnessResult, McpTestHarness};
use crate::common::Workspace;
use std::sync::Arc;

#[allow(unused_imports)]
pub(crate) use crate::sqlite_fixture as sqlite;

impl McpTestHarness {
    /// Builds an indexed workspace directly for this harness instance.
    pub fn indexed(files: &[(&str, &str)]) -> HarnessResult<Self> {
        let workspace = Arc::new(Workspace::with_files(files));
        workspace.build();
        Self::with_session(&workspace, "shared-index").map(|mut harness| {
            harness._workspace_owner = Some(workspace);
            harness
        })
    }

    pub fn indexed_workspace(files: &[(&str, &str)]) -> (Arc<Workspace>, HarnessResult<Self>) {
        let workspace = Arc::new(Workspace::with_files(files));
        workspace.build();
        let harness = Self::with_session(&workspace, "shared-index").map(|mut harness| {
            harness._workspace_owner = Some(Arc::clone(&workspace));
            harness
        });
        (workspace, harness)
    }

    /// Isolated files the test may mutate. `Workspace::new` stays in the harness.
    pub fn with_files(files: &[(&str, &str)]) -> (Workspace, HarnessResult<Self>) {
        let workspace = Workspace::with_files(files);
        workspace.build();
        let harness = Self::new(&workspace);
        (workspace, harness)
    }

    pub fn workspace(&self) -> &Workspace {
        self._workspace_owner
            .as_deref()
            .expect("workspace owner must be present on indexed harness")
    }
}

pub fn session_workspace() -> anyhow::Result<Workspace> {
    let workspace = Workspace::with_files(&[
        ("AGENTS.md", "# Rules\n"),
        (".gitignore", ".acdd/\n"),
        (
            "acdd.yaml",
            "roots: [src/]\nspecs:\n  milestones:\n    path: docs/milestones\n",
        ),
        ("src/lib.rs", "pub fn entry() {}\n"),
        (
            "docs/milestones/010-session.md",
            "---\nid: m-session\ntitle: Sessions\ndoc_type: milestone\nstatus: active\n---\n# Sessions\n\n### task: first\n```yaml\ntask_ref: first\ntarget: Session independence\nscope: [src/]\n```\n",
        ),
        (
            ".acdd/profile.yaml",
            "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\nroles:\n  worker: {mode: subagent}\ncommands:\n  check:\n    command: \"printf 'test result: ok. 1 passed; 0 failed;\\\\n'\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: worker\n          proof: none\n          context: [docs.instructions]\n          pass: review\n        - id: review\n          role: worker\n          proof: command\n          command: check\n          independent_from: [build]\n          need: [build]\n          pass: deliver\n        - id: deliver\n          role: worker\n          receipt: true\n          need: [build, review]\n          save:\n            post: none\n",
        ),
    ]);
    workspace.seed_git()?;
    Ok(workspace)
}

#[test]
fn indexed_workspace_is_removed_after_its_last_harness_owner_drops() {
    let (workspace, harness) = McpTestHarness::indexed_workspace(&[(
        "src/indexed-owner-regression.rs",
        "pub fn indexed_owner_regression() {}\n",
    )]);
    let root = workspace.root().to_path_buf();
    let harness = harness.expect("indexed harness initializes");

    drop(workspace);
    assert!(root.is_dir(), "the harness retains its indexed workspace");

    drop(harness);
    assert!(
        !root.exists(),
        "the workspace is removed after its final owner drops"
    );
}
