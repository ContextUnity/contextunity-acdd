use acdd_core::config::{default_specs, Adapter, LinkedWorkspace};
use acdd_core::errors::ErrorCode;
use acdd_core::git::git_command;
use acdd_core::ids::{NodeCursor, RunId, RunName, TaskRef};
use acdd_core::ports::{CheckCapability, CompositeDomainProvider, DomainCapabilities};
use acdd_core::response::ResponsePolicy;
use acdd_core::schema::SchemaDef;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_state::harness::RunHarness;
use acdd_state::machine::{evaluate_transition, ExtendScopeInput, TransitionInput};
use acdd_spec::parse_spec;
use acdd_state::workflow::resolve_optional_workflow_ref_with_dirs;
use acdd_state::store::traits::exec_tx_with;
use acdd_state::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, RunStatus,
    RunTombstoneRecord, StoragePreconditions, TerminalStatus,
};
use acdd_state::usecases::deps::verify_run_level_depends_on;
use acdd_state::usecases::overlay::validate_spec_dependencies;
use acdd_state::usecases::lifecycle::to_machine_profile;
use acdd_state::usecases::StateEngineConfig;
use acdd_state::validate::{validate_document, validate_spec_for_run, validate_workspace};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::common::StubDomainProvider;

fn full_providers() -> CompositeDomainProvider {
    let code = StubDomainProvider::new(
        "code",
        DomainCapabilities {
            checks: vec![
                CheckCapability {
                    name: "seam".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "lint".to_owned(),
                    input_schema: acdd_core::lint::lint_check_input_schema(),
                },
            ],
            context_lenses: BTreeSet::from([
                "symbols".to_owned(),
                "tests".to_owned(),
                "external_callers".to_owned(),
            ]),
            ..DomainCapabilities::default()
        },
    );
    let docs = StubDomainProvider::new(
        "docs",
        DomainCapabilities {
            checks: vec![
                CheckCapability {
                    name: "links".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "specs".to_owned(),
                    input_schema: SchemaDef::object(),
                },
            ],
            context_lenses: BTreeSet::from([
                "instructions".to_owned(),
                "adrs".to_owned(),
                "invariants".to_owned(),
                "workspace_config".to_owned(),
            ]),
            ..DomainCapabilities::default()
        },
    );
    CompositeDomainProvider::with_module_flags(
        vec![Arc::new(code), Arc::new(docs)],
        BTreeMap::new(),
    )
    .unwrap()
}

fn write_bound_profile(root: &Path, extra: &str) {
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        format!(
            "version: 2\ninherit: code\ncommands:\n  test: \"cargo test {{test_filter}}\"\n  build_check: \"cargo check\"\n  verify_all: \"cargo test\"\n{extra}"
        ),
    )
    .unwrap();
}

#[test]
fn validate_workspace_config_profile_and_spec_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let providers = full_providers();

    // 1. Unknown top-level key in acdd.yaml triggers ACDD_WORKFLOW_SCHEMA_INVALID via deny_unknown_fields.
    fs::write(root.join("acdd.yaml"), "bogus_unknown: true\n").unwrap();
    let res_schema = validate_workspace(root, None, None, &providers).unwrap();
    assert!(!res_schema.valid);
    assert_eq!(
        res_schema.diagnostics[0].code,
        ErrorCode::WorkflowSchemaInvalid.as_str()
    );

    // 2. Valid acdd.yaml + unbound commands with modules.state: true -> diagnostics[0] = CONFIG_VALID, diagnostics[1] = COMMAND_UNBOUND.
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: code\n",
    )
    .unwrap();
    let res_unbound = validate_workspace(root, None, None, &providers).unwrap();
    assert!(!res_unbound.valid);
    assert_eq!(res_unbound.diagnostics[0].code, "CONFIG_VALID");
    assert_eq!(res_unbound.diagnostics[1].code, "COMMAND_UNBOUND");

    // 4. Unbound command profile with modules.state: false passes (require_bound_commands = false).
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: false\n",
    )
    .unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: none\ndefault_tools: [acdd_read]\ncommands:\n  t:\n    unbound: true\ngates:\n  - id: verify\n    command: t\n    proof: command\n",
    )
    .unwrap();
    let res_state_off = validate_workspace(root, None, None, &providers).unwrap();
    assert!(res_state_off.valid, "{res_state_off:?}");

    // 5. Spec validation matrix (all invalid specs fail and NEVER emit SPEC_VALID).
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\n",
    )
    .unwrap();
    write_bound_profile(root, "protected_paths: [Cargo.toml, .acdd/workflows/default.yaml]\n");
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/plans")).unwrap();

    let bad_specs: &[(&str, &str)] = &[
        (
            "docs/milestones/AC-001-terminal.md",
            "---\nid: AC-001-terminal\ntitle: Done\nstatus: completed\n---\n",
        ),
        (
            "docs/milestones/BAD-002.md",
            "---\nid: BAD-002\ntitle: Wrong prefix\nstatus: planned\n---\n",
        ),
        (
            "docs/milestones/AC-1-short.md",
            "---\nid: AC-1-short\ntitle: Short digits\nstatus: planned\n---\n",
        ),
        (
            "docs/milestones/AC-003-planned-active.md",
            "---\nid: AC-003-planned-active\ntitle: Unreconciled\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: active\n```\n",
        ),
        (
            "docs/milestones/AC-004-bad-glob.md",
            "---\nid: AC-004-bad-glob\ntitle: Bad glob\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"src/{a,b\"]\n```\n",
        ),
        (
            "docs/milestones/AC-005-protected.md",
            "---\nid: AC-005-protected\ntitle: Protected\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [Cargo.toml]\n```\n",
        ),
        (
            "docs/milestones/AC-006-ext-dep.md",
            "---\nid: AC-006-ext-dep\ntitle: Missing external\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\ndepends_on: [\"AC-999:missing\"]\n```\n",
        ),
        (
            "docs/milestones/AC-007-spec-dep.md",
            "---\nid: AC-007-spec-dep\ntitle: Missing spec dep\nstatus: planned\ndepends_on: [AC-999]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        ),
    ];

    for &(rel, content) in bad_specs {
        fs::write(root.join(rel), content).unwrap();
        let doc_res = validate_document(root, Path::new(rel), None, None, &providers).unwrap();
        assert!(!doc_res.valid, "{rel} should fail: {doc_res:?}");
        assert!(
            !doc_res.diagnostics.iter().any(|d| d.code == "SPEC_VALID"),
            "{rel} must never emit SPEC_VALID when failing: {doc_res:?}"
        );
        fs::remove_file(root.join(rel)).unwrap();
    }

    // 6. Valid spec with lowercase plan prefix warning + cross-spec planned dependency passes.
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n  plans:\n    path: docs/plans\n    prefixes: [PLAN]\n    digits: 3\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-010-base.md"),
        "---\nid: AC-010-base\ntitle: Base\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"src/{a,b}.rs\"]\n```\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/plans/plan-001-case.md"),
        "---\nid: plan-001-case\ntitle: Lowercase plan\nstatus: planned\ndepends_on: [\"AC-010-base:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [docs/]\n```\n",
    )
    .unwrap();
    let ok_res = validate_workspace(root, None, None, &providers).unwrap();
    assert!(ok_res.valid, "{ok_res:?}");
    assert!(
        !ok_res.warnings.is_empty(),
        "expected case advisory warning"
    );

    // 6b. `Adapter.limits.max_metadata_bytes` validates `1..=2048` in `validate_workspace` while valid large specs (> 2048 bytes) pass.
    for bad_limit in [0usize, 2049usize] {
        fs::write(
            root.join("acdd.yaml"),
            format!(
                "modules:\n  code: true\n  docs: true\n  state: true\nlimits:\n  max_metadata_bytes: {bad_limit}\n"
            ),
        )
        .unwrap();
        let bad_lim_res = validate_workspace(root, None, None, &providers).unwrap();
        assert!(
            !bad_lim_res.valid,
            "limits.max_metadata_bytes={bad_limit} must fail: {bad_lim_res:?}"
        );
        assert_eq!(
            bad_lim_res.diagnostics[0].code,
            ErrorCode::WorkflowSchemaInvalid.as_str()
        );
    }
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\nlimits:\n  max_metadata_bytes: 1024\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n  plans:\n    path: docs/plans\n    prefixes: [PLAN]\n    digits: 3\n",
    )
    .unwrap();
    let large_spec_rel = "docs/milestones/AC-011-large-metadata.md";
    let large_padding = "x".repeat(2500);
    let large_spec_doc = format!(
        "---\nid: AC-011-large-metadata\ntitle: \"Large milestone {large_padding}\"\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: \"Task {large_padding}\"\nscope: [src/lib.rs]\n```\n"
    );
    fs::write(root.join(large_spec_rel), large_spec_doc).unwrap();
    let large_res =
        validate_document(root, Path::new(large_spec_rel), None, None, &providers).unwrap();
    assert!(
        large_res.valid,
        "spec frontmatter/task metadata > 2048 bytes must remain valid: {large_res:?}"
    );
    fs::remove_file(root.join(large_spec_rel)).unwrap();
    fs::write(
        root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n  plans:\n    path: docs/plans\n    prefixes: [PLAN]\n    digits: 3\n",
    )
    .unwrap();

    // 7. Reconciled planned spec with completed task in state.sqlite + legacy 0.5 archive + tombstone precedence + validate_spec_for_run.
    let archived_05_dir = root.join("docs/milestones/archive");
    fs::create_dir_all(&archived_05_dir).unwrap();
    fs::write(
        archived_05_dir.join("AC-090-legacy.md"),
        "---\nid: AC-090\ntitle: Legacy 0.5\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: completed\n```\n",
    )
    .unwrap();
    let dep_05_doc = parse_spec(
        "---\nid: AC-091\ntitle: Consumer\nstatus: planned\ndepends_on: [\"AC-090:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
    )
    .unwrap();

    let cfg = StateEngineConfig {
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        state_db_override: None,
        code_db_path: None,
        spec_collections: default_specs(),
        workflows_dirs: vec![".acdd/workflows".to_owned()],
        linked_workspaces: BTreeMap::new(),
        modules: SubsystemRegistry::default(),
        response_policy: ResponsePolicy::default(),
        retain_traces: 5,
        max_snapshot_file_bytes: None,
        allowed_binary_extensions: None,
    };
    let resolved_p =
        resolve_optional_workflow_ref_with_dirs(None, Some(root), Some(&cfg.modules), None).unwrap();
    let harness = RunHarness::in_memory().unwrap();
    validate_spec_for_run(&cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();

    // Tombstone precedence: Completed superseded by newer Cleared fails; Cleared superseded by newer Completed passes.
    let r_done = RunId::new("run-tomb-1").unwrap();
    let r_clear = RunId::new("run-tomb-2").unwrap();
    let g_done = RunGraphSnapshot {
        run: RunRecord {
            run_id: r_done.clone(),
            run_name: RunName::new("tomb-done").unwrap(),
            collection: Some("milestones".to_owned()),
            workspace: "default".to_owned(),
            profile_id: "default".to_owned(),
            profile_json: resolved_p.to_canonical_json().unwrap(),
            spec_path: Some("docs/milestones/AC-092.md".to_owned()),
            spec_id: Some("AC-092".to_owned()),
            spec_digest: "aa".repeat(32),
            is_ephemeral: false,
            isolation: IsolationMode::InPlace,
            target_base: None,
            target_base_commit: None,
            branch: None,
            worktree: None,
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            status: RunStatus::Active,
            cancellation_reason: None,
            root_cycle_count: 0,
            generation: 1,
            created_at: "2026-11-18T00:00:00.000Z".to_owned(),
            completed_at: None,
        },
        nodes: vec![GateNodeRecord {
            run_id: r_done.clone(),
            node_id: "task:t1".to_owned(),
            parent_id: None,
            kind: NodeKind::Task,
            task_ref: Some(TaskRef::new("t1").unwrap()),
            gate_id: "__task__".to_owned(),
            seq_order: 0,
            status: NodeStatus::Completed,
            node_version: 1,
            isolation: IsolationMode::InPlace,
            attempt: 0,
            cycle_count: 0,
            max_attempts: 1,
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
            invariants: Default::default(),
            scope: vec!["src/".to_owned()],
            extended_scope: Vec::new(),
            active_span_id: None,
        }],
        dependencies: Vec::new(),
        subtasks: Vec::new(),
        claims: Vec::new(),
        traces: Vec::new(),
        artifacts: Vec::new(),
    };
    let mut g_clear = g_done.clone();
    g_clear.run.run_id = r_clear.clone();
    g_clear.run.run_name = RunName::new("tomb-clear").unwrap();
    g_clear.nodes[0].run_id = r_clear.clone();
    let tomb_done = RunTombstoneRecord {
        run_id: r_done,
        run_name: RunName::new("tomb-done").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "default".to_owned(),
        spec_id: Some("AC-092".to_owned()),
        title: "Done".to_owned(),
        profile_id: "default".to_owned(),
        is_ephemeral: false,
        generation: 1,
        terminal_status: TerminalStatus::Completed,
        target_base: None,
        target_base_commit: None,
        archive_path: Some("docs/milestones/archive/AC-090-legacy.md".to_owned()),
        closed_commit: None,
        cleanup_pending: false,
        cleanup_manifest: Vec::new(),
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: "2026-11-18T01:00:00.000Z".to_owned(),
    };
    let tomb_clear = RunTombstoneRecord {
        run_id: r_clear,
        run_name: RunName::new("tomb-clear").unwrap(),
        terminal_status: TerminalStatus::Cleared,
        archive_path: None,
        closed_at: "2026-11-18T02:00:00.000Z".to_owned(),
        ..tomb_done.clone()
    };
    let precond = StoragePreconditions {
        expected_run_generation: 1,
        expected_node_versions: Vec::new(),
        expected_claim_revision: None,
        expected_contract_revision: None,
        expected_worker_id: None,
        required_predecessor_trees: Vec::new(),
    };
    exec_tx_with(&harness, |tx| {
        tx.insert_run_graph(&g_done)?;
        tx.archive_and_prune_run(&precond, &tomb_done)?;
        tx.insert_run_graph(&g_clear)?;
        tx.archive_and_prune_run(&precond, &tomb_clear)
    })
    .unwrap();
    let dep_cleared = parse_spec(
        "---\nid: AC-093\ntitle: Superseded by cleared\nstatus: planned\ndepends_on: [\"AC-092:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
    )
    .unwrap();
    let err_cleared =
        validate_spec_for_run(&cfg, &harness, &providers, &dep_cleared, &resolved_p).unwrap_err();
    assert_eq!(err_cleared.code, ErrorCode::GateVerificationFailed);

    // Git HEAD verification for uncommitted 0.5 archive inside a Git repo.
    let git_dir = tempfile::tempdir().unwrap();
    let groot = git_dir.path();
    git_command(groot, &["init", "-b", "main"]).unwrap();
    git_command(groot, &["config", "user.email", "t@t.local"]).unwrap();
    git_command(groot, &["config", "user.name", "T"]).unwrap();
    fs::write(groot.join("README.md"), "# R\n").unwrap();
    git_command(groot, &["add", "README.md"]).unwrap();
    git_command(groot, &["commit", "-m", "init"]).unwrap();
    fs::create_dir_all(groot.join("docs/milestones/archive")).unwrap();
    fs::write(
        groot.join("docs/milestones/archive/AC-090-legacy.md"),
        "---\nid: AC-090\ntitle: Uncommitted\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: completed\n```\n",
    )
    .unwrap();
    let mut g_cfg = cfg.clone();
    g_cfg.workspace_root = groot.to_path_buf();
    g_cfg.worktree = groot.to_path_buf();
    let uncommitted_err =
        validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap_err();
    assert_eq!(uncommitted_err.code, ErrorCode::SpecSchemaInvalid);
    let direct_dep_err = verify_run_level_depends_on(&g_cfg, &harness, &dep_05_doc).unwrap_err();
    assert_eq!(direct_dep_err.code, ErrorCode::GateVerificationFailed);
    git_command(groot, &["add", "docs/milestones/archive/AC-090-legacy.md"]).unwrap();
    git_command(groot, &["commit", "-m", "archive"]).unwrap();
    validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();

    // Pinned HEAD blob check: unstaged working-tree edits on committed archive must NOT alter resolution.
    fs::write(
        groot.join("docs/milestones/archive/AC-090-legacy.md"),
        "---\nid: AC-090\ntitle: Unstaged Corrupt\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: planned\n```\n",
    )
    .unwrap();
    validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();
    verify_run_level_depends_on(&g_cfg, &harness, &dep_05_doc).unwrap();
    // And an archive committed as `planned` with unstaged working-tree edit to `completed` must fail closed.
    fs::write(
        groot.join("docs/milestones/archive/AC-091-planned.md"),
        "---\nid: AC-091\ntitle: Planned In Head\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: p\nscope: [src/]\nstatus: planned\n```\n",
    )
    .unwrap();
    git_command(groot, &["add", "docs/milestones/archive/AC-091-planned.md"]).unwrap();
    git_command(groot, &["commit", "-m", "planned archive"]).unwrap();
    fs::write(
        groot.join("docs/milestones/archive/AC-091-planned.md"),
        "---\nid: AC-091\ntitle: Spoofed On Disk\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: p\nscope: [src/]\nstatus: completed\n```\n",
    )
    .unwrap();
    let dep_spoofed_doc = parse_spec(
        "---\nid: AC-094\ntitle: Spoofed Dep\nstatus: planned\ndepends_on: [\"AC-091:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
    )
    .unwrap();
    let spoof_err = verify_run_level_depends_on(&g_cfg, &harness, &dep_spoofed_doc).unwrap_err();
    assert_eq!(spoof_err.code, ErrorCode::GateVerificationFailed);

    // Reconciled planned spec with completed task in disk state.sqlite passes validate_workspace.
    let disk_harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
    let disk_root = disk_harness.workspace_root().unwrap();
    write_bound_profile(disk_root, "");
    fs::write(
        disk_root.join("acdd.yaml"),
        "modules:\n  code: true\n  docs: true\n  state: true\n",
    )
    .unwrap();
    fs::create_dir_all(disk_root.join("docs/milestones")).unwrap();
    fs::write(
        disk_root.join("docs/milestones/AC-020-reconciled.md"),
        "---\nid: AC-020-reconciled\ntitle: Reconciled\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: completed\n```\n",
    )
    .unwrap();
    let mut g_rec = g_done.clone();
    g_rec.run.run_id = RunId::new("run-reconcile").unwrap();
    g_rec.run.run_name = RunName::new("ac-020").unwrap();
    g_rec.run.spec_path = Some("docs/milestones/AC-020-reconciled.md".to_owned());
    g_rec.run.spec_id = Some("AC-020-reconciled".to_owned());
    g_rec.nodes[0].run_id = g_rec.run.run_id.clone();
    exec_tx_with(&disk_harness, |tx| tx.insert_run_graph(&g_rec)).unwrap();
    let rec_res = validate_workspace(disk_root, None, None, &providers).unwrap();
    assert!(
        rec_res.valid,
        "reconciled planned spec should pass: {rec_res:?}"
    );

    // Linked workspace custom specs + custom state_db resolution in validate_spec_for_run.
    let lw_dir = tempfile::tempdir().unwrap();
    let lw_root = lw_dir.path();
    fs::write(
        lw_root.join("acdd.yaml"),
        "state_db: .acdd/custom-lw.sqlite\nspecs:\n  rfcs:\n    path: docs/rfcs\n    prefixes: [RFC]\n    digits: 3\n",
    )
    .unwrap();
    fs::create_dir_all(lw_root.join("docs/rfcs/archive")).unwrap();
    fs::write(
        lw_root.join("docs/rfcs/archive/RFC-001.md"),
        "---\nid: RFC-001\ntitle: Custom collection\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: completed\n```\n",
    )
    .unwrap();
    let mut lw_cfg = cfg.clone();
    lw_cfg
        .linked_workspaces
        .insert("lib".to_owned(), lw_root.to_path_buf());
    let lw_consumer = parse_spec(
        "---\nid: AC-099\ntitle: LW Consumer\nstatus: planned\ndepends_on: [\"lib/RFC-001:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
    )
    .unwrap();
    validate_spec_for_run(&lw_cfg, &harness, &providers, &lw_consumer, &resolved_p).unwrap();

    // 8. Audit remediation regressions:
    // (a) validate_spec_for_run rejects non-normalized globs identically to validate_workspace.
    for bad_scope in ["../escape/**", "/abs/path", "C:/win/path", "src/{a"] {
        let bad_doc = parse_spec(&format!(
            "---\nid: AC-095\ntitle: Bad scope\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"{bad_scope}\"]\n```\n"
        ))
        .unwrap();
        let err =
            validate_spec_for_run(&cfg, &harness, &providers, &bad_doc, &resolved_p).unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "{bad_scope}");
    }

    // (b) plan_extend_scope rejects malformed brace globs.
    let mut g_ext = g_done.clone();
    g_ext.nodes[0].status = NodeStatus::Ready;
    let machine_prof = to_machine_profile(&resolved_p).unwrap();
    let ext_err = evaluate_transition(
        &g_ext,
        &machine_prof,
        &acdd_state::types::ValidatedObservations::default(),
        &TransitionInput::ExtendScope(ExtendScopeInput {
            node_cursor: NodeCursor::task(TaskRef::new("t1").unwrap(), Vec::new()),
            paths: vec!["src/{a".to_owned()],
            reason: "need malformed".to_owned(),
            worker_id: None,
        }),
    )
    .unwrap_err();
    assert_eq!(ext_err.code, ErrorCode::OutOfScopeChanges);

    // (c) validate_document surfaces broken .acdd/workflows/default.yaml rather than swallowing with .ok().
    let broken_ws = tempfile::tempdir().unwrap();
    fs::create_dir_all(broken_ws.path().join(".acdd/workflows")).unwrap();
    fs::create_dir_all(broken_ws.path().join("docs/milestones")).unwrap();
    fs::write(
        broken_ws.path().join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: missing_parent\n",
    )
    .unwrap();
    fs::write(
        broken_ws.path().join("docs/milestones/AC-001.md"),
        "---\nid: AC-001\ntitle: S\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
    )
    .unwrap();
    let doc_broken_prof = validate_document(
        broken_ws.path(),
        Path::new("docs/milestones/AC-001.md"),
        None,
        None,
        &providers,
    )
    .unwrap();
    assert!(!doc_broken_prof.valid);
    assert!(
        doc_broken_prof
            .diagnostics
            .iter()
            .any(|d| d.status == "fail" && d.path == ".acdd/workflows/default.yaml"),
        "expected .acdd/workflows/default.yaml diagnostic: {doc_broken_prof:?}"
    );

    // (d) Cross-spec task-level cycle detection & non-cyclic cross-spec task refs.
    let spec_a = parse_spec(
        "---\nid: AC-101\ntitle: A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"AC-102:t2\"]\n```\n",
    )
    .unwrap();
    let spec_b = parse_spec(
        "---\nid: AC-102\ntitle: B\nstatus: planned\n---\n### task: t2\n```yaml\ntarget: b\nscope: [src/b.rs]\ndepends_on: [\"AC-101:t1\"]\n```\n",
    )
    .unwrap();
    let cyc_issues = validate_spec_dependencies(
        root,
        &Adapter::default(),
        Some(&harness),
        None,
        &spec_a,
        &[spec_a.clone(), spec_b],
    );
    assert!(
        cyc_issues
            .iter()
            .any(|i| i.code == ErrorCode::SpecSchemaInvalid && i.message.contains("cross-spec")),
        "expected cross-spec task cycle diagnostic: {cyc_issues:?}"
    );

    let noncyc_a = parse_spec(
        "---\nid: AC-103\ntitle: A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"AC-104:t1\"]\n```\n### task: t2\n```yaml\ntarget: a2\nscope: [src/a2.rs]\n```\n",
    )
    .unwrap();
    let noncyc_b = parse_spec(
        "---\nid: AC-104\ntitle: B\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: b\nscope: [src/b.rs]\n```\n### task: t2\n```yaml\ntarget: b2\nscope: [src/b2.rs]\ndepends_on: [\"AC-103:t2\"]\n```\n",
    )
    .unwrap();
    let noncyc_issues = validate_spec_dependencies(
        root,
        &Adapter::default(),
        Some(&harness),
        None,
        &noncyc_a,
        &[noncyc_a.clone(), noncyc_b],
    );
    assert!(
        noncyc_issues.is_empty(),
        "non-cyclic cross-spec task edges should pass: {noncyc_issues:?}"
    );

    // Self-alias pointing back to primary root MUST participate in cycle & self-dep detection.
    let mut self_alias_adapter = Adapter::default();
    self_alias_adapter.linked_workspaces.push(LinkedWorkspace {
        name: "self_ws".to_owned(),
        path: root.canonicalize().unwrap(),
        roots: vec![root.canonicalize().unwrap()],
        ignored_names: Default::default(),
    });
    let alias_cyc_a = parse_spec(
        "---\nid: AC-105\ntitle: Alias A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"self_ws/AC-106:t2\"]\n```\n",
    )
    .unwrap();
    let alias_cyc_b = parse_spec(
        "---\nid: AC-106\ntitle: Alias B\nstatus: planned\n---\n### task: t2\n```yaml\ntarget: b\nscope: [src/b.rs]\ndepends_on: [\"AC-105:t1\"]\n```\n",
    )
    .unwrap();
    let alias_cyc_issues = validate_spec_dependencies(
        root,
        &self_alias_adapter,
        Some(&harness),
        None,
        &alias_cyc_a,
        &[alias_cyc_a.clone(), alias_cyc_b],
    );
    assert!(
        alias_cyc_issues
            .iter()
            .any(|i| i.code == ErrorCode::SpecSchemaInvalid && i.message.contains("cross-spec")),
        "self-alias cross-spec cycle must fail closed: {alias_cyc_issues:?}"
    );

    let self_dep_alias = parse_spec(
        "---\nid: AC-107\ntitle: Self Alias\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"self_ws/AC-107:t1\"]\n```\n",
    )
    .unwrap();
    let self_dep_issues = validate_spec_dependencies(
        root,
        &self_alias_adapter,
        Some(&harness),
        None,
        &self_dep_alias,
        std::slice::from_ref(&self_dep_alias),
    );
    assert!(
        self_dep_issues
            .iter()
            .any(|i| i.code == ErrorCode::SpecSchemaInvalid
                && i.message.contains("cannot depend on itself")),
        "self-alias self-dependency must fail closed: {self_dep_issues:?}"
    );

    // (e) Corrupt state DB in linked workspace propagates reader error instead of swallowing.
    fs::create_dir_all(lw_root.join(".acdd")).unwrap();
    fs::write(lw_root.join(".acdd/custom-lw.sqlite"), b"not a sqlite db").unwrap();
    let corrupt_db_err =
        validate_spec_for_run(&lw_cfg, &harness, &providers, &lw_consumer, &resolved_p)
            .unwrap_err();
    assert_ne!(corrupt_db_err.code, ErrorCode::GateVerificationFailed);
}
