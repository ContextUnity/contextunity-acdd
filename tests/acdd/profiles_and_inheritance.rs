use super::support::TestProvider;
use crate::common::Workspace;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::ErrorCode;
use acdd_state::profile::{
    embedded_profile, render_command_with_test_filter, resolve_command_for_scope,
    resolve_profile_from_str_with_dirs, resolve_profile_ref_with_dirs,
    schema::{DetailedCommandDef, PatternCommandEntry, TestFilterConfig},
    validate_profile, CommandDef, ProfileValidationOptions,
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

    // 1. Embedded profiles matrix (`base`, `code`, `audit`, `documentation`; removed `"workspace"` / `"in_place"` rejected).
    for removed in ["workspace", "in_place"] {
        assert!(embedded_profile(removed).is_none());
        let err = resolve_profile_ref_with_dirs(removed, None, None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{removed}");
    }

    type EmbeddedCase<'a> = (&'a str, Option<&'a SubsystemRegistry>, bool, Result<(), ErrorCode>);
    let embedded_cases: &[EmbeddedCase<'_>] = &[
        ("base", Some(&code_disabled), false, Ok(())),
        ("base", None, true, Ok(())),
        ("code", None, true, Err(ErrorCode::CommandUnbound)),
        ("code", None, false, Ok(())),
        ("code", Some(&code_disabled), false, Err(ErrorCode::DomainCapabilityUnavailable)),
        ("audit", None, true, Ok(())),
        ("audit", Some(&code_disabled), false, Ok(())),
        ("documentation", None, true, Ok(())),
        ("documentation", None, false, Ok(())),
        ("documentation", Some(&code_disabled), false, Err(ErrorCode::DomainCapabilityUnavailable)),
        ("documentation", Some(&docs_disabled), false, Err(ErrorCode::DomainCapabilityUnavailable)),
        ("spec_planning", None, true, Ok(())),
        ("spec_planning", None, false, Ok(())),
        ("spec_planning", Some(&code_disabled), false, Ok(())),
        ("spec_planning", Some(&docs_disabled), false, Ok(())),
    ];
    for &(name, mods, require_bound, expected) in embedded_cases {
        assert!(embedded_profile(name).is_some());
        let resolved = resolve_profile_ref_with_dirs(name, None, mods, None).expect("embedded resolves");
        let opts = ProfileValidationOptions {
            domain_providers: Some(providers.as_ref()),
            acdd_modules: mods,
            require_bound_commands: require_bound,
        };
        let actual = validate_profile(&resolved, &opts).map_err(|e| e.code);
        assert_eq!(actual, expected, "embedded profile {name}");
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
    ws.write(".acdd/profiles/parent.yaml", parent_yaml);
    let parent_sha = hex::encode(Sha256::digest(parent_yaml.as_bytes()));
    let good_pin = &parent_sha[..12];

    let child_ok = format!(
        r#"version: 2
inherit: ".acdd/profiles/parent.yaml:{good_pin}"
gates_patch:
  - id: tasks.build
    tools_add: [http_ping]
    tools_remove: [ast_grep_search]
"#
    );
    let resolved_child = resolve_profile_from_str_with_dirs(
        &child_ok,
        "child-ok",
        Some(".acdd/profiles/child-ok.yaml"),
        Some(ws.root()),
        None,
        None,
    )
    .unwrap();
    validate_profile(
        &resolved_child,
        &ProfileValidationOptions {
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

    // Bad SHA pin and cyclic inherit fail closed with ProfileSchemaInvalid.
    let bad_pin_yaml = "version: 2\ninherit: \".acdd/profiles/parent.yaml:deadbeefcafe\"\n";
    let err_pin = resolve_profile_from_str_with_dirs(
        bad_pin_yaml,
        "bad-pin",
        None,
        Some(ws.root()),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(err_pin.code, ErrorCode::ProfileSchemaInvalid);

    ws.write(
        ".acdd/profiles/cycle-a.yaml",
        "version: 2\ninherit: .acdd/profiles/cycle-b.yaml\n",
    );
    ws.write(
        ".acdd/profiles/cycle-b.yaml",
        "version: 2\ninherit: .acdd/profiles/cycle-a.yaml\n",
    );
    let err_cycle =
        resolve_profile_ref_with_dirs(".acdd/profiles/cycle-a.yaml", Some(ws.root()), None, None)
            .unwrap_err();
    assert_eq!(err_cycle.code, ErrorCode::ProfileSchemaInvalid);

    // 3. Reject legacy 0.5 keys and legacy tool names fail-closed.
    for (label, bad_yaml) in [
        ("legacy_persistence", "version: 2\ninherit: none\npersistence: commit\n"),
        (
            "legacy_tool",
            "version: 2\ninherit: none\ndefault_tools: [session_checkpoint]\n",
        ),
        (
            "unknown_plugin_tool",
            "version: 2\ninherit: none\ndefault_tools: [sql_query]\n",
        ),
    ] {
        let res = resolve_profile_from_str_with_dirs(bad_yaml, label, None, Some(ws.root()), None, None)
            .and_then(|p| {
            validate_profile(
                &p,
                &ProfileValidationOptions {
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
                ErrorCode::ProfileSchemaInvalid | ErrorCode::DomainCapabilityUnavailable
            ),
            "{label}: unexpected code {:?}",
            err.code
        );
    }
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

    let chained_err = render_command_with_test_filter(
        "cargo clippy && cargo test",
        None,
        Some("suite::x"),
    )
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
