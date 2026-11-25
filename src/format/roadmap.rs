//! Markdown presentation for `StateQuery::Roadmap` (`RoadmapResp`).

use acdd_state::usecases::{
    RoadmapDepEntry, RoadmapResp, RoadmapSpecEntry, RoadmapTaskEntry, RoadmapWave,
};
use std::collections::BTreeMap;

/// Render a [`RoadmapResp`] as compact operator Markdown.
///
/// `detail` must be `true` only when the request selected a spec (`spec_id.is_some()`).
pub fn render_roadmap_markdown(resp: &RoadmapResp, detail: bool) -> String {
    if detail {
        render_detail(resp)
    } else {
        render_overview(resp)
    }
}

fn render_overview(resp: &RoadmapResp) -> String {
    let wave_count = resp.waves.len();
    let mut out = format!(
        "# Roadmap\n\n_in progress_: **{}** | _waves_: **{wave_count}** | _archived_: **{}**\n",
        resp.in_progress.len(),
        resp.archived_count,
    );

    if !resp.in_progress.is_empty() {
        out.push_str("\n## In Progress\n\n");
        for spec in &resp.in_progress {
            out.push_str(&render_in_progress_spec(spec));
            out.push('\n');
        }
    }

    for wave in &resp.waves {
        out.push_str(&render_wave_section(wave));
    }

    if !resp.archived.is_empty() {
        out.push_str("\n## Archived\n\n");
        for spec in &resp.archived {
            out.push_str(&render_planned_line(spec, usize::MAX, None));
            out.push('\n');
        }
    }

    append_warnings_and_cycles(&mut out, resp);
    out
}

fn render_detail(resp: &RoadmapResp) -> String {
    let spec = resp
        .in_progress
        .first()
        .or_else(|| resp.waves.first().and_then(|w| w.specs.first()))
        .or_else(|| resp.archived.first());
    let Some(spec) = spec else {
        return "# Roadmap\n\n_no matching spec_\n".to_owned();
    };

    let mut out = format!("# {}\n\n", spec.spec_id);
    out.push_str(&format!(
        "**title**: {}\n**collection**: `{}` | **status**: `{}` | **path**: `{}`\n",
        spec.title, spec.collection, spec.status, spec.path
    ));
    if let Some(run_id) = spec.run_id.as_deref() {
        out.push_str(&format!("**run**: `{run_id}`\n"));
    }
    if let Some(wt) = spec.worktree.as_deref() {
        out.push_str(&format!("**worktree**: `{wt}`\n"));
    }
    out.push_str(&format!(
        "**progress**: {}/{} tasks\n",
        spec.completed_tasks, spec.total_tasks
    ));

    if !spec.active_gates.is_empty() {
        out.push_str("\n### Active gates\n\n");
        for gate in &spec.active_gates {
            let worker = gate
                .worker_id
                .as_deref()
                .map(|w| format!(" (`{w}`)"))
                .unwrap_or_default();
            out.push_str(&format!("- `{}` — {}{worker}\n", gate.cursor, gate.status));
        }
    }

    if !spec.depends_on.is_empty() {
        out.push_str("\n### depends_on\n\n");
        for dep in &spec.depends_on {
            out.push_str(&format_dep_line(dep));
        }
    }

    if !spec.blocks.is_empty() {
        out.push_str("\n### blocks\n\n");
        for blocker in &spec.blocks {
            out.push_str(&format!("- `{blocker}`\n"));
        }
    }

    if !spec.scope_overlaps.is_empty() {
        out.push_str(&format!(
            "\n⚠ **scope overlap**: {}\n",
            spec.scope_overlaps.join(", ")
        ));
    }

    if !spec.tasks.is_empty() {
        out.push_str("\n## Task waves\n\n");
        out.push_str(&render_task_waves(&spec.tasks));
    }

    append_warnings_and_cycles(&mut out, resp);
    out
}

fn render_in_progress_spec(spec: &RoadmapSpecEntry) -> String {
    let gates: Vec<String> = spec
        .active_gates
        .iter()
        .map(|g| {
            let worker = g
                .worker_id
                .as_deref()
                .map(|w| format!("@{w}"))
                .unwrap_or_default();
            format!("`{}`{worker}", g.cursor)
        })
        .collect();
    let wt = spec
        .worktree
        .as_deref()
        .map(|w| format!(" | worktree: `{w}`"))
        .unwrap_or_default();
    let mut block = format!(
        "- **{}** — {} ({}/{} tasks){wt}\n",
        spec.spec_id, spec.title, spec.completed_tasks, spec.total_tasks
    );
    if !gates.is_empty() {
        block.push_str(&format!("  - active: {}\n", gates.join(", ")));
    }
    block.push_str(&format!(
        "  - unblocks: {} specs, {} tasks\n",
        spec.unblocks_specs, spec.unblocks_tasks
    ));
    if !spec.tasks.is_empty() {
        block.push_str(&render_task_waves(&spec.tasks));
    }
    block
}

fn render_wave_section(wave: &RoadmapWave) -> String {
    if wave.specs.is_empty() {
        return String::new();
    }
    let heading = if wave.wave == 0 {
        "## Wave 0 — Ready Now\n\n".to_owned()
    } else {
        format!("## Wave {} — Blocked\n\n", wave.wave)
    };
    let mut out = heading;
    for (idx, spec) in wave.specs.iter().enumerate() {
        out.push_str(&render_planned_line(spec, wave.wave, Some(idx + 1)));
        if !spec.tasks.is_empty() {
            out.push_str(&render_task_waves(&spec.tasks));
        }
        out.push('\n');
    }
    out
}

fn render_planned_line(spec: &RoadmapSpecEntry, wave_idx: usize, number: Option<usize>) -> String {
    let prefix = number.map_or(String::new(), |n| format!("{n}. "));
    let overlap = if spec.parallel_safe {
        "parallel-safe".to_owned()
    } else {
        format!("⚠ scope overlap: {}", spec.scope_overlaps.join(", "))
    };
    let waiting = if wave_idx > 0 {
        let unsatisfied = format_waiting_on(&spec.depends_on);
        if unsatisfied.is_empty() {
            String::new()
        } else {
            format!("\n   waiting on: {unsatisfied}")
        }
    } else {
        String::new()
    };

    format!(
        "{prefix}**{}** — {} (_unblocks_: {} specs / {} tasks) — {overlap}{waiting}\n",
        spec.spec_id, spec.title, spec.unblocks_specs, spec.unblocks_tasks,
    )
}

fn format_waiting_on(deps: &[RoadmapDepEntry]) -> String {
    deps.iter()
        .filter(|d| !d.satisfied)
        .map(|d| d.raw.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_dep_line(dep: &RoadmapDepEntry) -> String {
    let mark = if dep.satisfied { "✓" } else { "✗" };
    format!(
        "- `{raw}` → **{status}** {mark}\n",
        raw = dep.raw,
        status = dep.status
    )
}

fn render_task_waves(tasks: &[RoadmapTaskEntry]) -> String {
    let mut by_wave: BTreeMap<usize, Vec<&RoadmapTaskEntry>> = BTreeMap::new();
    for task in tasks {
        by_wave.entry(task.wave).or_default().push(task);
    }
    let mut out = String::new();
    for (wave, rows) in by_wave {
        out.push_str(&format!("#### Wave {wave}\n\n"));
        for task in rows {
            let overlap = if task.scope_overlaps.is_empty() {
                String::new()
            } else {
                format!(" ⚠ overlap: {}", task.scope_overlaps.join(", "))
            };
            out.push_str(&format!(
                "- **{}** — `{}`{overlap}\n",
                task.task_ref, task.status
            ));
            let unsatisfied = format_waiting_on(&task.depends_on);
            if !unsatisfied.is_empty() {
                out.push_str(&format!("  - waiting on: {unsatisfied}\n"));
            }
        }
        out.push('\n');
    }
    out
}

fn append_warnings_and_cycles(out: &mut String, resp: &RoadmapResp) {
    if !resp.warnings.is_empty() {
        out.push_str("\n### Warnings\n\n");
        for warning in &resp.warnings {
            out.push_str(&format!("- {warning}\n"));
        }
    }
    if !resp.cycles.is_empty() {
        out.push_str("\n### Dependency cycles\n\n");
        for cycle in &resp.cycles {
            out.push_str(&format!("- {}\n", cycle.join(" → ")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acdd_state::usecases::{RoadmapDepEntry, RoadmapSpecEntry, RoadmapWave};

    fn spec(id: &str, overlaps: Vec<&str>) -> RoadmapSpecEntry {
        RoadmapSpecEntry {
            spec_id: id.into(),
            title: "Title".into(),
            collection: "milestones".into(),
            path: format!("docs/milestones/{id}.md"),
            doc_type: "contract".into(),
            status: "planned".into(),
            run_id: None,
            worktree: None,
            source_worktree: None,
            divergence: Vec::new(),
            completed_tasks: 0,
            total_tasks: 1,
            active_gates: Vec::new(),
            unblocks_specs: 2,
            unblocks_tasks: 1,
            parallel_safe: overlaps.is_empty(),
            scope_overlaps: overlaps.into_iter().map(str::to_owned).collect(),
            depends_on: Vec::new(),
            blocks: Vec::new(),
            tasks: Vec::new(),
        }
    }

    #[test]
    fn roadmap_markdown_overview_and_detail_contract() {
        let overview = RoadmapResp {
            waves: vec![RoadmapWave {
                wave: 0,
                specs: vec![spec("AC-001", vec!["AC-002"]), spec("AC-002", vec![])],
            }],
            archived_count: 1,
            ..Default::default()
        };
        let md = render_roadmap_markdown(&overview, false);
        assert!(md.contains("# Roadmap"));
        assert!(md.contains("## Wave 0 — Ready Now"));
        assert!(md.contains("scope overlap: AC-002"));

        let single_spec_overview = RoadmapResp {
            waves: vec![RoadmapWave {
                wave: 0,
                specs: vec![spec("AC-001", vec![])],
            }],
            ..Default::default()
        };
        let single_md = render_roadmap_markdown(&single_spec_overview, false);
        assert!(single_md.contains("# Roadmap"));
        assert!(single_md.contains("## Wave 0 — Ready Now"));
        assert!(!single_md.starts_with("# AC-001\n"));

        let detail = RoadmapResp {
            waves: vec![RoadmapWave {
                wave: 1,
                specs: vec![RoadmapSpecEntry {
                    spec_id: "AC-003".into(),
                    title: "Leaf".into(),
                    collection: "milestones".into(),
                    path: "docs/milestones/AC-003.md".into(),
                    doc_type: "contract".into(),
                    status: "planned".into(),
                    run_id: None,
                    worktree: None,
                    source_worktree: None,
                    divergence: Vec::new(),
                    completed_tasks: 0,
                    total_tasks: 1,
                    active_gates: Vec::new(),
                    unblocks_specs: 0,
                    unblocks_tasks: 0,
                    parallel_safe: true,
                    scope_overlaps: Vec::new(),
                    depends_on: vec![RoadmapDepEntry {
                        raw: "PLAN-001".into(),
                        status: "planned".into(),
                        satisfied: false,
                    }],
                    blocks: Vec::new(),
                    tasks: Vec::new(),
                }],
            }],
            ..Default::default()
        };
        let detail_md = render_roadmap_markdown(&detail, true);
        assert!(detail_md.contains("# AC-003"));
        assert!(detail_md.contains("depends_on"));
        assert!(!detail_md.contains("## Wave 0 — Ready Now"));
    }
}
