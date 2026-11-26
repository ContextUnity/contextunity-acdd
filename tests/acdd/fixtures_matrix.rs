use super::support::{base_engine_config, git_cmd, init_git_repo_on_branch, TestProvider};
use crate::common::Workspace;
use acdd_core::config::schema::AdapterFile;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::WorkerId;
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_state::harness::RunHarness;
use acdd_state::profile::{
    resolve_profile_from_str_with_dirs, validate_profile, ProfileValidationOptions,
};
use acdd_state::spec::parse_spec;
use acdd_state::types::SubmitAction;
use acdd_state::usecases::dto::{ClaimReq, ExtendScopeReq, StartReq, SubmitReq};
use acdd_state::usecases::StateCommand;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy)]
enum FixtureKind {
    ValidProfile,
    ValidSpec,
    ValidAdapterConfig,
    ValidCatalogJson,
    TemplateSpec,
    NegativeProfile(ErrorCode),
    NegativeSpecParse(ErrorCode),
    NegativeSpecStart(ErrorCode),
    NegativeSpecClaim(ErrorCode),
    NegativeSpecSubmit(ErrorCode),
    NegativeUnsafeScope(ErrorCode),
}

struct AuditedFixture {
    relpath: &'static str,
    kind: FixtureKind,
}

const FIXTURE_AUDIT: &[AuditedFixture] = &[
    // --- milestones/ (19) ---
    AuditedFixture {
        relpath: "milestones/archived_after_sync.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/archive_metrics.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/branching_deps.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/cancellation.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/configured_directories.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/context_retention.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/crash_recovery.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/custom_contract.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/declared_projects.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/default_aliases.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/deferred_defects.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/foreign_handoff.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/lifecycle_task.md",
        kind: FixtureKind::TemplateSpec,
    },
    AuditedFixture {
        relpath: "milestones/linear.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/minimal.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/owner_aliases.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/progress_catalog.json",
        kind: FixtureKind::ValidCatalogJson,
    },
    AuditedFixture {
        relpath: "milestones/queued_after_sync.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "milestones/verification_profile.yaml",
        kind: FixtureKind::ValidProfile,
    },
    // --- plans/ (18) ---
    AuditedFixture {
        relpath: "plans/acdd.yaml",
        kind: FixtureKind::ValidAdapterConfig,
    },
    AuditedFixture {
        relpath: "plans/commit.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "plans/completed-subtask.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "plans/configured-snapshot.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "plans/custom-config.yaml",
        kind: FixtureKind::ValidAdapterConfig,
    },
    AuditedFixture {
        relpath: "plans/dag.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "plans/drift-snapshot.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "plans/immutable-drift.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "plans/linear.md",
        kind: FixtureKind::ValidSpec,
    },
    AuditedFixture {
        relpath: "plans/snapshot.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "plans/rejected-profile.yaml",
        kind: FixtureKind::NegativeProfile(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "plans/catalog-invalid.yaml",
        kind: FixtureKind::NegativeProfile(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "plans/cycle.md",
        kind: FixtureKind::NegativeSpecStart(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "plans/unknown-dependency.md",
        kind: FixtureKind::NegativeSpecStart(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "plans/unsafe-scope.md",
        kind: FixtureKind::NegativeUnsafeScope(ErrorCode::OutOfScopeChanges),
    },
    AuditedFixture {
        relpath: "plans/missing-subtask-evidence.md",
        kind: FixtureKind::NegativeSpecParse(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "plans/pending-subtask.md",
        kind: FixtureKind::NegativeSpecSubmit(ErrorCode::GateVerificationFailed),
    },
    AuditedFixture {
        relpath: "plans/cancelled-dependency.md",
        kind: FixtureKind::NegativeSpecClaim(ErrorCode::GateVerificationFailed),
    },
    // --- workflows/ (10) ---
    AuditedFixture {
        relpath: "workflows/agents.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/artifact-refresh.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/catalog-invalid.yaml",
        kind: FixtureKind::NegativeProfile(ErrorCode::ProfileSchemaInvalid),
    },
    AuditedFixture {
        relpath: "workflows/count-free-role.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/failing.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/ordered.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/rejection_routing.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/retry.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/simple.yaml",
        kind: FixtureKind::ValidProfile,
    },
    AuditedFixture {
        relpath: "workflows/timeout.yaml",
        kind: FixtureKind::ValidProfile,
    },
];

#[test]
fn audit_all_47_fixtures_for_060_validity_and_negative_error_codes() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/acdd/fixtures");
    let mut disk_files = BTreeSet::new();
    for dir in ["milestones", "plans", "workflows"] {
        for entry in fs::read_dir(fixtures_dir.join(dir)).unwrap() {
            let p = entry.unwrap().path();
            if p.is_file() {
                disk_files.insert(format!(
                    "{dir}/{}",
                    p.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    let audited_set: BTreeSet<String> =
        FIXTURE_AUDIT.iter().map(|f| f.relpath.to_owned()).collect();
    assert_eq!(
        disk_files, audited_set,
        "every fixture under tests/acdd/fixtures/{{milestones,plans,workflows}} must be classified"
    );
    assert_eq!(FIXTURE_AUDIT.len(), 47);

    let providers = TestProvider::full_composite();
    let val_opts = ProfileValidationOptions {
        domain_providers: Some(providers.as_ref()),
        acdd_modules: None,
        require_bound_commands: true,
    };

    let ws = Workspace::new();
    init_git_repo_on_branch(ws.root(), "dev");
    let cfg = base_engine_config(ws.root());
    fs::create_dir_all(ws.path("docs/plans")).unwrap();
    fs::create_dir_all(ws.path(".acdd/profiles")).unwrap();
    fs::write(
        ws.path(".acdd/profiles/minimal.yaml"),
        "version: 2\ninherit: none\nsave: none\ngates:\n  - tasks:\n      gates:\n        - id: finish\n          need: []\n",
    )
    .unwrap();
    // Valid minimal profile with two gates for Start/Claim/Submit negative spec tests.
    let valid_two_gate_profile = r#"version: 2
inherit: none
save: none
gates:
  - tasks:
      gates:
        - id: work
          pass: finish
        - id: finish
          need: [work]
          receipt: true
"#;
    fs::write(
        ws.path(".acdd/profiles/two-gate.yaml"),
        valid_two_gate_profile,
    )
    .unwrap();
    git_cmd(ws.root(), &["add", "."]);
    git_cmd(ws.root(), &["commit", "-m", "seed profiles"]);

    for (idx, item) in FIXTURE_AUDIT.iter().enumerate() {
        let raw = fs::read_to_string(fixtures_dir.join(item.relpath))
            .unwrap_or_else(|e| panic!("read {}: {e}", item.relpath));
        match item.kind {
            FixtureKind::ValidProfile => {
                let resolved = resolve_profile_from_str_with_dirs(
                    &raw,
                    item.relpath,
                    Some(item.relpath),
                    Some(ws.root()),
                    None,
                    None,
                )
                .unwrap_or_else(|e| panic!("resolve {}: {e}", item.relpath));
                validate_profile(&resolved, &val_opts)
                    .unwrap_or_else(|e| panic!("validate {}: {e}", item.relpath));
            }
            FixtureKind::ValidSpec => {
                let doc =
                    parse_spec(&raw).unwrap_or_else(|e| panic!("parse_spec {}: {e}", item.relpath));
                assert!(!doc.id.is_empty(), "empty id in {}", item.relpath);
            }
            FixtureKind::ValidAdapterConfig => {
                let adapter: AdapterFile = serde_yaml::from_str(&raw)
                    .unwrap_or_else(|e| panic!("parse AdapterFile {}: {e}", item.relpath));
                assert!(
                    adapter.specs.as_ref().is_some_and(|s| !s.is_empty()),
                    "expected specs in {}",
                    item.relpath
                );
            }
            FixtureKind::ValidCatalogJson => {
                let obj: Value = serde_json::from_str(&raw)
                    .unwrap_or_else(|e| panic!("parse JSON {}: {e}", item.relpath));
                for (path, val) in obj.as_object().unwrap() {
                    if let Some(content) = val.as_str() {
                        if path == "acdd.yaml" {
                            let _: AdapterFile =
                                serde_yaml::from_str(content).unwrap_or_else(|e| {
                                    panic!("{} -> {path} AdapterFile invalid: {e}", item.relpath)
                                });
                        } else if path.ends_with(".md") && content.starts_with("---\n") {
                            if path.ends_with("README.md") {
                                continue;
                            }
                            let _ = parse_spec(content).unwrap_or_else(|e| {
                                panic!("{} -> {path} SpecDocument invalid: {e}", item.relpath)
                            });
                        }
                    }
                }
            }
            FixtureKind::TemplateSpec => {
                // Parameterized template (`{{ID}}`, etc.) — substitute placeholders and validate via `parse_spec`.
                let expanded = raw
                    .replace("{{ID}}", "AC-TPL-1")
                    .replace("{{STATUS}}", "status: active\n")
                    .replace("{{CLOSURE}}", "")
                    .replace("{{TASK}}", "t1")
                    .replace("{{TARGET}}", "Template target")
                    .replace("{{REVISION}}", "1")
                    .replace("{{SUBTASK}}", "s1");
                let doc = parse_spec(&expanded)
                    .unwrap_or_else(|e| panic!("expanded {}: {e}", item.relpath));
                assert_eq!(doc.id, "AC-TPL-1");
            }
            FixtureKind::NegativeProfile(expected) => {
                let res = resolve_profile_from_str_with_dirs(
                    &raw,
                    item.relpath,
                    Some(item.relpath),
                    Some(ws.root()),
                    None,
                    None,
                )
                .and_then(|p| validate_profile(&p, &val_opts));
                let err = res.expect_err(&format!("{} must fail validation", item.relpath));
                assert_eq!(err.code, expected, "{}", item.relpath);
            }
            FixtureKind::NegativeSpecParse(expected) => {
                let err =
                    parse_spec(&raw).expect_err(&format!("{} must fail parse_spec", item.relpath));
                assert_eq!(err.code, expected, "{}", item.relpath);
            }
            FixtureKind::NegativeSpecStart(expected) => {
                let harness = RunHarness::in_memory().unwrap();
                let rel = format!("docs/plans/neg-{idx}.md");
                fs::write(ws.path(&rel), &raw).unwrap();
                let err = harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Start(StartReq {
                            spec_path: Some(rel),
                            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
                            ..StartReq::default()
                        }),
                    )
                    .expect_err(&format!("{} must fail Start", item.relpath));
                assert_eq!(err.code, expected, "{}", item.relpath);
            }
            FixtureKind::NegativeSpecClaim(expected) => {
                let harness = RunHarness::in_memory().unwrap();
                let rel = format!("docs/plans/neg-claim-{idx}.md");
                fs::write(ws.path(&rel), &raw).unwrap();
                let started = match harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Start(StartReq {
                            spec_path: Some(rel),
                            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
                            ..StartReq::default()
                        }),
                    )
                    .unwrap()
                {
                    acdd_state::usecases::StateCommandOutput::Start(s) => s,
                    _ => unreachable!(),
                };
                let err = harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Claim(ClaimReq {
                            run: Some(started.run.run_id.to_string()),
                            node: Some("second-step.work".to_owned()),
                            worker_id: WorkerId::new("w1").unwrap(),
                            session_fingerprint: None,
                            force: false,
                            steal_remote: false,
                            worktree: None,
                            detail: Default::default(),
                        }),
                    )
                    .expect_err(&format!("{} must fail Claim", item.relpath));
                assert_eq!(err.code, expected, "{}", item.relpath);
            }
            FixtureKind::NegativeSpecSubmit(expected) => {
                let harness = RunHarness::in_memory().unwrap();
                let rel = format!("docs/plans/neg-sub-{idx}.md");
                fs::write(ws.path(&rel), &raw).unwrap();
                git_cmd(ws.root(), &["add", "docs/plans"]);
                git_cmd(ws.root(), &["commit", "-m", "add neg-sub"]);
                let started = match harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Start(StartReq {
                            spec_path: Some(rel),
                            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
                            ..StartReq::default()
                        }),
                    )
                    .unwrap()
                {
                    acdd_state::usecases::StateCommandOutput::Start(s) => s,
                    _ => unreachable!(),
                };
                let rid = started.run.run_id.to_string();
                let w = WorkerId::new("w1").unwrap();
                harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Claim(ClaimReq {
                            run: Some(rid.clone()),
                            node: Some("guarded-step.work".to_owned()),
                            worker_id: w.clone(),
                            session_fingerprint: None,
                            force: false,
                            steal_remote: false,
                            worktree: None,
                            detail: Default::default(),
                        }),
                    )
                    .unwrap();
                harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Submit(Box::new(SubmitReq {
                            run: Some(rid.clone()),
                            node: "guarded-step.work".to_owned(),
                            action: SubmitAction::Pass,
                            worker_id: w.clone(),
                            claim_revision: Some(1),
                            test_target: None,
                            review_evidence: None,
                            rejection_findings: None,
                            custom_evidence: None,
                            verified_invariants: Vec::new(),
                            summary: None,
                            allow_main: true,
                            worktree: None,
                        })),
                    )
                    .unwrap();
                harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Claim(ClaimReq {
                            run: Some(rid.clone()),
                            node: Some("guarded-step.finish".to_owned()),
                            worker_id: w.clone(),
                            session_fingerprint: None,
                            force: false,
                            steal_remote: false,
                            worktree: None,
                            detail: Default::default(),
                        }),
                    )
                    .unwrap();
                let err = harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Submit(Box::new(SubmitReq {
                            run: Some(rid),
                            node: "guarded-step.finish".to_owned(),
                            action: SubmitAction::Pass,
                            worker_id: w,
                            claim_revision: Some(1),
                            test_target: None,
                            review_evidence: None,
                            rejection_findings: None,
                            custom_evidence: None,
                            verified_invariants: Vec::new(),
                            summary: None,
                            allow_main: true,
                            worktree: None,
                        })),
                    )
                    .expect_err(&format!("{} must fail Submit", item.relpath));
                assert_eq!(err.code, expected, "{}", item.relpath);
            }
            FixtureKind::NegativeUnsafeScope(expected) => {
                let harness = RunHarness::in_memory().unwrap();
                let doc = parse_spec(&raw).unwrap();
                let unsafe_scope = &doc.tasks[0].scope;
                let policy = WorkspacePathPolicy::default();
                assert!(
                    !policy.is_scope_admitted("escape.rs", unsafe_scope),
                    "unsafe scope must never admit escape.rs"
                );
                let rel = format!("docs/plans/neg-scope-{idx}.md");
                fs::write(ws.path(&rel), &raw).unwrap();
                let start_err = harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Start(StartReq {
                            spec_path: Some(rel),
                            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
                            ..StartReq::default()
                        }),
                    )
                    .expect_err("Start must reject unsafe scope glob fail-closed");
                assert_eq!(
                    start_err.code,
                    ErrorCode::ProfileSchemaInvalid,
                    "{}",
                    item.relpath
                );

                let valid_raw = fs::read_to_string(fixtures_dir.join("plans/linear.md")).unwrap();
                let valid_rel = format!("docs/plans/neg-scope-valid-{idx}.md");
                fs::write(ws.path(&valid_rel), &valid_raw).unwrap();
                let started = match harness
                    .execute(
                        &cfg,
                        &providers,
                        StateCommand::Start(StartReq {
                            spec_path: Some(valid_rel),
                            profile: Some(".acdd/profiles/two-gate.yaml".to_owned()),
                            ..StartReq::default()
                        }),
                    )
                    .unwrap()
                {
                    acdd_state::usecases::StateCommandOutput::Start(s) => s,
                    _ => unreachable!(),
                };
                for bad_paths in [unsafe_scope.clone(), vec!["src/{a".to_owned()]] {
                    let err = harness
                        .execute(
                            &cfg,
                            &providers,
                            StateCommand::ExtendScope(ExtendScopeReq {
                                run: Some(started.run.run_id.to_string()),
                                node: "step-1".to_owned(),
                                paths: bad_paths.clone(),
                                reason: "attempt escaping or malformed scope".to_owned(),
                                worker_id: None,
                            }),
                        )
                        .expect_err(&format!("ExtendScope must reject {bad_paths:?}"));
                    assert_eq!(err.code, expected, "{}", item.relpath);
                }
            }
        }
    }
}
