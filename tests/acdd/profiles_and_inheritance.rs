use super::support::TestProvider;
use crate::common::Workspace;
use acdd_core::errors::ErrorCode;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_state::workflow::{
    embedded_workflow, render_command_with_test_filter, resolve_command_for_scope,
    resolve_workflow_from_str_with_dirs, resolve_workflow_ref_with_dirs,
    schema::{DetailedCommandDef, PatternCommandEntry, TestFilterConfig},
    validate_workflow, CommandDef, WorkflowValidationOptions,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn embedded_profiles_inheritance_pinning_and_patch_merge_matrix() {
    let providers = TestProvider::full_composite();
    let code_disabled = SubsystemRegistry {
        disabled: BTreeSet::from(["code".to_owned()]),
        ..SubsystemRegistry::default()
    };
    let docs_disabled = SubsystemRegistry {
        disabled: BTreeSet::from(["docs".to_owned()]),
        ..SubsystemRegistry::default()
    };

    // 1. Embedded workflows matrix (`base`, `code`, `documentation`; removed `"workspace"`, `"in_place"`, `"audit"`, `"spec_planning"` rejected).
    for removed in ["workspace", "in_place", "audit", "spec_planning"] {
        assert!(embedded_workflow(removed).is_none());
        let err = resolve_workflow_ref_with_dirs(removed, None, None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid, "{removed}");
    }

    type EmbeddedCase<'a> = (
        &'a str,
        Option<&'a SubsystemRegistry>,
        bool,
        Result<(), ErrorCode>,
    );
    let embedded_cases: &[EmbeddedCase<'_>] = &[
        ("base", Some(&code_disabled), false, Ok(())),
        ("base", None, true, Ok(())),
        ("code", None, true, Err(ErrorCode::CommandUnbound)),
        ("code", None, false, Ok(())),
        (
            "code",
            Some(&code_disabled),
            false,
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
        ("documentation", None, true, Ok(())),
        ("documentation", None, false, Ok(())),
        (
            "documentation",
            Some(&code_disabled),
            false,
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
        (
            "documentation",
            Some(&docs_disabled),
            false,
            Err(ErrorCode::DomainCapabilityUnavailable),
        ),
    ];
    for &(name, mods, require_bound, expected) in embedded_cases {
        assert!(embedded_workflow(name).is_some());
        let resolved =
            resolve_workflow_ref_with_dirs(name, None, mods, None).expect("embedded resolves");
        let opts = WorkflowValidationOptions {
            domain_providers: Some(providers.as_ref()),
            acdd_modules: mods,
            require_bound_commands: require_bound,
        };
        let actual = validate_workflow(&resolved, &opts).map_err(|e| e.code);
        assert_eq!(actual, expected, "embedded workflow {name}");
    }

    // 2. Local pinned inheritance (`inherit: <path>:<sha256_prefix>`) + `gates_patch:`.
    let ws = Workspace::new();
    let parent_yaml = r#"version: 2
inherit: code
commands:
  test:
    command: "cargo test {test_filter}"
    test_filter: "--exact {test}"
  build_check:
    command: "cargo test"
  verify_all:
    command: "cargo test --workspace"
"#;
    ws.write(".acdd/workflows/parent.yaml", parent_yaml);
    let parent_sha = hex::encode(Sha256::digest(parent_yaml.as_bytes()));
    let good_pin = &parent_sha[..12];

    let child_ok = format!(
        r#"version: 2
inherit: ".acdd/workflows/parent.yaml:{good_pin}"
gates_patch:
  - id: tasks.build
    tools_add: [http_ping]
    tools_remove: [ast_grep_search]
"#
    );
    let resolved_child = resolve_workflow_from_str_with_dirs(
        &child_ok,
        "child-ok",
        Some(".acdd/workflows/child-ok.yaml"),
        Some(ws.root()),
        None,
        None,
    )
    .unwrap();
    validate_workflow(
        &resolved_child,
        &WorkflowValidationOptions {
            domain_providers: Some(providers.as_ref()),
            acdd_modules: None,
            require_bound_commands: true,
        },
    )
    .expect("pinned child with http_ping plugin tool must validate");

    let tasks_block = resolved_child
        .gates
        .iter()
        .find_map(|g| g.tasks.as_ref())
        .expect("tasks block present");
    let build_gate = tasks_block
        .gates
        .iter()
        .find(|g| g.id == "build")
        .expect("build gate present");
    let build_tools = resolved_child.effective_tools(build_gate);
    assert!(build_tools.contains(&"http_ping".to_owned()));
    assert!(!build_tools.contains(&"ast_grep_search".to_owned()));

    // Bad SHA pin and cyclic inherit fail closed with WorkflowSchemaInvalid.
    let bad_pin_yaml = "version: 2\ninherit: \".acdd/workflows/parent.yaml:deadbeefcafe\"\n";
    let err_pin = resolve_workflow_from_str_with_dirs(
        bad_pin_yaml,
        "bad-pin",
        None,
        Some(ws.root()),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err_pin.code, ErrorCode::WorkflowSchemaInvalid);

    ws.write(
        ".acdd/workflows/cycle-a.yaml",
        "version: 2\ninherit: .acdd/workflows/cycle-b.yaml\n",
    );
    ws.write(
        ".acdd/workflows/cycle-b.yaml",
        "version: 2\ninherit: .acdd/workflows/cycle-a.yaml\n",
    );
    let err_cycle =
        resolve_workflow_ref_with_dirs(".acdd/workflows/cycle-a.yaml", Some(ws.root()), None, None)
            .unwrap_err();
    assert_eq!(err_cycle.code, ErrorCode::WorkflowSchemaInvalid);

    // 3. Reject legacy 0.5 keys and legacy tool names fail-closed.
    for (label, bad_yaml) in [
        (
            "legacy_persistence",
            "version: 2\ninherit: none\npersistence: commit\n",
        ),
        (
            "legacy_tool",
            "version: 2\ninherit: none\ndefault_tools: [session_checkpoint]\n",
        ),
        (
            "unknown_plugin_tool",
            "version: 2\ninherit: none\ndefault_tools: [sql_query]\n",
        ),
    ] {
        let res =
            resolve_workflow_from_str_with_dirs(bad_yaml, label, None, Some(ws.root()), None, None)
                .and_then(|p| {
                    validate_workflow(
                        &p,
                        &WorkflowValidationOptions {
                            domain_providers: Some(providers.as_ref()),
                            acdd_modules: None,
                            require_bound_commands: false,
                        },
                    )
                });
        let err = res.expect_err(label);
        assert!(
            matches!(
                err.code,
                ErrorCode::WorkflowSchemaInvalid | ErrorCode::DomainCapabilityUnavailable
            ),
            "{label}: unexpected code {:?}",
            err.code
        );
    }

    // 4. CLI AC-02 / AC-04 cutover matrix: `acdd init`, `preset: spec` (`validate`, `roadmap --spec`, `draft <spec>`),
    // `start --workflow` + `sync --rebind-workflow`, and fail-closed schema/flag rejection.
    let cli_ws = Workspace::new();
    let cli_root = cli_ws.root();
    super::support::init_git_repo_on_branch(cli_root, "main");
    let init_out = crate::common::cli::run_cli(&cli_ws, &["init"]);
    assert!(
        init_out.status.success(),
        "{}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    assert!(cli_root.join(".acdd/workflows/default.yaml").is_file());
    let init_cfg = std::fs::read_to_string(cli_root.join("acdd.yaml")).unwrap();
    assert!(
        init_cfg.contains("preset: full") && init_cfg.contains(".acdd/workflows"),
        "{init_cfg}"
    );

    cli_ws.write(
        "acdd.yaml",
        "preset: spec\nworkflows_dirs:\n  - .acdd/workflows\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    );
    cli_ws.write(
        ".acdd/workflows/default.yaml",
        "version: 2\ninherit: none\nsave: none\ngates:\n  - tasks:\n      gates:\n        - id: build\n  - id: handoff\n",
    );
    cli_ws.write(
        "docs/milestones/AC-001-cutover.md",
        "---\nid: AC-001\ntitle: Cutover Spec\ndoc_type: contract\nstatus: planned\n---\n### task: t1\n```yaml\ntask_ref: t1\ntarget: Check\nscope: [src/**]\n```\n",
    );
    for args in [
        vec!["validate"],
        vec!["roadmap", "--spec", "milestones"],
        vec!["draft", "milestones", "cutover-next"],
    ] {
        let out = crate::common::cli::run_cli(&cli_ws, &args);
        assert!(
            out.status.success(),
            "{args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(
        !cli_root.join(".acdd/state.sqlite").exists(),
        "preset: spec CLI validate/roadmap/draft must not create state.sqlite"
    );

    // Unknown `acdd.yaml` keys fail closed with canonical AcddError exit 2 across CLI commands.
    cli_ws.write("acdd.yaml", "unknown_field: 1\n");
    let invalid_cfg_validate = crate::common::cli::run_cli(&cli_ws, &["validate"]);
    assert!(!invalid_cfg_validate.status.success());
    let invalid_cfg_roadmap = crate::common::cli::run_cli(&cli_ws, &["roadmap"]);
    assert_eq!(
        invalid_cfg_roadmap.status.code(),
        Some(2),
        "invalid acdd.yaml must preserve typed AcddError exit code 2: stderr={}",
        String::from_utf8_lossy(&invalid_cfg_roadmap.stderr)
    );

    cli_ws.write(
        "acdd.yaml",
        "preset: full\nworkflows_dirs:\n  - .acdd/workflows\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\n",
    );
    let start_out = crate::common::cli::run_cli_json(
        &cli_ws,
        &[
            "start",
            "docs/milestones/AC-001-cutover.md",
            "--workflow",
            ".acdd/workflows/default.yaml",
            "--in-place",
        ],
    );
    assert!(
        start_out.status.success(),
        "{}",
        String::from_utf8_lossy(&start_out.stderr)
    );
    let sync_out = crate::common::cli::run_cli_json(&cli_ws, &["sync", "--rebind-workflow"]);
    assert!(
        sync_out.status.success(),
        "{}",
        String::from_utf8_lossy(&sync_out.stderr)
    );
}

#[test]
fn command_pattern_specificity_and_test_filter_quoting_matrix() {
    let cmd = CommandDef::Pattern(vec![
        PatternCommandEntry {
            pattern: "crates/**".to_owned(),
            command: "cargo test -p all {test_filter}".to_owned(),
            description: None,
            test_filter: Some(TestFilterConfig::Template("--exact {test}".to_owned())),
            timeout_seconds: None,
            scope_args_by_pattern: BTreeMap::new(),
        },
        PatternCommandEntry {
            pattern: "crates/acdd-state/src/*.rs".to_owned(),
            command: "cargo test -p acdd-state {test_filter}".to_owned(),
            description: None,
            test_filter: Some(TestFilterConfig::Template("--exact {test}".to_owned())),
            timeout_seconds: None,
            scope_args_by_pattern: BTreeMap::new(),
        },
    ]);
    let picked = resolve_command_for_scope(
        &cmd,
        &[
            "crates/acdd-state/src/spec.rs".to_owned(),
            "crates/acdd-core/src/lib.rs".to_owned(),
        ],
    )
    .unwrap();
    // Deterministic lexicographic execution order by matched path (`acdd-core` before `acdd-state`).
    assert_eq!(picked.len(), 2);
    assert!(picked[0].command.contains("cargo test -p all"));
    assert!(picked[1].command.contains("cargo test -p acdd-state"));

    let rendered = render_command_with_test_filter(
        "cargo test {test_filter}",
        Some(&TestFilterConfig::Template("--exact {test}".to_owned())),
        Some("suite::it's_ok"),
    )
    .unwrap();
    assert_eq!(rendered, "cargo test --exact 'suite::it'\\''s_ok'");

    let chained_err =
        render_command_with_test_filter("cargo clippy && cargo test", None, Some("suite::x"))
            .unwrap_err();
    assert_eq!(
        chained_err.code,
        ErrorCode::CommandTestFilterPlaceholderRequired
    );

    let disabled_err = render_command_with_test_filter(
        "cargo test",
        Some(&TestFilterConfig::Enabled(false)),
        Some("suite::x"),
    )
    .unwrap_err();
    assert_eq!(disabled_err.code, ErrorCode::CommandTestFilterUnsupported);

    let detailed = DetailedCommandDef {
        command: "echo {test_filter}".to_owned(),
        description: None,
        timeout_seconds: None,
        test_filter: None,
        scope_args_by_pattern: BTreeMap::new(),
    };
    let no_filter = render_command_with_test_filter(&detailed.command, None, None).unwrap();
    assert_eq!(no_filter.trim(), "echo");
}
