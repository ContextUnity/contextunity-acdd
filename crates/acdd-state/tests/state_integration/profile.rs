use acdd_core::errors::ErrorCode;
use acdd_core::ports::{
    ActionCapability, CheckCapability, CompositeDomainProvider, DomainCapabilities, DomainToolDef,
};
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use acdd_core::subsystems::SubsystemRegistry;
use acdd_state::profile::*;
use acdd_state::types::{IsolationMode, SaveAction, WorktreeLifecycle};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::Arc;

use super::common::StubDomainProvider;

fn code_and_docs_providers() -> CompositeDomainProvider {
    let code = StubDomainProvider::new(
        "code",
        DomainCapabilities {
            actions: vec![],
            checks: vec![
                CheckCapability {
                    name: "seam".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "signature_scope".to_owned(),
                    input_schema: SchemaDef::object(),
                },
            ],
            context_lenses: BTreeSet::from([
                "symbols".to_owned(),
                "tests".to_owned(),
                "external_callers".to_owned(),
            ]),
            tools: vec![],
        },
    );
    let docs = StubDomainProvider::new(
        "docs",
        DomainCapabilities {
            actions: vec![],
            checks: vec![CheckCapability {
                name: "links".to_owned(),
                input_schema: SchemaDef::object(),
            }],
            context_lenses: BTreeSet::from([
                "instructions".to_owned(),
                "adrs".to_owned(),
                "invariants".to_owned(),
            ]),
            tools: vec![],
        },
    );
    let mut url_schema = SchemaDef::object();
    url_schema
        .properties
        .insert("url".to_owned(), SchemaDef::string());
    url_schema.required = vec!["url".to_owned()];
    url_schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

    let http = StubDomainProvider::new(
        "http",
        DomainCapabilities {
            actions: vec![ActionCapability {
                name: "deploy".to_owned(),
                input_schema: url_schema.clone(),
                mutating: true,
                supports_result_lookup: true,
            }],
            checks: vec![CheckCapability {
                name: "health".to_owned(),
                input_schema: SchemaDef::object(),
            }],
            context_lenses: BTreeSet::from(["endpoints".to_owned()]),
            tools: vec![DomainToolDef {
                name: "probe".to_owned(),
                description: "Probe HTTP endpoint".to_owned(),
                input_schema: url_schema,
                read_only: true,
            }],
        },
    );
    CompositeDomainProvider::new(vec![Arc::new(code), Arc::new(docs), Arc::new(http)])
        .expect("mock providers valid")
}

fn disabled_code_and_docs_modules() -> SubsystemRegistry {
    SubsystemRegistry {
        disabled: BTreeSet::from(["code".to_owned(), "docs".to_owned()]),
        ..SubsystemRegistry::default()
    }
}

#[test]
fn embedded_profiles_resolve_and_validate_matrix() {
    let providers = code_and_docs_providers();
    let code_docs_disabled = disabled_code_and_docs_modules();

    // 1. Embedded profiles matrix (`base`, `code`, `audit`; `"workspace"` & `"in_place"` removed).
    for removed in ["workspace", "in_place"] {
        assert!(
            embedded_profile(removed).is_none(),
            "removed embedded profile '{removed}' must not exist"
        );
        let err = resolve_profile_ref_with_dirs(removed, None, None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{removed}");
    }

    type EmbeddedCase<'a> = (&'a str, Option<&'a SubsystemRegistry>, bool, Result<(), ErrorCode>);
    let cases: &[EmbeddedCase<'_>] = &[
        ("base", Some(&code_docs_disabled), false, Ok(())),
        ("base", None, false, Ok(())),
        ("base", None, true, Ok(())),
        ("code", None, false, Ok(())),
        ("code", None, true, Err(ErrorCode::CommandUnbound)),
        (
            "code",
            Some(&code_docs_disabled),
            false,
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
        ("audit", None, false, Ok(())),
        ("audit", None, true, Ok(())),
        ("audit", Some(&code_docs_disabled), false, Ok(())),
    ];

    for &(name, mods, require_bound, expected) in cases {
        let resolved =
            resolve_profile_ref_with_dirs(name, None, mods, None).expect("embedded resolves");
        let opts = ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: mods,
            require_bound_commands: require_bound,
        };
        let res = validate_profile(&resolved, &opts);
        match expected {
            Ok(()) => assert!(res.is_ok(), "{name} (require_bound={require_bound}): {res:?}"),
            Err(code) => {
                let err = res.expect_err("expected error");
                assert_eq!(err.code, code, "{name}: {err}");
            }
        }
    }

    let base_p = resolve_profile_ref_with_dirs("base", None, None, None).unwrap();
    assert_eq!(base_p.default_tools.len(), 7);
    assert!(base_p.gates.is_empty());

    let code_p = resolve_profile_ref_with_dirs("code", None, None, None).unwrap();
    assert!(!code_p.roles["independent_reviewer"].reuse_on_reject);
    assert!(code_p.roles["builder"].reuse_on_reject);
    assert_eq!(code_p.default_tools.len(), 19);
    assert!(code_p.tags.contains("docs"));
    assert!(code_p.roles["independent_reviewer"].guidance.as_deref().is_some_and(|g| g.contains("fresh") && g.contains("distinct")));
    assert!(match &code_p.guidance["task.review"] {
        GuidanceEntry::Structured(s) => s.criteria.iter().any(|c| c.contains("duplicates") && c.contains("dead_tails")),
        GuidanceEntry::Text(t) => t.contains("duplicates") && t.contains("dead_tails"),
    });
    let code_tasks = code_p
        .gates
        .iter()
        .find_map(|g| g.tasks.as_ref())
        .expect("tasks block");
    assert!(!code_tasks.gates.iter().any(|g| g.id == "contract" || g.id == "contract_review"));
    let code_build = code_tasks.gates.iter().find(|g| g.id == "build").unwrap();
    let code_review = code_tasks.gates.iter().find(|g| g.id == "review").unwrap();
    assert_eq!(code_build.save.post, Some(SaveAction::Snapshot));
    assert_eq!(code_build.pass.as_ref().map(|g| g.as_str()), Some("review"));
    assert_eq!(code_review.pass.as_ref().map(|g| g.as_str()), Some("deliver"));
    assert_eq!(code_review.fail_drift.as_ref().map(|g| g.as_str()), Some("rearchitect"));
    assert_eq!(code_build.checks.len(), 1);
    assert_eq!(code_build.checks[0].check, "code.seam");
    assert!(code_build.tools.is_none(), "inherited tools must stay deduplicated as None");
    assert_eq!(code_p.effective_tools(code_build).len(), 19);
    assert_eq!(code_build.context.first().map(String::as_str), Some("docs.instructions"));
    let code_rearch = code_tasks.gates.iter().find(|g| g.id == "rearchitect").unwrap();
    assert_eq!(code_rearch.pass.as_ref().map(|g| g.as_str()), Some("build"));
    assert_eq!(code_rearch.max_attempts, 3);
    assert_eq!(
        code_rearch.context,
        vec!["docs.instructions", "docs.adrs", "docs.invariants", "code.symbols"]
    );
    let plan = code_p.dispatch_plan();
    let plan_cursors: Vec<&str> = plan.iter().map(|g| g.cursor.as_str()).collect();
    assert_eq!(
        plan_cursors,
        vec![
            "blueprint_synthesis.synthesize",
            "blueprint_synthesis.review",
            "tasks.build",
            "tasks.review",
            "tasks.deliver",
            "tasks.rearchitect",
            "run_tests",
            "polish",
            "integration_review",
            "handoff",
        ]
    );
    assert_eq!(plan[2].role.as_deref(), Some("builder"));
    assert_eq!(plan[2].mode, Some(RoleExecutionMode::Subagent));
    assert_eq!(plan[2].reuse_on_reject, Some(true));
    assert_eq!(plan[6].role, None);
    assert_eq!(plan[6].command.as_deref(), Some("verify_all"));
    assert!(plan.iter().all(|g| g.guidance.is_none()), "dispatch_plan at start must omit per-role guidance: {plan:?}");

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo_p = resolve_profile_ref_with_dirs(".acdd/profile.yaml", Some(&repo_root), None, None).unwrap();
    let repo_plan = repo_p.dispatch_plan();
    let handoff = repo_plan.iter().find(|g| g.cursor == "handoff").unwrap();
    assert_eq!(handoff.role.as_deref(), Some("coordinator"));
    assert_eq!(handoff.mode, Some(RoleExecutionMode::Inline));
    assert_eq!(handoff.models, repo_p.roles["coordinator"].models);
    let build = repo_plan.iter().find(|g| g.cursor == "tasks.build").unwrap();
    assert_eq!(build.mode, Some(RoleExecutionMode::Subagent));
    assert_eq!(build.models, repo_p.roles["builder"].models);
    let custom_models = resolve_profile_from_str_with_dirs(
        "version: 2\ninherit: code\nroles:\n  builder:\n    models: ['model-a', 'model-b:high']\n",
        "custom",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let custom_build = custom_models
        .dispatch_plan()
        .into_iter()
        .find(|g| g.cursor == "tasks.build")
        .unwrap();
    assert_eq!(
        custom_build.models.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["model-a", "model-b:high"]
    );

    let audit_p = resolve_profile_ref_with_dirs("audit", None, None, None).unwrap();
    assert_eq!(audit_p.git.isolation, IsolationMode::InPlace);
    assert_eq!(audit_p.git.worktrees, WorktreeLifecycle::Manual);
    assert_eq!(audit_p.save, SaveAction::None);
    assert_eq!(audit_p.default_tools.len(), 19);
    let audit_gate_ids: Vec<&str> = audit_p.gates.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(audit_gate_ids, vec!["recon", "analyze", "triage"]);
    let triage = audit_p.gates.iter().find(|g| g.id == "triage").unwrap();
    assert!(triage.receipt);
    assert_eq!(
        triage.save.report.as_deref(),
        Some(".acdd/reports/{run_name}.md")
    );

    let code_deliver = code_tasks.gates.iter().find(|g| g.id == "deliver").unwrap();
    assert!(code_deliver.save.immutable);
    assert_eq!(code_deliver.save.post, Some(SaveAction::Commit));

    let code_handoff = code_p.gates.iter().find(|g| g.id == "handoff").unwrap();
    assert!(code_handoff.save.immutable);
    assert!(code_handoff.save.archive);
    assert!(code_handoff.save.prune);
}

#[test]
fn profile_schema_and_legacy_rejection_matrix() {
    let providers = code_and_docs_providers();
    let cases: &[(&str, &str, ErrorCode)] = &[
        ("wrong_version", "version: 1\n", ErrorCode::ProfileSchemaInvalid),
        (
            "legacy_persistence",
            "version: 2\npersistence: commit\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_policies",
            "version: 2\npolicies:\n  review:\n    independent_worker: true\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_agents",
            "version: 2\nagents:\n  builder:\n    mode: subagent\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_git_commit_note_singular",
            "version: 2\ngit_commit_note:\n  format: markdown\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_ast_delta",
            "version: 2\ngit_commit_notes:\n  default:\n    sections: [ast_delta]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_polish_scope",
            "version: 2\npolish_scope: [docs/**]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_gate_on_fail",
            "version: 2\ngates:\n  - id: a\n    on_fail: a\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_gate_steps",
            "version: 2\ngates:\n  - id: a\n    steps: [do something]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_gate_loop",
            "version: 2\ngates:\n  - id: a\n    loop:\n      scope: build\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_output_errors",
            "version: 2\ncommands:\n  t:\n    command: cargo test\n    output:\n      errors: ['^err']\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_proof_seam_test",
            "version: 2\ngates:\n  - id: a\n    proof: seam_test\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_proof_delivery",
            "version: 2\ngates:\n  - id: a\n    proof: delivery\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_tool_task_claim",
            "version: 2\ndefault_tools: [task_claim]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_tool_session_checkpoint",
            "version: 2\ndefault_tools: [session_checkpoint]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_tool_forge_guide",
            "version: 2\ndefault_tools: [forge_guide]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "when_on_root_gate",
            "version: 2\ntags: [code]\ngates:\n  - id: verify\n    when: code\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "archive_on_task_gate",
            "version: 2\ngates:\n  - tasks:\n      gates:\n        - id: build\n          save:\n            archive: true\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_pre_commit",
            "version: 2\ngates:\n  - id: verify\n    save:\n      pre: commit\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "terminal_root_gate_with_pass",
            "version: 2\ngates:\n  - id: a\n    pass: b\n  - id: b\n    pass: a\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "self_loop_pass",
            "version: 2\ngates:\n  - id: a\n    pass: a\n  - id: b\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "pass_equals_fail",
            "version: 2\ngates:\n  - id: a\n    pass: b\n    fail: b\n  - id: b\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "receipt_without_need",
            "version: 2\ngates:\n  - id: handoff\n    receipt: true\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "legacy_proof_contract",
            "version: 2\ncommands:\n  t: cargo test\ngates:\n  - id: c\n    command: t\n    proof: contract\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "removed_expected_failure",
            "version: 2\ncommands:\n  t: cargo test\ngates:\n  - id: b\n    command: t\n    proof: command\n    expected_failure:\n      exit_codes: [1]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "removed_command_output",
            "version: 2\ncommands:\n  t:\n    command: cargo test\n    output:\n      require_counts: false\ngates:\n  - id: b\n    command: t\n    proof: command\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "review_with_command",
            "version: 2\nroles:\n  rev:\n    mode: subagent\ncontours:\n  paths:\n    description: p\n    criteria: [c]\ncommands:\n  t: cargo test\ngates:\n  - id: r\n    role: rev\n    command: t\n    proof: review\n    contours: [paths]\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "review_without_contours",
            "version: 2\nroles:\n  rev:\n    mode: subagent\ngates:\n  - id: r\n    role: rev\n    proof: review\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "unknown_guidance_slot",
            "version: 2\ngates:\n  - id: a\n    guidance: [missing.slot]\n",
            ErrorCode::UnknownGuidanceSlot,
        ),
        (
            "legacy_gate_save_pre",
            "version: 2\ngates:\n  - id: a\n    save:\n      pre: none\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "fail_drift_unknown",
            "version: 2\ngates:\n  - id: a\n    fail_drift: missing\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "fail_drift_equals_pass",
            "version: 2\ngates:\n  - id: a\n    pass: b\n    fail_drift: b\n  - id: b\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "gates_merge_removed",
            "version: 2\ninherit: code\ngates_merge: patch\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "gates_and_gates_patch_mutually_exclusive",
            "version: 2\ninherit: code\ngates:\n  - id: a\ngates_patch:\n  - id: b\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "gates_patch_requires_inherit",
            "version: 2\ngates_patch:\n  - id: a\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "gates_patch_forbids_inherit_none",
            "version: 2\ninherit: none\ngates_patch:\n  - id: a\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_traversal",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: ../escape.md\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_absolute",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: /abs.md\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_unknown_var",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".acdd/reports/{unknown_var}.md\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_empty",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \"   \"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_backslash",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \"reports\\\\out.md\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_state_sqlite",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".acdd/state.sqlite\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_code_map_sqlite",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".acdd/code-map.sqlite\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_apfs_acdd_state",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".ACDD/state.sqlite\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_apfs_acdd_reports_case",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".ACDD/reports/{run_id}.md\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_git_config",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".git/config\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_apfs_git_head",
            "version: 2\ngates:\n  - id: a\n    save:\n      report: \".GIT/HEAD\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
        (
            "save_report_protected_cargo_toml",
            "version: 2\nprotected_paths: [Cargo.toml]\ngates:\n  - id: a\n    save:\n      report: \"CARGO.TOML\"\n",
            ErrorCode::ProfileSchemaInvalid,
        ),
    ];

    let opts = ProfileValidationOptions {
        domain_providers: Some(&providers),
        acdd_modules: None,
        require_bound_commands: false,
    };

    for &(label, yaml, expected_code) in cases {
        let err = resolve_profile_from_str_with_dirs(yaml, label, None, None, None, None)
            .and_then(|p| validate_profile(&p, &opts))
            .expect_err("expected validation failure");
        assert_eq!(err.code, expected_code, "case '{label}': {err}");
    }

    let cases = [
        ("default", "save: {action}\ngates:\n  - id: root", false),
        ("root", "gates:\n  - id: root\n    save: {action}", false),
        ("container", "gates:\n  - id: c\n    save: {action}\n    gates:\n      - id: leaf", false),
        ("container child", "gates:\n  - id: c\n    gates:\n      - id: leaf\n        save: {action}", false),
        ("tasks block", "gates:\n  - save: {action}\n    tasks:\n      gates:\n        - id: b", false),
        ("task container", "gates:\n  - tasks:\n      gates:\n        - id: c\n          save: {action}\n          gates:\n            - id: b", false),
        ("task gate", "gates:\n  - tasks:\n      gates:\n        - id: b\n          save: {action}", true),
    ];
    for (label, body, supported) in cases {
        for action in ["snapshot", "{ post: snapshot }"] {
            let yaml = format!("version: 2\ninherit: none\n{}", body.replace("{action}", action));
            let checked = resolve_profile_from_str_with_dirs(&yaml, label, None, None, None, None)
                .and_then(|p| validate_profile(&p, &opts));
            assert_eq!(checked.err().map(|err| err.code), (!supported).then_some(ErrorCode::ProfileSchemaInvalid), "{label}: {action}");
        }
    }
}

#[test]
fn inherit_pin_cycle_and_patch_matrix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join(".acdd/profiles")).unwrap();

    let base_bytes = br#"version: 2
inherit: code
tags: [default, code, docs]
roles:
  extra_role:
    mode: inline
    reuse_on_reject: true
commands:
  test: "cargo test {test_filter}"
  build_check: "cargo clippy -- -D warnings && cargo test"
  verify_all: "cargo test --workspace"
  extra_cmd: "echo hi"
guidance:
  extra.slot: "Extra guidance"
gates:
  - id: blueprint_synthesis
    gates:
      - id: synthesize
        role: architect
        proof: blueprint
        guidance: [milestone.synthesis]
        pass: review
      - id: review
        role: independent_reviewer
        proof: review
        contours: [claims]
        independent_from: [synthesize]
        guidance: [milestone.blueprint_review]
        save: { immutable: true }
        fail: synthesize
  - tasks:
      when: "code & !docs"
      gates:
        - id: build
          role: builder
          command: build_check
          proof: command
          tools_add: [code_map_search]
          tools_remove: [acdd_blackboard]
          save: { post: commit, immutable: true }
          pass: review
        - id: review
          role: independent_reviewer
          proof: review
          contours: [claims]
          independent_from: [build]
          pass: deliver
          fail: build
        - id: deliver
          role: independent_reviewer
          receipt: true
          need: [build, review]
          independent_from: [build]
          save: { post: commit, immutable: true }
  - id: verify
    command: verify_all
    proof: command
    pass: handoff
  - id: handoff
    receipt: true
    need: [verify]
    save: { post: commit, archive: true, prune: true }
"#;
    let base_rel = ".acdd/profiles/base.yaml";
    fs::write(root.join(base_rel), base_bytes).unwrap();
    let base_sha = acdd_core::format::sha256_hex(base_bytes);
    let short_pin = &base_sha[..12];

    // Cycle fixture: a.yaml -> b.yaml -> a.yaml
    fs::write(
        root.join(".acdd/profiles/cyc_a.yaml"),
        "version: 2\ninherit: .acdd/profiles/cyc_b.yaml\n",
    )
    .unwrap();
    fs::write(
        root.join(".acdd/profiles/cyc_b.yaml"),
        "version: 2\ninherit: .acdd/profiles/cyc_a.yaml\n",
    )
    .unwrap();
    // Self-inheritance on .acdd/profile.yaml
    fs::write(
        root.join(".acdd/profile.yaml"),
        "version: 2\ninherit: .acdd/profile.yaml\n",
    )
    .unwrap();

    // Template fixture
    let template_yaml = r#"version: 2
inherit: code
gate_templates:
  pair:
    guidance: [task.code]
    max_attempts: 2
    gates:
      - id: step1
        role: builder
        command: build_check
        proof: command
        pass: step2
      - id: step2
        role: independent_reviewer
        proof: review
        contours: [paths]
        independent_from: [step1]
commands:
  build_check: "cargo test"
gates:
  - id: container
    use: pair
  - id: finish
    receipt: true
    need: [container]
"#;

    let providers = code_and_docs_providers();
    let opts = ProfileValidationOptions {
        domain_providers: Some(&providers),
        acdd_modules: None,
        require_bound_commands: true,
    };

    // Valid pinned inherit + null deletion + field-wise patch + cursor + structural when + after/before + delete
    let valid_patch = format!(
        r#"version: 2
inherit: "{base_rel}:{short_pin}"
roles:
  extra_role: null
commands:
  extra_cmd: null
guidance:
  extra.slot: null
gates_patch:
  - id: "blueprint_synthesis.review"
    guidance:
      - inline: "Custom inline review guidance"
  - id: "tasks.build"
    save:
      post: none
  - tasks:
      when: "code+!docs"
      gates:
        - id: build_audit
          after: build
          role: independent_reviewer
          proof: review
          contours: [paths]
          independent_from: [build]
          pass: review
          fail: build
  - id: lint_gate
    before: handoff
    command: build_check
    proof: command
    pass: handoff
  - id: handoff
    need: [verify, lint_gate]
"#
    );

    let patched =
        resolve_profile_from_str_with_dirs(&valid_patch, "patched", None, Some(root), None, None)
            .unwrap();
    validate_profile(&patched, &opts).unwrap();
    assert!(!patched.roles.contains_key("extra_role"));
    assert!(!patched.commands.contains_key("extra_cmd"));
    assert!(!patched.guidance.contains_key("extra.slot"));

    let tasks_block = patched.gates.iter().find_map(|g| g.tasks.as_ref()).unwrap();
    assert_eq!(tasks_block.gates.len(), 4);
    assert_eq!(tasks_block.gates[1].id, "build_audit");
    assert!(patched
        .effective_tools(&tasks_block.gates[0])
        .contains(&"code_map_search".to_owned()));
    assert!(!patched
        .effective_tools(&tasks_block.gates[0])
        .contains(&"acdd_blackboard".to_owned()));
    // Field-wise save patch kept immutable while updating post to none:
    assert_eq!(tasks_block.gates[0].save.post, Some(SaveAction::None));
    assert!(tasks_block.gates[0].save.immutable);

    let lint_pos = patched.gates.iter().position(|g| g.id == "lint_gate").unwrap();
    let handoff_pos = patched.gates.iter().position(|g| g.id == "handoff").unwrap();
    assert_eq!(lint_pos + 1, handoff_pos);

    // Template expansion check
    let tpl_prof =
        resolve_profile_from_str_with_dirs(template_yaml, "tpl", None, None, None, None)
            .unwrap();
    validate_profile(&tpl_prof, &opts).unwrap();
    assert_eq!(tpl_prof.gates[0].gates.len(), 2);
    assert_eq!(tpl_prof.gates[0].gates[0].max_attempts, 2);

    // Failure cases table
    let bad_cases: &[(&str, &str, Option<&str>)] = &[
        (
            "pin_mismatch",
            "version: 2\ninherit: \".acdd/profiles/base.yaml:deadbeef0000\"\n",
            None,
        ),
        (
            "pin_too_short",
            "version: 2\ninherit: \".acdd/profiles/base.yaml:dead\"\n",
            None,
        ),
        (
            "cycle_detection",
            "version: 2\ninherit: .acdd/profiles/cyc_a.yaml\n",
            None,
        ),
        (
            "path_traversal",
            "version: 2\ninherit: ../outside.yaml\n",
            None,
        ),
        (
            "workspace_self_inherit",
            "version: 2\ninherit: .acdd/profile.yaml\n",
            Some(".acdd/profile.yaml"),
        ),
        (
            "patch_missing_target",
            "version: 2\ninherit: code\ngates_patch:\n  - id: nonexistent\n    max_attempts: 2\n",
            None,
        ),
        (
            "patch_missing_after_anchor",
            "version: 2\ninherit: code\ngates_patch:\n  - id: extra\n    after: missing_anchor\n",
            None,
        ),
        (
            "patch_missing_delete_target",
            "version: 2\ninherit: code\ngates_patch:\n  - missing_gate: null\n",
            None,
        ),
        (
            "patch_unmatched_tasks_when",
            "version: 2\ninherit: code\ngates_patch:\n  - tasks:\n      when: code\n      gates: []\n",
            None,
        ),
    ];

    for &(label, yaml, rel_path) in bad_cases {
        let err =
            resolve_profile_from_str_with_dirs(yaml, label, rel_path, Some(root), None, None)
                .expect_err("expected failure");
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{label}: {err}");
    }
}

#[test]
fn when_expression_parser_and_eval_matrix() {
    let allowed = BTreeSet::from([
        "code".to_owned(),
        "docs".to_owned(),
        "db_migration".to_owned(),
    ]);

    let eval_cases: &[(&str, &[&str], bool, &str)] = &[
        ("code", &["code"], true, "code"),
        ("code", &["docs"], false, "code"),
        ("code", &["default"], false, "code"),
        ("default", &["default"], true, "default"),
        ("!docs", &["code"], true, "!docs"),
        ("!docs", &["docs"], false, "!docs"),
        ("code + !docs", &["code"], true, "code & !docs"),
        ("code & !docs", &["code", "docs"], false, "code & !docs"),
        ("code + !docs", &["code", "docs"], false, "code & !docs"),
        (
            "(code | docs) & !db_migration",
            &["docs"],
            true,
            "(code | docs) & !db_migration",
        ),
        (
            "(code | docs) && !db_migration",
            &["docs", "db_migration"],
            false,
            "(code | docs) & !db_migration",
        ),
        ("default || code", &["default"], true, "default | code"),
    ];

    for &(raw, tags, expect_eval, expect_norm) in eval_cases {
        let expr = WhenExpr::parse(raw).expect("valid when");
        expr.validate_tags(&allowed, "test").expect("known tags");
        let tag_vec: Vec<String> = tags.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(expr.eval(&tag_vec), expect_eval, "eval {raw} on {tags:?}");
        assert_eq!(expr.normalized(), expect_norm, "norm {raw}");
    }

    let bad_cases: &[(&str, bool)] = &[
        ("", false),
        ("(code", false),
        ("code)", false),
        ("& code", false),
        ("code &", false),
        ("unknown_tag", true),
    ];

    for &(raw, parses_ok) in bad_cases {
        let parsed = WhenExpr::parse(raw);
        if parses_ok {
            let expr = parsed.expect("should parse");
            let err = expr.validate_tags(&allowed, "test").expect_err("unknown tag");
            assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        } else {
            let err = parsed.expect_err("should fail parse");
            assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
        }
    }
}

#[test]
fn command_pattern_test_filter_and_budget_matrix() {
    // Pattern specificity & YAML order tie-breaking
    let cmd = CommandDef::Pattern(vec![
        PatternCommandEntry {
            pattern: "**/*.rs".to_owned(),
            command: "cargo test --workspace".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::new(),
        },
        PatternCommandEntry {
            pattern: "crates/acdd-state/**/*.rs".to_owned(),
            command: "cargo test -p acdd-state".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: Some(TestFilterConfig::Template("-- {test}".to_owned())),
            scope_args_by_pattern: BTreeMap::new(),
        },
        PatternCommandEntry {
            pattern: "crates/*/*.rs".to_owned(),
            command: "cargo test --first".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::new(),
        },
        PatternCommandEntry {
            pattern: "crates/*/*.rs".to_owned(),
            command: "cargo test --second".to_owned(),
            description: None,
            timeout_seconds: None,
            test_filter: None,
            scope_args_by_pattern: BTreeMap::new(),
        },
    ]);

    let resolved = resolve_command_for_scope(
        &cmd,
        &[
            "src/lib.rs".to_owned(),
            "crates/acdd-state/src/spec.rs".to_owned(),
            "crates/acdd-state/src/profile/mod.rs".to_owned(),
            "crates/foo/bar.rs".to_owned(),
        ],
    )
    .unwrap();
    // Lexicographically: crates/acdd-state/src/profile/mod.rs, crates/acdd-state/src/spec.rs -> acdd-state
    // crates/foo/bar.rs -> tie between two `crates/*/*.rs` patterns -> first in YAML wins
    // src/lib.rs -> **/*.rs
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0].command, "cargo test -p acdd-state");
    assert_eq!(resolved[1].command, "cargo test --first");
    assert_eq!(resolved[2].command, "cargo test --workspace");

    // Test filter rendering & POSIX single-quote escaping
    type FilterCase<'a> = (
        &'a str,
        Option<TestFilterConfig>,
        Option<&'a str>,
        Result<&'a str, ErrorCode>,
    );
    let filter_cases: &[FilterCase<'_>] = &[
        (
            "cargo clippy && cargo test {test_filter}",
            None,
            None,
            Ok("cargo clippy && cargo test"),
        ),
        (
            "cargo clippy && cargo test {test_filter}",
            Some(TestFilterConfig::Template("-- {test}".to_owned())),
            Some("it's::test"),
            Ok("cargo clippy && cargo test -- 'it'\\''s::test'"),
        ),
        (
            "cargo test",
            None,
            Some("my_test"),
            Ok("cargo test 'my_test'"),
        ),
        (
            "cargo clippy && cargo test",
            None,
            Some("my_test"),
            Err(ErrorCode::CommandTestFilterPlaceholderRequired),
        ),
        (
            "cargo test {test_filter} && echo {test_filter}",
            None,
            Some("my_test"),
            Err(ErrorCode::CommandTestFilterUnsupported),
        ),
        (
            "cargo test",
            Some(TestFilterConfig::Enabled(false)),
            Some("my_test"),
            Err(ErrorCode::CommandTestFilterUnsupported),
        ),
        (
            "cargo test '{test_filter}'",
            None,
            Some("my_test"),
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            "cargo test \"{scope_target}\" {test_filter}",
            None,
            Some("my_test"),
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            "cargo test \\{test_filter}",
            None,
            Some("my_test"),
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            "cargo test 'unclosed {test_filter}",
            None,
            Some("my_test"),
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            "cargo test {test_filter}",
            Some(TestFilterConfig::Template("--filter '{test}'".to_owned())),
            Some("my_test"),
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            "true # don't change",
            None,
            None,
            Ok("true # don't change"),
        ),
        (
            "cargo test # don't append",
            None,
            Some("my_test"),
            Err(ErrorCode::CommandTestFilterPlaceholderRequired),
        ),
        (
            "cargo test # trailing newline comment\n",
            None,
            Some("my_test"),
            Err(ErrorCode::CommandTestFilterPlaceholderRequired),
        ),
        (
            "cargo test {test_filter} # safe comment with '",
            Some(TestFilterConfig::Template("--filter {test}".to_owned())),
            Some("my_test"),
            Ok("cargo test --filter 'my_test' # safe comment with '"),
        ),
    ];

    assert!(
        render_command_with_test_filter("true # a && b; |", None, Some("my_test"))
            .unwrap_err()
            .message
            .contains("commented command")
    );
    assert!(
        render_command_with_test_filter("true; # comment", None, Some("my_test"))
            .unwrap_err()
            .message
            .contains("chained command")
    );

    for (tpl, cfg, target, expected) in filter_cases {
        let res = render_command_with_test_filter(tpl, cfg.as_ref(), *target);
        match expected {
            Ok(exp) => assert_eq!(res.unwrap(), *exp),
            Err(code) => assert_eq!(res.unwrap_err().code, *code),
        }
    }

    // Guidance budget enforcement
    let limits = ResourceLimits {
        max_guidance_bytes: 100,
        ..ResourceLimits::default()
    };
    assert_eq!(check_guidance_budget(40, 50, &limits, 200).unwrap(), 10);
    assert_eq!(
        check_guidance_budget(40, 50, &limits, 80)
            .unwrap_err()
            .code,
        ErrorCode::GuidanceBudgetExceeded
    );
    assert_eq!(
        check_guidance_budget(60, 50, &limits, 200)
            .unwrap_err()
            .code,
        ErrorCode::GuidanceBudgetExceeded
    );
}

#[test]
fn domain_provider_capability_matrix() {
    let full_providers = code_and_docs_providers();
    let disabled_http = SubsystemRegistry {
        domains: BTreeMap::from([("http".to_owned(), false)]),
        ..SubsystemRegistry::default()
    };

    let cases: &[(&str, Option<&SubsystemRegistry>, Result<(), ErrorCode>)] = &[
        (
            r#"version: 2
roles:
  worker:
    mode: subagent
proofs:
  deploy_proof:
    schema:
      type: object
      required: [status]
      properties:
        status: { type: string }
gates:
  - id: deploy
    role: worker
    call: http.deploy
    with: { url: "https://example.test" }
    checks: [http.health]
    context: [http.endpoints]
    tools: [acdd_read, http_probe]
    proof: deploy_proof
"#,
            None,
            Ok(()),
        ),
        (
            r#"version: 2
gates:
  - id: deploy
    call: http.deploy
    with: { wrong: 1 }
    proof: call
"#,
            None,
            Err(ErrorCode::ProfileSchemaInvalid),
        ),
        (
            r#"version: 2
gates:
  - id: deploy
    call: http.deploy
    with: { url: "https://example.test" }
    proof: call
"#,
            Some(&disabled_http),
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
        (
            r#"version: 2
gates:
  - id: deploy
    tools: [http_probe]
"#,
            Some(&disabled_http),
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
        (
            r#"version: 2
gates:
  - id: deploy
    checks: [unknown.check]
"#,
            None,
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
    ];

    for &(yaml, mods, expected) in cases {
        let resolved =
            resolve_profile_from_str_with_dirs(yaml, "dom", None, None, mods, None).unwrap();
        let opts = ProfileValidationOptions {
            domain_providers: Some(&full_providers),
            acdd_modules: mods,
            require_bound_commands: false,
        };
        let res = validate_profile(&resolved, &opts);
        match expected {
            Ok(()) => assert!(res.is_ok(), "{res:?}"),
            Err(code) => assert_eq!(res.unwrap_err().code, code),
        }
    }
}

#[test]
fn v061_profile_purity_docs_strip_patches_and_scope_args_matrix() {
    let providers = code_and_docs_providers();

    // 1. code profile with docs: false strips get_doc, search_docs, docs.* context/checks and validates.
    let docs_disabled = SubsystemRegistry {
        disabled: BTreeSet::from(["docs".to_owned()]),
        ..SubsystemRegistry::default()
    };
    let code_no_docs =
        resolve_profile_ref_with_dirs("code", None, Some(&docs_disabled), None).unwrap();
    assert!(!code_no_docs.default_tools.iter().any(|t| t == "get_doc" || t == "search_docs"));
    assert!(code_no_docs.tags.contains("docs"));
    validate_profile(
        &code_no_docs,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: Some(&docs_disabled),
            require_bound_commands: false,
        },
    )
    .unwrap();

    // 3. Default resolution via resolve_optional_profile_ref_with_dirs (`.acdd/profile.yaml` -> embedded `code` fallback with disabled hooks stripped).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let default_no_ws = resolve_optional_profile_ref_with_dirs(
        None,
        Some(root),
        Some(&SubsystemRegistry::default()),
        None,
    )
    .unwrap();
    assert_eq!(default_no_ws.profile_id, "code");
    assert_eq!(default_no_ws.git.isolation, IsolationMode::Worktree);
    assert_eq!(default_no_ws.default_tools.len(), 19);

    let default_no_domains = resolve_optional_profile_ref_with_dirs(
        None,
        Some(root),
        Some(&disabled_code_and_docs_modules()),
        None,
    )
    .unwrap();
    validate_profile(
        &default_no_domains,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: Some(&disabled_code_and_docs_modules()),
            require_bound_commands: false,
        },
    )
    .unwrap();

    fs::create_dir_all(root.join(".acdd")).unwrap();
    fs::write(
        root.join(".acdd/profile.yaml"),
        "version: 2\ninherit: code\ncommands:\n  test: \"pytest {test_filter}\"\n  build_check: \"pytest\"\n  verify_all: \"pytest\"\n",
    )
    .unwrap();
    let ws_default = resolve_optional_profile_ref_with_dirs(None, Some(root), None, None).unwrap();
    assert!(matches!(ws_default.commands.get("test"), Some(CommandDef::Script(s)) if s == "pytest {test_filter}"));

    // Audit overlay with gates_patch + valid save.report templates
    let audit_overlay = resolve_profile_from_str_with_dirs(
        "version: 2\ninherit: audit\ngates_patch:\n  - id: analyze\n    save:\n      report: \".acdd/reports/{spec_id}-{run_id}.json\"\n  - id: triage\n    save:\n      report: \".acdd/reports/{run_name}-{gate_id}.md\"\n",
        "audit-overlay",
        None,
        Some(root),
        None,
        None,
    )
    .unwrap();
    validate_profile(
        &audit_overlay,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: true,
        },
    )
    .unwrap();
    assert_eq!(
        audit_overlay.gates.iter().find(|g| g.id == "analyze").unwrap().save.report.as_deref(),
        Some(".acdd/reports/{spec_id}-{run_id}.json")
    );
    assert_eq!(
        audit_overlay.gates.iter().find(|g| g.id == "triage").unwrap().save.report.as_deref(),
        Some(".acdd/reports/{run_name}-{gate_id}.md")
    );

    // 4. profiles_dirs custom lookup in resolve_profile_ref_with_dirs + nested inherit + cycle + broken config propagation.
    fs::write(
        root.join("acdd.yaml"),
        "profiles_dirs:\n  - custom/profiles\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("custom/profiles")).unwrap();
    fs::write(
        root.join("custom/profiles/team.yaml"),
        "version: 2\ninherit: code\ncommands:\n  test: \"go test ./...\"\n",
    )
    .unwrap();
    fs::write(
        root.join("custom/profiles/nested.yaml"),
        "version: 2\ninherit: team\n",
    )
    .unwrap();
    let custom_p = resolve_profile_ref_with_dirs("team", Some(root), None, None).unwrap();
    assert!(matches!(custom_p.commands.get("test"), Some(CommandDef::Script(s)) if s == "go test ./..."));
    let nested_p = resolve_profile_ref_with_dirs("nested", Some(root), None, None).unwrap();
    assert!(matches!(nested_p.commands.get("test"), Some(CommandDef::Script(s)) if s == "go test ./..."));

    fs::write(
        root.join("custom/profiles/cyc-a.yaml"),
        "version: 2\ninherit: cyc-b\n",
    )
    .unwrap();
    fs::write(
        root.join("custom/profiles/cyc-b.yaml"),
        "version: 2\ninherit: cyc-a\n",
    )
    .unwrap();
    let nested_cyc_err =
        resolve_profile_ref_with_dirs("cyc-a", Some(root), None, None).unwrap_err();
    assert_eq!(nested_cyc_err.code, ErrorCode::ProfileSchemaInvalid);

    // Broken acdd.yaml propagates error instead of silently falling back when resolving profiles_dirs.
    let broken_dir = tempfile::tempdir().unwrap();
    fs::write(broken_dir.path().join("acdd.yaml"), "bogus_unknown: true\n").unwrap();
    let broken_cfg_err =
        resolve_profile_ref_with_dirs("team", Some(broken_dir.path()), None, None).unwrap_err();
    assert_eq!(broken_cfg_err.code, ErrorCode::ProfileSchemaInvalid);

    // 4b. protected_paths malformed glob rejection + inherited when-tags preservation on tags: override.
    for bad_prot in ["src/{a", "../escape", "{a,b}/{c,d}/{e,f}/{g,h}/{i,j}/{k,l}/{m,n}.rs"] {
        let p_bad_prot = resolve_profile_from_str_with_dirs(
            &format!("version: 2\nprotected_paths: [\"{bad_prot}\"]\n"),
            "bad-prot",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let err = validate_profile(&p_bad_prot, &ProfileValidationOptions::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{bad_prot}");
    }

    fs::write(
        root.join("custom/profiles/parent-when.yaml"),
        "version: 2\ninherit: code\ntags: [default, docs]\ngates_patch:\n  - id: tasks.build\n    when: \"!docs\"\n",
    )
    .unwrap();
    let child_override_tags = resolve_profile_from_str_with_dirs(
        "version: 2\ninherit: parent-when\ntags: [frontend]\n",
        "child-tags",
        None,
        Some(root),
        None,
        None,
    )
    .unwrap();
    assert!(child_override_tags.tags.contains("docs"));
    assert!(child_override_tags.tags.contains("frontend"));
    validate_profile(
        &child_override_tags,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: false,
        },
    )
    .unwrap();

    let child_typo_tag = resolve_profile_from_str_with_dirs(
        "version: 2\ninherit: code\ntags: [frontend]\ngates_patch:\n  - id: tasks.build\n    when: typo_tag\n",
        "child-typo",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let typo_err = validate_profile(
        &child_typo_tag,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: false,
        },
    )
    .unwrap_err();
    assert_eq!(typo_err.code, ErrorCode::ProfileSchemaInvalid);

    // Undeclared parent `when:` tag typo must NOT be promoted into child `tags:` when child overrides `tags:`.
    fs::write(
        root.join("custom/profiles/parent-typo.yaml"),
        "version: 2\ninherit: code\ntags: [default]\ngates_patch:\n  - id: tasks.build\n    when: parent_undeclared_typo\n",
    )
    .unwrap();
    let child_of_typo = resolve_profile_from_str_with_dirs(
        "version: 2\ninherit: parent-typo\ntags: [frontend]\n",
        "child-of-typo",
        None,
        Some(root),
        None,
        None,
    )
    .unwrap();
    assert!(!child_of_typo.tags.contains("parent_undeclared_typo"));
    let parent_typo_err = validate_profile(
        &child_of_typo,
        &ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: false,
        },
    )
    .unwrap_err();
    assert_eq!(parent_typo_err.code, ErrorCode::ProfileSchemaInvalid);

    // 5. inherit.rs patch bugs: tools: null, tools + tools_add, use_template + inline overrides, scope_args_by_pattern.
    let patch_yaml = r#"version: 2
inherit: code
scope_args_by_pattern:
  "crates/a/**": "-p crate-a"
  "crates/b/**": "-p crate-b"
commands:
  test: "cargo test {scope_target} {test_filter}"
  build_check:
    command: "cargo check {scope_target}"
    scope_args_by_pattern:
      "crates/a/**": "--package override-a"
  plain_cmd: "echo ok"
gate_templates:
  pair:
    command: build_check
    guidance: [task.code]
    max_attempts: 4
    gates:
      - id: s1
        role: builder
        proof: command
        pass: s2
      - id: s2
        role: independent_reviewer
        proof: review
        contours: [paths]
        independent_from: [s1]
gates_patch:
  - id: tasks.review
    tools: null
  - id: tasks.build
    tools: [acdd_read]
    tools_add: [acdd_verify]
  - id: pair_container
    before: run_tests
    use: pair
    max_attempts: 2
    gates:
      - id: s1
        max_attempts: 5
"#;
    let patched =
        resolve_profile_from_str_with_dirs(patch_yaml, "patched", None, None, None, None)
            .unwrap();
    let tb = patched.gates.iter().find_map(|g| g.tasks.as_ref()).unwrap();
    let review_g = tb.gates.iter().find(|g| g.id == "review").unwrap();
    assert_eq!(review_g.tools.as_deref(), Some(&[][..]), "tools: null must clear to Some([])");
    assert!(patched.effective_tools(review_g).is_empty());
    let build_g = tb.gates.iter().find(|g| g.id == "build").unwrap();
    assert_eq!(
        patched.effective_tools(build_g),
        &["acdd_read".to_owned(), "acdd_verify".to_owned()]
    );

    let pair_g = patched.gates.iter().find(|g| g.id == "pair_container").unwrap();
    assert_eq!(pair_g.max_attempts, 2);
    assert_eq!(pair_g.gates.len(), 2);
    assert_eq!(pair_g.gates[0].id, "s1");
    assert_eq!(pair_g.gates[0].max_attempts, 5);
    assert_eq!(pair_g.gates[0].command.as_deref(), Some("build_check"));
    assert_eq!(pair_g.gates[1].id, "s2");

    // Top-level scope_args_by_pattern stays deduplicated on profile; per-command overrides stay sparse.
    assert_eq!(
        patched.scope_args_by_pattern.get("crates/a/**").map(String::as_str),
        Some("-p crate-a")
    );
    assert_eq!(
        patched.scope_args_by_pattern.get("crates/b/**").map(String::as_str),
        Some("-p crate-b")
    );
    assert!(matches!(
        patched.commands.get("test"),
        Some(CommandDef::Script(s)) if s == "cargo test {scope_target} {test_filter}"
    ));
    match patched.commands.get("build_check").unwrap() {
        CommandDef::Detailed(d) => {
            assert_eq!(
                d.scope_args_by_pattern.get("crates/a/**").map(String::as_str),
                Some("--package override-a")
            );
            assert!(!d.scope_args_by_pattern.contains_key("crates/b/**"));
        }
        other => panic!("expected Detailed, got {other:?}"),
    }
    assert!(matches!(patched.commands.get("plain_cmd"), Some(CommandDef::Script(s)) if s == "echo ok"));
}
