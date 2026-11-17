//! Version-2 `GateProfile` parser, inheritance/patch resolver, and fail-closed validator.

pub mod inherit;
pub mod schema;
pub mod validate;
pub mod when;

pub use inherit::{embedded_profile, resolve_profile_from_str, resolve_profile_ref};
pub use schema::{
    parse_profile, CheckRef, CommandCountsSpec, CommandDef, CommandOutputSpec,
    CompileErrorsFilter, CompileErrorsSpec, ContourDef, DetailedCommandDef, ExpectedFailurePatch,
    ExpectedFailureSpec, GateDef, GateGuidanceRef, GateTemplate, GatesMergeMode,
    GitCommitNoteFormat, GitCommitNotePolicy, GitCommitNotePolicyPatch, GitCommitNoteSection,
    GitPolicy, GitPolicyPatch, GuidanceEntry, InlineGuidance, ModelSpec, OutputTestsSpec,
    ParsedProfileFile, PatchField, PatternCommandEntry, ProofCatalogDef, ProofDef, ProofKind,
    RawGateDef, RawGateItem, RawTasksBlock, ResolvedGateProfile, ResourceLimits,
    ResourceLimitsPatch, RoleDef, RoleExecutionMode, RolePatch, SavePolicy, SavePolicyPatch,
    StructuredGuidance, SubsystemRegistryPatch, TasksBlock, TestFilterConfig, DEFAULT_MAX_ATTEMPTS,
};
pub use validate::{
    check_guidance_budget, glob_pattern_matches, glob_specificity, has_unescaped_chain_operator,
    posix_single_quote, render_command_with_test_filter, resolve_and_budget_guidance,
    resolve_command_for_scope, validate_profile, ProfileValidationOptions,
    ResolvedCommandInvocation,
};
pub use when::{normalize_optional_when, WhenExpr};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitMode, IntegrateStrategy, IsolationMode, SaveAction, WorktreeLifecycle};
    use acdd_core::subsystems::SubsystemRegistry;
    use acdd_core::errors::ErrorCode;
    use acdd_core::ports::{
        ActionCapability, CheckCapability, CompositeDomainProvider, DomainActionReq,
        DomainActionResult, DomainCapabilities, DomainCheckOutcome, DomainCheckReq,
        DomainContextReq, DomainProvider, DomainToolDef, DomainToolOutput, DomainToolReq,
    };
    use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::sync::Arc;

    struct MockDomainProvider {
        id: String,
        caps: DomainCapabilities,
    }

    impl MockDomainProvider {
        fn code_and_docs() -> CompositeDomainProvider {
            let code = Self {
                id: "code".to_owned(),
                caps: DomainCapabilities {
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
            };
            let docs = Self {
                id: "docs".to_owned(),
                caps: DomainCapabilities {
                    actions: vec![],
                    checks: vec![CheckCapability {
                        name: "links".to_owned(),
                        input_schema: SchemaDef::object(),
                    }],
                    context_lenses: BTreeSet::from(["adrs".to_owned(), "invariants".to_owned()]),
                    tools: vec![],
                },
            };
            let mut url_schema = SchemaDef::object();
            url_schema
                .properties
                .insert("url".to_owned(), SchemaDef::string());
            url_schema.required = vec!["url".to_owned()];
            url_schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

            let http = Self {
                id: "http".to_owned(),
                caps: DomainCapabilities {
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
            };
            CompositeDomainProvider::new(vec![Arc::new(code), Arc::new(docs), Arc::new(http)])
                .expect("mock providers valid")
        }
    }

    impl DomainProvider for MockDomainProvider {
        fn domain_id(&self) -> &str {
            &self.id
        }

        fn capabilities(&self) -> DomainCapabilities {
            self.caps.clone()
        }

        fn execute_action(
            &self,
            _action: &str,
            _params: &Value,
            _idempotency_key: &str,
            _req: &DomainActionReq,
        ) -> anyhow::Result<DomainActionResult> {
            Ok(DomainActionResult {
                summary: "ok".to_owned(),
                output: json!({}),
            })
        }

        fn run_check(
            &self,
            _check: &str,
            _params: &Value,
            _req: &DomainCheckReq,
        ) -> anyhow::Result<DomainCheckOutcome> {
            Ok(DomainCheckOutcome::Passed {
                summary: "ok".to_owned(),
                execution_digest_salt: None,
            })
        }

        fn provide_context(&self, _lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
            Ok(json!({}))
        }

        fn call_tool(
            &self,
            _tool: &str,
            _args: &Value,
            _req: &DomainToolReq,
        ) -> anyhow::Result<DomainToolOutput> {
            Ok(DomainToolOutput::Json(json!({"ok": true})))
        }
    }

    fn disabled_code_and_docs_modules() -> SubsystemRegistry {
        SubsystemRegistry {
            disabled: BTreeSet::from(["code".to_owned(), "docs".to_owned()]),
            ..SubsystemRegistry::default()
        }
    }

    #[test]
    fn embedded_profiles_resolve_and_validate_matrix() {
        let providers = MockDomainProvider::code_and_docs();
        let code_docs_disabled = disabled_code_and_docs_modules();

        type EmbeddedCase<'a> = (&'a str, Option<&'a SubsystemRegistry>, bool, Result<(), ErrorCode>);
        let cases: &[EmbeddedCase<'_>] = &[
            ("default", Some(&code_docs_disabled), false, Ok(())),
            ("default", None, false, Ok(())),
            ("default", None, true, Err(ErrorCode::CommandUnbound)),
            ("code", None, false, Ok(())),
            ("code", None, true, Err(ErrorCode::CommandUnbound)),
            (
                "code",
                Some(&code_docs_disabled),
                false,
                Err(ErrorCode::DomainCapabilityUnavailable),
            ),
            ("in_place", Some(&code_docs_disabled), false, Ok(())),
            ("in_place", None, true, Err(ErrorCode::CommandUnbound)),
        ];

        for &(name, mods, require_bound, expected) in cases {
            let resolved = resolve_profile_ref(name, None, mods).expect("embedded resolves");
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

        let default_p = resolve_profile_ref("default", None, None).unwrap();
        assert!(!default_p.roles["independent_reviewer"].reuse_on_reject);
        assert!(default_p.roles["builder"].reuse_on_reject);
        assert_eq!(default_p.default_tools.len(), 7);

        let code_p = resolve_profile_ref("code", None, None).unwrap();
        assert_eq!(code_p.default_tools.len(), 19);
        let code_tasks = code_p
            .gates
            .iter()
            .find_map(|g| g.tasks.as_ref())
            .expect("tasks block");
        let contract_gate = code_tasks
            .gates
            .iter()
            .find(|g| g.id == "contract")
            .unwrap();
        assert_eq!(contract_gate.checks.len(), 1);
        assert_eq!(contract_gate.checks[0].check, "code.seam");
        assert_eq!(contract_gate.tools.len(), 19);

        let in_place_p = resolve_profile_ref("in_place", None, None).unwrap();
        assert_eq!(in_place_p.git.isolation, IsolationMode::InPlace);
        assert_eq!(in_place_p.git.worktrees, WorktreeLifecycle::Manual);
        assert_eq!(in_place_p.git.commit_mode, CommitMode::Plumbing);
        assert_eq!(in_place_p.git.integrate, IntegrateStrategy::Manual);
        assert_eq!(in_place_p.save, SaveAction::Snapshot);

        let ip_tasks = in_place_p
            .gates
            .iter()
            .find_map(|g| g.tasks.as_ref())
            .unwrap();
        let deliver_gate = ip_tasks.gates.iter().find(|g| g.id == "deliver").unwrap();
        assert_eq!(deliver_gate.save.post, Some(SaveAction::Snapshot));
        assert!(deliver_gate.save.immutable);
        assert_eq!(deliver_gate.save.git_commit_note.as_deref(), Some("default"));

        let handoff_gate = in_place_p
            .gates
            .iter()
            .find(|g| g.id == "handoff")
            .unwrap();
        assert_eq!(handoff_gate.save.post, Some(SaveAction::Snapshot));
        assert!(handoff_gate.save.immutable);
        assert!(handoff_gate.save.archive);
        assert!(handoff_gate.save.prune);
        assert_eq!(handoff_gate.save.git_commit_note.as_deref(), Some("default"));
    }

    #[test]
    fn profile_schema_and_legacy_rejection_matrix() {
        let providers = MockDomainProvider::code_and_docs();
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
                "contract_missing_expected_failure",
                "version: 2\ncommands:\n  t: cargo test\ngates:\n  - id: c\n    command: t\n    proof: contract\n",
                ErrorCode::ProfileSchemaInvalid,
            ),
            (
                "expected_failure_on_command_proof",
                "version: 2\ncommands:\n  t: cargo test\ngates:\n  - id: b\n    command: t\n    proof: command\n    expected_failure:\n      exit_codes: [1]\n",
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
                "chained_contract_command_missing_placeholder",
                "version: 2\ncommands:\n  t: cargo clippy && cargo test\ngates:\n  - tasks:\n      gates:\n        - id: contract\n          command: t\n          proof: contract\n          expected_failure:\n            exit_codes: [101]\n",
                ErrorCode::CommandTestFilterPlaceholderRequired,
            ),
        ];

        let opts = ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: false,
        };

        for &(label, yaml, expected_code) in cases {
            let err = resolve_profile_from_str(yaml, label, None, None, None)
                .and_then(|p| validate_profile(&p, &opts))
                .expect_err("expected validation failure");
            assert_eq!(err.code, expected_code, "case '{label}': {err}");
        }
    }

    #[test]
    fn inherit_pin_cycle_and_patch_matrix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join(".acdd/profiles")).unwrap();

        let base_bytes = br#"version: 2
inherit: default
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
        - id: contract
          role: contract_author
          command: test
          proof: contract
          expected_failure: { exit_codes: [1, 101] }
          tools_add: [code_map_search]
          tools_remove: [acdd_blackboard]
          pass: build
        - id: build
          role: builder
          command: build_check
          proof: command
          save: { pre: snapshot, post: commit, immutable: true, git_commit_note: default }
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
            "version: 2\ninherit: workspace\n",
        )
        .unwrap();

        // Template fixture
        let template_yaml = r#"version: 2
inherit: default
gates_merge: replace
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

        let providers = MockDomainProvider::code_and_docs();
        let opts = ProfileValidationOptions {
            domain_providers: Some(&providers),
            acdd_modules: None,
            require_bound_commands: true,
        };

        // Valid pinned inherit + null deletion + field-wise patch + cursor + structural when + after/before + delete
        let valid_patch = format!(
            r#"version: 2
inherit: "{base_rel}:{short_pin}"
gates_merge: patch
roles:
  extra_role: null
commands:
  extra_cmd: null
guidance:
  extra.slot: null
gates:
  - id: "blueprint_synthesis.review"
    guidance:
      - inline: "Custom inline review guidance"
  - id: "tasks.build"
    save:
      post: snapshot
  - tasks:
      when: "code+!docs"
      gates:
        - id: contract_check
          after: contract
          role: independent_reviewer
          proof: review
          contours: [paths]
          independent_from: [contract]
          pass: build
          fail: contract
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
            resolve_profile_from_str(&valid_patch, "patched", None, Some(root), None).unwrap();
        validate_profile(&patched, &opts).unwrap();
        assert!(!patched.roles.contains_key("extra_role"));
        assert!(!patched.commands.contains_key("extra_cmd"));
        assert!(!patched.guidance.contains_key("extra.slot"));

        let tasks_block = patched.gates.iter().find_map(|g| g.tasks.as_ref()).unwrap();
        assert_eq!(tasks_block.gates.len(), 3);
        assert_eq!(tasks_block.gates[1].id, "contract_check");
        assert!(tasks_block.gates[0]
            .tools
            .contains(&"code_map_search".to_owned()));
        assert!(!tasks_block.gates[0]
            .tools
            .contains(&"acdd_blackboard".to_owned()));
        // Field-wise save patch kept immutable & git_commit_note & pre:
        assert_eq!(tasks_block.gates[2].save.post, Some(SaveAction::Snapshot));
        assert_eq!(tasks_block.gates[2].save.pre, Some(SaveAction::Snapshot));
        assert!(tasks_block.gates[2].save.immutable);
        assert_eq!(
            tasks_block.gates[2].save.git_commit_note.as_deref(),
            Some("default")
        );

        let lint_pos = patched.gates.iter().position(|g| g.id == "lint_gate").unwrap();
        let handoff_pos = patched.gates.iter().position(|g| g.id == "handoff").unwrap();
        assert_eq!(lint_pos + 1, handoff_pos);

        // Template expansion check
        let tpl_prof = resolve_profile_from_str(template_yaml, "tpl", None, None, None).unwrap();
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
                "version: 2\ninherit: workspace\n",
                Some(".acdd/profile.yaml"),
            ),
            (
                "patch_missing_target",
                "version: 2\ninherit: default\ngates_merge: patch\ngates:\n  - id: nonexistent\n    max_attempts: 2\n",
                None,
            ),
            (
                "patch_missing_after_anchor",
                "version: 2\ninherit: default\ngates_merge: patch\ngates:\n  - id: extra\n    after: missing_anchor\n",
                None,
            ),
            (
                "patch_missing_delete_target",
                "version: 2\ninherit: default\ngates_merge: patch\ngates:\n  - missing_gate: null\n",
                None,
            ),
            (
                "patch_unmatched_tasks_when",
                "version: 2\ninherit: default\ngates_merge: patch\ngates:\n  - tasks:\n      when: code\n      gates: []\n",
                None,
            ),
        ];

        for &(label, yaml, rel_path) in bad_cases {
            let err = resolve_profile_from_str(yaml, label, rel_path, Some(root), None)
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
            ("code", &["default"], false, "code"),
            ("!docs", &["code"], true, "!docs"),
            ("!docs", &["docs"], false, "!docs"),
            ("code + !docs", &["code"], true, "code & !docs"),
            ("code & !docs", &["code", "docs"], false, "code & !docs"),
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
                output: None,
            },
            PatternCommandEntry {
                pattern: "crates/acdd-state/**/*.rs".to_owned(),
                command: "cargo test -p acdd-state".to_owned(),
                description: None,
                timeout_seconds: None,
                test_filter: Some(TestFilterConfig::Template("-- {test}".to_owned())),
                scope_args_by_pattern: BTreeMap::new(),
                output: None,
            },
            PatternCommandEntry {
                pattern: "crates/*/*.rs".to_owned(),
                command: "cargo test --first".to_owned(),
                description: None,
                timeout_seconds: None,
                test_filter: None,
                scope_args_by_pattern: BTreeMap::new(),
                output: None,
            },
            PatternCommandEntry {
                pattern: "crates/*/*.rs".to_owned(),
                command: "cargo test --second".to_owned(),
                description: None,
                timeout_seconds: None,
                test_filter: None,
                scope_args_by_pattern: BTreeMap::new(),
                output: None,
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
        ];

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
        let full_providers = MockDomainProvider::code_and_docs();
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
            let resolved = resolve_profile_from_str(yaml, "dom", None, None, mods).unwrap();
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
}
