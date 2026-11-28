//! Named `items:` collections, 3-step validation, multi-spec `start`, and `docs.specs` seam tests (`AC-04`, `AC-05`).

use crate::acdd::support::init_git_repo_on_branch;
use acdd_core::config::{load_adapter, SpecConfig};
use acdd_core::errors::{
    ErrorCode, DIAG_ITEM_DEPENDENCY_UNKNOWN, DIAG_SPEC_FIELD_UNKNOWN, DIAG_SPEC_ITEM_UNKNOWN,
};
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::{GateId, NodeCursor, RunId, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{
    DomainCheckOutcome, DomainCheckReq, DomainContextReq, FreshnessObservation,
};
use acdd_core::schema::SchemaDef;
use acdd_spec::deps::validate_spec_dependencies;
use acdd_spec::overlay::NoopRuntimeOverlay;
use acdd_spec::discovery::parse_spec_at_relpath;
use acdd_spec::{
    effective_spec_collections, parse_spec_with_collections,
    render_archive_markdown_with_collections, ArchiveRenderInput, ClosureSummary, ReceiptRollup,
    SpecStatus,
};
use acdd_state::context::render_claim_prompt_markdown;
use acdd_state::harness::RunHarness;
use acdd_state::store::sqlite::migrations::{ensure_schema, read_meta};
use acdd_state::store::sqlite::schema::{CURRENT_SCHEMA_VERSION, SCHEMA_V1_SQL};
use acdd_state::types::{AttachedSpecItemSummary, AttachedSpecRecord};
use acdd_state::usecases::{
    ClaimReq, ClearReq, StartReq, StateCommand, StateCommandOutput, StateEngine, StateEngineConfig,
    SyncReq,
};
use acdd_state::validate::validate_workspace;
use contextunity_acdd::mcp::tools::inputs::StateInput;
use contextunity_acdd::state_bridge::build_domain_providers;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn write_research_workspace(root: &Path, include_res_spec: bool) {
    fs::create_dir_all(root.join(".acdd/specs")).unwrap();
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/research")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        "version: 2\nroles:\n  builder:\n    mode: subagent\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n",
    )
    .unwrap();
    fs::write(
        root.join(".acdd/specs/research.yaml"),
        "\
workflow: base
roadmap: false
prefixes: [RES]
fields:
  type: object
  required: [question]
  additionalProperties: false
  properties:
    question: { type: string, minLength: 1 }
items:
  probes:
    item: probe
    fields:
      type: object
      required: [method]
      additionalProperties: false
      properties:
        method: { type: string, minLength: 1 }
        findings: { type: array, items: { type: string } }
",
    )
    .unwrap();
    fs::write(
        root.join("acdd.yaml"),
        "\
modules:
  code: true
  docs: true
  state: true
specs_dirs:
  - .acdd/specs
workflows_dirs:
  - .acdd/workflows
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC, AUD]
    digits: 3
  research:
    path: docs/research
    ref: research.yaml
  memories:
    base: memory
    path: .acdd/memory
    prefixes: [MEM]
",
    )
    .unwrap();

    if include_res_spec {
        fs::write(
            root.join("docs/research/RES-001.md"),
            "\
---
id: RES-001
title: Search Strategy Probe
spec: research
status: active
question: Compare SQLite FTS vs trigram index
---

# Research RES-001

### 1. Background: why we investigate

Prose heading should never be treated as an unknown item type.

### probe: probe-a

```yaml
method: benchmark
findings:
  - fast lookup
```
",
        )
        .unwrap();
    }
}

fn open_engine_for_root(root: &Path) -> StateEngine {
    let adapter = load_adapter(root, None).unwrap();
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let cfg = StateEngineConfig {
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        state_db_override: None,
        code_db_path: None,
        spec_collections: adapter.specs.clone(),
        workflows_dirs: adapter.workflows_dirs.clone(),
        linked_workspaces: BTreeMap::new(),
        modules: adapter.modules.clone(),
        response_policy: adapter.response.clone(),
        retain_traces: adapter.retain_traces,
        max_snapshot_file_bytes: adapter.limits.max_snapshot_file_bytes,
        allowed_binary_extensions: None,
    };
    StateEngine::open(cfg, providers).unwrap()
}

#[test]
fn named_items_effective_base_and_three_step_validation_matrix() {
    // Wave 2 #650 reminder: SpecConfig::effective_builtin_base() preserves ref: without implicit base,
    // defaults collection key "memory" -> "memory", and defaults other collections -> "contract".
    let ref_cfg = SpecConfig {
        spec_ref: Some("research.yaml".to_owned()),
        ..SpecConfig::default()
    };
    assert_eq!(ref_cfg.effective_builtin_base(Some("research")), None);
    let mem_cfg = SpecConfig::default();
    assert_eq!(mem_cfg.effective_builtin_base(Some("memory")), Some("memory"));
    assert_eq!(
        mem_cfg.effective_builtin_base(Some("milestones")),
        Some("contract")
    );

    let dir = ScopedTempDir::create("acdd_items_3step").unwrap();
    let root = dir.path().canonicalize().unwrap();
    write_research_workspace(&root, true);

    let adapter = load_adapter(&root, None).unwrap();
    let cols = effective_spec_collections(&adapter.specs);
    let research_col = cols.get_key_value("research").map(|(k, v)| (k.as_str(), v));
    let res_raw = fs::read_to_string(root.join("docs/research/RES-001.md")).unwrap();
    let res_doc =
        parse_spec_with_collections(&res_raw, research_col, &cols).expect("RES-001 parses");
    assert!(res_doc.tasks.is_empty());
    assert_eq!(res_doc.items["probes"].len(), 1);
    assert_eq!(res_doc.items["probes"][0].item, "probe");
    assert_eq!(res_doc.items["probes"][0].item_ref.as_str(), "probe-a");

    // Step 1 -> Step 2 strict ordering & diagnostic codes matrix.
    let res_base = "---\nid: RES-002\ntitle: Step Check\nspec: research\n";
    struct ValidationCase<'a>(
        &'a str,
        &'a str,
        Option<&'a str>,
        &'a str,
        Option<&'a str>,
    );
    let cases = [
        ValidationCase(
            "step1_duplicate_ref_beats_step2_unknown_field",
            "---\nid: RES-002\ntitle: Dup\nspec: research\nquestoin: typo\n---\n\n### probe: p-1\n```yaml\nmethod: a\n```\n\n### probe: p-1\n```yaml\nmethod: b\n```\n",
            Some("research"),
            ErrorCode::SpecSchemaInvalid.as_str(),
            None,
        ),
        ValidationCase(
            "step1_malformed_item_ref_beats_step2_unknown_field",
            "---\nid: RES-002\ntitle: BadRef\nspec: research\nquestoin: typo\n---\n\n### probe: bad ref with spaces\n```yaml\nmethod: a\n```\n",
            Some("research"),
            ErrorCode::SpecSchemaInvalid.as_str(),
            None,
        ),
        ValidationCase(
            "step1_malformed_dep_beats_step2_unknown_field",
            "---\nid: AC-004\ntitle: Mixed Sep\nunknown_field: typo\n---\n\n### task: t1\n```yaml\ntask_ref: t1\ndepends_on:\n  - RES-001:a#b\nscope: [src/]\n```\n",
            None,
            DIAG_ITEM_DEPENDENCY_UNKNOWN,
            None,
        ),
        ValidationCase(
            "step2_unknown_spec_field_with_hint",
            "---\nid: RES-002\ntitle: Typo Spec\nspec: research\nquestion: ok\nquestoin: oops\n---\n",
            Some("research"),
            DIAG_SPEC_FIELD_UNKNOWN,
            Some("did you mean 'question'?"),
        ),
        ValidationCase(
            "step2_unknown_item_field_with_hint",
            "---\nid: RES-002\ntitle: Typo Item Field\nspec: research\nquestion: ok\n---\n\n### probe: p-1\n```yaml\nmethod: ok\nmethdo: oops\n```\n",
            Some("research"),
            DIAG_SPEC_FIELD_UNKNOWN,
            Some("did you mean 'method'?"),
        ),
        ValidationCase(
            "step2_unknown_item_type_with_hint",
            "---\nid: RES-002\ntitle: Typo Item Type\nspec: research\nquestion: ok\n---\n\n### proeb: p-1\n```yaml\nmethod: ok\n```\n",
            Some("research"),
            DIAG_SPEC_ITEM_UNKNOWN,
            Some("did you mean 'probe'?"),
        ),
        ValidationCase(
            "step2_memory_rejects_unknown_task_item",
            "---\nid: MEM-001\ntitle: Bad Mem\nspec: memory\n---\n\n### task: t-1\n```yaml\ntarget: nope\n```\n",
            Some("memories"),
            DIAG_SPEC_ITEM_UNKNOWN,
            None,
        ),
    ];
    let _ = res_base;
    for ValidationCase(label, raw, col_name, expected_diag, expected_hint) in cases {
        let owning = col_name.and_then(|c| cols.get_key_value(c).map(|(k, v)| (k.as_str(), v)));
        let err = parse_spec_with_collections(raw, owning, &cols)
            .expect_err(&format!("{label} must fail"));
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "{label}: {err}");
        assert_eq!(
            err.effective_diag_code(),
            expected_diag,
            "{label}: unexpected diag_code for {err}"
        );
        if let Some(hint) = expected_hint {
            assert_eq!(err.hint.as_deref(), Some(hint), "{label}: {err}");
        }
    }

    // Step 3: Valid cross-Spec item ref RES-001#probe-a passes; missing item/cycle fails.
    fs::write(
        root.join("docs/milestones/AC-001.md"),
        "\
---
id: AC-001
title: Cross Spec Consumer
status: planned
---

### task: t1

```yaml
task_ref: t1
target: use RES-001 probe
depends_on:
  - RES-001#probe-a
scope: [src/]
acceptance_criteria: [ok]
```
",
    )
    .unwrap();
    let providers = build_domain_providers(&root, &adapter, None, Vec::new()).unwrap();
    let ok_resp = validate_workspace(&root, None, None, &providers).unwrap();
    assert!(ok_resp.valid, "RES-001#probe-a should pass: {ok_resp:?}");

    let ac_missing_raw = "\
---
id: AC-002
title: Missing Cross Item
status: planned
---

### task: t1

```yaml
task_ref: t1
depends_on:
  - RES-001#missing-probe
scope: [src/]
```
";
    // Owning collection custom fields/items are preserved even when doc frontmatter sets `spec: memory`.
    let mut mem_fields = SchemaDef::object();
    mem_fields.required = vec!["topic".to_owned()];
    mem_fields.additional_properties =
        Some(acdd_core::schema::AdditionalPropertiesDef::Boolean(false));
    mem_fields.properties.insert("topic".to_owned(), SchemaDef::string());
    let mem_custom = SpecConfig {
        base: Some("memory".to_owned()),
        fields: Some(mem_fields),
        ..SpecConfig::default()
    };
    let mut custom_cols = cols.clone();
    custom_cols.insert("memories".to_owned(), mem_custom.clone());
    let mem_missing_err = parse_spec_with_collections(
        "---\nid: MEM-002\ntitle: Custom Mem\nspec: memory\n---\n",
        Some(("memories", &mem_custom)),
        &custom_cols,
    )
    .expect_err("custom memories.fields.required must still apply when spec: memory is set");
    assert!(
        mem_missing_err.to_string().contains("topic"),
        "{mem_missing_err}"
    );

    let ac_missing = parse_spec_with_collections(ac_missing_raw, None, &cols).unwrap();
    let active_docs = vec![res_doc.clone(), ac_missing.clone()];
    let missing_issues = validate_spec_dependencies(
        &root,
        &adapter,
        &NoopRuntimeOverlay,
        &ac_missing,
        &active_docs,
    );
    assert!(
        missing_issues
            .iter()
            .any(|i| i.diag_code == DIAG_ITEM_DEPENDENCY_UNKNOWN),
        "expected ITEM_DEPENDENCY_UNKNOWN: {missing_issues:?}"
    );

    // Cross-Spec cycle between AC-003#t1 and RES-002#probe-b.
    let res_cycle_raw = "\
---
id: RES-002
title: Cyclic Probe
spec: research
status: active
question: cycle
---

### probe: probe-b

```yaml
method: ping
depends_on:
  - AC-003:t1
```
";
    let ac_cycle_raw = "\
---
id: AC-003
title: Cyclic Task
status: planned
---

### task: t1

```yaml
task_ref: t1
depends_on:
  - RES-002#probe-b
scope: [src/]
```
";
    let res_cycle = parse_spec_with_collections(res_cycle_raw, research_col, &cols).unwrap();
    let ac_cycle = parse_spec_with_collections(ac_cycle_raw, None, &cols).unwrap();
    let cycle_issues = validate_spec_dependencies(
        &root,
        &adapter,
        &NoopRuntimeOverlay,
        &ac_cycle,
        &[res_doc, res_cycle, ac_cycle.clone()],
    );
    assert!(
        cycle_issues
            .iter()
            .any(|i| i.message.contains("cross-spec dependency cycle")),
        "expected cross-spec cycle diagnostic: {cycle_issues:?}"
    );
}

#[test]
fn multi_spec_start_sqlite_v3_and_claim_contract_matrix() {
    // 1. MCP grammar parity for 0..N specs.
    let mcp_input: StateInput = serde_json::from_value(json!({
        "action": "start",
        "spec_paths": ["AUD-001", "AC-006"]
    }))
    .unwrap();
    assert_eq!(
        mcp_input.spec_paths,
        Some(vec!["AUD-001".to_owned(), "AC-006".to_owned()])
    );

    // 2. SQLite v2 -> v3 migration preserves prior rows and adds runs.attached_specs_json.
    let mig_dir = ScopedTempDir::create("acdd_sqlite_v3_mig").unwrap();
    let db_path = mig_dir.path().join("state.sqlite");
    {
        let mut conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(SCHEMA_V1_SQL).unwrap();
        conn.execute_batch(
            "INSERT INTO _sqlite_meta (id, schema_version, write_seq, last_backup_seq) VALUES (1, 2, 0, 0);
             ALTER TABLE runs ADD COLUMN spec_id TEXT;
             PRAGMA user_version = 2;",
        )
        .unwrap();
        ensure_schema(&mut conn).unwrap();
        let meta = read_meta(&conn).unwrap();
        assert_eq!(meta.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(CURRENT_SCHEMA_VERSION, 3);
    }

    // 3. In-memory RunHarness roundtrip with attached_specs.
    let mut snap = RunHarness::build_sample_graph("run-mem-1", "mem-1", ".", None, &["t1"]).unwrap();
    snap.run.attached_specs.push(AttachedSpecRecord {
        spec_id: "RES-001".to_owned(),
        spec_type: "research".to_owned(),
        collection: Some("research".to_owned()),
        path: "docs/research/RES-001.md".to_owned(),
        title: "Probe".to_owned(),
        digest: "a".repeat(64),
        fields: BTreeMap::from([("question".to_owned(), "How fast?".to_owned())]),
        items: BTreeMap::from([(
            "probes".to_owned(),
            vec![AttachedSpecItemSummary {
                item: "probe".to_owned(),
                item_ref: "probe-a".to_owned(),
                status: "active".to_owned(),
            }],
        )]),
    });
    let mem = RunHarness::in_memory().unwrap();
    mem.seed_run(&snap).unwrap();
    let loaded = mem.reader().load_run_graph(&snap.run.run_id).unwrap().unwrap();
    assert_eq!(loaded.run.attached_specs, snap.run.attached_specs);

    // 4. End-to-end StateEngine multi-spec start by Spec IDs (`acdd start AUD-001 AC-006`).
    let dir = ScopedTempDir::create("acdd_multi_spec_start").unwrap();
    let root = dir.path().canonicalize().unwrap();
    init_git_repo_on_branch(&root, "main");
    write_research_workspace(&root, true);
    fs::write(
        root.join("docs/milestones/AUD-001.md"),
        "\
---
id: AUD-001
title: Primary Audit Spec
status: planned
---

### task: t1

```yaml
task_ref: t1
target: implement audited fix
scope: [src/]
acceptance_criteria: [passes]
```
",
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-006.md"),
        "\
---
id: AC-006
title: Secondary Context Spec
status: planned
---

# Secondary context
",
    )
    .unwrap();

    let engine = open_engine_for_root(&root);
    // Reject empty start (neither spec nor goal).
    let empty_err = engine
        .execute(StateCommand::Start(StartReq::default()))
        .expect_err("0 specs without --goal must fail");
    assert_eq!(empty_err.code, ErrorCode::WorkflowSchemaInvalid);

    // Start run attaching both AUD-001 and AC-006 by spec ID + RES-001 by path.
    let StateCommandOutput::Start(started) = engine
        .execute(StateCommand::Start(StartReq {
            spec_paths: vec![
                "AUD-001".to_owned(),
                "AC-006".to_owned(),
                "docs/research/RES-001.md".to_owned(),
            ],
            in_place: true,
            ..StartReq::default()
        }))
        .expect("multi-spec start should succeed")
    else {
        panic!("expected StartResp");
    };
    assert_eq!(started.run.profile_id, ".acdd/workflows/default.yaml");
    assert_eq!(started.run.attached_specs.len(), 3);
    assert_eq!(started.run.attached_specs[0].spec_id, "AUD-001");
    assert_eq!(started.run.attached_specs[1].spec_id, "AC-006");
    assert_eq!(started.run.attached_specs[2].spec_id, "RES-001");
    assert_eq!(
        started.run.attached_specs[2].items["probes"][0].item_ref,
        "probe-a"
    );

    // Reopen SQLite DB and verify persistence + ClaimResp.contract.attached_specs[] projection.
    let reopened = open_engine_for_root(&root);
    let StateCommandOutput::Claim(claim) = reopened
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(started.run.run_id.as_str().to_owned()),
            node: Some("t1.build".to_owned()),
            worker_id: WorkerId::new("w-multi").unwrap(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Compact,
        }))
        .expect("claim on multi-spec run")
    else {
        panic!("expected ClaimResp");
    };
    let projected = claim.contract["attached_specs"]
        .as_array()
        .expect("ClaimResp.contract.attached_specs[] must be present");
    let projected_ids: Vec<&str> = projected
        .iter()
        .filter_map(|v| v["spec_id"].as_str())
        .collect();
    assert_eq!(projected_ids, vec!["AUD-001", "AC-006", "RES-001"]);
    let md = render_claim_prompt_markdown(&claim, None);
    assert!(md.contains("### Attached Specs"), "{md}");
    assert!(md.contains("`AUD-001`") && md.contains("`AC-006`"), "{md}");

    // Tight Claim budget trims/omits attached_specs cleanly without GuidanceBudgetExceeded.
    let tight_adapter = load_adapter(&root, None).unwrap();
    let tight_providers = build_domain_providers(&root, &tight_adapter, None, Vec::new()).unwrap();
    let mut tight_policy = tight_adapter.response.clone();
    let tight_cap = claim.total_bytes.saturating_sub(500);
    tight_policy.max_output_bytes = tight_cap;
    let tight_engine = StateEngine::open(
        StateEngineConfig {
            workspace_root: root.clone(),
            worktree: root.clone(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: tight_adapter.specs.clone(),
            workflows_dirs: tight_adapter.workflows_dirs.clone(),
            linked_workspaces: BTreeMap::new(),
            modules: tight_adapter.modules.clone(),
            response_policy: tight_policy,
            retain_traces: tight_adapter.retain_traces,
            max_snapshot_file_bytes: tight_adapter.limits.max_snapshot_file_bytes,
            allowed_binary_extensions: None,
        },
        tight_providers,
    )
    .unwrap();
    let StateCommandOutput::Claim(tight_claim) = tight_engine
        .execute(StateCommand::Claim(ClaimReq {
            run: Some(started.run.run_id.as_str().to_owned()),
            node: Some("t1.build".to_owned()),
            worker_id: WorkerId::new("w-multi").unwrap(),
            session_fingerprint: None,
            force: true,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Compact,
        }))
        .expect("tight claim budget must reclaim attached_specs instead of GuidanceBudgetExceeded")
    else {
        panic!("expected ClaimResp");
    };
    assert!(tight_claim.total_bytes <= tight_cap, "{tight_claim:?}");
    assert!(
        tight_claim.contract["attached_specs_omitted"].as_u64() >= Some(1),
        "{:?}",
        tight_claim.contract
    );
    assert!(
        tight_claim
            .omitted_sections
            .iter()
            .any(|s| s.starts_with("attached_specs")),
        "{:?}",
        tight_claim.omitted_sections
    );

    reopened
        .execute(StateCommand::Clear(ClearReq {
            run: Some(started.run.run_id.as_str().to_owned()),
            reason: Some("switch to RES-003".to_owned()),
            force: true,
            ..ClearReq::default()
        }))
        .unwrap();

    // Custom Spec sync + archived `#<item_ref>` runtime dep enforcement (`parse_task_dependency_with_db` + `verify_run_level_depends_on`).
    fs::write(
        root.join("docs/research/RES-003.md"),
        "\
---
id: RES-003
title: Custom Spec Sync Target
spec: research
status: planned
question: verify sync custom spec parsing
---

### probe: probe-s

```yaml
method: sync-check
```
",
    )
    .unwrap();
    let StateCommandOutput::Start(res_run) = reopened
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/research/RES-003.md".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .expect("custom Spec start")
    else {
        panic!("expected StartResp");
    };
    reopened
        .execute(StateCommand::Sync(SyncReq {
            run: Some(res_run.run.run_id.as_str().to_owned()),
            rebind_workflow: false,
        }))
        .expect("StateCommand::Sync must parse custom Spec using owning collection");

    // Custom Spec `RES-003` (`spec: research` + `### probe:`) archives cleanly with collections
    // while bare `render_archive_markdown` without collections rejects its custom field/item.
    let res3_raw = fs::read_to_string(root.join("docs/research/RES-003.md")).unwrap();
    let res3_archive_input = ArchiveRenderInput {
        closure: ClosureSummary {
            status: SpecStatus::Completed,
            closed_at: "2026-11-27T16:00:00Z".to_owned(),
            target_base: Some("main".to_owned()),
            target_base_commit: None,
            closed_commit: None,
            task_receipts: BTreeMap::new(),
            rollup: ReceiptRollup::default(),
            metrics_summary: BTreeMap::new(),
        },
        tasks: BTreeMap::new(),
    };
    let bare_archive_err =
        render_archive_markdown_with_collections(&res3_raw, &res3_archive_input, None, &BTreeMap::new())
            .unwrap_err();
    assert_eq!(bare_archive_err.code, ErrorCode::SpecSchemaInvalid);
    let cols = effective_spec_collections(&tight_adapter.specs);
    let (res3_archived, _) = render_archive_markdown_with_collections(
        &res3_raw,
        &res3_archive_input,
        Some(("research", &tight_adapter.specs["research"])),
        &cols,
    )
    .expect("custom Spec RES-003 must archive cleanly via render_archive_markdown_with_collections");
    let (res3_col, res3_doc) =
        parse_spec_at_relpath(&res3_archived, "docs/research/archive/RES-003.md", &cols).unwrap();
    assert_eq!(res3_col.as_deref(), Some("research"));
    assert_eq!(res3_doc.status, SpecStatus::Completed);
    assert_eq!(res3_doc.items["probes"][0].item_ref.as_str(), "probe-s");
    reopened
        .execute(StateCommand::Clear(ClearReq {
            run: Some(res_run.run.run_id.as_str().to_owned()),
            reason: Some("switch to AC-008".to_owned()),
            force: true,
            ..ClearReq::default()
        }))
        .unwrap();

    fs::create_dir_all(root.join("docs/research/archive")).unwrap();
    fs::write(
        root.join("docs/research/archive/RES-008.md"),
        "\
---
id: RES-008
title: Completed Archived Research
spec: research
status: completed
question: archived dep
---

### probe: probe-done

```yaml
status: completed
method: archived
```
",
    )
    .unwrap();
    for args in [
        &["add", "docs/research/archive/RES-008.md"][..],
        &["commit", "-m", "archive RES-008"][..],
    ] {
        let status = std::process::Command::new("git")
            .current_dir(&root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
    fs::write(
        root.join("docs/milestones/AC-008.md"),
        "\
---
id: AC-008
title: Item Dep Consumer
status: planned
depends_on:
  - RES-008#probe-done
---

### task: t1

```yaml
task_ref: t1
target: consume archived named item
depends_on:
  - RES-008#probe-done
scope: [src/]
acceptance_criteria: [ok]
```
",
    )
    .unwrap();
    let StateCommandOutput::Start(dep_started) = reopened
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("AC-008".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .expect("archived RES-008#probe-done should satisfy run and task depends_on")
    else {
        panic!("expected StartResp");
    };
    assert_eq!(dep_started.run.spec_id.as_deref(), Some("AC-008"));

    fs::write(
        root.join("docs/milestones/AC-009.md"),
        "\
---
id: AC-009
title: Missing Item Dep Consumer
status: planned
depends_on:
  - RES-008#missing-probe
---

### task: t1

```yaml
task_ref: t1
target: missing item
scope: [src/]
acceptance_criteria: [ok]
```
",
    )
    .unwrap();
    reopened
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("AC-009".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .expect_err("missing item_ref RES-008#missing-probe must fail run-level depends_on");
}

#[test]
fn docs_specs_parameterized_required_and_freshness_matrix() {
    let dir = ScopedTempDir::create("acdd_docs_specs_req").unwrap();
    let root = dir.path().canonicalize().unwrap();
    write_research_workspace(&root, false);

    let adapter = load_adapter(&root, None).unwrap();
    let providers = build_domain_providers(&root, &adapter, None, Vec::new()).unwrap();
    providers
        .validate_check_ref("docs.specs", &json!({"spec": "research", "required": true}))
        .expect("docs.specs schema accepts spec + required");
    providers
        .validate_check_ref("docs.specs", &json!({"specs": ["research"], "required": true}))
        .expect("docs.specs schema accepts specs + required");
    assert!(
        providers
            .validate_check_ref("docs.specs", &json!({"unknown_param": true}))
            .is_err(),
        "docs.specs schema rejects unknown params"
    );

    let make_req = |changed: Vec<String>| DomainCheckReq {
        run_id: RunId::new("run-doc-req").unwrap(),
        node_cursor: "build".parse::<NodeCursor>().unwrap(),
        gate_id: GateId::new("build").unwrap(),
        workspace_root: root.clone(),
        worktree: root.clone(),
        admitted_scope: vec!["docs/research/**".to_owned(), "src/**".to_owned()],
        changed_paths: changed,
        test_target: None,
        path_policy: WorkspacePathPolicy::default(),
        baseline_ref: None,
        node_tags: Vec::new(),
    };

    // 1. Unknown spec selector fails closed.
    let unknown_err = providers
        .run_check(
            "docs.specs",
            &json!({"specs": ["nonexistent"], "required": true}),
            &make_req(vec![]),
        )
        .expect_err("unknown spec collection must fail closed");
    assert!(unknown_err.to_string().contains("nonexistent"), "{unknown_err}");

    // 2. required: true fails when no matching research spec exists on disk.
    let req_params = json!({"spec": "research", "required": true});
    let missing_outcome = providers
        .run_check("docs.specs", &req_params, &make_req(vec![]))
        .unwrap();
    assert!(
        matches!(missing_outcome, DomainCheckOutcome::Failed { .. }),
        "expected Failed when research spec is missing: {missing_outcome:?}"
    );

    // 2b. docs.workspace_config exposes collection effective_builtin_base / effective_roadmap_for.
    let ws_cfg_val = providers
        .provide_context(
            "docs.workspace_config",
            &DomainContextReq {
                run_id: RunId::new("run-doc-req").unwrap(),
                node_cursor: "build".parse::<NodeCursor>().unwrap(),
                gate_id: GateId::new("build").unwrap(),
                workspace_root: root.clone(),
                worktree: root.clone(),
                admitted_scope: vec!["src/**".to_owned()],
                target: None,
                max_bytes: 8_192,
                path_policy: WorkspacePathPolicy::default(),
                baseline_ref: None,
                lens_globs: Vec::new(),
                citation_text: None,
                params: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(ws_cfg_val["specs"]["memories"]["base"], "memory");
    assert_eq!(ws_cfg_val["specs"]["memories"]["roadmap"], false);
    assert_eq!(ws_cfg_val["specs"]["research"]["base"], Value::Null);
    assert_eq!(ws_cfg_val["specs"]["research"]["roadmap"], false);

    // 3. Create RES-001.md: required: true MUST fail on empty changed_paths, missing file, or non-spec files,
    // and passes + matches observe_freshness when active spec file is changed; required: false passes on empty changed_paths.
    write_research_workspace(&root, true);
    let empty_changed_req = providers
        .run_check("docs.specs", &req_params, &make_req(vec![]))
        .unwrap();
    assert!(
        matches!(empty_changed_req, DomainCheckOutcome::Failed { .. }),
        "expected Failed on empty changed_paths even with RES-001 on disk: {empty_changed_req:?}"
    );
    let deleted_spec_req = providers
        .run_check(
            "docs.specs",
            &req_params,
            &make_req(vec!["docs/research/MISSING.md".to_owned()]),
        )
        .unwrap();
    assert!(
        matches!(deleted_spec_req, DomainCheckOutcome::Failed { .. }),
        "expected Failed when changed_paths names deleted/missing spec: {deleted_spec_req:?}"
    );
    let optional_outcome = providers
        .run_check(
            "docs.specs",
            &json!({"spec": "research", "required": false}),
            &make_req(vec![]),
        )
        .unwrap();
    assert!(
        matches!(optional_outcome, DomainCheckOutcome::Passed { .. }),
        "required: false should pass on empty changed_paths: {optional_outcome:?}"
    );
    let non_spec_changed = providers
        .run_check(
            "docs.specs",
            &req_params,
            &make_req(vec!["src/lib.rs".to_owned()]),
        )
        .unwrap();
    assert!(
        matches!(non_spec_changed, DomainCheckOutcome::Failed { .. }),
        "expected Failed when changed_paths lacks target spec: {non_spec_changed:?}"
    );

    let pass_req = make_req(vec!["docs/research/RES-001.md".to_owned()]);
    let run_res = providers
        .run_check("docs.specs", &req_params, &pass_req)
        .unwrap();
    let DomainCheckOutcome::Passed {
        execution_digest_salt: Some(run_salt),
        ..
    } = run_res
    else {
        panic!("expected Passed for docs/research/RES-001.md, got {run_res:?}");
    };
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(fresh_salt),
        ..
    } = providers
        .observe_freshness("docs.specs", &req_params, &pass_req)
        .unwrap()
    else {
        panic!("expected Passed freshness observation");
    };
    assert_eq!(run_salt, fresh_salt);

    // Nested spec deeper than 3 directory levels affects compute_specs_digest freshness salt.
    let deep_dir = root.join("docs/research/a/b/c/d");
    fs::create_dir_all(&deep_dir).unwrap();
    fs::write(
        deep_dir.join("RES-009.md"),
        "---\nid: RES-009\ntitle: Deep\nspec: research\nstatus: planned\nquestion: deep\n---\n",
    )
    .unwrap();
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(deep_salt),
        ..
    } = providers
        .observe_freshness("docs.specs", &req_params, &pass_req)
        .unwrap()
    else {
        panic!("expected Passed deep freshness observation");
    };
    assert_ne!(deep_salt, fresh_salt);

    // Editing `.acdd/specs/research.yaml` (adapter.specs_dirs) invalidates the `observe_freshness` salt.
    let schema_path = root.join(".acdd/specs/research.yaml");
    let mut schema_raw = fs::read_to_string(&schema_path).unwrap();
    schema_raw.push_str("        summary: { type: string }\n");
    fs::write(&schema_path, schema_raw).unwrap();
    let FreshnessObservation::Passed {
        execution_digest_salt: Some(schema_salt),
        ..
    } = providers
        .observe_freshness("docs.specs", &req_params, &pass_req)
        .unwrap()
    else {
        panic!("expected Passed schema freshness observation");
    };
    assert_ne!(schema_salt, deep_salt);
    let _: Value = json!(run_salt);
}
