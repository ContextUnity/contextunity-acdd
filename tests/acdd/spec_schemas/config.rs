//! `acdd.yaml` `specs:` registry, confined ref resolution, `.acdd/lint/` merge, and workflow schema seam tests (AC-03).

use acdd_core::config::schema::{RoadmapColorMode, RoadmapDisplayConfig};
use acdd_core::config::{
    default_specs, load_adapter, load_adapter_report, policy, read_policy_from_value, SpecConfig,
};
use acdd_core::errors::ErrorCode;
use acdd_core::format::sha256_hex;
use acdd_core::fs::ScopedTempDir;
use acdd_index::docs::extract_with_details_with_zones;
use acdd_spec::effective_spec_collections;
use acdd_state::validate::{validate_document, validate_workspace};
use acdd_state::workflow::schema::parse_workflow;
use contextunity_acdd::state_bridge::build_domain_providers;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[test]
fn effective_spec_collections_falls_back_to_default_specs() {
    assert_eq!(effective_spec_collections(&BTreeMap::new()), default_specs());
}

#[test]
fn acdd_yaml_specs_ref_base_roadmap_and_lint_dirs_matrix() {
    let dir = ScopedTempDir::create("acdd_cfg_pos").unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".acdd/specs")).unwrap();
    fs::create_dir_all(root.join(".acdd/lint")).unwrap();
    fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
    fs::create_dir_all(root.join("docs/architecture")).unwrap();
    fs::write(
        root.join(".acdd/workflows/default.yaml"),
        "version: 2\ninherit: base\n",
    )
    .unwrap();

    let research_yaml = "\
workflow: code
roadmap: false
prefixes: [RES]
fields:
  type: object
  required: [question]
  properties:
    question: { type: string }
items:
  probes:
    item: probe
    fields:
      type: object
      required: [method]
      properties:
        method: { type: string }
  findings:
    fields:
      evidence: { type: array, items: { type: string } }
";
    fs::write(root.join(".acdd/specs/research.yaml"), research_yaml).unwrap();
    let pin = &sha256_hex(research_yaml.as_bytes())[..12];

    fs::write(
        root.join(".acdd/lint/01-zones.yaml"),
        "\
unused_public_api: true
complexity:
  severity: warn
  invariant: INV-BOUNDED-STRUCTURAL-COMPLEXITY
zones:
  core: [\"crates/core/**\"]
  app: [\"src/app/**\"]
",
    )
    .unwrap();
    fs::write(
        root.join(".acdd/lint/02-rules.yaml"),
        "\
rules:
  - id: INV-APP-NO-CORE
    severity: error
    kind: forbid_dependency
    from: { zones: [app] }
    to: { zones: [core] }
",
    )
    .unwrap();

    fs::write(
        root.join("acdd.yaml"),
        format!(
            "\
roots: [.]
specs_dirs: [.acdd/specs]
lint_dirs: [.acdd/lint]
specs:
  milestones:
    base: contract
    path: docs/milestones
    prefixes: [AC]
    digits: 3
    roadmap: true
    fields:
      services: {{ type: array, items: {{ type: string }} }}
      priority: {{ type: string, enum: [high, medium, low] }}
  memories:
    base: memory
    path: docs/memories
  research:
    ref: research
    path: docs/research
    fields:
      owner: {{ type: string }}
  pinned:
    ref: .acdd/specs/research.yaml:{pin}
    path: extensions/*/docs/research
    prefixes: [PIN]
roadmap:
  badges: [services, priority, readiness]
  group_by: wave
  color: always
"
        ),
    )
    .unwrap();

    let adapter = load_adapter(root, None).expect("valid acdd.yaml + specs_dirs + lint_dirs");
    assert_eq!(adapter.specs_dirs, vec![".acdd/specs"]);
    assert_eq!(adapter.lint_dirs, vec![".acdd/lint"]);
    assert_eq!(
        adapter.roadmap,
        RoadmapDisplayConfig {
            badges: vec![
                "services".to_owned(),
                "priority".to_owned(),
                "readiness".to_owned(),
            ],
            group_by: Some("wave".to_owned()),
            color: RoadmapColorMode::Always,
        }
    );

    let milestones = &adapter.specs["milestones"];
    assert_eq!(milestones.effective_builtin_base(Some("milestones")), Some("contract"));
    assert!(milestones.effective_roadmap(Some("milestones")));
    assert!(milestones
        .fields
        .as_ref()
        .is_some_and(|s| s.properties.contains_key("services") && s.properties.contains_key("priority")));

    let memories = &adapter.specs["memories"];
    assert_eq!(memories.effective_builtin_base(Some("memories")), Some("memory"));
    assert!(!memories.effective_roadmap(Some("memories")));

    let research = &adapter.specs["research"];
    assert_eq!(research.workflow.as_deref(), Some("code"));
    assert!(!research.effective_roadmap(Some("research")));
    assert_eq!(research.prefixes, vec!["RES"]);
    let r_fields = research.fields.as_ref().expect("merged fields");
    assert!(r_fields.properties.contains_key("question"));
    assert!(r_fields.properties.contains_key("owner"));
    assert_eq!(r_fields.required, vec!["question"]);
    assert_eq!(
        research.items["probes"].effective_item_token("probes"),
        "probe"
    );
    assert_eq!(
        research.items["findings"].effective_item_token("findings"),
        "findings"
    );

    let pinned = &adapter.specs["pinned"];
    assert_eq!(pinned.prefixes, vec!["PIN"]);

    assert!(adapter.architecture.unused_public_api);
    assert_eq!(
        adapter.architecture.complexity.invariant.as_deref(),
        Some("INV-BOUNDED-STRUCTURAL-COMPLEXITY")
    );
    assert_eq!(adapter.architecture.zones.len(), 2);
    assert_eq!(adapter.architecture.rules.len(), 1);
    assert!(!adapter.digest.is_empty());

    // Persisted policy roundtrip preserves specs, specs_dirs, lint_dirs, roadmap, and architecture.
    let persisted = policy(root, &adapter);
    let restored = read_policy_from_value(root, &persisted).expect("roundtrip adapter policy");
    assert_eq!(restored.specs, adapter.specs);
    assert_eq!(restored.specs_dirs, adapter.specs_dirs);
    assert_eq!(restored.lint_dirs, adapter.lint_dirs);
    assert_eq!(restored.roadmap, adapter.roadmap);
    assert_eq!(restored.architecture, adapter.architecture);

    // acdd validate validates workspace + individual lint_dirs and specs_dirs YAML files cleanly.
    let providers = build_domain_providers(root, &adapter, None, Vec::new()).unwrap();
    let ws_val = validate_workspace(root, None, None, &providers).unwrap();
    assert!(ws_val.valid, "{ws_val:?}");
    let lint_val =
        validate_document(root, Path::new(".acdd/lint/02-rules.yaml"), None, None, &providers)
            .unwrap();
    assert!(lint_val.valid, "{lint_val:?}");
    let spec_val =
        validate_document(root, Path::new(".acdd/specs/research.yaml"), None, None, &providers)
            .unwrap();
    assert!(spec_val.valid, "{spec_val:?}");

    // Broken lint_dirs fails validate_workspace closed.
    fs::write(root.join(".acdd/lint/03-dup.yaml"), "zones:\n  core: [\"x/**\"]\n").unwrap();
    let bad_val = validate_workspace(root, None, None, &providers).unwrap();
    assert!(!bad_val.valid, "{bad_val:?}");
    fs::remove_file(root.join(".acdd/lint/03-dup.yaml")).unwrap();

    // acdd-index docs extractor handles glob spec_paths without acdd-state coupling.
    let sections = extract_with_details_with_zones(
        "extensions/pkg/docs/research/PIN-001.md",
        "---\ntitle: Probe\n---\n# Probe\n",
        0.0,
        &adapter.spec_paths(),
        None,
    )
    .unwrap();
    assert_eq!(sections[0].0.doc_type, "contract");

    // Live repository acdd.yaml + .acdd/lint/ loads and passes strict validation.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_report = load_adapter_report(repo_root, None).expect("live repo acdd.yaml + .acdd/lint");
    assert!(repo_report.linked_workspace_errors.is_empty());
    assert!(!repo_report.adapter.architecture.zones.is_empty());
    assert!(!repo_report.adapter.architecture.rules.is_empty());
}

#[test]
fn acdd_yaml_specs_lint_and_workflow_fail_closed_matrix() {
    let dir = ScopedTempDir::create("acdd_cfg_neg").unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".acdd/specs")).unwrap();
    fs::create_dir_all(root.join(".acdd/lint")).unwrap();

    // 1. Inline architecture: in acdd.yaml is rejected (no legacy shim).
    fs::write(
        root.join("acdd.yaml"),
        "roots: [.]\narchitecture:\n  zones:\n    core: [\"src/**\"]\n",
    )
    .unwrap();
    let err = load_adapter(root, None).unwrap_err();
    assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid);

    // 2. Spec config validation failures (multi-hop base, base+ref, non-object schema, bad pin, traversal).
    let spec_cases: &[(&str, &str)] = &[
        (
            "multi_hop_base_value",
            "specs:\n  a:\n    base: custom\n    path: docs/a\n",
        ),
        (
            "base_and_ref_together",
            "specs:\n  a:\n    base: contract\n    ref: ok\n    path: docs/a\n",
        ),
        (
            "non_object_fields_schema",
            "specs:\n  a:\n    path: docs/a\n    fields:\n      type: string\n",
        ),
        (
            "ref_path_traversal",
            "specs:\n  a:\n    ref: ../escape.yaml\n    path: docs/a\n",
        ),
        (
            "ref_pin_mismatch",
            "specs:\n  a:\n    ref: .acdd/specs/ok.yaml:0000000\n    path: docs/a\n",
        ),
        (
            "external_spec_multi_hop_base",
            "specs:\n  a:\n    ref: bad_base\n    path: docs/a\n",
        ),
        (
            "external_spec_multi_hop_ref",
            "specs:\n  a:\n    ref: bad_ref\n    path: docs/a\n",
        ),
    ];
    fs::write(root.join(".acdd/specs/ok.yaml"), "roadmap: false\n").unwrap();
    fs::write(
        root.join(".acdd/specs/bad_base.yaml"),
        "base: contract\nroadmap: false\n",
    )
    .unwrap();
    fs::write(
        root.join(".acdd/specs/bad_ref.yaml"),
        "ref: ok\nroadmap: false\n",
    )
    .unwrap();

    for &(label, yaml) in spec_cases {
        fs::write(root.join("acdd.yaml"), yaml).unwrap();
        let err = load_adapter(root, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "{label}: {err}");
    }

    // 3. .acdd/lint/*.yaml collision matrix (duplicate zone, duplicate rule.id, conflicting scalar, traversal).
    fs::write(root.join("acdd.yaml"), "roots: [.]\n").unwrap();
    let lint_collision_cases: &[(&str, &str, &str)] = &[
        (
            "duplicate_zone",
            "zones:\n  core: [\"src/a/**\"]\n",
            "zones:\n  core: [\"src/b/**\"]\n",
        ),
        (
            "duplicate_rule_id",
            "zones:\n  core: [\"src/a/**\"]\nrules:\n  - id: R1\n    kind: doc_integrity\n",
            "rules:\n  - id: R1\n    kind: doc_integrity\n",
        ),
        (
            "conflicting_complexity_scalar",
            "complexity:\n  severity: warn\n",
            "complexity:\n  severity: error\n",
        ),
        (
            "conflicting_unused_public_api_scalar",
            "unused_public_api: true\n",
            "unused_public_api: false\n",
        ),
    ];
    for &(label, a_yaml, b_yaml) in lint_collision_cases {
        fs::write(root.join(".acdd/lint/a.yaml"), a_yaml).unwrap();
        fs::write(root.join(".acdd/lint/b.yaml"), b_yaml).unwrap();
        let err = load_adapter(root, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid, "{label}: {err}");
    }
    fs::remove_file(root.join(".acdd/lint/a.yaml")).unwrap();
    fs::remove_file(root.join(".acdd/lint/b.yaml")).unwrap();

    fs::write(
        root.join("acdd.yaml"),
        "roots: [.]\nlint_dirs: [../outside]\n",
    )
    .unwrap();
    assert_eq!(
        load_adapter(root, None).unwrap_err().code,
        ErrorCode::WorkflowSchemaInvalid
    );

    // 4. Symlinked spec ref and lint file fail closed.
    #[cfg(unix)]
    {
        let outside = ScopedTempDir::create("acdd_cfg_out").unwrap();
        fs::write(outside.path().join("ext.yaml"), "roadmap: false\n").unwrap();
        fs::write(outside.path().join("lint.yaml"), "zones:\n  x: [\"src/**\"]\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("ext.yaml"),
            root.join(".acdd/specs/sym.yaml"),
        )
        .unwrap();
        fs::write(
            root.join("acdd.yaml"),
            "specs:\n  a:\n    ref: sym\n    path: docs/a\n",
        )
        .unwrap();
        assert_eq!(
            load_adapter(root, None).unwrap_err().code,
            ErrorCode::SpecSchemaInvalid
        );

        std::os::unix::fs::symlink(
            outside.path().join("lint.yaml"),
            root.join(".acdd/lint/sym.yaml"),
        )
        .unwrap();
        fs::write(root.join("acdd.yaml"), "roots: [.]\n").unwrap();
        assert_eq!(
            load_adapter(root, None).unwrap_err().code,
            ErrorCode::WorkflowSchemaInvalid
        );
        fs::remove_file(root.join(".acdd/lint/sym.yaml")).unwrap();
    }

    // 5. Workflow YAML rejects duplicate specs: or roadmap: keys with ACDD_WORKFLOW_SCHEMA_INVALID.
    for bad_wf in [
        "version: 2\ninherit: base\nspecs:\n  milestones:\n    path: docs/milestones\n",
        "version: 2\ninherit: base\nroadmap:\n  color: always\n",
    ] {
        let err = parse_workflow(bad_wf).unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid, "{bad_wf}: {err}");
    }

    // Verify SpecConfig::default() preserves standard defaults.
    let def = SpecConfig::default();
    assert_eq!(def.digits, 3);
    assert!(def.effective_roadmap(None));
}
