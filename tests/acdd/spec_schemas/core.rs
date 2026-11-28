//! Core `SpecDocument` parsing and built-in `contract` / `memory` schema seam tests (Wave 1).

use acdd_core::errors::ErrorCode;
use acdd_core::ids::{RunId, RunName};
use acdd_core::schema::SchemaDef;
use acdd_spec::schema::{builtin_contract_schema, builtin_spec_schema};
use acdd_spec::spec::parse_spec_with_schema;
use acdd_spec::{
    materialize_ephemeral_goal_spec_with_options, parse_spec, SpecStatus,
};
use serde_json::json;

#[test]
fn default_contract_spec_without_explicit_spec_key_parses() {
    let raw = "---\nid: AC-999\ntitle: Baseline Contract\nstatus: planned\nowners: [crates/acdd-spec/src/]\ninvariants: [INV-1]\n---\n\n### task: t1\n```yaml\ntask_ref: t1\nscope: [crates/acdd-spec/src/]\nacceptance_criteria: [ok]\n```\n";
    let doc = parse_spec(raw).expect("default contract spec should parse");
    assert_eq!(doc.id, "AC-999");
    assert_eq!(doc.spec, None);
    assert_eq!(doc.status, SpecStatus::Planned);
    assert_eq!(doc.owners, vec!["crates/acdd-spec/src/"]);
    assert_eq!(doc.fields.get("owners"), Some(&json!(["crates/acdd-spec/src/"])));
    assert_eq!(doc.tasks.len(), 1);
}

#[test]
fn builtin_contract_and_memory_bases_resolve_and_validate() {
    let contract = builtin_contract_schema();
    assert_eq!(contract.name, "contract");
    assert!(contract.roadmap);
    assert_eq!(contract.workflow.as_deref(), Some("code"));
    assert_eq!(contract.items["tasks"].item.as_deref(), Some("task"));
    assert_eq!(builtin_spec_schema("contract"), Some(contract));

    let memory = builtin_spec_schema("memory").expect("built-in memory base");
    assert_eq!(memory.name, "memory");
    assert!(!memory.roadmap);
    assert_eq!(memory.workflow, None);
    assert_eq!(memory.items["rules"].item.as_deref(), Some("rule"));
    assert_eq!(
        memory.items["rules"].fields.required,
        vec!["scope", "statement", "evidence"]
    );
    assert_eq!(builtin_spec_schema("unknown"), None);

    let mem_raw = "---\nid: MEM-001\ntitle: Auth Memory\nspec: memory\nowners: [src/auth/]\ninvariants: [INV-AUTH]\n---\n\n# Auth Memory\n";
    let mem_doc = parse_spec(mem_raw).expect("built-in memory spec should parse");
    assert_eq!(mem_doc.spec.as_deref(), Some("memory"));
    assert_eq!(mem_doc.owners, vec!["src/auth/"]);
    assert!(mem_doc.tasks.is_empty());

    // Custom SchemaDef override validates extra fields cleanly.
    let mut custom_schema = contract.clone();
    let mut priority_schema = SchemaDef::string();
    priority_schema.enum_values = Some(vec![json!("high"), json!("low")]);
    custom_schema
        .fields
        .properties
        .insert("priority".to_owned(), priority_schema);

    let custom_raw = "---\nid: AC-777\ntitle: Custom Field Spec\nspec: contract\npriority: high\n---\n";
    let custom_doc =
        parse_spec_with_schema(custom_raw, Some(&custom_schema)).expect("custom schema parses");
    assert_eq!(custom_doc.fields.get("priority"), Some(&json!("high")));
}

#[test]
fn ephemeral_goal_writes_goal_frontmatter_instead_of_spec() {
    let run_id = RunId::new("run-goal-1").unwrap();
    let run_name = RunName::new("fix-auth").unwrap();

    for include_task in [true, false] {
        let rendered = materialize_ephemeral_goal_spec_with_options(
            &run_id,
            &run_name,
            "Fix token expiry bug",
            "code",
            &["src/auth.rs".to_owned()],
            &[],
            include_task,
        )
        .expect("materialize goal spec");

        assert!(rendered.contains("spec: contract"), "{rendered}");
        assert!(rendered.contains("goal: Fix token expiry bug"), "{rendered}");
        assert!(!rendered.contains("doc_type:"), "{rendered}");

        let doc = parse_spec(&rendered).expect("re-parse ephemeral goal spec");
        assert_eq!(doc.spec.as_deref(), Some("contract"));
        assert_eq!(doc.goal.as_deref(), Some("Fix token expiry bug"));
        assert_eq!(doc.tasks.len(), usize::from(include_task));
        if !include_task {
            assert_eq!(doc.owners, vec!["src/auth.rs"]);
        }
    }
}

#[test]
fn unknown_or_mistyped_core_and_schema_fields_fail_closed_matrix() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        (
            "typo_core_field_with_hint",
            "---\nid: AC-1\ntitle: Typo\nstauts: planned\n---\n",
            Some("did you mean 'status'?"),
        ),
        (
            "typo_schema_field_with_hint",
            "---\nid: AC-1\ntitle: Typo\nownrs: [src/]\n---\n",
            Some("did you mean 'owners'?"),
        ),
        (
            "mistyped_owners_array",
            "---\nid: AC-1\ntitle: Bad\nowners: src/\n---\n",
            None,
        ),
        (
            "memory_rejects_contract_acceptance_criteria",
            "---\nid: MEM-1\ntitle: Bad Memory\nspec: memory\nacceptance_criteria: [ok]\n---\n",
            None,
        ),
        (
            "memory_rejects_task_blocks",
            "---\nid: MEM-1\ntitle: Bad Memory\nspec: memory\n---\n\n### task: t1\n```yaml\ntarget: nope\n```\n",
            None,
        ),
        (
            "typo_task_field_with_hint",
            "---\nid: AC-1\ntitle: Task Typo\n---\n\n### task: t1\n```yaml\nscop: [src/]\n```\n",
            Some("did you mean 'scope'?"),
        ),
        (
            "mistyped_task_scope_item",
            "---\nid: AC-1\ntitle: Task Type\n---\n\n### task: t1\n```yaml\nscope: src/\n```\n",
            None,
        ),
    ];

    for &(label, raw, expected_hint) in cases {
        let err = parse_spec(raw).expect_err(&format!("{label} must fail closed"));
        assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "{label}: {err}");
        if let Some(hint) = expected_hint {
            assert_eq!(err.hint.as_deref(), Some(hint), "{label}: {err}");
        }
    }
}
