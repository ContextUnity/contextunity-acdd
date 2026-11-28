use super::DocsDomainProvider;
use acdd_core::config::Adapter;
use acdd_core::fs::ScopedTempDir;
use acdd_core::ids::RunId;
use acdd_core::ports::{
    DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
};
use std::fs;

fn assert_check_matches_freshness(
    provider: &DocsDomainProvider,
    check_name: &str,
    expected_prefix: &str,
    req: &DomainCheckReq,
) {
    let check = provider
        .run_check(check_name, &serde_json::Value::Null, req)
        .unwrap();
    let observed = provider
        .observe_freshness(check_name, &serde_json::Value::Null, req)
        .unwrap();
    match check {
        DomainCheckOutcome::Passed {
            execution_digest_salt,
            ..
        } => {
            assert!(execution_digest_salt
                .as_deref()
                .is_some_and(|s| s.starts_with(expected_prefix)));
            assert_eq!(
                observed,
                FreshnessObservation::Passed {
                    index_generation: None,
                    execution_digest_salt,
                }
            );
        }
        other => panic!("expected docs.{check_name} pass, got {other:?}"),
    }
}

#[test]
fn docs_domain_checks_links_and_filters_contracts_from_lenses() {
    let dir = ScopedTempDir::create("acdd-ut-docs-domain").unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("docs/adr")).unwrap();
    fs::create_dir_all(root.join("docs/milestones")).unwrap();
    fs::create_dir_all(root.join("docs/architecture")).unwrap();
    fs::write(
        root.join("docs/adr/ADR-0001-test.md"),
        "---\ntitle: Test ADR\n---\n# Test ADR\nSee [arch](../architecture/overview.md).\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/architecture/overview.md"),
        "# Architecture\n- INV-TEST-1: All invariants stay non-contract.\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/milestones/m1.md"),
        "# Spec M1\n- INV-CONTRACT-ONLY: Must not leak into docs.invariants.\n",
    )
    .unwrap();

    let provider = DocsDomainProvider::new(root.to_path_buf(), Adapter::default());
    let check_req = DomainCheckReq {
        run_id: RunId::new("r1").unwrap(),
        node_cursor: "t1.docs".parse().unwrap(),
        gate_id: "docs".parse().unwrap(),
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        admitted_scope: vec!["docs/**".into()],
        changed_paths: vec![],
        test_target: None,
        path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
        baseline_ref: None,
        node_tags: vec![],
    };
    assert_check_matches_freshness(&provider, "links", "docs_live=", &check_req);

    // Non-empty non-markdown scope must pass without scanning unrelated broken docs in the repo.
    fs::write(
        root.join("docs/unrelated_broken.md"),
        "[missing](./missing.md)\n",
    )
    .unwrap();
    let code_only_req = DomainCheckReq {
        admitted_scope: vec!["src/lib.rs".into()],
        ..check_req.clone()
    };
    let code_only_check = provider
        .run_check("links", &serde_json::Value::Null, &code_only_req)
        .unwrap();
    let code_only_observed = provider
        .observe_freshness("links", &serde_json::Value::Null, &code_only_req)
        .unwrap();
    match code_only_check {
        DomainCheckOutcome::Passed {
            execution_digest_salt,
            ..
        } => assert_eq!(
            code_only_observed,
            FreshnessObservation::Passed {
                index_generation: None,
                execution_digest_salt,
            }
        ),
        other => panic!("expected code-only scope docs.links pass, got {other:?}"),
    }

    // Brace-glob doc scope `docs/{a.md,b.md}` MUST inspect both files via canonical matcher.
    fs::write(root.join("docs/a.md"), "# Valid A\n").unwrap();
    fs::write(root.join("docs/b.md"), "[broken](./does_not_exist.md)\n").unwrap();
    let brace_doc_req = DomainCheckReq {
        admitted_scope: vec!["docs/{a.md,b.md}".into()],
        ..check_req.clone()
    };
    let brace_doc_outcome = provider
        .run_check("links", &serde_json::Value::Null, &brace_doc_req)
        .unwrap();
    assert!(
        matches!(brace_doc_outcome, DomainCheckOutcome::Failed { .. }),
        "docs/{{a.md,b.md}} must check both files and catch broken link in b.md: {brace_doc_outcome:?}"
    );

    let ctx_req = DomainContextReq {
        run_id: RunId::new("r1").unwrap(),
        node_cursor: "t1.docs".parse().unwrap(),
        gate_id: "docs".parse().unwrap(),
        workspace_root: root.to_path_buf(),
        worktree: root.to_path_buf(),
        admitted_scope: vec!["docs/**".into()],
        target: None,
        max_bytes: crate::state_bridge::common::TEST_DEFAULT_CONTEXT_MAX_BYTES,
        path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
        baseline_ref: None,
        lens_globs: Vec::new(),
        citation_text: None,
    };
    let caps = provider.capabilities().context_lenses;
    assert!(caps.contains("invariants"));
    assert!(caps.contains("workspace_config"));
    assert!(!caps.contains("instructions"));
    assert!(!caps.contains("adrs"));
    fs::create_dir_all(root.join("src/sub")).unwrap();
    fs::create_dir_all(root.join("tests/acdd")).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    fs::write(root.join("AGENTS.md"), "# Root Agent Rules\n## Routes\n").unwrap();
    fs::write(root.join("TESTS.md"), "# Required Tests\n## Matrix\n").unwrap();
    fs::write(root.join("src/AGENTS.md"), "# Src Rules\n## Ownership\n").unwrap();
    fs::write(root.join("tests/AGENTS.md"), "# Test Rules\n").unwrap();
    fs::write(root.join("tests/acdd/AGENTS.md"), "# Acdd Test Rules\n").unwrap();
    fs::write(root.join("other/AGENTS.md"), "# Unrelated\n").unwrap();

    let inst_globs = vec!["**/AGENTS.md".to_owned(), "TESTS.md".to_owned()];
    let adr_globs = vec!["docs/adr/**".to_owned()];
    let src_only_ctx = DomainContextReq {
        admitted_scope: vec!["src/sub/lib.rs".into()],
        lens_globs: inst_globs.clone(),
        ..ctx_req.clone()
    };
    let src_inst = provider
        .provide_context("paths.instructions", &src_only_ctx)
        .unwrap();
    let src_paths: Vec<&str> = src_inst["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(src_paths, vec!["src/AGENTS.md", "AGENTS.md"]);
    assert!(src_inst["items"][0].get("uri").is_none());
    assert!(src_inst["items"][0].get("title").is_none());
    assert!(src_inst["items"][0].get("content").is_none());
    assert!(src_inst["items"][0].get("headings").is_none());
    assert_eq!(
        src_inst["directive"],
        "MANDATORY: read repository rules for respective scope."
    );
    assert!(src_inst.get("mandatory_directives").is_none());

    let tests_ctx = DomainContextReq {
        admitted_scope: vec!["tests/**".into()],
        lens_globs: inst_globs.clone(),
        ..ctx_req.clone()
    };
    let tests_inst = provider
        .provide_context("paths.instructions", &tests_ctx)
        .unwrap();
    let tests_paths: Vec<&str> = tests_inst["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(
        tests_paths,
        vec![
            "tests/AGENTS.md",
            "TESTS.md",
            "tests/acdd/AGENTS.md",
            "AGENTS.md",
        ]
    );

    let tight_inst = provider
        .provide_context(
            "paths.instructions",
            &DomainContextReq {
                max_bytes: crate::state_bridge::common::TEST_TIGHT_INSTRUCTIONS_MAX_BYTES,
                ..tests_ctx
            },
        )
        .unwrap();
    assert_eq!(tight_inst["truncated"], true);
    assert!(
        acdd_core::format::estimate_json_bytes(&tight_inst)
            <= crate::state_bridge::common::TEST_TIGHT_INSTRUCTIONS_MAX_BYTES
    );
    assert!(
        !tight_inst["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "AGENTS.md"),
        "Tier 4 AGENTS.md must drop before Tier 2 scoped rules: {tight_inst}"
    );

    let adrs_req = DomainContextReq {
        lens_globs: adr_globs.clone(),
        ..ctx_req.clone()
    };
    let adrs = provider.provide_context("paths.adrs", &adrs_req).unwrap();
    assert_eq!(adrs["items"].as_array().map(|a| a.len()), Some(1));
    assert!(adrs["items"][0].get("doc_type").is_none());

    let invs = provider.provide_context("invariants", &ctx_req).unwrap();
    let inv_text = serde_json::to_string(&invs).unwrap();
    assert!(inv_text.contains("INV-TEST-1"), "{inv_text}");
    assert!(!inv_text.contains("INV-CONTRACT-ONLY"), "{inv_text}");

    // With indexed `code-map.sqlite` containing archived/README ADRs, `paths.adrs` must
    // exclude archived/README rows and still surface live unindexed ADRs, while
    // `paths.instructions` surfaces AGENTS.md only without repeating ADRs.
    fs::create_dir_all(root.join("docs/adr/archive")).unwrap();
    fs::write(
        root.join("docs/adr/archive/0000-old.md"),
        "---\ntitle: Old Archived ADR\ndoc_type: adr\n---\n# Old Archived ADR\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/adr/README.md"),
        "---\ntitle: ADR Index\ndoc_type: adr\n---\n# ADR Index\n",
    )
    .unwrap();
    crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
    fs::write(
        root.join("docs/adr/ADR-0002-live.md"),
        "---\ntitle: Live Unindexed ADR\ndoc_type: adr\n---\n# Live Unindexed ADR\n",
    )
    .unwrap();
    for (lens, globs, expected_paths, excluded_paths) in [
        (
            "paths.instructions",
            inst_globs.clone(),
            vec!["src/AGENTS.md", "AGENTS.md"],
            vec![
                "docs/adr/ADR-0001-test.md",
                "docs/adr/ADR-0002-live.md",
                "docs/adr/archive/0000-old.md",
                "docs/adr/README.md",
            ],
        ),
        (
            "paths.adrs",
            adr_globs,
            vec!["docs/adr/ADR-0001-test.md", "docs/adr/ADR-0002-live.md"],
            vec!["docs/adr/archive/0000-old.md", "docs/adr/README.md"],
        ),
    ] {
        let lens_req = DomainContextReq {
            lens_globs: globs,
            ..src_only_ctx.clone()
        };
        let res = provider.provide_context(lens, &lens_req).unwrap();
        let paths: Vec<&str> = res["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["path"].as_str())
            .collect();
        for expected in expected_paths {
            assert!(
                paths.contains(&expected),
                "{lens} missing {expected}: {paths:?}"
            );
        }
        for excluded in excluded_paths {
            assert!(
                !paths.contains(&excluded),
                "{lens} leaked {excluded}: {paths:?}"
            );
        }
    }
    let indexed_invs = provider
        .provide_context(
            "invariants",
            &DomainContextReq {
                lens_globs: Vec::new(),
                ..src_only_ctx.clone()
            },
        )
        .unwrap();
    let indexed_inv_text = serde_json::to_string(&indexed_invs).unwrap();
    assert!(
        indexed_inv_text.contains("INV-TEST-1"),
        "{indexed_inv_text}"
    );
    assert!(
        !indexed_inv_text.contains("\"id\":\"[]\"") && !indexed_inv_text.contains("0000-old.md"),
        "indexed fallback must skip empty JSON arrays and archived docs: {indexed_inv_text}"
    );

    fs::write(
        root.join("docs/adr/ADR-0001-test.md"),
        "# Broken\n[nope](../architecture/missing.md)\n",
    )
    .unwrap();
    let failed = provider
        .run_check("links", &serde_json::Value::Null, &check_req)
        .unwrap();
    assert!(matches!(failed, DomainCheckOutcome::Failed { .. }));

    // `docs.workspace_config` context lens and `docs.specs` validation + freshness check.
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: documentation\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".acdd/lint")).unwrap();
    fs::write(
        root.join(".acdd/lint/zones.yaml"),
        "zones:\n  core:\n    - \"crates/core/**\"\n",
    )
    .unwrap();
    fs::write(
        root.join("acdd.yaml"),
        "docs:\n  - docs\nworkflows_dirs:\n  - .acdd/workflows\nlimits:\n  max_metadata_bytes: 1024\nmodules:\n  code: true\n  docs: true\n  state: true\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    )
    .unwrap();
    let adapter_cfg = acdd_core::config::load_adapter_report(root, None)
        .unwrap()
        .adapter;
    let cfg_provider = DocsDomainProvider::new(root.to_path_buf(), adapter_cfg);
    let ws_cfg_lens = cfg_provider
        .provide_context("workspace_config", &ctx_req)
        .unwrap();
    assert_eq!(
        ws_cfg_lens["specs"]["milestones"]["next_ids"]["AC"],
        "AC-001"
    );
    assert_eq!(ws_cfg_lens["docs_roots"], serde_json::json!(["docs"]));
    assert_eq!(
        ws_cfg_lens["workflows_dirs"],
        serde_json::json!([".acdd/workflows"])
    );
    let avail_workflows = ws_cfg_lens["available_workflows"].as_array().unwrap();
    assert!(
        avail_workflows.iter().any(|p| p == "documentation")
            && avail_workflows
                .iter()
                .any(|p| p == ".acdd/workflows/default.yaml")
            && !avail_workflows.iter().any(|p| p == "spec_planning"),
        "available_workflows should include core triad + .acdd/workflows: {ws_cfg_lens}"
    );
    assert_eq!(ws_cfg_lens["limits"]["max_metadata_bytes"], 1024);
    assert!(ws_cfg_lens["architecture_zones"].get("core").is_some());
    let mut lenses = serde_json::Map::new();
    lenses.insert("docs.workspace_config".to_owned(), ws_cfg_lens);
    let claim = acdd_state::context::ClaimResp {
        run_id: RunId::new("r1").unwrap(),
        node_cursor: "synthesize".parse().unwrap(),
        claim_revision: 1,
        contract_revision: 1,
        worker_id: "w1".parse().unwrap(),
        worktree: ".".to_owned(),
        candidate_tree: acdd_core::ids::TreeOid::new("0".repeat(40)).unwrap(),
        inspect_cmd: "acdd show synthesize --run r1".to_owned(),
        context_reset_required: false,
        contract: serde_json::json!({}),
        guidance: Vec::new(),
        unresolved_findings: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard: Vec::new(),
        context_lenses: lenses,
        omitted_sections: Vec::new(),
        full_detail: false,
        omitted_proof_pointers: Vec::new(),
        total_bytes: 0,
    };
    let rendered_cfg = acdd_state::context::render_claim_prompt_markdown(&claim, None);
    assert!(
        rendered_cfg.contains(
            "- spec `milestones`: `docs/milestones` (prefixes: [AC], digits: 3, next: AC=AC-001)"
        ),
        "expected live spec rendering in Markdown:\n{rendered_cfg}"
    );
    assert!(
        rendered_cfg.contains("- zone `core`: [crates/core/**]"),
        "expected live zone rendering in Markdown:\n{rendered_cfg}"
    );
    assert!(!rendered_cfg.contains("``"), "{rendered_cfg}");

    // Valid planned spec passes `docs.specs` and matches `observe_freshness("specs", ...)`.
    fs::write(
        root.join("docs/milestones/m1.md"),
        "---\nid: AC-001\ntitle: Valid Spec\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: build thing\nscope: [src/lib.rs]\n```\n",
    )
    .unwrap();
    assert_check_matches_freshness(&cfg_provider, "specs", "specs_live=", &check_req);
    // Invalid spec fails `docs.specs`.
    fs::write(
        root.join("docs/milestones/BAD-999.md"),
        "---\nid: BAD-999\ntitle: Invalid prefix\nstatus: planned\n---\n",
    )
    .unwrap();
    let specs_fail = cfg_provider
        .run_check("specs", &serde_json::Value::Null, &check_req)
        .unwrap();
    assert!(
        matches!(specs_fail, DomainCheckOutcome::Failed { .. }),
        "invalid spec should fail docs.specs: {specs_fail:?}"
    );
}
