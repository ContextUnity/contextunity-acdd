use acdd_core::config::{SpecConfig, ACDD_DIR};
use acdd_core::errors::AcddError;
use acdd_core::ids::RunId;
use acdd_spec::frontmatter::split_frontmatter;
use acdd_spec::overlay::{
    NoopRuntimeOverlay, RuntimeActiveExternalDep, RuntimeActiveRun, RuntimeActiveTask,
    SpecRuntimeOverlay,
};
use acdd_spec::roadmap::{build_roadmap, RoadmapConfig};
use acdd_spec::types::{DraftReq, RoadmapReq};
use acdd_spec::validate::{
    build_resp, load_strict_adapter, pass, scan_collection_specs, validate_spec_static_checks,
};
use acdd_spec::{draft_spec, SpecDocument};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn parsed_toml(path: &Path) -> toml::Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn inv_spec_crate_boundary_and_workspace_deps_ssot() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // 1. Workspace SSOT `[workspace.package]` + `[workspace.dependencies]` inheritance.
    let root_cargo = parsed_toml(&manifest_dir.join("Cargo.toml"));
    assert!(
        root_cargo.get("workspace").and_then(|w| w.get("package")).is_some(),
        "root Cargo.toml must declare [workspace.package]"
    );
    assert!(
        root_cargo
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .is_some(),
        "root Cargo.toml must declare [workspace.dependencies]"
    );
    let members: Vec<&str> = root_cargo["workspace"]["members"]
        .as_array()
        .expect("workspace.members array")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert!(
        members.contains(&"crates/acdd-spec"),
        "workspace.members must contain crates/acdd-spec"
    );

    for rel in [
        "Cargo.toml",
        "crates/acdd-core/Cargo.toml",
        "crates/acdd-spec/Cargo.toml",
        "crates/acdd-index/Cargo.toml",
        "crates/acdd-state/Cargo.toml",
    ] {
        let parsed = parsed_toml(&manifest_dir.join(rel));
        assert_eq!(
            parsed["package"]["version"]["workspace"].as_bool(),
            Some(true),
            "{rel}: package.version must inherit from workspace"
        );
        assert_eq!(
            parsed["package"]["edition"]["workspace"].as_bool(),
            Some(true),
            "{rel}: package.edition must inherit from workspace"
        );
    }

    // 2. `crates/acdd-core` never depends on `acdd-spec`; `crates/acdd-spec` only depends on
    //    `acdd-core` + `{serde, serde_json, serde_yaml, schemars}`.
    let core_cargo = parsed_toml(&manifest_dir.join("crates/acdd-core/Cargo.toml"));
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = core_cargo.get(section).and_then(toml::Value::as_table) {
            assert!(
                !table.contains_key("acdd-spec"),
                "crates/acdd-core/Cargo.toml [{section}] must not depend on acdd-spec"
            );
        }
    }

    let spec_cargo = parsed_toml(&manifest_dir.join("crates/acdd-spec/Cargo.toml"));
    let allowed_spec_deps: BTreeSet<&str> =
        ["acdd-core", "serde", "serde_json", "serde_yaml", "schemars"]
            .into_iter()
            .collect();
    let deps = spec_cargo
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("crates/acdd-spec/Cargo.toml [dependencies]");
    for (dep_name, val) in deps {
        assert!(
            allowed_spec_deps.contains(dep_name.as_str()),
            "crates/acdd-spec/Cargo.toml [dependencies] contains disallowed crate '{dep_name}'"
        );
        assert_eq!(
            val.get("workspace").and_then(toml::Value::as_bool),
            Some(true),
            "crates/acdd-spec dependency '{dep_name}' must use workspace = true"
        );
    }
    for section in ["dev-dependencies", "build-dependencies"] {
        if let Some(table) = spec_cargo.get(section).and_then(toml::Value::as_table) {
            for dep_name in table.keys() {
                assert!(
                    allowed_spec_deps.contains(dep_name.as_str()),
                    "crates/acdd-spec/Cargo.toml [{section}] contains disallowed crate '{dep_name}'"
                );
            }
        }
    }

    // 3. `crates/acdd-spec/src/**` source boundary: zero SQLite, index, state, regex, walkdir, or runtimes.
    let mut spec_rs = Vec::new();
    collect_rs_files(&manifest_dir.join("crates/acdd-spec/src"), &mut spec_rs);
    assert!(!spec_rs.is_empty());
    let banned_in_spec = [
        "acdd_index",
        "acdd_state",
        "SqliteStateReader",
        "rusqlite",
        "tree_sitter",
        "regex::",
        "use regex",
        "walkdir::",
        "use walkdir",
        "ignore::",
        "use ignore",
        "clap::",
        "use clap",
        "rmcp::",
        "use rmcp",
        "tokio::",
        "use tokio",
        "pulldown_cmark",
        "env::temp_dir",
    ];
    for file in &spec_rs {
        let code = std::fs::read_to_string(file).unwrap();
        let rel = file.strip_prefix(manifest_dir).unwrap_or(file);
        for pat in banned_in_spec {
            assert!(
                !code.contains(pat),
                "{}: disallowed dependency token '{pat}' in crates/acdd-spec",
                rel.display()
            );
        }
    }

    // 4. `crates/acdd-index` may only enter `crates/acdd-spec` through `acdd_spec::frontmatter`
    //    and `crates/acdd-index/src/docs.rs` must not define a duplicate `fn split_frontmatter`.
    let mut index_rs = Vec::new();
    collect_rs_files(&manifest_dir.join("crates/acdd-index/src"), &mut index_rs);
    for file in &index_rs {
        let code = std::fs::read_to_string(file).unwrap();
        let rel = file.strip_prefix(manifest_dir).unwrap_or(file);
        for line in code.lines() {
            if line.contains("acdd_spec::") {
                assert!(
                    line.contains("acdd_spec::frontmatter::"),
                    "{}: crates/acdd-index may only reference acdd_spec::frontmatter::*, found: {line}",
                    rel.display()
                );
            }
        }
    }
    let docs_rs = std::fs::read_to_string(manifest_dir.join("crates/acdd-index/src/docs.rs")).unwrap();
    assert!(
        docs_rs.contains("acdd_spec::frontmatter::split_frontmatter"),
        "crates/acdd-index/src/docs.rs must reuse acdd_spec::frontmatter::split_frontmatter"
    );
    assert!(
        !docs_rs.contains("fn split_frontmatter"),
        "crates/acdd-index/src/docs.rs must not redefine fn split_frontmatter"
    );

    // 5. `crates/acdd-state` removed `src/spec.rs` and `validate.rs`/`deps/validate.rs`/`roadmap/**`
    //    route through `SpecRuntimeOverlay` without direct `SqliteStateReader`.
    assert!(
        !manifest_dir.join("crates/acdd-state/src/spec.rs").exists(),
        "crates/acdd-state/src/spec.rs must remain deleted (moved to crates/acdd-spec)"
    );
    assert!(
        !manifest_dir
            .join("crates/acdd-state/src/usecases/deps/validate.rs")
            .exists(),
        "crates/acdd-state/src/usecases/deps/validate.rs must remain consolidated into validate.rs + acdd-spec"
    );
    let mut roadmap_rs = Vec::new();
    collect_rs_files(
        &manifest_dir.join("crates/acdd-state/src/usecases/roadmap"),
        &mut roadmap_rs,
    );
    for rel in std::iter::once(PathBuf::from("crates/acdd-state/src/validate.rs")).chain(
        roadmap_rs
            .into_iter()
            .map(|p| p.strip_prefix(manifest_dir).unwrap().to_path_buf()),
    ) {
        let code = std::fs::read_to_string(manifest_dir.join(&rel)).unwrap();
        assert!(
            !code.contains("SqliteStateReader"),
            "{}: must use SpecRuntimeOverlay instead of direct SqliteStateReader",
            rel.display()
        );
    }
}

#[test]
fn noop_runtime_overlay_and_static_spec_seams_never_touch_sqlite() {
    let ws = crate::common::Workspace::new();
    let root = ws.root();
    std::fs::create_dir_all(root.join("docs/milestones")).unwrap();
    std::fs::write(
        root.join("acdd.yaml"),
        "specs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    )
    .unwrap();

    let split = split_frontmatter("---\nid: AC-001\n---\n# Body\n");
    assert!(split.yaml.is_some_and(|s| s.contains("id: AC-001")));
    assert_eq!(split.body.trim(), "# Body");

    let mut collections = BTreeMap::new();
    collections.insert(
        "milestones".to_owned(),
        SpecConfig {
            path: Some("docs/milestones".to_owned()),
            prefixes: vec!["AC".to_owned()],
            ..Default::default()
        },
    );

    let overlay = NoopRuntimeOverlay;
    let drafted = draft_spec(
        root,
        &collections,
        &DraftReq {
            spec: "milestones".to_owned(),
            slug: "pure-spec-crate".to_owned(),
            prefix: Some("AC".to_owned()),
            title: Some("Pure Spec Crate".to_owned()),
            workflow: None,
        },
        &overlay,
    )
    .expect("draft_spec succeeds over NoopRuntimeOverlay");
    assert_eq!(drafted.spec_id, "AC-001");

    let adapter = load_strict_adapter(root, None).expect("load_strict_adapter");
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();
    let (scanned, failed) = scan_collection_specs(root, &adapter, &mut diagnostics);
    assert_eq!(failed, 0);
    assert_eq!(scanned.len(), 1);
    let active_docs: Vec<SpecDocument> = scanned.iter().map(|s| s.doc.clone()).collect();
    let doc_failed = validate_spec_static_checks(
        root,
        &adapter,
        &overlay,
        scanned[0].col.as_ref(),
        &scanned[0].rel_path,
        &scanned[0].doc,
        &active_docs,
        true,
        &mut diagnostics,
        &mut warnings,
    );
    assert!(!doc_failed, "validate_spec_static_checks: {diagnostics:?}");
    diagnostics.push(pass(
        &scanned[0].rel_path,
        "SPEC_VALID",
        "validated".to_owned(),
    ));
    let ws_val = build_resp("workspace", diagnostics, warnings, scanned.len());
    assert!(ws_val.valid, "build_resp: {ws_val:?}");

    let linked = BTreeMap::new();
    let roadmap = build_roadmap(
        &RoadmapConfig {
            workspace_root: root,
            worktree: root,
            spec_collections: &collections,
            linked_workspaces: &linked,
        },
        &overlay,
        RoadmapReq::default(),
    )
    .expect("build_roadmap succeeds over NoopRuntimeOverlay");
    assert_eq!(roadmap.waves.len(), 1);
    assert_eq!(roadmap.waves[0].specs.len(), 1);
    assert_eq!(roadmap.waves[0].specs[0].spec_id, "AC-001");
    assert!(
        !root.join(ACDD_DIR).join("state.sqlite").exists(),
        "NoopRuntimeOverlay spec seams must never initialize state.sqlite"
    );

    // Verify SpecRuntimeOverlay lazy dispatch: excluded/dependency-free runs never trigger
    // active_run_external_deps, and project_active_run_tasks runs only when requested.
    struct LazyOverlay {
        runs: BTreeMap<String, Vec<RuntimeActiveRun>>,
        ext_calls: Cell<usize>,
        task_calls: Cell<usize>,
    }
    impl SpecRuntimeOverlay for LazyOverlay {
        fn load_roadmap_active_runs(
            &self,
        ) -> Result<BTreeMap<String, Vec<RuntimeActiveRun>>, AcddError> {
            Ok(self.runs.clone())
        }
        fn active_run_external_deps(
            &self,
            _run_id: &RunId,
            _owned_worktrees: &[PathBuf],
        ) -> Result<Vec<RuntimeActiveExternalDep>, AcddError> {
            self.ext_calls.set(self.ext_calls.get() + 1);
            Ok(Vec::new())
        }
        fn project_active_run_tasks(
            &self,
            _run_id: &RunId,
            _owned_worktrees: &[PathBuf],
        ) -> Result<Vec<RuntimeActiveTask>, AcddError> {
            self.task_calls.set(self.task_calls.get() + 1);
            Ok(vec![RuntimeActiveTask {
                task_ref: "T1".to_owned(),
                status: "ready".to_owned(),
                active_gates: Vec::new(),
                scope: Vec::new(),
                depends_on: Vec::new(),
                unsatisfied_local_blockers: BTreeSet::new(),
                has_unsatisfied_external_dep: false,
            }])
        }
    }
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let lazy_runs = BTreeMap::from([
        (
            "AC-001".to_owned(),
            vec![RuntimeActiveRun {
                run_id: RunId::new("run-local").unwrap(),
                spec_id: Some("AC-001".to_owned()),
                spec_path: Some(drafted.spec_path.clone()),
                worktree: Some(canon_root.to_string_lossy().into_owned()),
                branch: None,
                total_tasks: 1,
                completed_tasks: 0,
                active_gates: Vec::new(),
                completed_task_refs: BTreeSet::new(),
                local_task_blocks: Vec::new(),
                has_external_deps: false,
            }],
        ),
        (
            "AC-999".to_owned(),
            vec![RuntimeActiveRun {
                run_id: RunId::new("run-other-wt").unwrap(),
                spec_id: Some("AC-999".to_owned()),
                spec_path: Some("docs/milestones/AC-999.md".to_owned()),
                worktree: Some("/nonexistent/other-wt".to_owned()),
                branch: None,
                total_tasks: 1,
                completed_tasks: 0,
                active_gates: Vec::new(),
                completed_task_refs: BTreeSet::new(),
                local_task_blocks: Vec::new(),
                has_external_deps: true,
            }],
        ),
    ]);
    let lazy = LazyOverlay {
        runs: lazy_runs,
        ext_calls: Cell::new(0),
        task_calls: Cell::new(0),
    };
    let rm_cfg = RoadmapConfig {
        workspace_root: root,
        worktree: root,
        spec_collections: &collections,
        linked_workspaces: &linked,
    };
    let rm_default = build_roadmap(
        &rm_cfg,
        &lazy,
        RoadmapReq {
            current_only: true,
            include_tasks: false,
            ..RoadmapReq::default()
        },
    )
    .expect("scoped lazy roadmap");
    assert_eq!(lazy.ext_calls.get(), 0, "no eager external_deps under current_only/no-ext");
    assert_eq!(lazy.task_calls.get(), 0, "no eager task projection without include_tasks");
    assert!(rm_default.in_progress[0].tasks.is_empty());

    let rm_with_tasks = build_roadmap(
        &rm_cfg,
        &lazy,
        RoadmapReq {
            current_only: true,
            include_tasks: true,
            ..RoadmapReq::default()
        },
    )
    .expect("scoped roadmap with include_tasks");
    assert_eq!(lazy.ext_calls.get(), 0);
    assert_eq!(lazy.task_calls.get(), 1);
    assert_eq!(rm_with_tasks.in_progress[0].tasks.len(), 1);
}
