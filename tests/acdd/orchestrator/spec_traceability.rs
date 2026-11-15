use crate::acdd::{support::*, tasks::harness::TaskHarness};

#[test]
fn task_claim_projects_only_applicable_spec_acceptance_criteria() {
    let source = MilestoneFixtureBuilder::new("m-spec-traceability")
        .task("first", &["src/lib.rs"])
        .task("second", &["src/second.rs"])
        .build()
        .replace(
            "task_ref: first",
            "task_ref: first\nspec: docs/specs/first.md\nacceptance_criteria: [AC-001]",
        )
        .replace(
            "task_ref: second",
            "task_ref: second\nspec: docs/specs/second.md\nacceptance_criteria: [AC-002]",
        );
    let parsed = Milestone::parse(&source, "forge-mcp");
    assert!(
        parsed.is_ok(),
        "optional spec and AC-* fields must enter manifest authority: {parsed:#?}"
    );
    let harness =
        TaskHarness::disk_with_profile(&source, "docs/milestones/spec.md", fixture_profile());
    let claim =
        TaskHarness::claim_at(harness.root(), &harness.id, "contract", "spec-author").unwrap();
    let bundle = &claim["context_bundle"];
    let projected = bundle.to_string();
    assert!(
        projected.contains("docs/specs/first.md"),
        "claim must retain the task's specification reference: {projected}"
    );
    assert!(
        projected.contains("AC-001"),
        "claim must project applicable acceptance criteria: {projected}"
    );
    assert!(!projected.contains("docs/specs/second.md"));
    assert!(!projected.contains("AC-002"));
    assert!(
        claim["workflow_guidance"]["expected_evidence_template"]["proof"].is_object(),
        "spec metadata must preserve typed gate proof requirements"
    );
}

#[test]
fn blueprint_schema_exposes_acceptance_and_design_traceability() {
    let profile = acdd_tasks::tasks::profile::compiled();
    let blueprint = profile.proofs["blueprint"].schema.as_ref().unwrap();
    let mut missing = Vec::new();
    for field in [
        "ac_mapping",
        "c2_boundary_impact",
        "adr_required",
        "sad_updates",
    ] {
        if !blueprint.properties.contains_key(field) {
            missing.push(field);
        }
    }
    assert!(
        missing.is_empty(),
        "blueprint proof must capture Specify/Design decisions: {missing:?}"
    );
    for field in [
        "cited_invariants",
        "cited_symbols",
        "tasks",
        "blackboard_decision_id",
    ] {
        assert!(
            blueprint.required.iter().any(|required| required == field),
            "traceability must preserve existing required proof: {field}"
        );
    }
}
