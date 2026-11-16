use crate::acdd::tasks::harness::TaskHarness;
use contextunity_acdd::core::tasks::gates::Evidence;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

impl TaskHarness {
    pub fn rejecting_pre_commit_hook(root: &Path) -> anyhow::Result<PathBuf> {
        let hook = root.join(".git/forced-hooks/pre-commit");
        fs::create_dir_all(hook.parent().unwrap())?;
        Self::reject_pre_commit_hook(&hook)?;
        Ok(hook)
    }

    pub fn reject_pre_commit_hook(hook: &Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        fs::write(hook, "#!/bin/sh\nexit 47\n")?;
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    pub fn recover_with_rewind_fault(
        root: &Path,
        checkpoint_ref: &str,
    ) -> anyhow::Result<(Output, Output)> {
        let real_git = TaskHarness::revision(root, "HEAD");
        let ref_lock = root.join(".git").join(format!("{checkpoint_ref}.lock"));
        fs::create_dir_all(ref_lock.parent().unwrap())?;
        // Hold the real ref lock so Git's update-ref fails through recovery's production path.
        let lock_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ref_lock)?;
        drop(lock_file);
        let recovery = crate::acdd::mcp::in_process_cli::run_cli_args(&[
            "--root".into(),
            root.as_os_str().to_owned(),
            "milestone".into(),
            "db-recover".into(),
            "--format".into(),
            "json".into(),
        ]);
        fs::remove_file(ref_lock)?;
        Ok((real_git, recovery))
    }
}

pub(crate) struct GcPublicationFixture {
    root: PathBuf,
    shim: PathBuf,
}

pub(crate) struct GcMatrixFixture {
    workspace: crate::common::Workspace,
    baseline: crate::acdd::snapshot::FixtureSnapshot,
    pub seed_outputs: Vec<Output>,
}

impl GcMatrixFixture {
    pub fn begin_case(&self, pending: bool) -> anyhow::Result<(TaskHarness, GcPublicationFixture)> {
        self.baseline.restore(&self.workspace);
        let mut task = TaskHarness::attach(&self.workspace, "acdd/m-gc:candidate");
        task.advance_to("build")?;
        task.claim_gate("build", "builder");
        self.workspace.write(
            "src/lib.rs",
            &format!("pub fn unique_provisional_candidate() -> bool {{ {pending} }}\n"),
        );
        let fault = TaskHarness::gc_publication_fault(&self.workspace, pending)?;
        Ok((task, fault))
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GcWindow {
    pub candidate: String,
    pub exists_after_gc: bool,
    pub gc_attempted: bool,
}

#[derive(serde::Deserialize)]
pub(crate) struct GcConstruction {
    pub gc_attempted: bool,
    pub exit: i32,
}

impl TaskHarness {
    pub fn gc_publication_fault(
        workspace: &crate::common::Workspace,
        pending: bool,
    ) -> anyhow::Result<GcPublicationFixture> {
        use std::os::unix::fs::PermissionsExt;
        workspace.write(
            ".git/info/exclude",
            ".acdd/\nfixture-gc-window.json\nfixture-gc-construction.json\n",
        );
        let shim = workspace.root().join("fixture-bin/git");
        if pending {
            workspace.write(
                "fixture-publication-fault",
                "isolate post-SQL keeper failure\n",
            );
        }
        // Most Git calls cannot reach either fault injection point. Avoid
        // starting the instrumentation binary for those calls while retaining
        // the same real Git subprocess and publication/GC boundaries.
        let role_helper = crate::acdd::snapshot::role_helper_path()
            .to_string_lossy()
            .replace('\'', "'\\''");
        workspace.write(
            "fixture-bin/git",
            &format!("#!/bin/sh\ncase \"$1\" in\n  -c|commit-tree) exec '{role_helper}' git-instrumented \"$@\" ;;\n  *) exec /usr/bin/git \"$@\" ;;\nesac\n"),
        );
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        Ok(GcPublicationFixture {
            root: workspace.root().into(),
            shim,
        })
    }
}

impl GcPublicationFixture {
    pub fn temporary_pack_present(&self) -> anyhow::Result<bool> {
        for entry in fs::read_dir(self.root.join(".git/objects/pack"))? {
            if entry?.path().extension().is_some_and(|ext| ext == "keep") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn release_publication_lock(&self) -> anyhow::Result<()> {
        fs::remove_file(
            self.root
                .join(".git/refs/acdd/state/acdd/acdd/m-gc/candidate.lock"),
        )?;
        Ok(())
    }

    pub fn recover(&self) -> Output {
        crate::acdd::mcp::in_process_cli::run_cli_args(
            &[
                "--root",
                self.root.to_str().unwrap(),
                "--format",
                "json",
                "milestone",
                "db-recover",
            ]
            .map(Into::into),
        )
    }

    pub fn submit_build(&self, id: &str, evidence: &Evidence) -> anyhow::Result<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_acdd"))
            .args([
                "--root",
                self.root.to_str().unwrap(),
                "--format",
                "json",
                "task",
                "submit",
                id,
                "--stage",
                "build",
                "--action",
                "pass",
                "--evidence",
                &serde_json::to_string(evidence)?,
            ])
            .env(
                "ACDD_AGENT_SESSION_ID",
                TaskHarness::session_for(&evidence.worker_id),
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.shim.parent().unwrap().display(),
                    std::env::var("PATH")?
                ),
            )
            .current_dir(&self.root)
            .output()?)
    }

    pub fn observations(&self) -> anyhow::Result<(GcWindow, GcConstruction)> {
        Ok((
            serde_json::from_slice(&fs::read(self.root.join("fixture-gc-window.json"))?)?,
            serde_json::from_slice(&fs::read(self.root.join("fixture-gc-construction.json"))?)?,
        ))
    }
}

impl TaskHarness {
    pub fn gc_matrix() -> GcMatrixFixture {
        let workspace = Self::gc_workspace();
        let seed_outputs = Self::protect_gc_seed(workspace.root());
        let baseline = crate::acdd::snapshot::FixtureSnapshot::capture(&workspace);
        GcMatrixFixture {
            workspace,
            baseline,
            seed_outputs,
        }
    }

    pub fn gc_workspace() -> crate::common::Workspace {
        let isolated = crate::acdd::support::acdd_workspace(crate::acdd::support::DEFAULT_SEAM, "");
        isolated.write(
            "acdd.yaml",
            "roots: [src]\ndocs: [docs]\ntasks_db: .acdd/tasks.sqlite\n",
        );
        isolated.write("src/lib.rs", "pub fn before() {}\n");
        isolated.write("docs/milestones/010-gc.md", "---\nid: m-gc\nstatus: active\n---\n```yaml\ntask_ref: candidate\ntarget: Preserve provisional candidate\nproof_policy: direct-proof\nscope: [src/]\n```\n");
        contextunity_acdd::engine::tasks::manage(
            isolated.root(),
            serde_json::from_value(
                serde_json::json!({"action":"sync","milestone_ref":"docs/milestones/010-gc.md"}),
            )
            .unwrap(),
        )
        .unwrap();
        isolated
    }
}
