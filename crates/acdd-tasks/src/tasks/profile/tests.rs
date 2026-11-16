use super::*;

#[test]
fn proof_schema_validates_additional_properties_recursively() {
    let schema: SchemaDef = serde_yaml::from_str(
        r#"
type: object
required: [tasks]
properties:
  tasks:
    type: object
    additionalProperties:
      type: object
      required: [layer]
      properties:
        layer: { type: string }
"#,
    )
    .unwrap();
    schema
        .validate_definition("proofs.blueprint.schema")
        .unwrap();

    let valid = serde_json::json!({"tasks": {"profile-inheritance": {"layer": "application"}}});
    schema.validate_value(&valid, "proof").unwrap();

    let invalid = serde_json::json!({"tasks": {"profile-inheritance": {"layer": 42}}});
    assert!(schema.validate_value(&invalid, "proof").is_err());
}

#[test]
fn repository_profile_inherits_defaults_and_binds_commands_by_gate_id() {
    let profile = validate_profile_document(include_str!("../../../../../.acdd/profile.yaml"))
        .expect("repository profile should compile over the embedded defaults");

    for stage in ["contract", "build", "seam.integrate"] {
        let command = profile
            .by_stage(stage)
            .and_then(|gate| gate.command.as_deref())
            .expect("gate command");
        assert!(
            profile.commands.contains_key(command),
            "{stage} command binding"
        );
    }
    let verify = profile.find_gate("verify").unwrap();
    assert!(profile
        .commands
        .contains_key(verify.command.as_deref().unwrap()));
    assert!(
        verify.role.is_none(),
        "verification is an observed command gate"
    );
    assert_eq!(verify.driver(), Some(super::GateDriver::Command));
    assert_eq!(verify.proof.kind(), Some(ProofKind::Command));
    assert!(profile.roles.contains_key("architect"));
    assert!(profile.guidance.contains_key("milestone.synthesis"));
}

#[test]
fn executable_paths_expand_templates_and_disambiguate_leaf_aliases() {
    let profile: GateProfile = serde_yaml::from_str(
        r#"
gate_templates:
  cycle:
    gates:
      - id: build
      - id: review
gates:
  - id: first
    use: cycle
  - id: second
    use: cycle
"#,
    )
    .unwrap();
    let paths = profile
        .executable_gate_paths()
        .into_iter()
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "first.build",
            "first.review",
            "second.build",
            "second.review"
        ]
    );
    assert_eq!(profile.index_of("second.review"), Some(3));
    assert!(profile.by_stage("review").is_none());
    assert!(profile.by_stage("unknown.review").is_none());
}

#[test]
fn gate_guidance_renders_structured_criteria() {
    for (entry, expected) in [
        (GuidanceEntry::Text("plain".into()), "plain"),
        (
            GuidanceEntry::Structured {
                description: "empty".into(),
                criteria: vec![],
            },
            "empty",
        ),
        (
            GuidanceEntry::Structured {
                description: "review".into(),
                criteria: vec!["check scope".into(), "check invariants".into()],
            },
            "review\n- check scope\n- check invariants",
        ),
    ] {
        let mut profile = GateProfile::default();
        profile.guidance.insert("task.review".into(), entry.clone());
        let gate: GateDef = serde_yaml::from_str("id: review\nguidance: [task.review]\n").unwrap();
        assert_eq!(entry.rendered_text(), expected);
        assert_eq!(
            profile.resolve_gate_guidance(&gate, &BTreeMap::new()),
            vec![("task.review".into(), expected.into())],
        );
    }
}

#[test]
fn command_scope_contract_matrix() {
    struct Case {
        profile: GateProfile,
        overlay: Option<&'static str>,
        expected: Vec<(Option<&'static str>, &'static str, Option<&'static str>)>,
    }
    let local = parse_profile(
        r#"commands:
  check:
    command: cargo test
    scope_args_by_pattern:
      "crates/acdd-tasks/**/*.rs": "--package acdd-tasks --lib"
"#,
    )
    .unwrap();
    let CommandDef::Detailed {
        scope_args_by_pattern,
        ..
    } = &local.commands["check"]
    else {
        panic!("expected a detailed command");
    };
    assert_eq!(
        scope_args_by_pattern["crates/acdd-tasks/**/*.rs"],
        "--package acdd-tasks --lib"
    );
    let base = parse_profile(
        r#"commands:
  alpha:
    command: echo alpha
  beta:
    command: echo beta
    scope_args_by_pattern:
      "**/*.rs": "--beta-only"
"#,
    )
    .unwrap();
    let cases = [
        Case {
            profile: local,
            overlay: None,
            expected: vec![
                (
                    Some("check"),
                    "crates/acdd-tasks/src/tasks/profile.rs",
                    Some("--package acdd-tasks --lib"),
                ),
                (None, "crates/acdd-tasks/src/tasks/profile.rs", None),
            ],
        },
        Case {
            profile: serde_yaml::from_str(
                r#"scope_args_by_pattern:
  "**/*.rs": "--lib"
  "tests/special/*.rs": "--test special"
  "tests/tie/*.rs": "--test global-tie"
commands:
  check:
    command: cargo test
    scope_args_by_pattern:
      "tests/*.rs": "--test {file_stem}"
      "tests/**/*.rs": "--test nested"
      "checks/*.rs": "--test {file_stem}"
      "tests/tie/*.rs": "--test local-tie"
      "tests/special/deep/*.rs": "--test deep"
"#,
            )
            .unwrap(),
            overlay: None,
            expected: vec![
                (Some("check"), "src/lib.rs", Some("--lib")),
                (Some("check"), "tests/example.rs", Some("--test nested")),
                (
                    Some("check"),
                    "tests/special/case.rs",
                    Some("--test special"),
                ),
                (
                    Some("check"),
                    "tests/special/deep/case.rs",
                    Some("--test deep"),
                ),
                (Some("check"), "tests/tie/case.rs", Some("--test local-tie")),
                (Some("check"), "checks/example.rs", Some("--test example")),
                (Some("check"), "README.md", None),
                (None, "checks/example.rs", Some("--lib")),
                (None, "README.md", None),
                (None, "src/lib.rs", Some("--lib")),
            ],
        },
        Case {
            profile: base.clone(),
            overlay: None,
            expected: vec![
                (None, "src/lib.rs", None),
                (Some("alpha"), "src/lib.rs", None),
                (Some("beta"), "src/lib.rs", Some("--beta-only")),
            ],
        },
        Case {
            profile: base,
            overlay: Some(
                r#"commands:
  beta:
    command: echo replacement
"#,
            ),
            expected: vec![
                (None, "src/lib.rs", None),
                (Some("beta"), "src/lib.rs", None),
            ],
        },
    ];
    for case in cases {
        let mut profile = case.profile;
        if let Some(overlay) = case.overlay {
            profile.merge_override(parse_profile(overlay).unwrap());
        }
        for (command, path, expected) in case.expected {
            let actual = match command {
                Some(command) => {
                    profile.scope_args_for_command_path(&profile.commands[command], path)
                }
                None => profile.scope_args_for_path(path),
            };
            assert_eq!(actual.as_deref(), expected, "{command:?}: {path}");
        }
    }
}

#[test]
fn root_pattern_commands_include_every_matching_language_check() {
    let command = CommandDef::Pattern(PatternCommands {
        by_pattern: BTreeMap::from([
            ("src/*.rs".into(), "check-rust".into()),
            ("src/*.py".into(), "check-python".into()),
            ("docs/*.md".into(), "check-docs".into()),
        ]),
        description: None,
        timeout_seconds: None,
        output: None,
    });

    assert_eq!(
        command.commands_for_paths(&["src/lib.rs", "src/module.py"]),
        ["check-python", "check-rust"]
    );
    assert_eq!(command.commands_for_paths(&["src/lib.rs"]), ["check-rust"]);
}

#[test]
fn profile_structural_merge_matrix() {
    fn child_condition(profile: &GateProfile) -> String {
        profile
            .gates
            .iter()
            .find(|gate| gate.tasks.is_some())
            .unwrap()
            .tasks
            .as_ref()
            .unwrap()
            .gates[0]
            .when
            .as_deref()
            .unwrap()
            .to_owned()
    }
    enum MergeMode {
        Parsed,
        Raw,
        Normalized,
        Document,
    }
    type Expectation = fn(&GateProfile);
    struct Case {
        base: GateProfile,
        initial: Expectation,
        steps: Vec<(&'static str, MergeMode, Expectation)>,
    }
    let cases = [
        Case {
            base: parse_profile(
                r#"inherit: none
version: 2
roles:
  contract_author: {}
  builder: {}
  architect: {}
proofs: {}
gates:
  - id: tasks
    tasks:
      gates:
        - id: contract
          role: contract_author
          guidance: [task.contract]
          expected_failure: { exit_codes: [1] }
        - id: build
          role: builder
          command: old-check
          pass: review
  - id: blueprint
    gates:
      - id: synthesize
        role: architect
"#,
            )
            .unwrap(),
            initial: |_| {},
            steps: vec![(
                r#"inherit: default
gates:
  - id: tasks
    tasks:
      gates:
        - id: contract
          command: new-contract
        - id: build
          command: new-check
  - id: blueprint
    gates:
      - id: synthesize
        guidance: [milestone.synthesis]
"#,
                MergeMode::Parsed,
                |profile| {
                    let tasks = profile.find_gate("tasks").unwrap();
                    let contract = tasks
                        .tasks
                        .as_ref()
                        .unwrap()
                        .gates
                        .iter()
                        .find(|gate| gate.id.rsplit('.').next() == Some("contract"))
                        .unwrap();
                    assert!(contract.role.is_some());
                    assert_eq!(contract.command.as_deref(), Some("new-contract"));
                    let build = tasks
                        .tasks
                        .as_ref()
                        .unwrap()
                        .gates
                        .iter()
                        .find(|gate| gate.id.rsplit('.').next() == Some("build"))
                        .unwrap();
                    assert_eq!(build.command.as_deref(), Some("new-check"));
                    assert!(build.role.is_some());
                    let synthesize = profile.find_gate("synthesize").unwrap();
                    assert!(
                        synthesize
                            .guidance
                            .iter()
                            .any(|guidance| guidance == "milestone.synthesis")
                    );
                    assert_eq!(synthesize.role.as_deref(), Some("architect"));
                },
            )],
        },
        Case {
            base: (*compiled()).clone(),
            initial: |_| {},
            steps: vec![(
                "inherit: default\ngates:\n  - tasks:\n      gates:\n        - id: contract\n          command: test\n        - id: build\n          command: check\n",
                MergeMode::Document,
                |profile| {
                    assert_eq!(
                        profile
                            .gates
                            .iter()
                            .filter(|gate| gate.tasks.is_some())
                            .count(),
                        1,
                        "the id-less tasks container should merge structurally"
                    );
                    let tasks = profile
                        .gates
                        .iter()
                        .find_map(|gate| gate.tasks.as_ref())
                        .unwrap();
                    let contract = tasks
                        .gates
                        .iter()
                        .find(|gate| gate.id == "contract")
                        .unwrap();
                    assert_eq!(contract.command.as_deref(), Some("test"));
                    assert_eq!(contract.role.as_deref(), Some("contract_author"));
                    assert!(
                        contract
                            .guidance
                            .iter()
                            .any(|guidance| guidance == "task.contract")
                    );
                    let build = tasks.gates.iter().find(|gate| gate.id == "build").unwrap();
                    assert_eq!(build.command.as_deref(), Some("check"));
                    assert_eq!(build.role.as_deref(), Some("builder"));
                    assert_eq!(build.proof.kind(), Some(ProofKind::Command));
                },
            )],
        },
        Case {
            base: {
                let mut profile = super::normalize::parse_profile_document(
                    r#"
inherit: none
version: 2
gates:
  - tasks:
      when: code
      gates:
        - id: check
          command: code-check
  - tasks:
      when: doc
      gates:
        - id: check
          command: doc-check
"#,
                )
                .unwrap()
                .profile;
                super::normalize::normalize_profile(&mut profile);
                profile
            },
            initial: |_| {},
            steps: vec![(
                r#"
inherit: none
gates:
  - tasks:
      when: doc
      gates:
        - id: check
          command: doc-check-new
"#,
                MergeMode::Raw,
                |profile| {
                    assert_eq!(profile.gates.len(), 2);
                    let code_check = profile
                        .gates
                        .iter()
                        .find(|gate| {
                            gate.tasks.as_ref().and_then(|tasks| tasks.when.as_deref())
                                == Some("code")
                        })
                        .unwrap()
                        .tasks
                        .as_ref()
                        .unwrap()
                        .gates
                        .iter()
                        .find(|gate| gate.id == "check")
                        .unwrap();
                    assert_eq!(code_check.command.as_deref(), Some("code-check"));
                    let doc_check = profile
                        .gates
                        .iter()
                        .find(|gate| {
                            gate.tasks.as_ref().and_then(|tasks| tasks.when.as_deref())
                                == Some("doc")
                        })
                        .unwrap()
                        .tasks
                        .as_ref()
                        .unwrap()
                        .gates
                        .iter()
                        .find(|gate| gate.id == "check")
                        .unwrap();
                    assert_eq!(doc_check.command.as_deref(), Some("doc-check-new"));
                },
            )],
        },
        Case {
            base: {
                let mut profile = GateProfile::default();
                let base = super::normalize::parse_profile_document(
    "inherit: none\nversion: 2\ncommands:\n  old:\n    command: old-command\ngates:\n  - id: seam\n    gates:\n      - id: integrate\n        command: old\n",
)
.unwrap();
                super::normalize::merge_profile_document(&mut profile, base);
                profile
            },
            initial: |_| {},
            steps: vec![(
                "commands:\n  new:\n    command: new-command\ngates:\n  - id: seam\n    gates:\n      - id: integrate\n        command: new\n",
                MergeMode::Document,
                |profile| {
                    let seam = profile.find_gate("seam").unwrap();
                    assert_eq!(seam.gates.len(), 1);
                    assert_eq!(seam.gates[0].id, "seam.integrate");
                    assert_eq!(seam.gates[0].command.as_deref(), Some("new"));
                },
            )],
        },
        Case {
            base: {
                let mut profile = super::normalize::parse_profile_document(
                    r#"
inherit: none
version: 2
gates:
  - tasks:
      when: code
      gates:
        - id: check
          when: ready
"#,
                )
                .unwrap()
                .profile;
                super::normalize::normalize_profile(&mut profile);
                profile
            },
            initial: |profile| {
                assert_eq!(child_condition(profile), "code & ready");
            },
            steps: vec![
                (
                    "inherit: none\nversion: 2\ngates:\n  - tasks:\n      when: doc\n      gates:\n        - id: check\n          command: check-doc\n",
                    MergeMode::Normalized,
                    |profile| {
                        assert_eq!(child_condition(profile), "doc & ready");
                    },
                ),
                (
                    "inherit: none\nversion: 2\ngates:\n  - tasks:\n      when: release\n      gates:\n        - id: check\n          command: check-release\n",
                    MergeMode::Normalized,
                    |profile| {
                        assert_eq!(child_condition(profile), "release & ready");
                        assert_eq!(
                            profile
                                .gates
                                .iter()
                                .find(|gate| gate.tasks.is_some())
                                .unwrap()
                                .tasks
                                .as_ref()
                                .unwrap()
                                .gates[0]
                                .command
                                .as_deref(),
                            Some("check-release")
                        );
                    },
                ),
            ],
        },
    ];
    for case in cases {
        let mut profile = case.base;
        (case.initial)(&profile);
        for (yaml, mode, expect) in case.steps {
            match mode {
                MergeMode::Document => {
                    let overlay = super::normalize::parse_profile_document(yaml).unwrap();
                    super::normalize::merge_profile_document(&mut profile, overlay);
                }
                MergeMode::Parsed => profile.merge_override(parse_profile(yaml).unwrap()),
                MergeMode::Raw | MergeMode::Normalized => {
                    let mut overlay = super::normalize::parse_profile_document(yaml)
                        .unwrap()
                        .profile;
                    if matches!(mode, MergeMode::Normalized) {
                        super::normalize::normalize_profile(&mut overlay);
                    }
                    profile.merge_override(overlay);
                }
            }
            expect(&profile);
        }
    }
}

#[test]
fn built_in_proof_names_keep_typed_semantics_and_round_trip() {
    for (name, kind) in [
        ("contract", ProofKind::Contract),
        ("command", ProofKind::Command),
        ("review", ProofKind::Review),
        ("seam_test", ProofKind::SeamTest),
        ("delivery", ProofKind::Delivery),
        ("none", ProofKind::None),
    ] {
        let proof: ProofDef = serde_yaml::from_str(name).unwrap();
        assert_eq!(proof.kind(), Some(kind), "{name}");
        assert_eq!(serde_yaml::to_string(&proof).unwrap().trim(), name);
    }

    let named: ProofDef = serde_yaml::from_str("blueprint").unwrap();
    assert!(matches!(&named, ProofDef::Named(name) if name == "blueprint"));
    assert_eq!(serde_yaml::to_string(&named).unwrap().trim(), "blueprint");

    let unspecified = ProofDef::Unspecified;
    let serialized = serde_yaml::to_string(&unspecified).unwrap();
    assert_eq!(
        serde_yaml::from_str::<ProofDef>(&serialized).unwrap(),
        unspecified
    );
}

#[test]
fn compiled_default_profile_normalizes_omitted_delivery_proof() {
    assert_eq!(
        compiled().find_gate("handoff").unwrap().proof.kind(),
        Some(ProofKind::Delivery)
    );
}

#[test]
fn merge_reinfers_proof_from_the_final_gate_attributes() {
    let mut profile = parse_profile(
        "inherit: none\nversion: 2\nroles:\n  builder: {}\ngates:\n  - id: check_gate\n    role: builder\n",
    )
    .unwrap();
    super::normalize::normalize_profile(&mut profile);
    assert_eq!(
        profile.find_gate("check_gate").unwrap().proof.kind(),
        Some(ProofKind::None)
    );

    let overlay = parse_profile(
        "commands:\n  check:\n    command: check-command\ngates:\n  - id: check_gate\n    command: check\n",
    )
    .unwrap();
    profile.merge_override(overlay);

    assert_eq!(
        profile.find_gate("check_gate").unwrap().proof.kind(),
        Some(ProofKind::Command)
    );
}

#[test]
fn profile_field_override_matrix() {
    type Expectation = fn(&GateProfile);
    let cases: [(&str, &str, Expectation); 4] = [
        (
            "inherit: none\ngates:\n  - id: delivery\n    auto_commit: false\n",
            "gates:\n  - id: delivery\n    command: check\n",
            |profile| {
                assert!(!profile.find_gate("delivery").unwrap().auto_commit);
            },
        ),
        (
            "gates:\n  - id: delivery\n",
            "gates:\n  - id: delivery\n    auto_commit: false\n",
            |profile| {
                assert!(!profile.find_gate("delivery").unwrap().auto_commit);
            },
        ),
        (
            "inherit: none\ngates:\n  - id: build\n    save:\n      pre: snapshot\n      post: commit\n      immutable: true\n",
            "gates:\n  - id: build\n    save:\n      post: snapshot\n",
            |profile| {
                let save = &profile.find_gate("build").unwrap().persistence;
                assert_eq!(save.pre, Some(PersistenceAction::Snapshot));
                assert_eq!(save.post, Some(PersistenceAction::Snapshot));
                assert!(save.immutable);
            },
        ),
        (
            "inherit: none\ngates:\n  - id: build\n    symbols: [fn:old]\n",
            "gates:\n  - id: build\n    symbols: [fn:new]\n",
            |profile| {
                assert_eq!(profile.find_gate("build").unwrap().symbols, ["fn:new"]);
            },
        ),
    ];
    for (base, overlay, expect) in cases {
        let mut profile = parse_profile(base).unwrap();
        profile.merge_override(parse_profile(overlay).unwrap());
        expect(&profile);
    }
}

#[test]
fn named_proof_catalog_matrix() {
    for (yaml, expected_error) in [
        (
            r#"version: 2
proofs:
  blueprint:
    schema:
      type: object
      required: [title]
      properties:
        title: { type: string }
gates:
  - id: synthesize
    proof: blueprint
"#,
            None,
        ),
        (
            r#"inherit: none
version: 2
roles:
  architect: {}
gates:
  - id: synthesize
    role: architect
    proof: absent
"#,
            Some("absent"),
        ),
    ] {
        let mut profile = parse_profile(yaml).unwrap();
        if let Some(name) = expected_error {
            let error = validate_profile_document(yaml).unwrap_err();
            let chain = format!("{error:#}");
            assert!(chain.contains("ACDD_PROFILE_SCHEMA_INVALID"));
            assert!(chain.contains(name));
        } else {
            assert!(matches!(
                &profile.find_gate("synthesize").unwrap().proof,
                ProofDef::Named(name) if name == "blueprint"
            ));
            super::normalize::normalize_profile(&mut profile);
            let gate = profile.find_gate("synthesize").unwrap();
            assert!(
                matches!(&gate.proof, ProofDef::ResolvedNamed { name, .. } if name == "blueprint")
            );
            let schema = gate.proof.resolve_schema(&profile.proofs).unwrap();
            schema
                .validate_value(&serde_json::json!({"title":"ok"}), "$")
                .unwrap();
            assert!(schema
                .validate_value(&serde_json::json!({"title":1}), "$")
                .is_err());
        }
    }
}

#[test]
fn save_alias_and_scalar_deserialization_works() {
    let yaml = r#"
gates:
  - id: gate1
    command: test_cmd
    save: commit
  - id: gate2
    command: test_cmd
    save: snapshot
  - id: gate3
    command: test_cmd
    save: logfile
  - id: gate4
    command: test_cmd
    save: none
  - id: gate5
    command: test_cmd
    save:
      pre: snapshot
      post: commit
"#;
    let profile = parse_profile(yaml).unwrap();
    assert_eq!(
        profile.gates[0].persistence.post,
        Some(PersistenceAction::Commit)
    );
    assert_eq!(
        profile.gates[1].persistence.post,
        Some(PersistenceAction::Snapshot)
    );
    assert_eq!(
        profile.gates[2].persistence.post,
        Some(PersistenceAction::Logfile)
    );
    assert_eq!(
        profile.gates[3].persistence.post,
        Some(PersistenceAction::None)
    );
    assert_eq!(
        profile.gates[4].persistence.pre,
        Some(PersistenceAction::Snapshot)
    );
    assert_eq!(
        profile.gates[4].persistence.post,
        Some(PersistenceAction::Commit)
    );
}

#[test]
fn loop_absorb_and_need_aliases_work() {
    let yaml = r#"
gates:
  - id: review
    role: reviewer
    loop:
      scope: build
      symbols: contract
  - id: deliver
    receipt: true
    need:
      - review
  - id: handoff
    command: test_cmd
    absorb: true
"#;
    let profile = parse_profile(yaml).unwrap();
    let loop_route = profile.gates[0].reject_routing.as_ref().unwrap();
    assert_eq!(loop_route.scope.as_deref(), Some("build"));
    assert_eq!(loop_route.symbols.as_deref(), Some("contract"));
    assert_eq!(profile.gates[1].review_sources, vec!["review"]);
    assert!(profile.gates[2].absorb_working_tree);
}

#[test]
fn commands_for_root_ignores_directory_names_and_discovers_untracked_nested_files() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    let cmd = CommandDef::Pattern(PatternCommands {
        by_pattern: BTreeMap::from([
            ("**/*.rs".into(), "rust-check".into()),
            ("**/*.py".into(), "python-check".into()),
        ]),
        description: None,
        timeout_seconds: None,
        output: None,
    });
    assert_eq!(cmd.commands_for_root(r), Vec::<&str>::new());
    std::fs::create_dir(r.join("directory.rs")).unwrap();
    assert_eq!(cmd.commands_for_root(r), Vec::<&str>::new());
    std::fs::remove_dir(r.join("directory.rs")).unwrap();

    let _ = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(r)
        .status();
    std::fs::write(r.join("README.md"), "readme").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "README.md"])
        .current_dir(r)
        .status();
    std::fs::create_dir(r.join("nested")).unwrap();
    std::fs::write(r.join("nested/source.py"), "print(1)").unwrap();
    assert_eq!(cmd.commands_for_root(r), vec!["python-check"]);
}

#[test]
fn duplicate_sibling_gate_ids_within_single_layer_are_rejected() {
    let top_level_dup =
        "version: 2\ngates:\n  - id: verify\n    proof: none\n  - id: verify\n    proof: none\n";
    let Err(err) = super::normalize::parse_profile_document(top_level_dup) else {
        panic!("expected duplicate top-level gate id error");
    };
    let err = err.to_string();
    assert!(
        err.contains("ACDD_PROFILE_INVALID: duplicate gate id"),
        "{err}"
    );

    let nested_dup = "version: 2\ngates:\n  - id: review_group\n    gates:\n      - id: review\n        proof: none\n      - id: review\n        proof: none\n";
    let Err(err) = super::normalize::parse_profile_document(nested_dup) else {
        panic!("expected duplicate nested gate id error");
    };
    let err = err.to_string();
    assert!(
        err.contains("ACDD_PROFILE_INVALID: duplicate gate id"),
        "{err}"
    );
}

#[test]
fn pinned_profile_retains_workspace_config_metadata_limit_over_inherit_none() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".acdd")).unwrap();
    std::fs::write(
        root.join("acdd.yaml"),
        "roots: [.]\nlimits:\n  max_metadata_bytes: 512\n",
    )
    .unwrap();
    let pinned_yaml = "\
version: 2
inherit: none
limits:
  max_metadata_bytes: 4096
roles:
  worker: {}
gates:
  - id: handoff
    role: worker
    receipt: true
    proof: delivery
";
    let digest = format!("{:x}", Sha256::digest(pinned_yaml.as_bytes()));
    let pin_rel = ".acdd/pin.yaml";
    std::fs::write(root.join(pin_rel), pinned_yaml).unwrap();
    let reference = format!("{pin_rel}:{}", &digest[..12]);
    let loaded = super::load_pinned_over(root, &reference, &super::compiled()).unwrap();
    assert_eq!(loaded.limits.max_metadata_bytes, 512);
}
