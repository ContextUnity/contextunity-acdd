pub mod cycle_contracts;
mod lifecycle;
mod spawner;
pub mod spec_traceability;
mod types;
pub mod unified_plan_engine;
mod waves;
pub mod workflow_inheritance;

#[test]
fn adr_reconciliation_and_contract_matrix_contract() {
    use crate::acdd::support::Milestone;
    use std::{fs, path::Path};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut missing = Vec::new();
    // Read live authority: build-stage amendments must satisfy this same binary.
    for (path, required) in [
        (
            "docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md",
            &[
                "variant a",
                "tasksstore",
                "task_runner",
                "plan",
                "milestone",
                "taskless",
                "inherit: workspace",
                "save: log",
            ][..],
        ),
        (
            "docs/adr/0015-acdd-lifecycle-snapshots-and-proactive-defect-resolution.md",
            &[
                "option 1",
                ".acdd/",
                "runtime",
                "exclusion",
                "candidate_drift_warning",
                "mandatory",
                "rebind",
                "re-verify",
                "re-review",
                "verification",
                "independent",
                "review",
                "receipt",
            ][..],
        ),
        (
            "docs/adr/0020-hierarchical-acdd-gates-and-milestone-orchestration.md",
            &[
                "option 1",
                ".acdd/",
                "runtime",
                "exclusion",
                "candidate_drift_warning",
                "mandatory",
                "rebind",
                "re-verify",
                "re-review",
                "verification",
                "independent",
                "review",
                "receipt",
            ][..],
        ),
    ] {
        let text = fs::read_to_string(root.join(path)).unwrap();
        let normalized = text
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        for term in required {
            if !normalized.contains(term) {
                missing.push(format!("{path}: missing {term}"));
            }
        }
    }

    let source = fs::read_to_string(
        root.join("docs/milestones/AC-004-unified-execution-and-spec-pipeline.md"),
    )
    .unwrap();
    let milestone = Milestone::parse(&source, "forge-mcp").unwrap();
    let plan = milestone
        .tasks
        .iter()
        .find(|task| task.task_ref == "unify-plan-execution-on-task-store")
        .unwrap();
    // Decision #201: admit subject dispatch in the existing owners, serialized
    // after cycle/profile work and before traceability; no second registry.
    for path in [
        "crates/acdd-tasks/src/engine/mod.rs",
        "crates/acdd-tasks/src/engine/gates/submit.rs",
        "crates/acdd-tasks/src/milestones/mod.rs",
        "crates/acdd-tasks/src/engine/context.rs",
        "crates/acdd-tasks/src/engine/lifecycle/root_gates.rs",
    ] {
        if !plan
            .scope
            .iter()
            .chain(&plan.extended_scope)
            .any(|scope| scope == path)
        {
            missing.push(format!("Plan subject dispatch is not admitted: {path}"));
        }
    }
    let cycle = milestone
        .tasks
        .iter()
        .find(|task| task.task_ref == "fix-role-command-rejection-and-findings")
        .unwrap();
    let profiles = milestone
        .tasks
        .iter()
        .find(|task| task.task_ref == "builtin-profile-presets-and-workflow-inheritance")
        .unwrap();
    for left in cycle.scope.iter().chain(&cycle.extended_scope) {
        for right in profiles.scope.iter().chain(&profiles.extended_scope) {
            assert!(
                !contextunity_acdd::core::tasks::path_matches_scope_entry(left, right)
                    && !contextunity_acdd::core::tasks::path_matches_scope_entry(right, left),
                "same-wave owners overlap: {left} / {right}"
            );
        }
    }
    for predecessor in [&cycle.task_ref, &profiles.task_ref] {
        assert!(
            plan.depends_on.contains(predecessor),
            "Plan must follow {predecessor}"
        );
    }
    assert!(
        missing.is_empty(),
        "AC-004 authority reconciliation required:\n{}",
        missing.join("\n")
    );
}
