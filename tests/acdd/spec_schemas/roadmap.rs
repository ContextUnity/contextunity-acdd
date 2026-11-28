//! `acdd.yaml` `roadmap:` display config, ANSI color, and schema filter seam tests (AC-08).

use acdd_core::config::load_adapter;
use acdd_core::fs::ScopedTempDir;
use acdd_spec::overlay::NoopRuntimeOverlay;
use acdd_spec::roadmap::{build_roadmap, RoadmapConfig};
use acdd_spec::types::RoadmapReq;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(abs, content).unwrap();
    }
}

fn run_acdd_roadmap(root: &Path, args: &[&str], no_color: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut cmd);
    cmd.arg("--root").arg(root).arg("roadmap").args(args);
    match no_color {
        Some(val) => {
            cmd.env("NO_COLOR", val);
        }
        None => {
            cmd.env_remove("NO_COLOR");
        }
    }
    cmd.output().expect("execute acdd roadmap")
}

#[test]
fn roadmap_display_config_schema_filters_badges_grouping_and_no_color_matrix() {
    let dir = ScopedTempDir::create("acdd_roadmap_ac08").unwrap();
    let root = dir.path();

    write_files(
        root,
        &[
            (
                "acdd.yaml",
                "\
preset: full
roots: [src]
specs:
  milestones:
    base: contract
    path: docs/milestones
    prefixes: [AC]
    roadmap: true
    fields:
      priority: { type: string, enum: [high, low] }
      services: { type: array, items: { type: string } }
      tags: { type: array, items: { type: string } }
  memories:
    base: memory
    path: docs/memories
  research:
    base: contract
    path: docs/research
    prefixes: [RES]
    roadmap: false
    fields:
      priority: { type: string, enum: [high, low] }
      tags: { type: array, items: { type: string } }
roadmap:
  badges: [services, priority, readiness]
  group_by: wave
  color: always
",
            ),
            (
                "docs/milestones/AC-001-auth.md",
                "\
---
id: AC-001
title: Auth Gateway
status: planned
priority: high
services: [auth, billing]
tags: [security]
---
### task: T-01
```yaml
target: Build auth gateway
scope: [src/auth/**]
tags: [default]
```
",
            ),
            (
                "docs/milestones/AC-002-cli.md",
                "\
---
id: AC-002
title: CLI Polish
status: planned
priority: low
services: [cli]
depends_on: [AC-001]
---
### task: T-01
```yaml
target: Polish CLI
scope: [src/cli/**]
tags: [backend]
```
",
            ),
            (
                "docs/memories/MEM-001-rule.md",
                "\
---
id: MEM-001
title: Auth Scope Rule
spec: memories
status: active
owners: [src/**]
---
### rule: R-001
```yaml
scope: [src/**]
statement: Keep auth isolated
evidence: [\"1\"]
```
",
            ),
            (
                "docs/research/RES-001-probe.md",
                "\
---
id: RES-001
title: Security Probe
status: planned
priority: high
tags: [security]
---
### task: T-01
```yaml
target: Probe auth
scope: [src/probe/**]
```
",
            ),
            (
                "docs/milestones/MEM-002-override.md",
                "\
---
id: MEM-002
title: Frontmatter Spec Override Rule
spec: memories
status: active
owners: [src/**]
---
### rule: R-002
```yaml
scope: [src/**]
statement: 'Frontmatter spec overrides owning collection roadmap: true'
evidence: [\"1\"]
```
",
            ),
        ],
    );

    // 1. Direct acdd-spec API verifies roadmap: false hiding, --all/--spec/--tag/--field, badges, and group_by.
    let adapter = load_adapter(root, None).unwrap();
    let linked = BTreeMap::new();
    let cfg = RoadmapConfig {
        workspace_root: root,
        worktree: root,
        spec_collections: &adapter.specs,
        linked_workspaces: &linked,
    };
    let default_resp = build_roadmap(
        &cfg,
        &NoopRuntimeOverlay,
        RoadmapReq {
            badges: adapter.roadmap.badges.clone(),
            group_by: adapter.roadmap.group_by.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(default_resp.waves.len(), 2);
    assert_eq!(default_resp.waves[0].specs[0].spec_id, "AC-001");
    assert!(
        default_resp.waves[0].specs[0].parallel_safe,
        "hidden roadmap: false MEM-001 must not cause false scope overlaps"
    );
    assert_eq!(default_resp.waves[1].specs[0].spec_id, "AC-002");
    assert_eq!(
        default_resp.waves[0].specs[0]
            .badges
            .iter()
            .map(|b| (b.name.as_str(), b.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("services", "auth, billing"),
            ("priority", "high"),
            ("readiness", "ready"),
        ]
    );
    assert_eq!(
        default_resp.waves[1].specs[0].badges.last().map(|b| b.value.as_str()),
        Some("blocked")
    );

    // 2. CLI matrix across default hiding, --all, --spec, drill-down, --field, --tag, --group-by, and color/NO_COLOR.
    let default_cli = run_acdd_roadmap(root, &[], None);
    assert!(default_cli.status.success(), "{:?}", default_cli);
    let default_md = String::from_utf8_lossy(&default_cli.stdout);
    assert!(default_md.contains("AC-001"));
    assert!(default_md.contains("AC-002"));
    assert!(!default_md.contains("MEM-001"), "memory spec must hide by default: {default_md}");
    assert!(
        !default_md.contains("MEM-002"),
        "frontmatter spec: memories inside roadmap: true dir must hide by default: {default_md}"
    );
    assert!(!default_md.contains("RES-001"), "roadmap: false research spec must hide by default: {default_md}");
    assert!(default_md.contains("[services: auth, billing]"));
    assert!(default_md.contains("[priority: high]"));
    assert!(default_md.contains("[readiness: ready]"));
    assert!(default_md.contains('\x1b'), "roadmap.color: always must emit ANSI without NO_COLOR");

    let no_color_cli = run_acdd_roadmap(root, &[], Some("1"));
    assert!(no_color_cli.status.success());
    let no_color_md = String::from_utf8_lossy(&no_color_cli.stdout);
    assert!(
        !no_color_md.contains('\x1b'),
        "NO_COLOR=1 must suppress all ANSI escapes even under roadmap.color: always: {no_color_md}"
    );

    let color_never_cli = run_acdd_roadmap(root, &["--color", "never"], None);
    assert!(color_never_cli.status.success());
    assert!(
        !String::from_utf8_lossy(&color_never_cli.stdout).contains('\x1b'),
        "--color=never must emit zero ANSI escapes"
    );

    let color_always_over_no_color = run_acdd_roadmap(root, &["--color", "always"], Some("1"));
    assert!(color_always_over_no_color.status.success());
    assert!(
        String::from_utf8_lossy(&color_always_over_no_color.stdout).contains('\x1b'),
        "CLI --color=always must override NO_COLOR=1"
    );

    let all_cli = run_acdd_roadmap(root, &["--all", "--color", "never"], None);
    assert!(all_cli.status.success());
    let all_md = String::from_utf8_lossy(&all_cli.stdout);
    assert!(all_md.contains("MEM-001"));
    assert!(all_md.contains("MEM-002"));
    assert!(all_md.contains("RES-001"));

    let spec_milestones_cli = run_acdd_roadmap(root, &["--spec", "milestones", "--color", "never"], None);
    assert!(spec_milestones_cli.status.success());
    let ms_md = String::from_utf8_lossy(&spec_milestones_cli.stdout);
    assert!(ms_md.contains("AC-001"));
    assert!(ms_md.contains("parallel-safe"));
    assert!(!ms_md.contains("scope overlap"), "--spec milestones must not emit false scope overlaps: {ms_md}");
    assert!(!ms_md.contains("MEM-001"), "--spec milestones must not leak hidden MEM-001 overlap: {ms_md}");
    assert!(!ms_md.contains("MEM-002"), "--spec milestones must exclude frontmatter spec: memories override: {ms_md}");

    let spec_memories_cli = run_acdd_roadmap(root, &["--spec", "memories", "--color", "never"], None);
    assert!(spec_memories_cli.status.success());
    let mems_md = String::from_utf8_lossy(&spec_memories_cli.stdout);
    assert!(mems_md.contains("MEM-001"), "--spec memories must include MEM-001: {mems_md}");
    assert!(mems_md.contains("MEM-002"), "--spec memories must include frontmatter spec: memories override MEM-002: {mems_md}");
    assert!(!mems_md.contains("**AC-001**"));

    let grouped_spec_cli = run_acdd_roadmap(root, &["--all", "--group-by", "spec", "--color", "never"], None);
    assert!(grouped_spec_cli.status.success());
    let grouped_spec_md = String::from_utf8_lossy(&grouped_spec_cli.stdout);
    let memories_section = grouped_spec_md
        .split("## spec: memories")
        .nth(1)
        .and_then(|rest| rest.split("\n## spec: ").next())
        .expect("expected ## spec: memories section");
    assert!(memories_section.contains("MEM-001"), "MEM-001 must group under ## spec: memories: {grouped_spec_md}");
    assert!(memories_section.contains("MEM-002"), "MEM-002 must group under ## spec: memories: {grouped_spec_md}");

    let spec_research_cli = run_acdd_roadmap(root, &["--spec", "research", "--color", "never"], None);
    assert!(spec_research_cli.status.success());
    let res_md = String::from_utf8_lossy(&spec_research_cli.stdout);
    assert!(res_md.contains("RES-001"));
    assert!(res_md.contains("parallel-safe"));
    assert!(!res_md.contains("AC-001"));
    assert!(!res_md.contains("MEM-001"), "--spec research must not leak hidden MEM-001 overlap: {res_md}");
    assert!(!res_md.contains("MEM-002"), "--spec research must not leak hidden MEM-002 overlap: {res_md}");

    let drill_ac001_cli = run_acdd_roadmap(root, &["AC-001", "--color", "never"], None);
    assert!(drill_ac001_cli.status.success());
    let ac001_md = String::from_utf8_lossy(&drill_ac001_cli.stdout);
    assert!(ac001_md.contains("# AC-001"));
    assert!(!ac001_md.contains("MEM-001"), "AC-001 detail must not leak hidden MEM-001 overlap: {ac001_md}");
    assert!(!ac001_md.contains("MEM-002"), "AC-001 detail must not leak hidden MEM-002 overlap: {ac001_md}");

    let drill_mem_cli = run_acdd_roadmap(root, &["MEM-001", "--color", "never"], None);
    assert!(drill_mem_cli.status.success());
    let mem_md = String::from_utf8_lossy(&drill_mem_cli.stdout);
    assert!(mem_md.contains("# MEM-001"));

    let field_cli = run_acdd_roadmap(
        root,
        &["--field", "priority=high", "--field", "services=auth", "--color", "never"],
        None,
    );
    assert!(field_cli.status.success());
    let field_md = String::from_utf8_lossy(&field_cli.stdout);
    assert!(field_md.contains("AC-001"));
    assert!(!field_md.contains("AC-002"));
    assert!(!field_md.contains("RES-001"));

    // --field status=... matches displayed roadmap status badge consistently (AC-001 is ready, not planned).
    let status_ready = run_acdd_roadmap(root, &["--field", "status=ready", "--color", "never"], None);
    assert!(status_ready.status.success());
    let status_ready_md = String::from_utf8_lossy(&status_ready.stdout);
    assert!(status_ready_md.contains("AC-001"), "--field status=ready must include ready AC-001: {status_ready_md}");
    let status_planned = run_acdd_roadmap(root, &["--field", "status=planned", "--color", "never"], None);
    assert!(status_planned.status.success());
    let status_planned_md = String::from_utf8_lossy(&status_planned.stdout);
    assert!(!status_planned_md.contains("**AC-001**"), "--field status=planned must not match ready AC-001: {status_planned_md}");

    let tag_cli = run_acdd_roadmap(root, &["--tag", "backend", "--color", "never"], None);
    assert!(tag_cli.status.success());
    let tag_md = String::from_utf8_lossy(&tag_cli.stdout);
    assert!(tag_md.contains("**AC-002**"));
    assert!(!tag_md.contains("**AC-001**"));

    let fm_tag_cli = run_acdd_roadmap(root, &["--tag", "security", "--all", "--color", "never"], None);
    assert!(fm_tag_cli.status.success());
    let fm_tag_md = String::from_utf8_lossy(&fm_tag_cli.stdout);
    assert!(fm_tag_md.contains("**AC-001**"));
    assert!(fm_tag_md.contains("**RES-001**"));
    assert!(!fm_tag_md.contains("**AC-002**"));
    assert!(!root.join(".acdd/state.sqlite").exists(), "roadmap must stay read-only without creating state.sqlite");

    let grouped_cli = run_acdd_roadmap(root, &["--group-by", "priority", "--color", "never"], None);
    assert!(grouped_cli.status.success());
    let grouped_md = String::from_utf8_lossy(&grouped_cli.stdout);
    assert!(grouped_md.contains("## priority: high"));
    assert!(grouped_md.contains("## priority: low"));
    assert!(grouped_md.contains("[priority: high]"));

    // 3. Invalid --field syntax fails closed, and JSON format remains ANSI-free with badges + groups.
    let bad_field = run_acdd_roadmap(root, &["--field", "priority"], None);
    assert!(!bad_field.status.success());

    let mut json_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut json_cmd);
    let json_out = json_cmd
        .args(["--root"])
        .arg(root)
        .args(["--format", "json", "roadmap", "--group-by", "priority", "--all"])
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let json_str = String::from_utf8_lossy(&json_out.stdout);
    assert!(!json_str.contains('\x1b'));
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    let payload = parsed.get("Roadmap").unwrap_or(&parsed);
    assert_eq!(payload["group_by"], "priority");
    assert!(payload["groups"].as_array().is_some_and(|g| g.len() >= 2));

    // 4. Two-checkout linked worktree regression:
    //    - migrated worktree resolves short prefix (`AC-009` -> `AC-009-long-stored-milestone`) structurally
    //      while primary still has legacy top-level `architecture:`,
    //    - positional drill-down keeps own-spec divergence evidence and excludes unrelated divergent archives,
    //    - sibling checkout uses its OWN `specs` collections and `roadmap: false` visibility (`private_specs` ->
    //      `PRI-777-hidden-sibling` hidden by default, visible under `--all` and `--spec private_specs`),
    //    - unrelated unknown key (`roadmp: false`) in primary fails closed.
    crate::acdd::support::init_git_repo_on_branch(root, "main");
    let wt_dir = ScopedTempDir::create("acdd-roadmap-linked-wt").unwrap();
    let wt_path = wt_dir.path().join("migrated-wt");
    let add_wt = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["worktree", "add", "-b", "migrated-branch"])
        .arg(&wt_path)
        .output()
        .unwrap();
    assert!(
        add_wt.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&add_wt.stderr)
    );
    // Add a long-ID milestone in both checkouts with divergent status (planned in primary, active in migrated)
    // plus an unrelated divergent archived spec (`AR-003`) to verify positional drill-down warning scoping.
    fs::write(
        root.join("docs/milestones/AC-009-long-stored-milestone.md"),
        r#"---
id: AC-009-long-stored-milestone
title: Long Stored Milestone
status: planned
doc_type: milestone
---
### task: long-task-alpha
```yaml
target: Long Task Alpha
scope: [src/alpha.rs]
```
"#,
    )
    .unwrap();
    fs::write(
        wt_path.join("docs/milestones/AC-009-long-stored-milestone.md"),
        r#"---
id: AC-009-long-stored-milestone
title: Long Stored Milestone
status: active
doc_type: milestone
---
### task: long-task-alpha
```yaml
target: Long Task Alpha
scope: [src/alpha.rs]
```
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("docs/milestones/archive")).unwrap();
    fs::create_dir_all(wt_path.join("docs/milestones/archive")).unwrap();
    fs::write(
        root.join("docs/milestones/archive/AR-003-unrelated.md"),
        "---\nid: AR-003-unrelated\ntitle: Unrelated Primary\nstatus: completed\ndoc_type: milestone\n---\n",
    )
    .unwrap();
    for args in [
        &["add", "docs/milestones/archive/AR-003-unrelated.md"][..],
        &["commit", "-m", "archive AR-003 primary"][..],
    ] {
        let out = Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    fs::write(
        wt_path.join("docs/milestones/archive/AR-003-unrelated.md"),
        "---\nid: AR-003-unrelated\ntitle: Unrelated Migrated\nstatus: completed\ndoc_type: milestone\n---\n",
    )
    .unwrap();
    for args in [
        &["add", "docs/milestones/archive/AR-003-unrelated.md"][..],
        &["commit", "-m", "archive AR-003 migrated"][..],
    ] {
        let out = Command::new("git").arg("-C").arg(&wt_path).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    // Add a primary-only spec under primary's own `roadmap: false` collection (`private_specs`) alongside legacy `architecture:`.
    fs::create_dir_all(root.join("docs/private_specs")).unwrap();
    fs::write(
        root.join("docs/private_specs/PRI-777-hidden-sibling.md"),
        r#"---
id: PRI-777-hidden-sibling
title: Hidden Sibling Spec
status: planned
doc_type: milestone
---
### task: sibling-task
```yaml
target: Sibling Task
scope: [src/sibling.rs]
```
"#,
    )
    .unwrap();
    let base_primary_cfg = fs::read_to_string(root.join("acdd.yaml")).unwrap();
    // Insert `private_specs` (`roadmap: false`) under `specs:` cleanly and append top-level `architecture:`.
    let primary_cfg_with_sibling_col = base_primary_cfg
        .replace(
            "specs:\n",
            "specs:\n  private_specs:\n    path: docs/private_specs\n    base: contract\n    prefixes: [PRI]\n    roadmap: false\n",
        )
        + "\narchitecture:\n  zones:\n    legacy: [\"src/**\"]\n";
    fs::write(root.join("acdd.yaml"), &primary_cfg_with_sibling_col).unwrap();

    let mut legacy_primary = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut legacy_primary);
    let legacy_out = legacy_primary
        .args(["-C"])
        .arg(root)
        .args(["roadmap", "AC-001", "--color", "never", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        !legacy_out.status.success(),
        "primary checkout with legacy inline architecture: must still fail closed"
    );

    // Positional drill-down resolves `AC-009` -> `AC-009-long-stored-milestone`, keeps own-spec divergence,
    // and excludes unrelated divergent archive `AR-003-unrelated`.
    let mut migrated_wt_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut migrated_wt_cmd);
    let migrated_out = migrated_wt_cmd
        .args(["-C"])
        .arg(&wt_path)
        .args(["roadmap", "AC-009", "--color", "never", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        migrated_out.status.success(),
        "migrated linked worktree roadmap must succeed even when primary has legacy architecture: stderr={}",
        String::from_utf8_lossy(&migrated_out.stderr)
    );
    let migrated_json: serde_json::Value =
        serde_json::from_slice(&migrated_out.stdout).expect("valid roadmap json");
    let migrated_payload = migrated_json.get("Roadmap").unwrap_or(&migrated_json);
    let drill_warnings: Vec<&str> = migrated_payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        drill_warnings
            .iter()
            .any(|w| w.contains("AC-009-long-stored-milestone")),
        "positional drill-down must keep own-spec divergence warning: {drill_warnings:?}"
    );
    assert!(
        drill_warnings.iter().all(|w| !w.contains("AR-003-unrelated")),
        "positional drill-down must exclude unrelated divergent archive warnings: {drill_warnings:?}"
    );
    let ac009_entry = migrated_payload["waves"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|w| w["specs"].as_array().into_iter().flatten())
        .find(|s| s["spec_id"] == "AC-009-long-stored-milestone")
        .expect("short prefix AC-009 must resolve structurally to AC-009-long-stored-milestone");
    assert_eq!(
        ac009_entry["tasks"][0]["task_ref"], "long-task-alpha",
        "resolved AC-009 entry must include its task identity structurally: {ac009_entry}"
    );

    // Default migrated roadmap hides sibling-only `roadmap: false` (`PRI-777-hidden-sibling`) while
    // keeping overview divergence warnings (including `AR-003-unrelated`).
    let mut default_wt_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut default_wt_cmd);
    let default_wt_out = default_wt_cmd
        .args(["-C"])
        .arg(&wt_path)
        .args(["roadmap", "--color", "never", "--format", "json"])
        .output()
        .unwrap();
    assert!(default_wt_out.status.success());
    let default_wt_json: serde_json::Value =
        serde_json::from_slice(&default_wt_out.stdout).expect("valid default roadmap json");
    let default_wt_payload = default_wt_json.get("Roadmap").unwrap_or(&default_wt_json);
    let default_wt_ids: Vec<&str> = default_wt_payload["waves"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|w| w["specs"].as_array().into_iter().flatten())
        .filter_map(|s| s["spec_id"].as_str())
        .collect();
    assert!(
        !default_wt_ids.contains(&"PRI-777-hidden-sibling"),
        "sibling-only roadmap: false collection must be hidden by default: {default_wt_ids:?}"
    );
    let overview_warnings: Vec<&str> = default_wt_payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        overview_warnings
            .iter()
            .any(|w| w.contains("AR-003-unrelated")),
        "overview roadmap must retain cross-worktree divergence warnings: {overview_warnings:?}"
    );

    // `--all` and `--spec private_specs` both surface `PRI-777-hidden-sibling` using sibling's own collection.
    for extra_args in [&["--all"][..], &["--spec", "private_specs"][..]] {
        let mut sibling_col_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
        crate::common::isolate_acdd_env(&mut sibling_col_cmd);
        let sibling_col_out = sibling_col_cmd
            .args(["-C"])
            .arg(&wt_path)
            .arg("roadmap")
            .args(extra_args)
            .args(["--color", "never", "--format", "json"])
            .output()
            .unwrap();
        assert!(
            sibling_col_out.status.success(),
            "migrated worktree roadmap {extra_args:?} must succeed: stderr={}",
            String::from_utf8_lossy(&sibling_col_out.stderr)
        );
        let sibling_col_json: serde_json::Value =
            serde_json::from_slice(&sibling_col_out.stdout).expect("valid sibling roadmap json");
        let sibling_col_payload = sibling_col_json.get("Roadmap").unwrap_or(&sibling_col_json);
        let pri777_entry = sibling_col_payload["waves"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|w| w["specs"].as_array().into_iter().flatten())
            .find(|s| s["spec_id"] == "PRI-777-hidden-sibling")
            .unwrap_or_else(|| {
                panic!("PRI-777-hidden-sibling must appear under {extra_args:?}: {sibling_col_payload}")
            });
        assert_eq!(pri777_entry["spec"], "private_specs");
    }

    // Unrelated unknown key (`roadmp: false`) in primary checkout must fail closed from migrated worktree.
    fs::write(
        root.join("acdd.yaml"),
        format!("{base_primary_cfg}\nroadmp: false\n"),
    )
    .unwrap();
    let mut bad_sibling_cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
    crate::common::isolate_acdd_env(&mut bad_sibling_cmd);
    let bad_sibling_out = bad_sibling_cmd
        .args(["-C"])
        .arg(&wt_path)
        .args(["roadmap", "AC-009", "--color", "never", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        !bad_sibling_out.status.success(),
        "unrelated unknown key `roadmp: false` in sibling checkout must fail closed"
    );

    let _ = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["worktree", "remove", "--force"])
        .arg(&wt_path)
        .output();
}

#[test]
fn roadmap_cross_worktree_run_visibility_and_archive_schema_cache_isolation() {
    let dir = ScopedTempDir::create("acdd_roadmap_r5_isolation").unwrap();
    let root = dir.path();

    let strict_cfg = "\
preset: full
roots: [src]
specs:
  milestones:
    base: contract
    path: docs/milestones
    prefixes: [AC]
    roadmap: true
";
    let hidden_cfg = strict_cfg.replace("roadmap: true", "roadmap: false");
    let custom_field_cfg = format!(
        "{strict_cfg}    fields:\n      priority: {{ type: string, enum: [high, low] }}\n"
    );

    write_files(
        root,
        &[
            ("acdd.yaml", strict_cfg),
            (
                ".acdd/workflows/default.yaml",
                "\
version: 2
inherit: code
commands:
  build_check: \"true\"
  verify_all: \"true\"
  check_full_repo: \"true\"
  polish_check: \"true\"
",
            ),
            ("src/lib.rs", "pub fn ok() {}\n"),
            (
                "docs/milestones/AC-901-shared.md",
                "\
---
id: AC-901-shared
title: Shared Active Milestone
status: planned
doc_type: milestone
---
### task: shared-task
```yaml
target: Shared Task
scope: [src/lib.rs]
```
",
            ),
            (
                "docs/milestones/archive/AC-902-archive.md",
                "\
---
id: AC-902-archive
title: Custom Field Archive
status: completed
doc_type: milestone
priority: high
---
",
            ),
        ],
    );
    crate::acdd::support::init_git_repo_on_branch(root, "main");

    // 1. Two active runs for the same AC-901-shared milestone in separate managed worktrees:
    //    `a-hidden` sets `roadmap: false` while `b-visible` keeps `roadmap: true`.
    for run_name in ["a-hidden", "b-visible"] {
        let mut start = Command::new(env!("CARGO_BIN_EXE_acdd"));
        crate::common::isolate_acdd_env(&mut start);
        let out = start
            .args(["-C"])
            .arg(root)
            .args([
                "start",
                "docs/milestones/AC-901-shared.md",
                "--worktree",
                "--name",
                run_name,
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "start {run_name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let wt_a = root.join(".worktrees/a-hidden");
    let wt_b = root.join(".worktrees/b-visible");
    fs::write(wt_a.join("acdd.yaml"), &hidden_cfg).unwrap();

    let run_roadmap_json = |wt: &Path, extra: &[&str]| -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_acdd"));
        crate::common::isolate_acdd_env(&mut cmd);
        let out = cmd
            .args(["-C"])
            .arg(wt)
            .arg("roadmap")
            .args(extra)
            .args(["--color", "never", "--format", "json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "roadmap {extra:?} from {} failed: {}",
            wt.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("valid roadmap json");
        parsed.get("Roadmap").cloned().unwrap_or(parsed)
    };
    let in_progress_worktrees = |payload: &serde_json::Value| -> Vec<String> {
        payload["in_progress"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["worktree"].as_str().map(str::to_owned))
            .collect()
    };

    for wt in [&wt_a, &wt_b] {
        let runs = in_progress_worktrees(&run_roadmap_json(wt, &[]));
        assert_eq!(
            runs.len(),
            1,
            "default roadmap from {} must show only b-visible: {runs:?}",
            wt.display()
        );
        assert!(
            runs[0].ends_with("b-visible"),
            "default roadmap from {} must return b-visible, got {runs:?}",
            wt.display()
        );
    }
    for extra in [&["--all"][..], &["--spec", "milestones"][..]] {
        let runs = in_progress_worktrees(&run_roadmap_json(&wt_a, extra));
        assert_eq!(
            runs.len(),
            2,
            "roadmap {extra:?} from a-hidden must return both active runs: {runs:?}"
        );
    }

    // 2. Archive schema-registry cache isolation:
    //    `wt_a` and `wt_b` share the exact same Git HEAD and `docs/milestones` path,
    //    but only `wt_b` declares custom `priority` field used by archived `AC-902-archive`.
    //    Querying from strict `wt_a` must not poison `wt_b`'s archive parse.
    fs::write(wt_b.join("acdd.yaml"), &custom_field_cfg).unwrap();
    for wt in [&wt_a, &wt_b] {
        let payload = run_roadmap_json(wt, &["AC-902-archive", "--all"]);
        assert_eq!(
            payload["archived_count"].as_u64(),
            Some(1),
            "roadmap AC-902-archive --all from {} must discover archived spec via schema-isolated cache: {payload}",
            wt.display()
        );
    }

    // 3. Unresolved active-run checkout isolation (#695):
    //    Pointing `a-hidden`'s `specs.milestones` collection path to an empty `docs/hidden`
    //    while its active run and file stay in place must never borrow `b-visible`'s `SpecInfo`.
    let moved_path_cfg = strict_cfg.replace("path: docs/milestones", "path: docs/hidden");
    fs::create_dir_all(wt_a.join("docs/hidden")).unwrap();
    fs::write(wt_a.join("acdd.yaml"), &moved_path_cfg).unwrap();
    for wt in [&wt_a, &wt_b] {
        let runs = in_progress_worktrees(&run_roadmap_json(wt, &[]));
        assert_eq!(
            runs.len(),
            1,
            "default roadmap from {} must still show only b-visible when a-hidden spec is unresolved: {runs:?}",
            wt.display()
        );
        assert!(
            runs[0].ends_with("b-visible"),
            "default roadmap from {} must not borrow b-visible for a-hidden, got {runs:?}",
            wt.display()
        );
    }

    // 4. Swapped linked checkout directories isolation (#700):
    //    Swapping `wt_a` and `wt_b` on disk without updating `.git/worktrees/*/gitdir`
    //    must never attribute `b-visible`'s spec metadata (`title`, `priority: high`)
    //    to `a-hidden` in either default or priority-grouped roadmap JSON.
    fs::write(wt_a.join("acdd.yaml"), &hidden_cfg).unwrap();
    fs::write(
        wt_b.join("docs/milestones/AC-901-shared.md"),
        "\
---
id: AC-901-shared
title: Visible Sibling Milestone
status: planned
doc_type: milestone
priority: high
---
### task: shared-task
```yaml
target: Shared Task
scope: [src/lib.rs]
```
",
    )
    .unwrap();
    let swap_tmp = root.join(".worktrees/swap-tmp");
    fs::rename(&wt_a, &swap_tmp).unwrap();
    fs::rename(&wt_b, &wt_a).unwrap();
    fs::rename(&swap_tmp, &wt_b).unwrap();
    for wt in [root, wt_a.as_path(), wt_b.as_path()] {
        let payload = run_roadmap_json(wt, &[]);
        let runs = in_progress_worktrees(&payload);
        assert!(
            !runs.iter().any(|w| w.ends_with("a-hidden")),
            "swapped checkouts from {} must never attribute b-visible to a-hidden: {payload}",
            wt.display()
        );
        let grouped = run_roadmap_json(wt, &["--group-by", "priority"]);
        let grouped_runs: Vec<String> = grouped["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["specs"].as_array().into_iter().flatten())
            .filter_map(|e| e["worktree"].as_str().map(str::to_owned))
            .collect();
        assert!(
            !grouped_runs.iter().any(|w| w.ends_with("a-hidden")),
            "grouped roadmap on swapped checkouts from {} must never attribute b-visible to a-hidden: {grouped}",
            wt.display()
        );
    }
    fs::rename(&wt_a, &swap_tmp).unwrap();
    fs::rename(&wt_b, &wt_a).unwrap();
    fs::rename(&swap_tmp, &wt_b).unwrap();

    // 5. Stale symlinked/aliased worktree isolation (#698):
    //    Replacing `a-hidden`'s recorded worktree directory with a symlink to `b-visible`
    //    while `a-hidden` has a distinct recorded branch must never emit an entry or group
    //    borrowing `b-visible`'s metadata.
    #[cfg(unix)]
    {
        fs::remove_dir_all(&wt_a).unwrap();
        std::os::unix::fs::symlink(&wt_b, &wt_a).unwrap();
        for wt in [&wt_b, &wt_a] {
            let payload = run_roadmap_json(wt, &[]);
            let runs = in_progress_worktrees(&payload);
            assert_eq!(
                runs.len(),
                1,
                "symlinked a-hidden -> b-visible from {} must show only b-visible: {runs:?}",
                wt.display()
            );
            assert!(
                runs[0].ends_with("b-visible"),
                "symlinked a-hidden -> b-visible from {} must return b-visible, got {runs:?}",
                wt.display()
            );
            let grouped = run_roadmap_json(wt, &["--group-by", "priority"]);
            let grouped_runs: Vec<String> = grouped["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["specs"].as_array().into_iter().flatten())
                .filter_map(|e| e["worktree"].as_str().map(str::to_owned))
                .collect();
            assert_eq!(
                grouped_runs.len(),
                1,
                "grouped roadmap from {} must show only b-visible: {grouped}",
                wt.display()
            );
            assert!(
                grouped_runs[0].ends_with("b-visible"),
                "grouped roadmap from {} must not borrow b-visible for a-hidden: {grouped}",
                wt.display()
            );
        }
    }

    // 6. Detached linked common-dir alias rejection + valid Git coordinates (#701):
    //    - A detached linked checkout whose `.git` file is repointed to `<primary>/.git`
    //      while primary is detached must be omitted in both default and priority-grouped JSON.
    //    - An intact detached checkout (moved to a `0xFF` Unix name) and a linked checkout
    //      whose private `HEAD` uses `ref:\trefs/heads/...` must remain discoverable.
    let wt_det_alias = root.join(".worktrees/det-alias");
    let wt_det_intact = root.join(".worktrees/det-intact");
    for wt_path in [&wt_det_alias, &wt_det_intact] {
        assert!(Command::new("git")
            .current_dir(root)
            .args(["worktree", "add", "--detach"])
            .arg(wt_path)
            .status()
            .unwrap()
            .success());
    }

    for (wt_path, run_name) in [
        (&wt_det_alias, "det-alias"),
        (&wt_det_intact, "det-intact"),
    ] {
        fs::write(wt_path.join("acdd.yaml"), &custom_field_cfg).unwrap();
        let mut start = Command::new(env!("CARGO_BIN_EXE_acdd"));
        crate::common::isolate_acdd_env(&mut start);
        let out = start
            .args(["-C"])
            .arg(wt_path)
            .args(["start", "docs/milestones/AC-901-shared.md", "--name", run_name])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "start {run_name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Rewrite `wt_b`'s private HEAD to use a tab separator (`ref:\trefs/heads/...`).
    let wt_b_git_dir_out = Command::new("git")
        .current_dir(&wt_b)
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .output()
        .unwrap();
    assert!(wt_b_git_dir_out.status.success());
    let wt_b_git_dir =
        std::path::PathBuf::from(String::from_utf8_lossy(&wt_b_git_dir_out.stdout).trim_end());
    let orig_head = fs::read_to_string(wt_b_git_dir.join("HEAD")).unwrap();
    assert!(orig_head.starts_with("ref: refs/heads/"));
    fs::write(
        wt_b_git_dir.join("HEAD"),
        orig_head.replacen("ref: ", "ref:\t", 1),
    )
    .unwrap();

    // Detach primary and repoint `wt_det_alias/.git` to `<primary>/.git`.
    assert!(Command::new("git")
        .current_dir(root)
        .args(["checkout", "--detach", "HEAD"])
        .status()
        .unwrap()
        .success());
    let primary_git_dir = root.join(".git").canonicalize().unwrap();
    fs::write(
        wt_det_alias.join(".git"),
        format!("gitdir: {}\n", primary_git_dir.display()),
    )
    .unwrap();

    // Unix non-UTF-8 (`0xFF`) moved detached checkout remains discoverable from primary root.
    // macOS APFS rejects non-UTF-8 filenames with EILSEQ.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::ffi::OsStrExt;
        let wt_raw_src = root.join(".worktrees/det-raw-src");
        assert!(Command::new("git")
            .current_dir(root)
            .args(["worktree", "add", "--detach"])
            .arg(&wt_raw_src)
            .status()
            .unwrap()
            .success());
        fs::write(
            wt_raw_src.join("docs/milestones/AC-903-nonutf8.md"),
            "\
---
id: AC-903-nonutf8
title: Non-UTF8 Sibling Spec
status: active
doc_type: milestone
---
### task: raw-task
```yaml
target: Raw Task
scope: [src/lib.rs]
```
",
        )
        .unwrap();
        let wt_raw_dst = root
            .join(".worktrees")
            .join(std::ffi::OsStr::from_bytes(b"det-raw-\xff"));
        assert!(Command::new("git")
            .current_dir(root)
            .args(["worktree", "move"])
            .arg(&wt_raw_src)
            .arg(&wt_raw_dst)
            .status()
            .unwrap()
            .success());
        let raw_payload = run_roadmap_json(root, &["AC-903-nonutf8"]);
        assert!(
            !raw_payload["waves"].as_array().is_none_or(Vec::is_empty)
                || !raw_payload["in_progress"]
                    .as_array()
                    .is_none_or(Vec::is_empty),
            "moved 0xFF detached checkout spec must remain discoverable from root: {raw_payload}"
        );
    }

    let payload = run_roadmap_json(root, &[]);
    let runs = in_progress_worktrees(&payload);
    assert!(
        !runs.iter().any(|w| w.contains("det-alias")),
        "detached alias repointed to primary .git must be omitted: {payload}"
    );
    assert!(
        runs.iter().any(|w| w.contains("det-intact")),
        "intact detached checkout must remain discoverable: {payload}"
    );
    assert!(
        runs.iter().any(|w| w.ends_with("b-visible")),
        "tab-delimited HEAD checkout b-visible must remain discoverable: {payload}"
    );

    let grouped = run_roadmap_json(root, &["--group-by", "priority"]);
    let grouped_runs: Vec<String> = grouped["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["specs"].as_array().into_iter().flatten())
        .filter_map(|e| e["worktree"].as_str().map(str::to_owned))
        .collect();
    assert!(
        !grouped_runs.iter().any(|w| w.contains("det-alias")),
        "grouped roadmap must omit detached alias repointed to primary .git: {grouped}"
    );
    assert!(
        grouped_runs.iter().any(|w| w.contains("det-intact")),
        "grouped roadmap must keep intact detached checkout: {grouped}"
    );
    assert!(
        grouped_runs.iter().any(|w| w.ends_with("b-visible")),
        "grouped roadmap must keep tab-delimited HEAD checkout b-visible: {grouped}"
    );

    // 7. Symlinked `.git` alias rejection & trailing `\r` worktree discovery (#703):
    #[cfg(unix)]
    {
        fs::remove_file(wt_det_alias.join(".git")).unwrap();
        std::os::unix::fs::symlink(&primary_git_dir, wt_det_alias.join(".git")).unwrap();

        let sym_payload = run_roadmap_json(root, &[]);
        let sym_runs = in_progress_worktrees(&sym_payload);
        assert!(
            !sym_runs.iter().any(|w| w.contains("det-alias")),
            "symlinked .git alias must be omitted from default roadmap: {sym_payload}"
        );
        assert!(
            sym_runs.iter().any(|w| w.contains("det-intact")),
            "intact detached checkout must remain visible with symlinked alias present: {sym_payload}"
        );
        for group_by in ["spec", "priority"] {
            let sym_grouped = run_roadmap_json(root, &["--group-by", group_by]);
            let sym_grouped_runs: Vec<String> = sym_grouped["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["specs"].as_array().into_iter().flatten())
                .filter_map(|e| e["worktree"].as_str().map(str::to_owned))
                .collect();
            assert!(
                !sym_grouped_runs.iter().any(|w| w.contains("det-alias")),
                "--group-by {group_by} must omit symlinked .git alias: {sym_grouped}"
            );
            assert!(
                sym_grouped_runs.iter().any(|w| w.contains("det-intact")),
                "--group-by {group_by} must keep intact detached checkout: {sym_grouped}"
            );
        }

        let wt_cr_src = root.join(".worktrees/det-cr-src");
        assert!(Command::new("git")
            .current_dir(root)
            .args(["worktree", "add", "--detach"])
            .arg(&wt_cr_src)
            .status()
            .unwrap()
            .success());
        fs::write(
            wt_cr_src.join("docs/milestones/AC-904-cr.md"),
            "\
---
id: AC-904-cr
title: Trailing CR Sibling Spec
status: active
doc_type: milestone
---
### task: cr-task
```yaml
target: CR Task
scope: [src/lib.rs]
```
",
        )
        .unwrap();
        let wt_cr_dst = root.join(".worktrees/det-cr-\r");
        assert!(Command::new("git")
            .current_dir(root)
            .args(["worktree", "move"])
            .arg(&wt_cr_src)
            .arg(&wt_cr_dst)
            .status()
            .unwrap()
            .success());
        let cr_payload = run_roadmap_json(root, &["AC-904-cr"]);
        assert!(
            !cr_payload["waves"].as_array().is_none_or(Vec::is_empty)
                || !cr_payload["in_progress"]
                    .as_array()
                    .is_none_or(Vec::is_empty),
            "moved trailing-CR detached checkout spec must remain discoverable from root: {cr_payload}"
        );
    }
}
