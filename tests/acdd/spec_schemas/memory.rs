//! Built-in `memory` Spec, `memory_synthesis` gate, and `specs.memories` seam tests (Wave 5).

use acdd_core::config::{load_adapter, validate_external_spec_yaml};
use acdd_core::errors::ErrorCode;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::WorkerId;
use acdd_state::types::{BlackboardTopic, SubmitAction};
use acdd_state::usecases::dto::{
    BlackboardPostReq, ClaimReq, StartReq, SubmitReq, VerifyReq,
};
use acdd_state::usecases::{StateCommand, StateCommandOutput, StateEngine};
use contextunity_acdd::state_bridge::build_domain_providers;
use std::fs;
use std::path::{Path, PathBuf};

use crate::acdd::support::{base_engine_config, init_git_repo_on_branch};

fn write_memory_workspace(root: &Path) {
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/memories")).unwrap();
    fs::create_dir_all(root.join("src/billing")).unwrap();
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(root.join("src/billing/mod.rs"), "pub fn charge() {}\n").unwrap();
    fs::write(
        root.join("acdd.yaml"),
        r#"preset: full
roots:
  - src
specs:
  milestones:
    path: docs/milestones
    base: contract
  memories:
    path: docs/memories
    base: memory
    roadmap: false
workflows_dirs:
  - .acdd/workflows
state_db: .acdd/state.sqlite
modules:
  code: true
  docs: true
  state: true
  blackboard: true
"#,
    )
    .unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        r#"version: 2
inherit: documentation
git:
  isolation: in_place
roles:
  builder: {}
gates:
  - id: memory_synthesis
    use: memory_synthesis
  - tasks:
      gates:
        - id: build
          role: builder
          context:
            - specs.memories
"#,
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/AC-001-billing.md"),
        r#"---
id: AC-001
title: Billing memory test
status: planned
---

### task: t1
```yaml
target: Implement billing helper
scope:
  - src/billing/mod.rs
```
"#,
    )
    .unwrap();
}

#[test]
fn memory_synthesis_gate_evidence_and_context_delivery_matrix() {
    let dir = ScopedTempDir::create("acdd-memory-gate").unwrap();
    let root = dir.path();
    write_memory_workspace(root);
    init_git_repo_on_branch(root, "main");

    let adapter = load_adapter(root, None).unwrap();
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let mut cfg = base_engine_config(root);
    cfg.spec_collections = adapter.specs.clone();
    let engine = StateEngine::open(cfg, providers).unwrap();

    // Start run on AC-001.
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

    // Post a real blackboard finding (as produced by a rejected review).
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
    let verify_mem = |run_id: &str| VerifyReq {
        node: "memory_synthesis.synthesize".to_owned(),
        run: Some(run_id.to_owned()),
        worker_id: Some(worker1.clone()),
        test_target: None,
        claim_revision: None,
        worktree: None,
    };

    // Claim single-role memory_synthesis.synthesize gate.
    let _claim = engine
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
    let zero_edit = engine
        .execute(StateCommand::Verify(verify_mem(&run_id)))
        .unwrap();
    match zero_edit {
        StateCommandOutput::Verify(v) => assert!(v.passed, "zero-edit verify should pass"),
        other => panic!("unexpected verify output: {other:?}"),
    }

    // 2. Fabricated / malformed blackboard evidence IDs fail closed with MEMORY_EVIDENCE_UNKNOWN,
    //    including omitted `spec:`, `spec: memories`, and malformed `#+1`/`+1` IDs.
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
            format!(
                "---\nid: MEM-001\n{spec_line}title: Billing idempotency rules\nstatus: active\n---\n\n### rule: idempotency-key\n```yaml\nscope:\n  - src/billing/**\nstatement: Always require an idempotency key on billing mutations.\nevidence:\n  - \"{ev_str}\"\n```\n"
            ),
        )
        .unwrap();
        let err = engine
            .execute(StateCommand::Verify(verify_mem(&run_id)))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateVerificationFailed, "{label}: {err}");
        assert!(
            err.message.contains("MEMORY_EVIDENCE_UNKNOWN"),
            "{label}: expected MEMORY_EVIDENCE_UNKNOWN in message, got: {}",
            err.message
        );
    }

    // 3. Valid blackboard evidence ID passes verify (with a second disjoint-scope memory rule) and submits cleanly.
    fs::write(
        &mem_path,
        format!(
            r##"---
id: MEM-001
spec: memory
title: Billing idempotency rules
status: active
---

### rule: idempotency-key
```yaml
scope:
  - src/billing/**
statement: Always require an idempotency key on billing mutations.
evidence:
  - "#{finding_id}"
```
"##
        ),
    )
    .unwrap();
    fs::write(
        root.join("docs/memories/MEM-002-auth.md"),
        format!(
            r##"---
id: MEM-002
title: Auth disjoint rules
status: active
---

### rule: token-expiry
```yaml
scope:
  - src/auth/**
statement: Rotate auth tokens every 15 minutes.
evidence:
  - "#{finding_id}"
```
"##
        ),
    )
    .unwrap();
    let valid_verify = engine
        .execute(StateCommand::Verify(verify_mem(&run_id)))
        .unwrap();
    match valid_verify {
        StateCommandOutput::Verify(v) => assert!(v.passed, "valid evidence verify should pass"),
        other => panic!("unexpected verify output: {other:?}"),
    }

    let _submit = engine
        .execute(StateCommand::Submit(Box::new(SubmitReq {
            node: "memory_synthesis.synthesize".to_owned(),
            run: Some(run_id.clone()),
            worker_id: worker1.clone(),
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

    // 4. Subsequent task receives the relevant memory rule via `context_lenses["specs.memories"]`
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
    let lens_json = serde_json::to_string(
        t1_claim
            .context_lenses
            .get("specs.memories")
            .expect("specs.memories context lens must be populated"),
    )
    .unwrap();
    assert!(
        lens_json.contains("MEM-001") && lens_json.contains("idempotency-key"),
        "specs.memories lens should deliver matching billing rule, got: {lens_json}"
    );
    assert!(
        !lens_json.contains("MEM-002") && !lens_json.contains("token-expiry"),
        "specs.memories lens should exclude disjoint auth rule, got: {lens_json}"
    );
}

#[test]
fn reusable_spec_examples_validate_cleanly() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = manifest_dir.join("skills/acdd/references/acdd-spec-examples");
    for name in ["audit_brief.yaml", "audit_report.yaml", "proposal.yaml"] {
        let path = examples_dir.join(name);
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        validate_external_spec_yaml(name, &content)
            .unwrap_or_else(|e| panic!("spec example {name} failed validation: {e}"));
    }

    // Regression: draft_spec honors minItems (e.g. memory.yaml evidence: minItems: 1),
    // spec_matches_selector enforces ADR 0014 §2 precedence without cross-collection leaks,
    // and prose headings with whitespace refs before YAML fences do not fail TaskRef::new.
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

    // ADR 0014 §2 precedence matrix
    assert!(acdd_spec::schema::spec_matches_selector(
        Some("contract"),
        Some("milestones"),
        "milestones",
        &cols
    ));
    assert!(!acdd_spec::schema::spec_matches_selector(
        Some("contract"),
        Some("milestones"),
        "plans",
        &cols
    ));
    assert!(acdd_spec::schema::spec_matches_selector(
        Some("memories"),
        Some("milestones"),
        "memories",
        &cols
    ));
    assert!(!acdd_spec::schema::spec_matches_selector(
        Some("memories"),
        Some("milestones"),
        "milestones",
        &cols
    ));
    assert!(!acdd_spec::schema::spec_matches_selector(
        Some("memory"),
        Some("milestones"),
        "milestones",
        &cols
    ));

    // Prose heading with whitespace ref + YAML fence must not fail TaskRef::new
    let prose_doc = "---\nid: AC-001\ntitle: Prose Heading\nspec: contract\nstatus: planned\n---\n\n### Example: Adding a rule\n\n```yaml\nfoo: bar\n```\n";
    let parsed_prose = acdd_spec::parse_spec(prose_doc)
        .expect("prose heading with whitespace ref before YAML fence must parse cleanly");
    assert_eq!(parsed_prose.id, "AC-001");
}
