//! Host `CodeDomainProvider` (`domain_id = "code"`) implementation.

mod common;
mod context;
mod lint;
mod seam;
mod signature_scope;

pub use common::CodeDomainProvider;
pub(crate) use lint::ensure_fresh_worktree_index;

use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ports::{
    CheckCapability, ContextCapability, ContextScopeMode, DomainActionReq, DomainActionResult,
    DomainCapabilities, DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
    FreshnessObservation,
};
use acdd_core::schema::SchemaDef;
use acdd_core::subsystems::SubsystemId;
use anyhow::Result;
use serde_json::Value;

impl DomainProvider for CodeDomainProvider {
    fn domain_id(&self) -> &str {
        SubsystemId::Code.as_str()
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: Vec::new(),
            checks: vec![
                CheckCapability {
                    name: "seam".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "signature_scope".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "lint".to_owned(),
                    input_schema: acdd_core::lint::lint_check_input_schema(),
                },
            ],
            context: vec![
                ContextCapability {
                    name: "symbols".to_owned(),
                    scope_mode: ContextScopeMode::NodeOnly,
                    default_paths_key: None,
                    input_schema: SchemaDef::object(),
                },
                ContextCapability {
                    name: "tests".to_owned(),
                    scope_mode: ContextScopeMode::NodeOnly,
                    default_paths_key: None,
                    input_schema: SchemaDef::object(),
                },
                ContextCapability::simple("external_callers"),
            ],
            supports_collection_context: false,
            tools: Vec::new(),
        }
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        super::common::unsupported_domain_action(SubsystemId::Code.as_str(), action)
    }

    fn run_check(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "seam" => self.run_seam_check(params, req),
            "signature_scope" => self.run_signature_scope_check(req),
            "lint" => self.run_lint_check(params, req),
            _ => Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code check 'code.{check}'"),
            )
            .into()),
        }
    }

    fn observe_freshness(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        match check {
            "seam" => {
                let state = self.prepare_seam_state(req)?;
                let salt = state.salt();
                Ok(FreshnessObservation::Passed {
                    index_generation: Some(state.index_gen),
                    execution_digest_salt: Some(salt),
                })
            }
            "signature_scope" => {
                let (_, index_gen, _, live_digest) = self.observe_signature_scope_state(req)?;
                let salt = format!("index_generation={index_gen};live_files_digest={live_digest}");
                Ok(FreshnessObservation::Passed {
                    index_generation: Some(index_gen),
                    execution_digest_salt: Some(salt),
                })
            }
            "lint" => self.observe_lint_freshness(params, req),
            _ => Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code check 'code.{check}'"),
            )
            .into()),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "symbols" => self.provide_symbols(req),
            "tests" => self.provide_tests(req),
            "external_callers" => self.provide_external_callers(req),
            _ => Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code context lens 'code.{lens}'"),
            )
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CodeDomainProvider;
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::ids::RunId;
    use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, DomainProvider};
    use std::fs;

    fn assert_seam_cases(
        provider: &CodeDomainProvider,
        base_req: &DomainCheckReq,
        scope: &[&str],
        cases: &[(&str, Vec<String>, bool, &str)],
    ) {
        let admitted_scope: Vec<String> = scope.iter().map(|s| (*s).to_owned()).collect();
        for (target, changed, expect_pass, label) in cases {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: admitted_scope.clone(),
                        changed_paths: changed.clone(),
                        test_target: Some((*target).to_owned()),
                        ..base_req.clone()
                    },
                )
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                *expect_pass,
                "{label}: {outcome:?}"
            );
        }
    }

    fn assert_seam_outcome(
        provider: &CodeDomainProvider,
        req: &DomainCheckReq,
        expect_pass: bool,
        msg: &str,
    ) {
        let outcome = provider
            .run_check("seam", &serde_json::Value::Null, req)
            .unwrap();
        assert_eq!(
            matches!(outcome, DomainCheckOutcome::Passed { .. }),
            expect_pass,
            "{msg}: {outcome:?}"
        );
    }

    #[test]
    fn code_seam_checks_live_worktree_hops_and_signature_scope() {
        let dir = ScopedTempDir::create("contextunity-test-code-domain").unwrap();
        let root = dir.path();
        let run_git = |args: &[&str]| {
            let out = acdd_core::git::git_command(root, args).unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"seam_demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod outside;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/engine.rs"),
            "pub fn target_entry(x: i32) -> i32 { x + 1 }\npub fn dead_fn() -> i32 { 0 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/outside.rs"),
            "use crate::engine::target_entry;\npub fn external_user() -> i32 { target_entry(2) }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_test.rs"),
            "use seam_demo::engine::target_entry;\n#[test]\nfn test_live_seam() {\n    let got = target_entry(1);\n    assert_eq!(got, 2);\n}\n",
        )
        .unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);
        // Mutate public signature of `target_entry` in working tree without touching `src/outside.rs`.
        fs::write(
            root.join("src/engine.rs"),
            "pub fn target_entry(x: i32, y: i32) -> i32 { x + y }\npub fn dead_fn() -> i32 { 0 }\n",
        )
        .unwrap();

        let provider = CodeDomainProvider::new(root.to_path_buf(), Adapter::default());
        let seam_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/engine.rs".into(), "tests/seam_test.rs".into()],
            changed_paths: vec![],
            test_target: Some("tests/seam_test.rs::test_live_seam".to_owned()),
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
            node_tags: vec![],
        };
        let cases = [
            (
                "tests/seam_test.rs::test_live_seam",
                true,
                "live test reaching admitted_scope",
            ),
            (
                "tests/seam_test.rs::missing_test",
                false,
                "missing test target fails closed",
            ),
        ];
        for (target, expect_pass, label) in cases {
            let req = DomainCheckReq {
                test_target: Some(target.to_owned()),
                ..seam_req.clone()
            };
            let outcome = provider
                .run_check("seam", &serde_json::Value::Null, &req)
                .unwrap();
            match (&outcome, expect_pass) {
                (
                    DomainCheckOutcome::Passed {
                        execution_digest_salt,
                        ..
                    },
                    true,
                ) => {
                    let salt = execution_digest_salt.as_deref().unwrap_or_default();
                    assert!(salt.contains("index_generation="), "{salt}");
                    assert!(salt.contains("live_files_digest="), "{salt}");
                    let observed = provider
                        .observe_freshness("seam", &serde_json::Value::Null, &req)
                        .unwrap();
                    assert_eq!(
                        observed,
                        acdd_core::ports::FreshnessObservation::Passed {
                            index_generation: Some("no-index".to_owned()),
                            execution_digest_salt: Some(salt.to_owned()),
                        }
                    );
                }
                (DomainCheckOutcome::Failed { .. }, false) => {
                    // Freshness observation computes deterministic input salts without rerunning validation.
                    let observed = provider
                        .observe_freshness("seam", &serde_json::Value::Null, &req)
                        .unwrap();
                    match observed {
                        acdd_core::ports::FreshnessObservation::Passed {
                            index_generation,
                            execution_digest_salt,
                        } => {
                            assert_eq!(index_generation.as_deref(), Some("no-index"));
                            let salt = execution_digest_salt.as_deref().unwrap_or_default();
                            assert!(salt.contains("index_generation="), "{salt}");
                            assert!(salt.contains("live_files_digest="), "{salt}");
                        }
                        other => panic!(
                            "expected deterministic freshness observation for {label}: {other:?}"
                        ),
                    }
                }
                _ => panic!("unexpected outcome for {label}: {outcome:?}"),
            }
        }

        // Verify pure-deletion, pure-test, pure-const, and docs-with---test scopes.
        fs::write(
            root.join("src/consts.rs"),
            "pub const MAX_ITEMS: usize = 8;\n",
        )
        .unwrap();
        fs::write(root.join("docs_note.md"), "# Note\n").unwrap();
        for (scope, changed, target, expect_pass, label) in [
            (
                vec!["src/deleted.rs".to_owned()],
                vec!["src/deleted.rs".to_owned()],
                None,
                true,
                "pure deletion scope",
            ),
            (
                vec!["src/never_created.rs".to_owned()],
                vec![],
                None,
                false,
                "nonexistent never-created code scope fails",
            ),
            (
                vec!["src/{never_a.rs,never_b.rs}".to_owned()],
                vec![],
                None,
                false,
                "nonexistent brace-glob code scope fails",
            ),
            (
                vec!["tests/seam_test.rs".to_owned()],
                vec![],
                Some("tests/seam_test.rs::test_live_seam"),
                true,
                "pure test scope",
            ),
            (
                vec!["src/consts.rs".to_owned(), "tests/seam_test.rs".to_owned()],
                vec![],
                Some("tests/seam_test.rs::test_live_seam"),
                true,
                "pure const module with overlapping test",
            ),
            (
                vec!["docs_note.md".to_owned()],
                vec![],
                Some("tests/seam_test.rs::missing_test"),
                true,
                "non-code scope with --test",
            ),
        ] {
            let req = DomainCheckReq {
                admitted_scope: scope,
                changed_paths: changed,
                test_target: target.map(str::to_owned),
                ..seam_req.clone()
            };
            assert_seam_outcome(&provider, &req, expect_pass, label);
        }

        // Verify `docs`-tagged tasks: non-Rust tooling passes without --test; Rust prod code still enforces seam BFS.
        fs::create_dir_all(root.join("website")).unwrap();
        fs::write(
            root.join("website/build.py"),
            "def render_site():\n    return 'ok'\n",
        )
        .unwrap();
        fs::write(root.join("website/index.html"), "<html></html>\n").unwrap();
        let docs_tooling_req = DomainCheckReq {
            admitted_scope: vec!["website/build.py".into(), "website/index.html".into()],
            changed_paths: vec!["website/build.py".into()],
            test_target: None,
            node_tags: vec!["docs".to_owned()],
            ..seam_req.clone()
        };
        let docs_tooling_outcome = provider
            .run_check("seam", &serde_json::Value::Null, &docs_tooling_req)
            .unwrap();
        let docs_salt = match docs_tooling_outcome {
            DomainCheckOutcome::Passed {
                summary,
                execution_digest_salt,
            } => {
                assert_eq!(
                    summary,
                    "code.seam: docs-tagged task with no Rust production code in scope"
                );
                let obs = provider
                    .observe_freshness("seam", &serde_json::Value::Null, &docs_tooling_req)
                    .unwrap();
                assert_eq!(
                    obs,
                    acdd_core::ports::FreshnessObservation::Passed {
                        index_generation: Some("no-index".to_owned()),
                        execution_digest_salt: execution_digest_salt.clone(),
                    }
                );
                execution_digest_salt
            }
            other => panic!("expected docs-tagged non-Rust tooling to pass: {other:?}"),
        };
        let invalid_test_req = DomainCheckReq {
            test_target: Some("tests/missing.rs::nonexistent_test".to_owned()),
            ..docs_tooling_req.clone()
        };
        assert_seam_outcome(
            &provider,
            &invalid_test_req,
            false,
            "docs-tagged task still validates explicit test target before passing",
        );
        // `observe_freshness` observes live input salts without rerunning seam validation;
        // `run_check` fails closed so no passing verify trace is recorded for invalid explicit targets.
        assert_eq!(
            provider
                .observe_freshness("seam", &serde_json::Value::Null, &invalid_test_req)
                .unwrap(),
            acdd_core::ports::FreshnessObservation::Passed {
                index_generation: Some("no-index".to_owned()),
                execution_digest_salt: docs_salt.clone(),
            }
        );
        let sym_cfg = serde_json::json!({"target_symbol": "nonexistent_symbol"});
        let invalid_target_sym_outcome = provider
            .run_check("seam", &sym_cfg, &docs_tooling_req)
            .unwrap();
        assert!(
            matches!(invalid_target_sym_outcome, DomainCheckOutcome::Failed { .. }),
            "docs-tagged task still validates explicit target_symbol before passing: {invalid_target_sym_outcome:?}"
        );
        assert_eq!(
            provider
                .observe_freshness("seam", &sym_cfg, &docs_tooling_req)
                .unwrap(),
            acdd_core::ports::FreshnessObservation::Passed {
                index_generation: Some("no-index".to_owned()),
                execution_digest_salt: docs_salt,
            }
        );
        assert_seam_outcome(
            &provider,
            &DomainCheckReq {
                admitted_scope: vec!["website/build.py".into(), "src/engine.rs".into()],
                changed_paths: vec!["src/engine.rs".into()],
                test_target: None,
                node_tags: vec!["docs".to_owned()],
                ..seam_req.clone()
            },
            false,
            "docs-tagged task with Rust prod code fails closed without seam test",
        );
        assert_seam_outcome(
            &provider,
            &DomainCheckReq {
                admitted_scope: vec!["website/build.py".into(), "src/engine.rs".into()],
                changed_paths: vec!["src/engine.rs".into()],
                test_target: Some("tests/seam_test.rs::test_live_seam".to_owned()),
                node_tags: vec!["docs".to_owned()],
                ..seam_req.clone()
            },
            true,
            "docs-tagged task with Rust prod code passes when seam test reaches it",
        );

        // Verify signature_scope flags an external non-test caller when a public signature changes.
        let sig_fail = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &DomainCheckReq {
                    run_id: RunId::new("r1").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["src/engine.rs".into()],
                    changed_paths: vec!["src/engine.rs".into()],
                    test_target: None,
                    path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
                    baseline_ref: None,
                    node_tags: vec![],
                },
            )
            .unwrap();
        assert!(
            matches!(sig_fail, DomainCheckOutcome::Failed { .. }),
            "expected un-scoped external caller to fail: {sig_fail:?}"
        );

        let sig_pass_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/engine.rs".into(), "src/outside.rs".into()],
            changed_paths: vec!["src/engine.rs".into()],
            test_target: None,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
            node_tags: vec![],
        };
        let sig_pass = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &sig_pass_req,
            )
            .unwrap();
        let sig_observed = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &sig_pass_req)
            .unwrap();
        match sig_pass {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => assert_eq!(
                sig_observed,
                acdd_core::ports::FreshnessObservation::Passed {
                    index_generation: Some("no-index".to_owned()),
                    execution_digest_salt,
                }
            ),
            other => panic!("expected scoped external caller to pass: {other:?}"),
        }

        // Build code-map.sqlite, then delete `src/outside.rs` without rebuilding:
        // stale `code_db` edges from the deleted file must be filtered out.
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        fs::remove_file(root.join("src/outside.rs")).unwrap();
        let sig_after_delete = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/engine.rs".into()],
                    ..sig_pass_req
                },
            )
            .unwrap();
        assert!(
            matches!(sig_after_delete, DomainCheckOutcome::Passed { .. }),
            "deleted external caller in stale code_db must be skipped: {sig_after_delete:?}"
        );

        // #12: Qualified paths, imports, aliases, re-exports, and methods resolve via `acdd-index::linker`.
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod other;\npub mod outer;\npub mod seam_qualifiers;\npub mod seam_crate_prefix;\npub mod seam_nested_src;\npub use engine::target_entry as reexported_entry;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/engine.rs"),
            "pub struct Engine;\nimpl Engine { pub fn run_method(&self) -> i32 { 1 } }\npub fn target_entry(x: i32, y: i32) -> i32 { x + y }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/other.rs"),
            "pub fn target_entry(x: i32) -> i32 { x }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_qualifiers.rs"),
            "use crate::other;\nuse crate::engine::{Engine, target_entry as aliased_entry};\nuse crate::engine as eng_mod;\n#[test]\nfn bad_qualified() { other::target_entry(1); }\n#[test]\nfn bad_imported_shadow() { use crate::other::target_entry; target_entry(1); }\n#[test]\nfn good_qualified() { crate::engine::target_entry(1, 2); }\n#[test]\nfn good_crate_reexport() { crate::reexported_entry(1, 2); }\n#[test]\nfn good_aliased_fn() { aliased_entry(1, 2); }\n#[test]\nfn good_aliased_mod() { eng_mod::target_entry(1, 2); }\n#[test]\nfn good_method(e: Engine) { let _ = e.run_method(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_direct_crate.rs"),
            "#[test]\nfn direct_crate_fn() { seam_demo::engine::target_entry(1, 2); }\n#[test]\nfn direct_crate_reexport() { seam_demo::reexported_entry(1, 2); }\n",
        )
        .unwrap();
        assert_seam_cases(
            &provider,
            &seam_req,
            &["src/engine.rs"],
            &[
                (
                    "src/seam_qualifiers.rs::bad_qualified",
                    vec![],
                    false,
                    "unrelated :: qualifier fails",
                ),
                (
                    "src/seam_qualifiers.rs::bad_imported_shadow",
                    vec![],
                    false,
                    "unrelated import fails",
                ),
                (
                    "src/seam_qualifiers.rs::good_qualified",
                    vec![],
                    true,
                    "crate:: qualified path passes",
                ),
                (
                    "src/seam_qualifiers.rs::good_crate_reexport",
                    vec![],
                    true,
                    "crate:: re-export path passes",
                ),
                (
                    "src/seam_qualifiers.rs::good_aliased_fn",
                    vec![],
                    true,
                    "aliased function call passes",
                ),
                (
                    "src/seam_qualifiers.rs::good_aliased_mod",
                    vec![],
                    true,
                    "aliased module call passes",
                ),
                (
                    "src/seam_qualifiers.rs::good_method",
                    vec![],
                    true,
                    "instance obj.method() passes",
                ),
                (
                    "tests/seam_direct_crate.rs::direct_crate_fn",
                    vec![],
                    true,
                    "direct crate::fn without use passes",
                ),
                (
                    "tests/seam_direct_crate.rs::direct_crate_reexport",
                    vec![],
                    true,
                    "direct crate re-export without use passes",
                ),
            ],
        );

        // #8: `crate::service::build` MUST NOT match `src/outer/service.rs::build`, while `crate::outer::service::build` passes.
        fs::create_dir_all(root.join("src/outer/src")).unwrap();
        fs::write(
            root.join("src/outer/mod.rs"),
            "pub mod service;\npub mod src;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/outer/service.rs"),
            "pub fn build() -> i32 { 7 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_crate_prefix.rs"),
            "#[test]\nfn false_crate_root() { crate::service::build(); }\n#[test]\nfn exact_crate_outer() { crate::outer::service::build(); }\n",
        )
        .unwrap();
        assert_seam_cases(
            &provider,
            &seam_req,
            &["src/outer/service.rs"],
            &[
                (
                    "src/seam_crate_prefix.rs::false_crate_root",
                    vec![],
                    false,
                    "crate::service::build must not match src/outer/service.rs",
                ),
                (
                    "src/seam_crate_prefix.rs::exact_crate_outer",
                    vec![],
                    true,
                    "crate::outer::service::build matches src/outer/service.rs",
                ),
            ],
        );

        // #4: Nested `src/` (`src/outer/src/service.rs`) must resolve from crate root `src/`, not inner `src/`.
        fs::write(root.join("src/outer/src/mod.rs"), "pub mod service;\n").unwrap();
        fs::write(
            root.join("src/outer/src/service.rs"),
            "pub fn nested_build() -> i32 { 9 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_nested_src.rs"),
            "#[test]\nfn bad_inner() { crate::service::nested_build(); }\n#[test]\nfn good_outer_src() { crate::outer::src::service::nested_build(); }\n",
        )
        .unwrap();
        assert_seam_cases(
            &provider,
            &seam_req,
            &["src/outer/src/service.rs"],
            &[
                (
                    "src/seam_nested_src.rs::bad_inner",
                    vec![],
                    false,
                    "crate::service::nested_build rejects src/outer/src/service.rs",
                ),
                (
                    "src/seam_nested_src.rs::good_outer_src",
                    vec![],
                    true,
                    "crate::outer::src::service::nested_build matches src/outer/src/service.rs",
                ),
            ],
        );

        // Python & TypeScript barrel hydration (`__init__.py` & `index.ts`) resolves automatically.
        fs::create_dir_all(root.join("py_pkg")).unwrap();
        fs::create_dir_all(root.join("ts_pkg")).unwrap();
        fs::write(
            root.join("py_pkg/__init__.py"),
            "from .worker import py_target\n",
        )
        .unwrap();
        fs::write(
            root.join("py_pkg/worker.py"),
            "def py_target():\n    return 1\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/test_py_seam.py"),
            "from py_pkg import py_target\ndef test_py_barrel():\n    return py_target()\n",
        )
        .unwrap();
        fs::write(
            root.join("ts_pkg/index.ts"),
            "export { tsTarget } from './worker';\n",
        )
        .unwrap();
        fs::write(
            root.join("ts_pkg/worker.ts"),
            "export function tsTarget(): number { return 1; }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/ts_seam.test.ts"),
            "import { tsTarget } from '../ts_pkg';\nfunction test_ts_barrel() { return tsTarget(); }\n",
        )
        .unwrap();
        for (scope, target, label) in [
            (
                "py_pkg/worker.py",
                "tests/test_py_seam.py::test_py_barrel",
                "Python __init__.py barrel",
            ),
            (
                "ts_pkg/worker.ts",
                "tests/ts_seam.test.ts::test_ts_barrel",
                "TypeScript index.ts barrel",
            ),
        ] {
            assert_seam_cases(
                &provider,
                &seam_req,
                &[scope],
                &[(target, vec![], true, label)],
            );
        }

        // #28: Deleted tracked file in `admitted_scope` is included in signature_scope digest, check, and external_callers.
        fs::write(
            root.join("src/deleted_mod.rs"),
            "pub fn doomed_api() -> i32 { 7 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/caller_of_deleted.rs"),
            "use crate::deleted_mod::doomed_api;\npub fn still_calls() -> i32 { doomed_api() }\n",
        )
        .unwrap();
        run_git(&["add", "src/deleted_mod.rs", "src/caller_of_deleted.rs"]);
        run_git(&["commit", "-m", "add deleted_mod and caller"]);
        let _ = fs::remove_file(root.join(".acdd/code-map.sqlite"));
        let del_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/deleted_mod.rs".into()],
            changed_paths: vec!["src/deleted_mod.rs".into()],
            test_target: None,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
            node_tags: vec![],
        };
        let obs_before_del = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        fs::remove_file(root.join("src/deleted_mod.rs")).unwrap();
        let obs_after_del = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        assert_ne!(
            obs_before_del, obs_after_del,
            "live_digest must change when tracked scoped file is deleted"
        );
        let del_sig_outcome = provider
            .run_check("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        assert!(
            matches!(del_sig_outcome, DomainCheckOutcome::Failed { .. }),
            "deleting tracked scoped file with un-scoped external caller must fail: {del_sig_outcome:?}"
        );
        let ext_callers = provider
            .provide_context(
                "external_callers",
                &acdd_core::ports::DomainContextReq {
                    run_id: RunId::new("r1").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["src/deleted_mod.rs".into()],
                    target: None,
                    max_bytes: crate::state_bridge::common::TEST_LARGE_CONTEXT_MAX_BYTES,
                    path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
                    baseline_ref: None,
                    lens_globs: Vec::new(),
                    citation_text: None,
                    params: serde_json::Value::Null,
                },
            )
            .unwrap();
        assert!(
            ext_callers["items"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|item| {
                    item["path"] == "src/deleted_mod.rs"
                        && item["signature_changed"] == true
                        && item["external_callers"]
                            .as_array()
                            .is_some_and(|c| c.iter().any(|p| p == "src/caller_of_deleted.rs"))
                })),
            "external_callers must report deleted file external callers: {ext_callers}"
        );

        // #11: Signature check with `baseline_ref` catches committed deletions/edits since claim even after commit advances HEAD.
        run_git(&["checkout", "--", "src/deleted_mod.rs"]);
        let baseline_out = acdd_core::git::git_command(root, &["rev-parse", "HEAD"]).unwrap();
        let claim_baseline = String::from_utf8_lossy(&baseline_out.stdout)
            .trim()
            .to_owned();
        fs::remove_file(root.join("src/deleted_mod.rs")).unwrap();
        run_git(&["add", "-u", "src/deleted_mod.rs"]);
        run_git(&["commit", "-m", "commit deletion after claim"]);
        let committed_del_req = DomainCheckReq {
            baseline_ref: Some(claim_baseline),
            ..del_req.clone()
        };
        let committed_del_outcome = provider
            .run_check(
                "signature_scope",
                &serde_json::Value::Null,
                &committed_del_req,
            )
            .unwrap();
        assert!(
            matches!(committed_del_outcome, DomainCheckOutcome::Failed { .. }),
            "baseline_ref must detect committed deletion since claim: {committed_del_outcome:?}"
        );

        // #6: Even if live `code-map.sqlite` is refreshed WITHOUT deleted `src/deleted_mod.rs`,
        // `run_signature_scope_check` still consults `baseline_ref` callers and fails!
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        let refreshed_del_outcome = provider
            .run_check(
                "signature_scope",
                &serde_json::Value::Null,
                &committed_del_req,
            )
            .unwrap();
        assert!(
            matches!(refreshed_del_outcome, DomainCheckOutcome::Failed { .. }),
            "refreshed code-map.sqlite missing deleted target must still fail via baseline_ref: {refreshed_del_outcome:?}"
        );

        // Stale `code_db` edge validation against `src_live` references/imports.
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod other;\npub mod outer;\npub mod hop1;\npub mod hop2;\npub mod hop3;\npub mod hop4;\npub mod wrong_hop4;\npub mod seam_qualifiers;\npub mod seam_crate_prefix;\npub mod seam_nested_src;\npub use engine::target_entry as reexported_entry;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop4.rs"),
            "pub fn step4() -> i32 { crate::engine::target_entry(1, 2) }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/wrong_hop4.rs"),
            "pub fn step4() -> i32 { 0 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop3.rs"),
            "pub fn step3() -> i32 { crate::hop4::step4() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop2.rs"),
            "pub fn step2() -> i32 { crate::hop3::step3() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop1.rs"),
            "pub fn step1() -> i32 { crate::hop2::step2() }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_stale.rs"),
            "#[test]\nfn direct_stale() { seam_demo::engine::target_entry(1, 2); }\n#[test]\nfn import_stale() { use seam_demo::engine::target_entry; target_entry(1, 2); }\n#[test]\nfn alias_stale() { use seam_demo::engine as eng; eng::target_entry(1, 2); }\n#[test]\nfn deep_chain() { seam_demo::hop1::step1(); }\n",
        )
        .unwrap();
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();

        // Mutate live files WITHOUT rebuilding `code-map.sqlite`:
        fs::write(
            root.join("tests/seam_stale.rs"),
            "#[test]\nfn direct_stale() { seam_demo::other::target_entry(1); }\n#[test]\nfn import_stale() { use seam_demo::other::target_entry; target_entry(1); }\n#[test]\nfn alias_stale() { use seam_demo::other as eng; eng::target_entry(1); }\n#[test]\nfn deep_chain() { seam_demo::hop1::step1(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop3.rs"),
            "pub fn step3() -> i32 { crate::wrong_hop4::step4() }\n",
        )
        .unwrap();
        assert_seam_cases(
            &provider,
            &seam_req,
            &["src/engine.rs"],
            &[
                (
                    "tests/seam_stale.rs::direct_stale",
                    vec![],
                    false,
                    "stale qualified code_db edge rejected after edit to other::target_entry",
                ),
                (
                    "tests/seam_stale.rs::import_stale",
                    vec![],
                    false,
                    "stale bare call rejected after import changed to other::target_entry",
                ),
                (
                    "tests/seam_stale.rs::alias_stale",
                    vec![],
                    false,
                    "stale aliased module call rejected after alias changed to other",
                ),
                (
                    "tests/seam_stale.rs::deep_chain",
                    vec!["src/hop3.rs".to_owned()],
                    false,
                    "stale mid-chain code_db edge rejected after hop3 edited to wrong_hop4::step4",
                ),
            ],
        );

        // Restoring `src/hop3.rs` via an aliased import to `hop4` still traverses unhydrated `hop4 -> engine` in `code_db`.
        fs::write(
            root.join("src/hop3.rs"),
            "use crate::hop4 as h4;\npub fn step3() -> i32 { h4::step4() }\n",
        )
        .unwrap();
        let deep_req = DomainCheckReq {
            admitted_scope: vec!["src/engine.rs".into()],
            changed_paths: vec![],
            test_target: Some("tests/seam_stale.rs::deep_chain".to_owned()),
            ..seam_req.clone()
        };
        assert_seam_outcome(
            &provider,
            &DomainCheckReq {
                changed_paths: vec!["src/hop3.rs".to_owned()],
                ..deep_req.clone()
            },
            true,
            "valid cross-file hop via code_db must still pass",
        );

        // Rebuild code_db, then test untouched chain, line-shifted live edit, and deleted target symbol.
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        assert_seam_outcome(&provider, &deep_req, true, "untouched deep chain passes");
        fs::write(
            root.join("src/hop3.rs"),
            "// shift 1\n// shift 2\nuse crate::hop4 as h4;\npub fn step3() -> i32 { h4::step4() }\n",
        )
        .unwrap();
        assert_seam_outcome(&provider, &deep_req, true, "line-shifted hop3 passes");
        fs::write(
            root.join("src/engine.rs"),
            "pub struct Engine;\nimpl Engine { pub fn run_method(&self) -> i32 { 1 } }\npub fn renamed_entry(x: i32, y: i32) -> i32 { x + y }\n",
        )
        .unwrap();
        assert_seam_outcome(&provider, &deep_req, false, "renamed target fails");
    }
}
