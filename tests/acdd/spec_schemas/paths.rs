//! AC-06 contract tests for workflow `paths:`, `specs.*`/`paths.*` scope & context expansion,
//! legacy rejection hints, empty-glob warnings, and 4-tier relevance ranking.

use acdd_core::config::{load_adapter, SpecConfig};
use acdd_core::errors::ErrorCode;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::{CommitOid, GateId, RunId, RunName, TaskRef, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::DomainContextReq;
use acdd_spec::spec::TaskSpec;
use acdd_spec::{effective_spec_collections, InvariantsState, SpecDocument, SpecStatus};
use acdd_state::context::{
    render_claim_prompt_markdown, ClaimContextRequest, GateContextAssembler,
};
use acdd_state::machine::{
    build_initial_run_graph, container_scope, evaluate_transition, BuildGraphInput,
    MachineGraphItem, MachineSpecInput, MachineTaskSpec, SyncInput, TransitionInput,
    TransitionOutcome,
};
use acdd_state::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, RunRecord, RunStatus,
    ValidatedObservations,
};
use acdd_state::usecases::lifecycle::{collect_integration_inputs, to_machine_profile};
use acdd_state::validate::{validate_document, validate_workspace};
use acdd_state::workflow::{
    collect_workflow_glob_warnings, resolve_workflow_from_str_with_dirs,
    resolve_workflow_ref_with_dirs, validate_workflow, WorkflowValidationOptions,
};
use contextunity_acdd::state_bridge::build_domain_providers;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(abs, content).unwrap();
    }
}

#[test]
fn workflow_paths_defaults_inherit_merge_validation_and_glob_warnings() {
    let dir = ScopedTempDir::create("acdd_paths_wf").unwrap();
    let root = dir.path();
    write_files(
        root,
        &[
            ("acdd.yaml", "roots: [src/]\ndocs: [docs/]\n"),
            ("AGENTS.md", "# Root\n"),
            ("docs/adr/0001-base.md", "# Base ADR\n"),
            ("docs/design/arch.md", "# Design\n"),
            ("src/lib.rs", "pub fn ok() {}\n"),
        ],
    );

    let adapter = load_adapter(root, None).unwrap();
    let effective_cols = effective_spec_collections(&adapter.specs);
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();

    // Embedded `base` & `code` define default `paths: {docs, adrs, instructions}`.
    let code_wf = resolve_workflow_ref_with_dirs("code", None, None, None).unwrap();
    assert!(code_wf.paths.contains_key("docs"));
    assert!(code_wf.paths.contains_key("adrs"));
    assert!(code_wf.paths.contains_key("instructions"));

    // Child workflow inherits `paths:` key-wise: overrides `docs`, adds `design`, deletes `adrs`.
    let child_yaml = r#"version: 2
inherit: base
paths:
  docs: ["docs/design/**"]
  design: ["docs/design/**"]
  adrs: null
  empty_notes: ["docs/missing_dir/**"]
roles:
  worker: { mode: subagent }
gates:
  - id: review
    role: worker
    proof: none
    scope: [paths.design]
    context: [paths.instructions, paths.design]
"#;
    let child = resolve_workflow_from_str_with_dirs(
        child_yaml,
        "child-paths",
        None,
        Some(root),
        None,
        None,
    )
    .unwrap();
    assert_eq!(child.paths.get("docs"), Some(&vec!["docs/design/**".to_owned()]));
    assert_eq!(
        child.paths.get("design"),
        Some(&vec!["docs/design/**".to_owned()])
    );
    assert!(!child.paths.contains_key("adrs"));
    assert!(child.paths.contains_key("instructions"));

    let val_opts = WorkflowValidationOptions {
        domain_providers: Some(providers.as_ref()),
        acdd_modules: None,
        require_bound_commands: false,
        spec_collections: Some(&effective_cols),
    };
    validate_workflow(&child, &val_opts).expect("valid paths inheritance must pass");

    // Empty glob matches emit structured `EMPTY_GLOB_MATCH` warning without blocking validation.
    let mut warnings: Vec<Value> = Vec::new();
    collect_workflow_glob_warnings(
        root,
        ".acdd/workflows/default.yaml",
        &child,
        &effective_cols,
        &mut warnings,
    );
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "EMPTY_GLOB_MATCH" && w["lens"] == "paths.empty_notes"),
        "expected EMPTY_GLOB_MATCH for paths.empty_notes, got {warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| w["lens"] == "paths.design" || w["lens"] == "paths.instructions"),
        "matching globs must not warn: {warnings:?}"
    );

    // Table-driven invalid workflow lens / scope / legacy redirect failures.
    let bad_cases: &[(&str, &str, &str)] = &[
        (
            "legacy_docs_instructions",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    context: [docs.instructions]\n",
            "paths.instructions",
        ),
        (
            "legacy_docs_adrs",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    context: [docs.adrs]\n",
            "paths.adrs",
        ),
        (
            "unknown_paths_context",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    context: [paths.nope]\n",
            "paths.nope",
        ),
        (
            "unknown_specs_context",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    context: [specs.nope]\n",
            "specs.nope",
        ),
        (
            "unknown_paths_scope_root",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    scope: [paths.nope]\n",
            "paths.nope",
        ),
        (
            "unknown_specs_scope_task",
            "version: 2\ninherit: base\nroles:\n  w: { mode: subagent }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: w\n          proof: none\n          scope: [specs.nope]\n",
            "specs.nope",
        ),
        (
            "empty_paths_scope_root",
            "version: 2\ninherit: base\npaths:\n  empty_docs: []\nroles:\n  w: { mode: subagent }\ngates:\n  - id: g\n    role: w\n    proof: none\n    scope: [paths.empty_docs]\n",
            "paths.empty_docs",
        ),
    ];

    for &(label, yaml, expected_substr) in bad_cases {
        let resolved =
            resolve_workflow_from_str_with_dirs(yaml, label, None, Some(root), None, None).unwrap();
        let err = validate_workflow(&resolved, &val_opts)
            .expect_err(&format!("{label} must fail workflow validation"));
        assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid, "{label}: {err}");
        let full_diag = format!("{err}");
        assert!(
            full_diag.contains(expected_substr),
            "{label}: expected '{expected_substr}' in '{full_diag}'"
        );
        if label == "empty_paths_scope_root" {
            let expand_err = to_machine_profile(&resolved, Some(&effective_cols))
                .expect_err("expand_gate_scope must also fail closed on empty paths.<name> in scope:");
            assert_eq!(expand_err.code, ErrorCode::WorkflowSchemaInvalid, "{expand_err}");
        }
    }

    // `validate_workspace` & `validate_document` surface `EMPTY_GLOB_MATCH` from `.acdd/workflows/default.yaml`.
    write_files(
        root,
        &[
            (
                ".acdd/workflows/default.yaml",
                "version: 2\ninherit: documentation\npaths:\n  ghost: [\"docs/ghost/**\"]\n",
            ),
            (
                "docs/milestones/AC-001.md",
                "---\nid: AC-001\ntitle: M1\nstatus: active\n---\n### task: t1\n```yaml\ntarget: docs\nscope: [docs/design/**]\n```\n",
            ),
        ],
    );
    let doc_report = validate_document(
        root,
        Path::new("docs/milestones/AC-001.md"),
        None,
        None,
        providers.as_ref(),
    )
    .unwrap();
    assert_eq!(doc_report.summary.failed, 0, "{doc_report:?}");
    let ws_report = validate_workspace(root, None, None, providers.as_ref()).unwrap();
    assert!(
        ws_report
            .warnings
            .iter()
            .any(|w| w["code"] == "EMPTY_GLOB_MATCH" && w["lens"] == "paths.ghost"),
        "validate_workspace must emit EMPTY_GLOB_MATCH: {ws_report:?}"
    );

    // Migrated sibling workflow examples under `skills/acdd/references/acdd-workflow-examples/` validate cleanly.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let example_dirs = vec!["skills/acdd/references/acdd-workflow-examples".to_owned()];
    for (name, raw) in [
        (
            "architecture-hygiene",
            include_str!(
                "../../../skills/acdd/references/acdd-workflow-examples/architecture-hygiene.workflow.yaml"
            ),
        ),
        (
            "pr-code-review",
            include_str!(
                "../../../skills/acdd/references/acdd-workflow-examples/pr-code-review.workflow.yaml"
            ),
        ),
    ] {
        let resolved = resolve_workflow_from_str_with_dirs(
            raw,
            name,
            None,
            Some(repo_root),
            None,
            Some(&example_dirs),
        )
        .unwrap();
        validate_workflow(&resolved, &val_opts)
            .unwrap_or_else(|e| panic!("migrated workflow example '{name}' must validate: {e}"));
    }
}

#[test]
fn specs_and_paths_scope_expansion_and_docs_invariants_over_custom_paths_docs() {
    let dir = ScopedTempDir::create("acdd_paths_scope").unwrap();
    let root = dir.path();
    write_files(
        root,
        &[
            (
                "acdd.yaml",
                "roots: [src/]\ndocs: [docs/]\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n  notes:\n    path: .acdd/notes\n    prefixes: [NOTE]\n  contracts:\n    path: docs/contracts\n    prefixes: [CTR]\n",
            ),
            ("AGENTS.md", "# Root\n"),
            ("docs/arch/rules.md", "# Arch\n- INV-ARCH-1: Keep modules layered.\n"),
            ("docs/other/ignored.md", "# Other\n- INV-OTHER-9: Ignored by custom paths.docs.\n"),
            (
                "docs/contracts/CTR-001.md",
                "---\nid: CTR-001\ntitle: Contract Spec\nstatus: active\n---\n- INV-CTR-SPEC-ONLY: Registered spec must stay out of docs.invariants.\n",
            ),
            ("docs/notes/n1.md", "# Note 1\n"),
            (".acdd/notes/NOTE-001.md", "---\nid: NOTE-001\ntitle: N1\nstatus: planned\n---\n"),
            (
                ".acdd/workflows/default.yaml",
                "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\npaths:\n  docs: [\"docs/arch/**\"]\n  notes: [\"docs/notes/**\"]\nroles:\n  worker: { mode: subagent }\ngates:\n  - id: review_notes\n    role: worker\n    proof: none\n    scope: [paths.notes]\n    context: [paths.notes, specs.notes]\n  - tasks:\n      gates:\n        - id: build\n          role: worker\n          proof: none\n          scope: [specs.notes]\n          context: [docs.invariants, paths.instructions]\n",
            ),
        ],
    );

    let adapter = load_adapter(root, None).unwrap();
    let cols: BTreeMap<String, SpecConfig> = effective_spec_collections(&adapter.specs);
    let wf = resolve_workflow_ref_with_dirs(".acdd/workflows/default.yaml", Some(root), None, None)
        .unwrap();
    let mprof = to_machine_profile(&wf, Some(&cols)).unwrap();
    let root_gate = mprof
        .gates
        .iter()
        .find_map(|item| match item {
            MachineGraphItem::Gate(g) if g.id.as_str() == "review_notes" => Some(g),
            _ => None,
        })
        .unwrap();
    assert_eq!(root_gate.scope, vec!["docs/notes/**".to_owned()]);
    assert!(root_gate.scope.iter().all(|s| !s.starts_with("paths.")));

    let task_gate = mprof
        .gates
        .iter()
        .find_map(|item| match item {
            MachineGraphItem::TasksBlock(tb) => {
                tb.gates.iter().find(|g| g.id.as_str() == "build")
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(task_gate.scope, vec![".acdd/notes".to_owned()]);
    assert!(WorkspacePathPolicy::default()
        .is_scope_admitted(".acdd/notes/NOTE-001.md", &task_gate.scope));
    assert!(!wf.paths.contains_key("contracts"));

    // Runtime container_scope unions Task.scope with expanded TaskGate.scope, and sync preserves it.
    let spec_input = MachineSpecInput {
        tasks: vec![MachineTaskSpec {
            task_ref: TaskRef::new("t1").unwrap(),
            tags: vec!["default".to_owned()],
            scope: vec!["src/lib.rs".to_owned()],
            extended_scope: Vec::new(),
            invariants: Vec::new(),
            depends_on: Vec::new(),
            subtasks: Vec::new(),
        }],
        spec_digest: "a".repeat(64),
    };
    let snapshot = build_initial_run_graph(
        &mprof,
        &BuildGraphInput {
            run_id: RunId::new("run-gate-scope").unwrap(),
            run_name: RunName::new("gate-scope").unwrap(),
            collection: Some("milestones".to_owned()),
            workspace: root.to_string_lossy().into_owned(),
            spec_path: Some("docs/milestones/AC-001.md".to_owned()),
            is_ephemeral: false,
            target_base: Some("main".to_owned()),
            target_base_commit: Some(CommitOid::new("1".repeat(40)).unwrap()),
            branch: Some("main".to_owned()),
            worktree: Some(root.to_string_lossy().into_owned()),
            trace_id: "2".repeat(32),
            created_at: "2026-11-27T00:00:00Z".to_owned(),
            spec: spec_input.clone(),
        },
    )
    .unwrap();
    let build_node = snapshot
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::TaskGate && n.gate_id == "build")
        .unwrap();
    let effective_gate_scope = container_scope(&snapshot, build_node);
    assert_eq!(
        effective_gate_scope,
        vec!["src/lib.rs".to_owned(), ".acdd/notes".to_owned()]
    );
    assert!(WorkspacePathPolicy::default()
        .is_scope_admitted(".acdd/notes/NOTE-001.md", &effective_gate_scope));

    let sync_outcome = evaluate_transition(
        &snapshot,
        &mprof,
        &ValidatedObservations::default(),
        &TransitionInput::Sync(SyncInput {
            spec: spec_input,
            new_profile: None,
            rebind_workflow: false,
        }),
    )
    .unwrap();
    let TransitionOutcome::Sync(sync_plan) = sync_outcome else {
        panic!("expected Sync outcome");
    };
    let build_patch = sync_plan
        .transition
        .node_patches
        .iter()
        .find(|p| p.node_id == build_node.node_id)
        .unwrap();
    assert_eq!(
        build_patch.scope.as_deref(),
        Some(&["src/lib.rs".to_owned(), ".acdd/notes".to_owned()][..])
    );

    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let invs = providers
        .provide_context(
            "docs.invariants",
            &DomainContextReq {
                run_id: RunId::new("r-inv").unwrap(),
                node_cursor: "t1.build".parse().unwrap(),
                gate_id: "build".parse().unwrap(),
                workspace_root: root.to_path_buf(),
                worktree: root.to_path_buf(),
                admitted_scope: vec!["src/**".to_owned()],
                target: None,
                max_bytes: 16_384,
                path_policy: WorkspacePathPolicy::default(),
                baseline_ref: None,
                lens_globs: wf.paths.get("docs").cloned().unwrap_or_default(),
                citation_text: None,
                params: serde_json::Value::Null,
            },
        )
        .unwrap();
    let inv_json = serde_json::to_string(&invs).unwrap();
    assert!(inv_json.contains("INV-ARCH-1"), "{inv_json}");
    assert!(!inv_json.contains("INV-OTHER-9"), "{inv_json}");
    assert!(!inv_json.contains("INV-CTR-SPEC-ONLY"), "{inv_json}");

    // Terminal `collect_integration_inputs` includes child `TaskGate` expanded scopes (`.acdd/notes`).
    let mut completed_snapshot = snapshot.clone();
    let tree_a = TreeOid::new("1".repeat(40)).unwrap();
    let tree_b = TreeOid::new("2".repeat(40)).unwrap();
    for node in &mut completed_snapshot.nodes {
        if node.kind == NodeKind::Task && node.task_ref.as_ref().is_some_and(|t| t.as_str() == "t1")
        {
            node.status = NodeStatus::Completed;
            node.claim_tree = Some(tree_a.clone());
            node.accepted_tree = Some(tree_b.clone());
        }
    }
    let integration_inputs = collect_integration_inputs(&completed_snapshot);
    assert_eq!(integration_inputs.len(), 1);
    assert_eq!(
        integration_inputs[0].admitted_scope,
        vec!["src/lib.rs".to_owned(), ".acdd/notes".to_owned()]
    );
}

#[test]
fn four_tier_relevance_ranking_budget_eviction_and_markdown_rendering() {
    let dir = ScopedTempDir::create("acdd_paths_rank").unwrap();
    let root = dir.path();
    write_files(
        root,
        &[
            (
                "acdd.yaml",
                "roots: [src/]\ndocs: [docs/]\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n  memories:\n    path: .acdd/memories\n    base: memory\n    prefixes: [MEM]\n",
            ),
            (".gitignore", ".acdd/\n"),
            ("AGENTS.md", "# Global Fallback Rules\n"),
            ("src/owned/AGENTS.md", "# Owned Src Rules\n"),
            ("docs/unrelated/AGENTS.md", "# Unrelated Rules\n"),
            ("src/owned/mod.rs", "pub fn target_worker() -> i32 { 1 }\n"),
            (
                "docs/notes/tier1_cited.md",
                "---\nid: NOTE-CITED\ntitle: Cited Note Title\nscope: [frontend/**]\n---\n# Cited Note\n",
            ),
            (
                "docs/notes/tier2_scoped.md",
                "---\nid: NOTE-SCOPED\ntitle: Scoped Note\nscope: [src/owned/**]\n---\n# Scoped Note\n",
            ),
            (
                "docs/notes/tier3_linked.md",
                "---\nid: NOTE-LINKED\ntitle: Linked Note\nscope: [never_match/**]\n---\n# Linked Note\nDocuments `target_worker`.\n",
            ),
            (
                "docs/notes/tier4_global.md",
                "---\nid: NOTE-GLOBAL\ntitle: Global Fallback Note\nscope: [\"**\"]\n---\n# Global Note\n",
            ),
            (
                "docs/notes/global_1.md",
                "---\ntitle: G1\nscope: [\"**\"]\n---\n# G1\n",
            ),
            (
                "docs/notes/global_2.md",
                "---\ntitle: G2\nscope: [\"**\"]\n---\n# G2\n",
            ),
            (
                "docs/notes/global_3.md",
                "---\ntitle: G3\nscope: [\"**\"]\n---\n# G3\n",
            ),
            (
                "docs/notes/global_4.md",
                "---\ntitle: G4\nscope: [\"**\"]\n---\n# G4\n",
            ),
            (
                "docs/notes/global_5.md",
                "---\ntitle: G5\nscope: [\"**\"]\n---\n# G5\n",
            ),
            (
                "docs/notes/disjoint_excluded.md",
                "---\nid: NOTE-DISJOINT\ntitle: Disjoint Excluded Note\nscope: [crates/other/**]\n---\n# Disjoint Note\n",
            ),
            (
                "docs/utf8_notes/u1.md",
                "---\ntitle: Éclair Guide\nscope: [never_match/**]\n---\n# Éclair Guide\n",
            ),
            (
                ".acdd/memories/MEM-001.md",
                "---\nid: MEM-001\ntitle: Scoped Memory\nstatus: active\nowners: [src/owned/**]\n---\n# Scoped Memory\n",
            ),
            (
                ".acdd/memories/MEM-002.md",
                "---\nid: MEM-002\ntitle: Cited Memory\nstatus: active\nowners: [frontend/**]\n---\n# Cited Memory\n",
            ),
            (
                ".acdd/memories/MEM-003.md",
                "---\nid: MEM-003\ntitle: Global Memory\nstatus: active\n---\n# Global Memory\n",
            ),
            (
                ".acdd/memories/MEM-004.md",
                "---\nid: MEM-004\ntitle: Disjoint Memory\nstatus: active\nowners: [crates/other/**]\n---\n# Disjoint Memory\n",
            ),
            (
                ".acdd/memories/MEM-RULE.md",
                "---\nid: MEM-RULE\ntitle: Itemized Memory Rules\nspec: memory\nstatus: active\n---\n# Rules\n\n### rule: rule-cited\n```yaml\nscope: [frontend/**]\nstatement: Cited memory rule must win\nevidence: [\"bb#1\"]\n```\n\n### rule: rule-scoped\n```yaml\nscope: [src/owned/**]\nstatement: Scoped memory rule applies\nevidence: [\"bb#2\"]\n```\n\n### rule: rule-global\n```yaml\nscope: [\"**\"]\nstatement: Global fallback rule applies\nevidence: [\"bb#3\"]\n```\n\n### rule: rule-disjoint\n```yaml\nscope: [crates/other/**]\nstatement: Disjoint rule must stay excluded\nevidence: [\"bb#4\"]\n```\n",
            ),
            (
                ".acdd/workflows/default.yaml",
                "version: 2\ngit:\n  isolation: in_place\n  worktrees: manual\npaths:\n  notes: [\"docs/notes/**\"]\n  instructions: [\"**/AGENTS.md\"]\nroles:\n  worker: { mode: subagent }\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: worker\n          proof: none\n          context: [paths.notes, specs.memories, paths.instructions]\n",
            ),
        ],
    );

    contextunity_acdd::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None)
        .unwrap();
    let adapter = load_adapter(root, None).unwrap();
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let wf = resolve_workflow_ref_with_dirs(".acdd/workflows/default.yaml", Some(root), None, None)
        .unwrap();

    let spec = SpecDocument {
        id: "AC-999".to_owned(),
        title: "Context Ranking Spec".to_owned(),
        doc_type: "contract".to_owned(),
        workflow: None,
        status: SpecStatus::Active,
        cancellation_reason: None,
        spec: Some("Root".to_owned()),
        goal: None,
        depends_on: Vec::new(),
        owners: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        guidance: None,
        required_skills: Vec::new(),
        repositories: Vec::new(),
        linked_to: Vec::new(),
        closure: None,
        spec_deltas: Vec::new(),
        fields: BTreeMap::new(),
        tasks: vec![TaskSpec {
            task_ref: TaskRef::new("t1").unwrap(),
            target: Some("Implement MEM-002 integration".to_owned()),
            spec: None,
            scope: vec!["src/owned/**".to_owned()],
            extended_scope: Vec::new(),
            depends_on: Vec::new(),
            tags: vec!["default".to_owned()],
            invariants: Vec::new(),
            acceptance_criteria: vec!["Ranked correctly".to_owned()],
            contract_revision: 1,
            status: SpecStatus::Active,
            subtasks: Vec::new(),
            receipt: None,
        }],
        items: BTreeMap::new(),
        body: "## Architecture\nFollow NOTE-CITED and MEM-RULE#rule-cited for state wiring; see AGENTS.md, agents, and tests guidance.\n".to_owned(),
        digest: "b".repeat(64),
    };

    let ws = root.to_string_lossy().into_owned();
    let tree = TreeOid::new("1".repeat(40)).unwrap();
    let commit = CommitOid::new("2".repeat(40)).unwrap();
    let run_id = RunId::new("run-rank").unwrap();
    let worker_id = WorkerId::new("w1").unwrap();
    let run = RunRecord {
        run_id: run_id.clone(),
        run_name: RunName::new("rank").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: ws.clone(),
        profile_id: wf.workflow_id.clone(),
        profile_json: "{}".to_owned(),
        spec_path: Some("docs/milestones/AC-999.md".to_owned()),
        spec_id: Some("AC-999".to_owned()),
        spec_digest: "a".repeat(64),
        attached_specs: Vec::new(),
        is_ephemeral: false,
        isolation: IsolationMode::InPlace,
        target_base: Some("main".to_owned()),
        target_base_commit: Some(commit),
        branch: Some("main".to_owned()),
        worktree: Some(ws.clone()),
        trace_id: "3".repeat(32),
        status: RunStatus::Active,
        cancellation_reason: None,
        root_cycle_count: 0,
        generation: 1,
        created_at: "2026-11-27T00:00:00Z".to_owned(),
        completed_at: None,
    };
    let gate_def = wf.gates[0].tasks.as_ref().unwrap().gates[0].clone();
    let node = GateNodeRecord {
        run_id: run_id.clone(),
        node_id: "task:t1:build".to_owned(),
        parent_id: Some("task:t1".to_owned()),
        kind: NodeKind::TaskGate,
        task_ref: Some(TaskRef::new("t1").unwrap()),
        gate_id: "build".to_owned(),
        seq_order: 1,
        status: NodeStatus::InProgress,
        node_version: 1,
        isolation: IsolationMode::InPlace,
        attempt: 1,
        cycle_count: 0,
        max_attempts: 3,
        claim_revision: 1,
        contract_revision: 1,
        worker_id: Some(worker_id.clone()),
        session_fingerprint: None,
        branch: None,
        worktree: Some(ws),
        baseline_commit: None,
        claim_tree: Some(tree.clone()),
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["default".to_owned()],
        invariants: InvariantsState::default(),
        scope: vec!["src/owned/**".to_owned()],
        extended_scope: Vec::new(),
        active_span_id: None,
    };

    let req = ClaimContextRequest {
        run: run.clone(),
        node,
        gate: gate_def,
        profile: wf.clone(),
        spec: Some(spec.clone()),
        live_subtasks: Vec::new(),
        claim_revision: 1,
        worker_id,
        worktree: root.to_path_buf(),
        candidate_tree: tree,
        path_policy: WorkspacePathPolicy::default(),
        unresolved_findings: None,
        failed_trace_log_tail: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard_items: Vec::new(),
        context_reset_required: false,
        response_max_bytes: 32_768,
        hard_max_bytes: None,
        full_detail: true,
        proof_pointers: Vec::new(),
    };

    let assembled = GateContextAssembler::assemble(&req, providers.as_ref()).unwrap();
    let note_paths: Vec<&str> = assembled.context_lenses["paths.notes"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(
        note_paths,
        vec![
            "docs/notes/tier1_cited.md",
            "docs/notes/tier2_scoped.md",
            "docs/notes/tier3_linked.md",
            "docs/notes/global_1.md",
            "docs/notes/global_2.md",
            "docs/notes/global_3.md",
            "docs/notes/global_4.md",
            "docs/notes/global_5.md",
            "docs/notes/tier4_global.md",
        ],
        "expected strict Tier 1 > 2 > 3 > 4 and disjoint exclusion"
    );

    let mem_items = assembled.context_lenses["specs.memories"]["items"]
        .as_array()
        .unwrap();
    let mem_labels: Vec<String> = mem_items
        .iter()
        .map(|v| match (v["spec_id"].as_str(), v["ref"].as_str()) {
            (Some(id), Some(r)) => format!("{id}#{r}"),
            (Some(id), None) => id.to_owned(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(
        mem_labels,
        vec![
            "MEM-002",
            "MEM-RULE#rule-cited",
            "MEM-001",
            "MEM-RULE#rule-scoped",
            "MEM-003",
            "MEM-RULE#rule-global",
        ],
        "expected Tier 1 > Tier 2 > Tier 4 and disjoint doc/rule exclusion"
    );

    let inst_paths: Vec<&str> = assembled.context_lenses["paths.instructions"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(inst_paths, vec!["src/owned/AGENTS.md", "AGENTS.md"]);

    let md = render_claim_prompt_markdown(&assembled, None);
    assert!(md.contains("paths.notes"), "{md}");
    assert!(md.contains("specs.memories"), "{md}");
    assert!(md.contains("NOTE-CITED") || md.contains("Cited Note Title"), "{md}");
    assert!(md.contains("- `MEM-002` (active, `.acdd/memories/MEM-002.md`): Cited Memory"), "{md}");
    assert!(
        md.contains("- `MEM-RULE#rule-cited` (planned, `.acdd/memories/MEM-RULE.md`): Cited memory rule must win"),
        "{md}"
    );
    assert!(!md.contains("- ``"), "render_specs_lens must not emit empty labels: {md}");

    // Tight per-lens max_bytes keeps Tier 1 cited item even when 6 smaller Tier 4 items could fit.
    let tight_notes = providers
        .provide_context(
            "paths.notes",
            &DomainContextReq {
                run_id: run_id.clone(),
                node_cursor: "t1.build".parse().unwrap(),
                gate_id: GateId::new("build").unwrap(),
                workspace_root: root.to_path_buf(),
                worktree: root.to_path_buf(),
                admitted_scope: vec!["src/owned/**".to_owned()],
                target: spec.tasks[0].target.clone(),
                max_bytes: 255,
                path_policy: WorkspacePathPolicy::default(),
                baseline_ref: None,
                lens_globs: wf.paths.get("notes").cloned().unwrap_or_default(),
                citation_text: Some(spec.body),
                params: serde_json::Value::Null,
            },
        )
        .unwrap();
    let tight_paths: Vec<&str> = tight_notes["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(tight_notes["truncated"], json!(true), "{tight_notes}");
    assert!(
        tight_paths.contains(&"docs/notes/tier1_cited.md"),
        "Tier 1 cited item must never be evicted by multiple small Tier 4 items: {tight_notes}"
    );
    assert!(
        !tight_paths.contains(&"docs/notes/tier4_global.md"),
        "Tier 4 global item must drop first under tight max_bytes: {tight_notes}"
    );

    // UTF-8 char-boundary advance in `contains_ascii_ci_word`: non-matching prefix + standalone match.
    let utf8_non_match = providers
        .provide_context(
            "paths.utf8",
            &DomainContextReq {
                run_id: run_id.clone(),
                node_cursor: "t1.build".parse().unwrap(),
                gate_id: GateId::new("build").unwrap(),
                workspace_root: root.to_path_buf(),
                worktree: root.to_path_buf(),
                admitted_scope: vec!["src/owned/**".to_owned()],
                target: None,
                max_bytes: 4096,
                path_policy: WorkspacePathPolicy::default(),
                baseline_ref: None,
                lens_globs: vec!["docs/utf8_notes/**".to_owned()],
                citation_text: Some("See xÉclair Guide only".to_owned()),
                params: serde_json::Value::Null,
            },
        )
        .unwrap();
    assert_eq!(
        utf8_non_match["items"].as_array().unwrap().len(),
        0,
        "identifier-adjacent UTF-8 prefix must not panic or match Tier 1: {utf8_non_match}"
    );
    let utf8_match = providers
        .provide_context(
            "paths.utf8",
            &DomainContextReq {
                run_id,
                node_cursor: "t1.build".parse().unwrap(),
                gate_id: GateId::new("build").unwrap(),
                workspace_root: root.to_path_buf(),
                worktree: root.to_path_buf(),
                admitted_scope: vec!["src/owned/**".to_owned()],
                target: None,
                max_bytes: 4096,
                path_policy: WorkspacePathPolicy::default(),
                baseline_ref: None,
                lens_globs: vec!["docs/utf8_notes/**".to_owned()],
                citation_text: Some("See xÉclair Guide and Éclair Guide".to_owned()),
                params: serde_json::Value::Null,
            },
        )
        .unwrap();
    let utf8_paths: Vec<&str> = utf8_match["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(utf8_paths, vec!["docs/utf8_notes/u1.md"]);
}
