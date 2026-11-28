//! Built-in `memory` Spec, `memory_synthesis` gate, and `specs.memories` seam tests (Wave 5).

use acdd_core::config::{load_adapter, validate_external_spec_yaml};
use acdd_core::errors::ErrorCode;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::{GateId, RunId, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{DomainCheckReq, DomainContextReq};
use acdd_state::types::{BlackboardTopic, SubmitAction};
use acdd_state::usecases::dto::{
    BlackboardPostReq, ClaimReq, StartReq, SubmitReq, VerifyReq,
};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use contextunity_acdd::state_bridge::build_domain_providers;
use std::fs;
use std::path::{Path, PathBuf};

use crate::acdd::support::{base_engine_config, git_cmd, init_git_repo_on_branch};

const VALID_RETRO: &str = r#"---
id: RETRO-001
title: AC-006 Retrospective
spec: retrospectives
status: active
workflow: code
run_id: run-ac-006
outcome: completed
blueprint_quality: Clean task seams, 0 drift
code_quality: 0 clones, 0 dead_tails
---

### gate_stat: review
```yaml
gate: tasks.review
attempts: 3
rejections: 2
cycles: 2
top_reasons: [Missing edge-case matrix row]
```

### problem: worktree-isolation
```yaml
domain: worktree-isolation
stage: integration_review
count: 2
summary: Active-run branch lookup diverged across roadmap renderers
root_cause: Duplicate checkout matchers
```

### tool_usage: context-check
```yaml
tool: acdd_context
pattern: Skipped pre-submit context re-evaluation
issue: Diff-matched memory rule discovered only on review
instruction_gap: Add pre-submit acdd context check to tasks.build guidance
```
"#;

fn write_memory_workspace(root: &Path) {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for dir in [
        "docs/milestones",
        "docs/memories/.retro/code",
        "src/billing",
        ".acdd/workflows",
        ".acdd/specs",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::copy(
        repo_root.join(".acdd/specs/retrospective.yaml"),
        root.join(".acdd/specs/retrospective.yaml"),
    )
    .unwrap();
    fs::write(root.join("AGENTS.md"), "# Agent Router\n").unwrap();
    fs::write(root.join(".gitignore"), "/docs/memories/.retro/\n").unwrap();
    fs::write(root.join("src/billing/mod.rs"), "pub fn charge() {}\n").unwrap();
    fs::write(root.join("docs/memories/.retro/code/AC-006.md"), VALID_RETRO).unwrap();
    fs::write(
        root.join("acdd.yaml"),
        r#"preset: full
roots: [src]
docs: [AGENTS.md, docs/memories]
specs_dirs: [.acdd/specs]
specs:
  milestones: { path: docs/milestones, base: contract }
  memories: { path: docs/memories, base: memory, roadmap: false }
  retrospectives: { path: docs/memories/.retro, ref: retrospective, roadmap: false }
workflows_dirs: [.acdd/workflows]
state_db: .acdd/state.sqlite
modules: { code: true, docs: true, state: true, blackboard: true }
"#,
    )
    .unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        r#"version: 2
inherit: documentation
git: { isolation: in_place }
roles: { builder: {} }
gates:
  - id: memory_synthesis
    use: memory_synthesis
  - tasks:
      gates:
        - id: build
          role: builder
          scope: [specs.memory, src/billing/**]
          context:
            - context: specs.memories
              with: {}
            - specs.memory
"#,
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-001-billing.md"),
        "---\nid: AC-001\ntitle: Billing memory test\nstatus: planned\n---\n\n### task: t1\n```yaml\ntarget: Implement billing helper\nscope: [src/billing/mod.rs]\n```\n",
    )
    .unwrap();
}

#[test]
fn memory_synthesis_gate_evidence_and_context_delivery_matrix() {
    let dir = ScopedTempDir::create("acdd-memory-gate").unwrap();
    let root = dir.path();
    write_memory_workspace(root);
    // Commit a baseline domain memory file containing a historical `#12` rule on `HEAD`.
    let legacy_path = root.join("docs/memories/legacy.md");
    let legacy_base = "---\nid: MEM-LEGACY\ntitle: Legacy Billing Memory\nspec: memories\nstatus: active\n---\n\n### rule: prior-run-rule\n```yaml\nscope: [src/billing/**]\nstatement: Historical rule committed in an earlier run.\nstatus: active\nevidence: [\"#12\"]\n```\n";
    fs::write(&legacy_path, legacy_base).unwrap();
    init_git_repo_on_branch(root, "main");

    let adapter = load_adapter(root, None).unwrap();
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let mut cfg = base_engine_config(root);
    cfg.spec_collections = adapter.specs.clone();
    let engine = StateEngine::open(cfg, providers).unwrap();

    let run_id = match engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-001-billing.md".to_owned()),
            workflow: Some("default".to_owned()),
            in_place: true,
            ..StartReq::default()
        }))
        .unwrap()
    {
        StateCommandOutput::Start(resp) => resp.run.run_id.as_str().to_owned(),
        other => panic!("unexpected start output: {other:?}"),
    };

    let finding_id = match engine
        .execute(StateCommand::BlackboardPost(BlackboardPostReq {
            run: Some(run_id.clone()),
            node: Some("memory_synthesis.synthesize".to_owned()),
            recipient_node: None,
            subtask_ref: None,
            gate: Some("review".to_owned()),
            topic: BlackboardTopic::Findings,
            author: "reviewer-1".to_owned(),
            payload: serde_json::json!({
                "title": "Missing idempotency key",
                "body": "All billing mutations must require an explicit idempotency key."
            })
            .to_string(),
        }))
        .unwrap()
    {
        StateCommandOutput::BlackboardPost(resp) => resp.entry.id.expect("assigned blackboard id"),
        other => panic!("unexpected blackboard output: {other:?}"),
    };

    let worker1 = WorkerId::new("builder-1").unwrap();
    let verify_mem = || VerifyReq {
        node: "memory_synthesis.synthesize".to_owned(),
        run: Some(run_id.clone()),
        worker_id: Some(worker1.clone()),
        test_target: None,
        claim_revision: None,
        worktree: None,
    };

    engine
        .execute(StateCommand::Claim(ClaimReq {
            node: Some("memory_synthesis.synthesize".to_owned()),
            run: Some(run_id.clone()),
            worker_id: worker1.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Compact,
        }))
        .unwrap();

    // 1. Zero-edit tolerance: verify passes cleanly when no memory rule is emitted.
    match engine.execute(StateCommand::Verify(verify_mem())).unwrap() {
        StateCommandOutput::Verify(v) => assert!(v.passed, "zero-edit verify should pass"),
        other => panic!("unexpected verify output: {other:?}"),
    }

    // 2. Fabricated / malformed blackboard evidence IDs fail closed with MEMORY_EVIDENCE_UNKNOWN.
    let mem_path = root.join("docs/memories/MEM-001-billing.md");
    for (label, spec_line, ev_str) in [
        ("fabricated-explicit", "spec: memory\n", "#999999"),
        ("omitted-spec", "", "#999999"),
        ("collection-spec", "spec: memories\n", "#999999"),
        ("malformed-hash-plus", "spec: memory\n", "#+1"),
        ("malformed-plus", "spec: memory\n", "+1"),
    ] {
        fs::write(
            &mem_path,
            format!("---\nid: MEM-001\n{spec_line}title: Billing\nstatus: active\n---\n\n### rule: idempotency-key\n```yaml\nscope: [src/billing/**]\nstatement: Require idempotency key.\nevidence: [\"{ev_str}\"]\n```\n"),
        )
        .unwrap();
        let err = engine.execute(StateCommand::Verify(verify_mem())).unwrap_err();
        assert_eq!(err.code, ErrorCode::GateVerificationFailed, "{label}: {err}");
        assert!(err.message.contains("MEMORY_EVIDENCE_UNKNOWN"), "{label}: {}", err.message);
    }
    fs::remove_file(&mem_path).unwrap();

    // 3. Historical `#12` on baseline preserved while newly appended bad IDs fail and valid RETRO-* pass.
    let write_appended_legacy = |new_ev: &str| {
        fs::write(
            &legacy_path,
            format!("{legacy_base}\n### rule: appended-rule\n```yaml\nscope: [src/billing/**]\nstatement: Newly appended rule.\nstatus: active\nevidence: [\"{new_ev}\"]\n```\n"),
        )
        .unwrap();
    };
    for (label, bad_ev) in [
        ("fabricated-blackboard", "#999999"),
        ("fabricated-retro-spec", "RETRO-999"),
        ("fabricated-retro-item", "RETRO-001#missing-problem"),
    ] {
        write_appended_legacy(bad_ev);
        let err = engine.execute(StateCommand::Verify(verify_mem())).unwrap_err();
        assert_eq!(err.code, ErrorCode::GateVerificationFailed, "{label}: {err}");
        assert!(err.message.contains("MEMORY_EVIDENCE_UNKNOWN"), "{label}: {}", err.message);
    }
    for (label, good_ev) in [
        ("retro-spec-id", "RETRO-001"),
        ("retro-item-ref", "RETRO-001#worktree-isolation"),
    ] {
        write_appended_legacy(good_ev);
        match engine.execute(StateCommand::Verify(verify_mem())).unwrap() {
            StateCommandOutput::Verify(v) => {
                assert!(v.passed, "{label}: baseline #12 + {good_ev} must pass verify")
            }
            other => panic!("unexpected verify output: {other:?}"),
        }
    }

    // 4. Regression: fabricated citation committed AFTER node.baseline_commit still fails closed.
    write_appended_legacy("#999999");
    git_cmd(root, &["add", "docs/memories/legacy.md"]);
    git_cmd(root, &["commit", "-m", "commit fabricated citation after baseline"]);
    let committed_err = engine.execute(StateCommand::Verify(verify_mem())).unwrap_err();
    assert_eq!(committed_err.code, ErrorCode::GateVerificationFailed);
    assert!(committed_err.message.contains("MEMORY_EVIDENCE_UNKNOWN"));
    write_appended_legacy("RETRO-001");

    // 5. Valid blackboard evidence ID passes verify (with disjoint-scope MEM-002) and submits cleanly.
    fs::write(
        &mem_path,
        format!("---\nid: MEM-001\nspec: memory\ntitle: Billing\nstatus: active\n---\n\n### rule: idempotency-key\n```yaml\nscope: [src/billing/**]\nstatement: Always require an idempotency key.\nevidence: [\"#{finding_id}\"]\n```\n"),
    )
    .unwrap();
    fs::write(
        root.join("docs/memories/MEM-002-auth.md"),
        format!("---\nid: MEM-002\ntitle: Auth disjoint\nstatus: active\n---\n\n### rule: token-expiry\n```yaml\nscope: [src/auth/**]\nstatement: Rotate auth tokens.\nevidence: [\"#{finding_id}\"]\n```\n"),
    )
    .unwrap();
    match engine.execute(StateCommand::Verify(verify_mem())).unwrap() {
        StateCommandOutput::Verify(v) => assert!(v.passed, "valid evidence verify should pass"),
        other => panic!("unexpected verify output: {other:?}"),
    }
    engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            node: "memory_synthesis.synthesize".to_owned(),
            run: Some(run_id.clone()),
            worker_id: worker1,
            claim_revision: None,
            action: SubmitAction::Pass,
            summary: Some("Recorded billing idempotency rule".to_owned()),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            allow_main: true,
            worktree: None,
        })))
        .unwrap();

    // 6. Subsequent task receives relevant rules via `specs.memories` & `specs.memory` base-selector
    //    and excludes disjoint-scope rules.
    let t1_claim = match engine
        .execute(StateCommand::Claim(ClaimReq {
            node: Some("t1.build".to_owned()),
            run: Some(run_id),
            worker_id: WorkerId::new("builder-2").unwrap(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Full,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(resp) => resp,
        other => panic!("unexpected claim output: {other:?}"),
    };
    for lens_key in ["specs.memories", "specs.memory"] {
        let lens_json = serde_json::to_string(
            t1_claim
                .context_lenses
                .get(lens_key)
                .unwrap_or_else(|| panic!("{lens_key} context lens must be populated")),
        )
        .unwrap();
        assert!(
            lens_json.contains("MEM-001") && lens_json.contains("idempotency-key"),
            "{lens_key} should deliver matching billing rule: {lens_json}"
        );
        assert!(
            !lens_json.contains("MEM-002") && !lens_json.contains("token-expiry"),
            "{lens_key} should exclude disjoint auth rule: {lens_json}"
        );
    }
}

#[test]
fn reusable_spec_examples_validate_cleanly() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = manifest_dir.join("skills/acdd/references/acdd-spec-examples");
    for name in [
        "audit_brief.yaml",
        "audit_report.yaml",
        "proposal.yaml",
        "retrospective.yaml",
    ] {
        let path = examples_dir.join(name);
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        validate_external_spec_yaml(name, &content)
            .unwrap_or_else(|e| panic!("spec example {name} failed validation: {e}"));
    }

    // Strict declarative `retrospective.yaml` parsing + negative matrix.
    let repo_adapter = load_adapter(&manifest_dir, None).unwrap();
    let repo_cols = acdd_spec::effective_spec_collections(&repo_adapter.specs);
    let (_, parsed_retro) = acdd_spec::discovery::parse_spec_at_relpath(
        VALID_RETRO,
        "docs/memories/.retro/code/AC-006.md",
        &repo_cols,
    )
    .expect("strict declarative retrospective spec must parse");
    assert_eq!(parsed_retro.workflow.as_deref(), Some("code"));
    assert_eq!(parsed_retro.items["gate_stats"].len(), 1);
    assert_eq!(parsed_retro.items["problems"].len(), 1);
    assert_eq!(parsed_retro.items["tool_usage"].len(), 1);
    for bad_retro in [
        "---\nid: RETRO-002\ntitle: Bad\nspec: retrospectives\nstatus: active\nworkflow: code\nrun_id: r1\noutcome: completed\nextra_noise: true\n---\n",
        "---\nid: RETRO-003\ntitle: Bad\nspec: retrospectives\nstatus: active\nworkflow: code\nrun_id: r1\n---\n",
        "---\nid: RETRO-004\ntitle: Bad\nspec: retrospectives\nstatus: active\nworkflow: code\nrun_id: r1\noutcome: completed\n---\n\n### problem: p1\n```yaml\ndomain: tests\nstage: review\ncount: 1\nsummary: bad\nbloat: nope\n```\n",
    ] {
        assert!(
            acdd_spec::discovery::parse_spec_at_relpath(
                bad_retro,
                "docs/memories/.retro/code/bad.md",
                &repo_cols,
            )
            .is_err(),
            "invalid retrospective must fail closed:\n{bad_retro}"
        );
    }

    // Regression: draft_spec honors minItems, ADR 0014 §2 precedence, and prose heading before YAML fence.
    let tmp = ScopedTempDir::create("draft-memories-reg").unwrap();
    let mut cols = acdd_core::config::default_specs();
    cols.insert(
        "memories".to_owned(),
        acdd_core::config::SpecConfig {
            path: Some("docs/memories".to_owned()),
            base: Some("memory".to_owned()),
            prefixes: vec!["MEM".to_owned()],
            roadmap: Some(false),
            ..Default::default()
        },
    );
    let drafted = acdd_spec::draft::draft_spec(
        tmp.path(),
        &cols,
        &acdd_spec::DraftReq {
            spec: "memories".to_owned(),
            slug: "auth-rule".to_owned(),
            prefix: None,
            title: Some("Auth Rule".to_owned()),
            workflow: None,
        },
        &acdd_spec::overlay::NoopRuntimeOverlay,
    )
    .expect("draft_spec memories must honor minItems: 1 on evidence");
    let drafted_raw = fs::read_to_string(tmp.path().join(&drafted.spec_path)).unwrap();
    let (_, parsed_mem) =
        acdd_spec::discovery::parse_spec_at_relpath(&drafted_raw, &drafted.spec_path, &cols)
            .expect("drafted memories doc must pass parse_spec_at_relpath");
    assert_eq!(parsed_mem.spec.as_deref(), Some("memories"));

    for (doc_spec, dir_col, selector, expected) in [
        (Some("contract"), Some("milestones"), "milestones", true),
        (Some("contract"), Some("milestones"), "plans", false),
        (Some("memories"), Some("milestones"), "memories", true),
        (Some("memories"), Some("milestones"), "milestones", false),
        (Some("memory"), Some("milestones"), "milestones", false),
    ] {
        assert_eq!(
            acdd_spec::schema::spec_matches_selector(doc_spec, dir_col, selector, &cols),
            expected
        );
    }

    let prose_doc = "---\nid: AC-001\ntitle: Prose Heading\nspec: contract\nstatus: planned\n---\n\n### Example: Adding a rule\n\n```yaml\nfoo: bar\n```\n";
    assert_eq!(acdd_spec::parse_spec(prose_doc).unwrap().id, "AC-001");
}

#[test]
fn retrospective_ignore_and_diff_aware_memory_ranking_matrix() {
    let tmp = ScopedTempDir::create("retro-diff-memory-matrix").unwrap();
    let root = tmp.path();
    write_memory_workspace(root);
    fs::write(
        root.join("src/billing/mod.rs"),
        "pub fn refund() {\n    let _ = 1;\n}\n\npub fn charge() {\n    let _ = 2;\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs/memories/.retro/code/.processed")).unwrap();
    fs::create_dir_all(root.join("docs/memories/archive")).unwrap();
    fs::write(
        root.join("docs/memories/.retro/code/.processed/AC-005.md"),
        "# Processed legacy note without frontmatter\n",
    )
    .unwrap();
    fs::write(root.join("docs/memories/archive/retired.md"), "# Retired\n").unwrap();
    fs::write(
        root.join("docs/memories/billing.md"),
        "---\nid: MEM-BILLING\ntitle: Billing\nspec: memories\nstatus: active\nowners: [src/billing/**]\n---\n\n### rule: refund-symbol\n```yaml\nscope: [src/billing/**]\nsymbols: [refund]\nstatement: Validate refund() eligibility.\nstatus: active\nevidence: [RETRO-001]\n```\n\n### rule: charge-symbol\n```yaml\nscope: [src/billing/**]\nsymbols: [charge]\nstatement: Keep charge() idempotent.\nstatus: active\nevidence: [RETRO-001]\n```\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/memories/instructions.md"),
        "---\nid: MEM-INSTRUCTIONS\ntitle: Instructions\nspec: memories\nstatus: active\nowners: [AGENTS.md]\n---\n\n### rule: preserve-router\n```yaml\nscope: [AGENTS.md]\nstatement: Preserve agent router invariants.\nstatus: active\nevidence: [RETRO-001]\n```\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/memories/edge_boost.md"),
        "---\nid: MEM-EDGE\ntitle: Edge-Boosted Billing\nspec: memories\nstatus: active\n---\n\n### rule: alpha-unlinked\n```yaml\nscope: [src/billing/**]\nstatement: Unlinked billing rule.\nstatus: active\nevidence: [RETRO-001]\n```\n\n### rule: zeta-edge-linked\n```yaml\nscope: [src/billing/**]\nstatement: Linked via doc_section edge without symbols.\nstatus: active\nevidence: [RETRO-001]\n```\nSee [`charge`](../../src/billing/mod.rs).\n",
    )
    .unwrap();

    // (a) SpecConfig.ignore isolates memories from .retro/ and .retro/ from .processed/ + archive/.
    let adapter = load_adapter(root, None).unwrap();
    let effective_cols = acdd_spec::effective_spec_collections(&adapter.specs);
    let scan = acdd_spec::discovery::collect_workspace_specs(root, &effective_cols);
    assert!(scan.errors.is_empty(), "ignore must skip archive/.processed: {:?}", scan.errors);
    assert!(scan.specs.iter().any(|s| {
        s.doc.spec.as_deref() == Some("retrospectives")
            && s.rel_path == "docs/memories/.retro/code/AC-006.md"
    }));
    assert!(!scan.specs.iter().any(|s| s.rel_path.contains(".processed/")));

    init_git_repo_on_branch(root, "main");
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let make_req = |scope: Vec<String>| DomainContextReq {
        run_id: RunId::new("run-diff").unwrap(),
        node_cursor: "t1.review".parse().unwrap(),
        gate_id: GateId::new("review").unwrap(),
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        admitted_scope: scope,
        target: Some("Review changes".to_owned()),
        max_bytes: 16_384,
        path_policy: WorkspacePathPolicy::default(),
        baseline_ref: Some("HEAD".to_owned()),
        lens_globs: Vec::new(),
        citation_text: None,
        params: serde_json::Value::Null,
    };

    // (b) specs.retrospectives delivers pending items with schema-driven summaries.
    let retro_str = serde_json::to_string(
        &providers
            .provide_context("specs.retrospectives", &make_req(Vec::new()))
            .unwrap(),
    )
    .unwrap();
    assert!(
        retro_str.contains("RETRO-001") && retro_str.contains("Active-run branch lookup"),
        "specs.retrospectives must surface item summaries: {retro_str}"
    );

    // (c) Tier 2 file-diff ranking on AGENTS.md WITHOUT code.sqlite.
    fs::write(root.join("AGENTS.md"), "# Agent Router\nUpdated rule\n").unwrap();
    let doc_diff = providers
        .provide_context(
            "specs.memories",
            &make_req(vec!["src/**".to_owned(), "AGENTS.md".to_owned()]),
        )
        .unwrap();
    assert_eq!(doc_diff["items"][0]["ref"].as_str(), Some("preserve-router"));
    fs::write(root.join("AGENTS.md"), "# Agent Router\n").unwrap();

    // (d) & (e) Hunk-exact `symbols:` ranking + `doc_section` code-map edge boosting.
    let code_db = root.join(".acdd/code-map.sqlite");
    contextunity_acdd::engine::pipeline::build(root, &code_db, Some(&root.join("acdd.yaml")))
        .expect("pipeline::build should index src and docs/memories");
    {
        // Markdown extraction does not emit `doc_section -> fn` symbol edges from `### rule:` headings;
        // seed the `documents` edge directly to prove `diff_symbol_sections` boosts un-`symbols:` rules.
        let conn = rusqlite::Connection::open(&code_db).unwrap();
        conn.execute_batch(
            "INSERT OR IGNORE INTO path_dictionary (path) VALUES ('docs/memories/edge_boost.md');
             INSERT OR IGNORE INTO coverage_evidence (evidence) VALUES ('test-doc-edge');
             INSERT OR IGNORE INTO nodes (id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id)
               SELECT 'doc:edge_boost:zeta', 'doc_section', 'rule: zeta-edge-linked', 'rule: zeta-edge-linked', path_id, 18, 28, 0, 'markdown', 0, '{}', 999001, path_id
               FROM path_dictionary WHERE path = 'docs/memories/edge_boost.md';",
        )
        .unwrap();
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO edges (src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count)
                 SELECT 999001, 'documents', n.node_hash, p.path_id, 18, e.evidence_id, e.evidence_id, 1
                 FROM nodes n, path_dictionary p, coverage_evidence e
                 WHERE n.name = 'charge' AND p.path = 'docs/memories/edge_boost.md' AND e.evidence = 'test-doc-edge'",
                [],
            )
            .unwrap();
        assert!(inserted > 0, "expected documents edge to 'charge'");
    }
    fs::write(
        root.join("src/billing/mod.rs"),
        "pub fn refund() {\n    let _ = 1;\n}\n\npub fn charge() {\n    let _ = 99;\n}\n",
    )
    .unwrap();
    let providers_code =
        build_domain_providers(root, &adapter, Some(&code_db), Vec::new()).unwrap();
    let sym_diff = providers_code
        .provide_context("specs.memories", &make_req(vec!["src/billing/**".to_owned()]))
        .unwrap();
    let ranked_refs: Vec<&str> = sym_diff["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["ref"].as_str())
        .collect();
    assert_eq!(ranked_refs[0], "charge-symbol", "hunk-exact charge() must rank first: {ranked_refs:?}");
    assert!(
        sym_diff["items"][0].get("fields").is_none()
            && sym_diff["items"][0].get("item").is_none(),
        "specs.* itemized payloads must stay compact without unrendered fields/item keys: {}",
        sym_diff["items"][0]
    );
    let pos = |name: &str| {
        ranked_refs
            .iter()
            .position(|r| *r == name)
            .unwrap_or_else(|| panic!("{name} missing from {ranked_refs:?}"))
    };
    assert!(pos("charge-symbol") < pos("refund-symbol"));
    assert!(
        pos("zeta-edge-linked") < pos("alpha-unlinked"),
        "doc_section edge-linked rule without symbols: must outrank unlinked sibling: {ranked_refs:?}"
    );

    // TreeOid baseline (`in_place` `claim_tree`) also triggers hunk-exact symbol ranking cleanly.
    let head_tree = crate::acdd::support::git_cmd(root, &["rev-parse", "HEAD^{tree}"]);
    let mut tree_req = make_req(vec!["src/billing/**".to_owned()]);
    tree_req.baseline_ref = Some(head_tree);
    let tree_diff = providers_code
        .provide_context("specs.memories", &tree_req)
        .unwrap();
    assert_eq!(
        tree_diff["items"][0]["ref"].as_str(),
        Some("charge-symbol"),
        "TreeOid baseline in in_place mode must trigger hunk-exact symbol boost: {tree_diff}"
    );

    // Linked worktree inherits gitignored specs (docs/memories/.retro) from workspace_root and
    // verify_memory_spec_evidence resolves RETRO-001 from workspace_root inside managed worktrees.
    crate::acdd::support::git_cmd(root, &["checkout", "--", "src/billing/mod.rs"]);
    let linked_wt_parent = ScopedTempDir::create("retro-linked-wt").unwrap();
    let linked_wt = linked_wt_parent.path().join("wt");
    crate::acdd::support::git_cmd(
        root,
        &["worktree", "add", "-b", "wt-retro", linked_wt.to_str().unwrap(), "HEAD"],
    );
    assert!(!linked_wt.join("docs/memories/.retro/code/AC-006.md").exists());
    let mut wt_retro_req = make_req(Vec::new());
    wt_retro_req.worktree = linked_wt.clone();
    let wt_retro_str = serde_json::to_string(
        &providers_code
            .provide_context("specs.retrospectives", &wt_retro_req)
            .unwrap(),
    )
    .unwrap();
    assert!(
        wt_retro_str.contains("RETRO-001"),
        "linked worktree specs.retrospectives must surface gitignored .retro from workspace_root: {wt_retro_str}"
    );
    let _ = acdd_core::git::git_command(
        root,
        &["worktree", "remove", "--force", linked_wt.to_str().unwrap()],
    );

    let mut wt_cfg = base_engine_config(root);
    wt_cfg.spec_collections = adapter.specs.clone();
    let providers_wt = build_domain_providers(root, &adapter, Some(&code_db), Vec::new()).unwrap();
    let wt_engine = StateEngine::open(wt_cfg, providers_wt).unwrap();
    let wt_run_id = match wt_engine
        .execute(StateCommand::Start(StartReq {
            spec_path: Some("docs/milestones/AC-001-billing.md".to_owned()),
            workflow: Some("default".to_owned()),
            worktree: true,
            ..StartReq::default()
        }))
        .unwrap()
    {
        StateCommandOutput::Start(resp) => resp.run.run_id.as_str().to_owned(),
        other => panic!("unexpected start: {other:?}"),
    };
    let wt_worker = WorkerId::new("wt-mem-builder").unwrap();
    let wt_claim = match wt_engine
        .execute(StateCommand::Claim(ClaimReq {
            node: Some("memory_synthesis.synthesize".to_owned()),
            run: Some(wt_run_id.clone()),
            worker_id: wt_worker.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
            detail: acdd_core::response::Detail::Compact,
        }))
        .unwrap()
    {
        StateCommandOutput::Claim(c) => c,
        other => panic!("unexpected claim: {other:?}"),
    };
    let managed_wt = PathBuf::from(&wt_claim.worktree);
    assert!(!managed_wt.join("docs/memories/.retro/code/AC-006.md").exists());
    fs::write(
        managed_wt.join("docs/memories/wt_rule.md"),
        "---\nid: MEM-WT\ntitle: Worktree Rule\nspec: memories\nstatus: active\n---\n\n### rule: wt-retro-cite\n```yaml\nscope: [src/billing/**]\nstatement: Cite primary gitignored RETRO-001 from linked worktree.\nstatus: active\nevidence: [RETRO-001]\n```\n",
    )
    .unwrap();
    match wt_engine
        .execute(StateCommand::Verify(VerifyReq {
            node: "memory_synthesis.synthesize".to_owned(),
            run: Some(wt_run_id),
            worker_id: Some(wt_worker),
            test_target: None,
            claim_revision: None,
            worktree: None,
        }))
        .unwrap()
    {
        StateCommandOutput::Verify(v) => assert!(
            v.passed,
            "verify_memory_spec_evidence in managed worktree must resolve gitignored RETRO-001 from workspace_root"
        ),
        other => panic!("unexpected verify: {other:?}"),
    }
    fs::write(
        root.join("src/billing/mod.rs"),
        "pub fn refund() {\n    let _ = 1;\n}\n\npub fn charge() {\n    let _ = 99;\n}\n",
    )
    .unwrap();
    let base_mem = providers_code
        .provide_context("specs.memory", &make_req(vec!["src/**".to_owned()]))
        .unwrap();
    assert_eq!(base_mem["items"][0]["ref"].as_str(), Some("charge-symbol"));

    // (f) compute_specs_digest via observe_freshness("docs.specs") changes on add/modify under dot-dir .retro/.
    let freshness_req = DomainCheckReq {
        run_id: RunId::new("run-diff").unwrap(),
        node_cursor: "t1.review".parse().unwrap(),
        gate_id: GateId::new("review").unwrap(),
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        admitted_scope: vec![],
        changed_paths: vec![],
        test_target: None,
        path_policy: WorkspacePathPolicy::default(),
        baseline_ref: None,
        node_tags: vec![],
    };
    let d0 = providers_code
        .observe_freshness("docs.specs", &serde_json::json!({}), &freshness_req)
        .unwrap();
    let retro_extra = root.join("docs/memories/.retro/code/run-extra.md");
    fs::write(&retro_extra, "---\nid: RETRO-EXTRA\n---\ninitial\n").unwrap();
    let d1 = providers_code
        .observe_freshness("docs.specs", &serde_json::json!({}), &freshness_req)
        .unwrap();
    assert_ne!(d0, d1, "adding file under .retro/ must change compute_specs_digest");
    fs::write(&retro_extra, "---\nid: RETRO-EXTRA\n---\nmodified\n").unwrap();
    let d2 = providers_code
        .observe_freshness("docs.specs", &serde_json::json!({}), &freshness_req)
        .unwrap();
    assert_ne!(d1, d2, "modifying file under .retro/ must change compute_specs_digest");

    // (g) Live repo `.acdd/workflows/default.yaml` and `.acdd/workflows/memory_optimize.yaml` resolve cleanly.
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (wf_path, gate_id, expected_lens) in [
        (".acdd/workflows/default.yaml", "polish", "specs.memories"),
        (".acdd/workflows/memory_optimize.yaml", "consolidate", "specs.retrospectives"),
    ] {
        let wf = acdd_state::workflow::resolve_workflow_ref_with_dirs(
            wf_path,
            Some(&repo_root),
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{wf_path} must resolve cleanly: {e}"));
        assert!(
            wf.gates
                .iter()
                .any(|g| g.id.as_str() == gate_id && g.context.iter().any(|c| c == expected_lens)),
            "{wf_path} gate {gate_id} must include {expected_lens}"
        );
    }
}
