//! CLI and formatter contract tests for `acdd roadmap`.

use crate::common::cli::{run_cli, run_cli_json};
use crate::common::Workspace;
use std::fs;
use std::str;

const SINGLE_SPEC_ADAPTER: &str = r#"roots: [.]
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
modules:
  state: true
  code: false
  docs: false
"#;

const MINIMAL_ADAPTER: &str = r#"roots: [.]
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3
modules:
  state: true
  code: false
  docs: false
"#;

fn seed_roadmap_workspace(ws: &Workspace) {
    ws.write("acdd.yaml", MINIMAL_ADAPTER);
    fs::create_dir_all(ws.path("docs/milestones/archive")).unwrap();
    fs::create_dir_all(ws.path("docs/plans")).unwrap();
    ws.write(
        "docs/milestones/archive/AC-000-arch.md",
        "---\nid: AC-000\ntitle: Archived Base\nstatus: completed\ndepends_on: [AC-900]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/arch/**]\nstatus: completed\nacceptance_criteria: [done]\n```\n",
    );
    ws.write(
        "docs/milestones/AC-001-core.md",
        "---\nid: AC-001\ntitle: Core Ready\nstatus: planned\ndepends_on: [AC-000]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/shared/**]\nacceptance_criteria: [ok]\n```\n",
    );
    ws.write(
        "docs/milestones/AC-002-overlap.md",
        "---\nid: AC-002\ntitle: Overlap Ready\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/shared/sub/**]\nacceptance_criteria: [ok]\n```\n",
    );
    ws.write(
        "docs/plans/PLAN-001-mid.md",
        "---\nid: PLAN-001\ntitle: Mid Plan\ndoc_type: plan\nstatus: planned\ndepends_on: [AC-001]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [crates/plan/**]\nacceptance_criteria: [ok]\n```\n",
    );
    ws.write(
        "docs/milestones/AC-003-leaf.md",
        "---\nid: AC-003\ntitle: Leaf Spec\nstatus: planned\ndepends_on: [PLAN-001]\n---\n### task: T-01\n```yaml\ntask_ref: T-01\ndepends_on: [\"PLAN-001:T-01\"]\nscope: [crates/leaf/**]\nacceptance_criteria: [ok]\n```\n",
    );
}

#[test]
fn roadmap_cli_markdown_json_filters_and_missing_state_db_matrix() {
    let ws = Workspace::new();
    seed_roadmap_workspace(&ws);
    let state_db = ws.path(".acdd/state.sqlite");
    assert!(!state_db.exists());

    let md = run_cli(&ws, &["roadmap"]);
    assert!(md.status.success(), "{}", str::from_utf8(&md.stderr).unwrap_or(""));
    let md_text = String::from_utf8_lossy(&md.stdout);
    assert!(md_text.contains("# Roadmap"));
    assert!(md_text.contains("## Wave 0 — Ready Now"));
    assert!(md_text.contains("AC-001"));
    assert!(md_text.contains("scope overlap: AC-002") || md_text.contains("AC-002"));
    assert!(!state_db.exists(), "roadmap must not create state.sqlite");

    let json_out = run_cli_json(&ws, &["roadmap"]);
    assert!(json_out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    let roadmap = payload.get("Roadmap").expect("externally tagged Roadmap variant");
    assert_eq!(roadmap["archived_count"], 1);
    assert_eq!(roadmap["waves"][0]["specs"][0]["spec_id"], "AC-001");

    let single_ws = Workspace::new();
    fs::create_dir_all(single_ws.path("docs/milestones")).unwrap();
    single_ws.write("acdd.yaml", SINGLE_SPEC_ADAPTER);
    single_ws.write(
        "docs/milestones/AC-001-only.md",
        "---\nid: AC-001\ntitle: Only Ready\nstatus: planned\n---\n### task: T-01\n```yaml\ntask_ref: T-01\nscope: [src/**]\nacceptance_criteria: [ok]\n```\n",
    );
    let single_md = run_cli(&single_ws, &["roadmap"]);
    assert!(single_md.status.success(), "{}", str::from_utf8(&single_md.stderr).unwrap_or(""));
    let single_text = String::from_utf8_lossy(&single_md.stdout);
    assert!(single_text.contains("# Roadmap"));
    assert!(single_text.contains("## Wave 0 — Ready Now"));
    assert!(!single_text.starts_with("# AC-001\n"));

    let single_detail = run_cli(&single_ws, &["roadmap", "AC-001"]);
    assert!(single_detail.status.success());
    let single_detail_md = String::from_utf8_lossy(&single_detail.stdout);
    assert!(single_detail_md.contains("# AC-001"));
    assert!(single_detail_md.contains("Only Ready"));

    struct Case<'a> {
        args: &'a [&'a str],
        wave0_only: bool,
        expect_id: Option<&'a str>,
    }
    let cases = [
        Case {
            args: &["roadmap", "--collection", "milestones", "--ready"],
            wave0_only: true,
            expect_id: Some("AC-001"),
        },
        Case {
            args: &["roadmap", "--ready"],
            wave0_only: true,
            expect_id: Some("AC-001"),
        },
        Case {
            args: &["roadmap", "--active"],
            wave0_only: false,
            expect_id: None,
        },
        Case {
            args: &["roadmap", "--tasks"],
            wave0_only: false,
            expect_id: None,
        },
        Case {
            args: &["roadmap", "AC-003"],
            wave0_only: false,
            expect_id: Some("AC-003"),
        },
        Case {
            args: &["roadmap", "--collection", "plans"],
            wave0_only: false,
            expect_id: Some("PLAN-001"),
        },
    ];

    for case in cases {
        let out = run_cli_json(&ws, case.args);
        assert!(out.status.success(), "{:?}: {}", case.args, String::from_utf8_lossy(&out.stderr));
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let roadmap = &body["Roadmap"];
        if case.wave0_only {
            assert!(
                roadmap["waves"]
                    .as_array()
                    .map(|waves| waves.iter().all(|w| w["wave"] == 0))
                    .unwrap_or(true)
            );
            assert!(roadmap["in_progress"].as_array().is_some_and(|v| v.is_empty()));
        }
        if let Some(id) = case.expect_id {
            let found = roadmap["in_progress"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(
                    roadmap["waves"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|w| w["specs"].as_array().into_iter().flatten()),
                )
                .any(|row| row["spec_id"] == id);
            assert!(found, "{id} missing for args {:?}", case.args);
        }
        assert!(!state_db.exists());
    }

    let detail = run_cli(&ws, &["roadmap", "AC-003"]);
    assert!(detail.status.success());
    let detail_md = String::from_utf8_lossy(&detail.stdout);
    assert!(detail_md.contains("# AC-003"));
    assert!(detail_md.contains("Task waves") || detail_md.contains("depends_on"));
}
