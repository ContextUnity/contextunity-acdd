//! Markdown presentation for `StateQuery::Roadmap` (`RoadmapResp`).

use acdd_spec::types::{RoadmapBadge, RoadmapGroup};
use acdd_state::usecases::{
    RoadmapDepEntry, RoadmapResp, RoadmapSpecEntry, RoadmapTaskEntry, RoadmapWave,
};
use std::collections::BTreeMap;

/// Pure display options for [`render_roadmap_markdown_with_options`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoadmapRenderOptions {
    /// Emit ANSI SGR color sequences (resolved upstream from `--color`, `NO_COLOR`, TTY, and config).
    pub ansi_color: bool,
    /// Optional grouping axis override (`wave`, `spec`, or custom schema field).
    pub group_by: Option<String>,
}

fn paint(ansi: bool, code: &str, text: &str) -> String {
    if ansi && !text.is_empty() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn status_ansi_code(status: &str) -> &'static str {
    match status {
        "ready" | "completed" | "satisfied" => "32",
        "in_progress" | "active" => "33",
        "planned" | "blocked" => "34",
        "cancelled" | "missing" | "cleared" => "31",
        "archived" => "2",
        _ => "36",
    }
}

fn format_badges(badges: &[RoadmapBadge], ansi: bool) -> String {
    if badges.is_empty() {
        return String::new();
    }
    let formatted: Vec<String> = badges
        .iter()
        .map(|b| {
            let token = format!("[{}: {}]", b.name, b.value);
            let code = if b.name == "readiness" || b.name == "status" {
                status_ansi_code(&b.value)
            } else {
                "36"
            };
            paint(ansi, code, &token)
        })
        .collect();
    format!(" {}", formatted.join(" "))
}

/// Render a [`RoadmapResp`] as compact operator Markdown without ANSI colors.
///
/// `detail` must be `true` only when the request selected a spec (`spec_id.is_some()`).
pub fn render_roadmap_markdown(resp: &RoadmapResp, detail: bool) -> String {
    render_roadmap_markdown_with_options(resp, detail, &RoadmapRenderOptions::default())
}

/// Render a [`RoadmapResp`] as compact operator Markdown using pure [`RoadmapRenderOptions`].
///
/// `detail` must be `true` only when the request selected a spec (`spec_id.is_some()`).
pub fn render_roadmap_markdown_with_options(
    resp: &RoadmapResp,
    detail: bool,
    options: &RoadmapRenderOptions,
) -> String {
    if detail {
        render_detail(resp, options.ansi_color)
    } else {
        render_overview(resp, options)
    }
}

fn render_overview(resp: &RoadmapResp, options: &RoadmapRenderOptions) -> String {
    let ansi = options.ansi_color;
    let wave_count = resp.waves.len();
    let title = paint(ansi, "1;36", "# Roadmap");
    let opt_axis = options
        .group_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resp_axis = resp
        .group_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let effective_axis = opt_axis.or(resp_axis);
    let group_meta = effective_axis
        .filter(|&a| a != "wave")
        .map(|a| format!(" | _grouped by_: **{a}**"))
        .unwrap_or_default();
    let mut out = format!(
        "{title}\n\n_in progress_: **{}** | _waves_: **{wave_count}** | _archived_: **{}**{group_meta}\n",
        resp.in_progress.len(),
        resp.archived_count,
    );

    let use_groups = effective_axis.is_some_and(|a| a != "wave")
        || (effective_axis.is_none() && !resp.groups.is_empty());
    let reuse_resp_groups =
        use_groups && !resp.groups.is_empty() && (opt_axis.is_none() || effective_axis == resp_axis);
    let regrouped: Vec<RoadmapGroup> = if use_groups && !reuse_resp_groups {
        acdd_spec::roadmap::group_roadmap_resp(resp, effective_axis.unwrap_or("group"))
    } else {
        Vec::new()
    };
    let groups = if reuse_resp_groups {
        resp.groups.as_slice()
    } else {
        regrouped.as_slice()
    };

    if use_groups {
        if groups.is_empty() {
            out.push_str("\n_no matching specs_\n");
        } else {
            let axis_prefix = effective_axis.unwrap_or("group");
            for group in groups {
                let heading = format!("## {axis_prefix}: {}", group.group);
                out.push_str(&format!("\n{}\n\n", paint(ansi, "1;36", &heading)));
                let mut planned_num = 0usize;
                for spec in &group.specs {
                    if spec.status == "in_progress" {
                        out.push_str(&render_in_progress_spec(spec, ansi));
                        out.push('\n');
                    } else {
                        planned_num += 1;
                        let show_waiting = !spec.depends_on.iter().all(|d| d.satisfied);
                        out.push_str(&render_planned_line(
                            spec,
                            usize::from(show_waiting),
                            Some(planned_num),
                            ansi,
                        ));
                        if !spec.tasks.is_empty() {
                            out.push_str(&render_task_waves(&spec.tasks, ansi));
                        }
                        out.push('\n');
                    }
                }
            }
        }
        append_warnings_and_cycles(&mut out, resp, ansi);
        return out;
    }

    if !resp.in_progress.is_empty() {
        out.push_str(&format!("\n{}\n\n", paint(ansi, "1;36", "## In Progress")));
        for spec in &resp.in_progress {
            out.push_str(&render_in_progress_spec(spec, ansi));
            out.push('\n');
        }
    }

    for wave in &resp.waves {
        out.push_str(&render_wave_section(wave, ansi));
    }

    if !resp.archived.is_empty() {
        out.push_str(&format!("\n{}\n\n", paint(ansi, "1;36", "## Archived")));
        for spec in &resp.archived {
            out.push_str(&render_planned_line(spec, usize::MAX, None, ansi));
            out.push('\n');
        }
    }

    append_warnings_and_cycles(&mut out, resp, ansi);
    out
}

fn render_detail(resp: &RoadmapResp, ansi: bool) -> String {
    let spec = resp
        .in_progress
        .first()
        .or_else(|| resp.waves.first().and_then(|w| w.specs.first()))
        .or_else(|| resp.archived.first());
    let Some(spec) = spec else {
        return format!("{}\n\n_no matching spec_\n", paint(ansi, "1;36", "# Roadmap"));
    };

    let heading = paint(ansi, "1;36", &format!("# {}", spec.spec_id));
    let status_colored = paint(ansi, status_ansi_code(&spec.status), &spec.status);
    let mut out = format!("{heading}\n\n");
    out.push_str(&format!(
        "**title**: {}\n**spec**: `{}` | **status**: `{}` | **path**: `{}`\n",
        spec.title, spec.spec, status_colored, spec.path
    ));
    if !spec.badges.is_empty() {
        out.push_str(&format!(
            "**badges**:{}\n",
            format_badges(&spec.badges, ansi)
        ));
    }
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
            out.push_str(&format_dep_line(dep, ansi));
        }
    }

    if !spec.blocks.is_empty() {
        out.push_str("\n### blocks\n\n");
        for blocker in &spec.blocks {
            out.push_str(&format!("- `{blocker}`\n"));
        }
    }

    if !spec.scope_overlaps.is_empty() {
        let overlap_text = paint(
            ansi,
            "33",
            &format!("⚠ **scope overlap**: {}", spec.scope_overlaps.join(", ")),
        );
        out.push_str(&format!("\n{overlap_text}\n"));
    }

    if !spec.tasks.is_empty() {
        out.push_str("\n## Task waves\n\n");
        out.push_str(&render_task_waves(&spec.tasks, ansi));
    }

    append_warnings_and_cycles(&mut out, resp, ansi);
    out
}

fn render_in_progress_spec(spec: &RoadmapSpecEntry, ansi: bool) -> String {
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
    let badges = format_badges(&spec.badges, ansi);
    let id_label = paint(ansi, "1", &spec.spec_id);
    let mut block = format!(
        "- **{id_label}** — {}{badges} ({}/{} tasks){wt}\n",
        spec.title, spec.completed_tasks, spec.total_tasks
    );
    if !gates.is_empty() {
        block.push_str(&format!("  - active: {}\n", gates.join(", ")));
    }
    block.push_str(&format!(
        "  - unblocks: {} specs, {} tasks\n",
        spec.unblocks_specs, spec.unblocks_tasks
    ));
    if !spec.tasks.is_empty() {
        block.push_str(&render_task_waves(&spec.tasks, ansi));
    }
    block
}

fn render_wave_section(wave: &RoadmapWave, ansi: bool) -> String {
    if wave.specs.is_empty() {
        return String::new();
    }
    let heading = if wave.wave == 0 {
        format!("\n{}\n\n", paint(ansi, "1;36", "## Wave 0 — Ready Now"))
    } else {
        format!(
            "\n{}\n\n",
            paint(ansi, "1;36", &format!("## Wave {} — Blocked", wave.wave))
        )
    };
    let mut out = heading;
    for (idx, spec) in wave.specs.iter().enumerate() {
        out.push_str(&render_planned_line(spec, wave.wave, Some(idx + 1), ansi));
        if !spec.tasks.is_empty() {
            out.push_str(&render_task_waves(&spec.tasks, ansi));
        }
        out.push('\n');
    }
    out
}

fn render_planned_line(
    spec: &RoadmapSpecEntry,
    wave_idx: usize,
    number: Option<usize>,
    ansi: bool,
) -> String {
    let prefix = number.map_or(String::new(), |n| format!("{n}. "));
    let overlap = if spec.parallel_safe {
        paint(ansi, "32", "parallel-safe")
    } else {
        paint(
            ansi,
            "33",
            &format!("⚠ scope overlap: {}", spec.scope_overlaps.join(", ")),
        )
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
    let badges = format_badges(&spec.badges, ansi);
    let id_label = paint(ansi, "1", &spec.spec_id);

    format!(
        "{prefix}**{id_label}** — {}{badges} (_unblocks_: {} specs / {} tasks) — {overlap}{waiting}\n",
        spec.title, spec.unblocks_specs, spec.unblocks_tasks,
    )
}

fn format_waiting_on(deps: &[RoadmapDepEntry]) -> String {
    deps.iter()
        .filter(|d| !d.satisfied)
        .map(|d| d.raw.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_dep_line(dep: &RoadmapDepEntry, ansi: bool) -> String {
    let mark = if dep.satisfied {
        paint(ansi, "32", "✓")
    } else {
        paint(ansi, "31", "✗")
    };
    let status = paint(ansi, status_ansi_code(&dep.status), &dep.status);
    format!(
        "- `{raw}` → **{status}** {mark}\n",
        raw = dep.raw,
    )
}

fn render_task_waves(tasks: &[RoadmapTaskEntry], ansi: bool) -> String {
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
                paint(
                    ansi,
                    "33",
                    &format!(" ⚠ overlap: {}", task.scope_overlaps.join(", ")),
                )
            };
            let status = paint(ansi, status_ansi_code(&task.status), &task.status);
            out.push_str(&format!(
                "- **{}** — `{status}`{overlap}\n",
                task.task_ref
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

fn append_warnings_and_cycles(out: &mut String, resp: &RoadmapResp, ansi: bool) {
    if !resp.warnings.is_empty() {
        out.push_str(&format!("\n{}\n\n", paint(ansi, "33", "### Warnings")));
        for warning in &resp.warnings {
            out.push_str(&format!("- {}\n", paint(ansi, "33", warning)));
        }
    }
    if !resp.cycles.is_empty() {
        out.push_str(&format!(
            "\n{}\n\n",
            paint(ansi, "31", "### Dependency cycles")
        ));
        for cycle in &resp.cycles {
            out.push_str(&format!("- {}\n", paint(ansi, "31", &cycle.join(" → "))));
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
            spec: "milestones".into(),
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
            badges: Vec::new(),
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
        assert!(!md.contains('\x1b'));

        let colored = render_roadmap_markdown_with_options(
            &overview,
            false,
            &RoadmapRenderOptions {
                ansi_color: true,
                group_by: None,
            },
        );
        assert!(colored.contains("\x1b[1;36m# Roadmap\x1b[0m"));
        assert!(colored.contains("\x1b[32mparallel-safe\x1b[0m"));
        assert!(colored.contains("\x1b[33m⚠ scope overlap: AC-002\x1b[0m"));

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

        let mut badged = spec("AC-003", vec![]);
        badged.badges = vec![
            RoadmapBadge {
                name: "priority".into(),
                value: "high".into(),
            },
            RoadmapBadge {
                name: "readiness".into(),
                value: "blocked".into(),
            },
        ];
        badged.depends_on = vec![RoadmapDepEntry {
            raw: "PLAN-001".into(),
            status: "planned".into(),
            satisfied: false,
        }];
        let grouped = RoadmapResp {
            group_by: Some("priority".into()),
            groups: vec![RoadmapGroup {
                group: "high".into(),
                specs: vec![badged.clone()],
            }],
            waves: vec![RoadmapWave {
                wave: 1,
                specs: vec![badged.clone()],
            }],
            ..Default::default()
        };
        let grouped_md = render_roadmap_markdown_with_options(
            &grouped,
            false,
            &RoadmapRenderOptions {
                ansi_color: false,
                group_by: Some("priority".into()),
            },
        );
        assert!(grouped_md.contains("## priority: high"));
        assert!(grouped_md.contains("[priority: high] [readiness: blocked]"));
        assert!(!grouped_md.contains('\x1b'));

        let wave_override_md = render_roadmap_markdown_with_options(
            &grouped,
            false,
            &RoadmapRenderOptions {
                ansi_color: false,
                group_by: Some("wave".into()),
            },
        );
        assert!(wave_override_md.contains("## Wave 1 — Blocked"));
        assert!(!wave_override_md.contains("## wave:"));

        let spec_override_md = render_roadmap_markdown_with_options(
            &grouped,
            false,
            &RoadmapRenderOptions {
                ansi_color: false,
                group_by: Some("spec".into()),
            },
        );
        assert!(spec_override_md.contains("## spec: milestones"));
        assert!(!spec_override_md.contains("## spec: high"));

        let mut active_spec = spec("AC-000", vec![]);
        active_spec.status = "in_progress".into();
        let ws_grouped = RoadmapResp {
            group_by: Some("priority".into()),
            groups: vec![RoadmapGroup {
                group: "high".into(),
                specs: vec![active_spec, badged.clone()],
            }],
            ..grouped.clone()
        };
        let ws_md = render_roadmap_markdown_with_options(
            &ws_grouped,
            false,
            &RoadmapRenderOptions {
                ansi_color: false,
                group_by: Some("   ".into()),
            },
        );
        assert!(ws_md.contains("## priority: high"));
        assert!(!ws_md.contains("## group:"));
        assert!(ws_md.contains("1. **AC-003**"));

        let detail = RoadmapResp {
            waves: vec![RoadmapWave {
                wave: 1,
                specs: vec![RoadmapSpecEntry {
                    spec_id: "AC-003".into(),
                    title: "Leaf".into(),
                    spec: "milestones".into(),
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
                    badges: vec![RoadmapBadge {
                        name: "priority".into(),
                        value: "high".into(),
                    }],
                    tasks: Vec::new(),
                }],
            }],
            ..Default::default()
        };
        let detail_md = render_roadmap_markdown(&detail, true);
        assert!(detail_md.contains("# AC-003"));
        assert!(detail_md.contains("depends_on"));
        assert!(detail_md.contains("[priority: high]"));
        assert!(!detail_md.contains("## Wave 0 — Ready Now"));
    }
}
