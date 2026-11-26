//! Compact Markdown presentation for the 6 `code_map_analyze` diagnostic modes.

use super::core::{escape_text, inline_code, page_items, scalar_text, text_field, u64_field};
use super::traversal::{format_repo_location, page_has_no_display_entries, write_page_stats};
use serde_json::Value;

pub(super) fn render_if_analyze(value: &Value, title: &str) -> Option<String> {
    if title != "code_map_analyze" && !title.ends_with("analyze") {
        return None;
    }
    let mode = text_field(value, "mode")?;
    let mut out = format!("# {} ({mode})\n\n", escape_text(title));
    match mode {
        "hotspots" => render_hotspots(&mut out, value),
        "boundary" => render_boundary(&mut out, value),
        "path" => render_path(&mut out, value),
        "entrypoints" => render_entrypoints(&mut out, value),
        "diff_impact" => render_diff_impact(&mut out, value),
        "cochange" => render_cochange(&mut out, value),
        _ => return None,
    }
    Some(out)
}

fn write_summary(out: &mut String, obj: &Value, keys: &[(&str, &str)]) {
    let parts: Vec<String> = keys
        .iter()
        .filter_map(|(k, lbl)| {
            obj.get(*k)
                .filter(|v| !v.is_null())
                .map(|v| format!("**{lbl}:** `{}`", scalar_text(v)))
        })
        .collect();
    if !parts.is_empty() {
        out.push_str(&format!("- {}\n", parts.join(" | ")));
    }
}

fn write_obj_summary(out: &mut String, root: &Value, field: &str, keys: &[(&str, &str)]) {
    if let Some(sub) = root.get(field) {
        write_summary(out, sub, keys);
    }
}

fn render_section(
    out: &mut String,
    value: &Value,
    field: &str,
    heading: &str,
    mut fmt_item: impl FnMut(&Value) -> String,
) {
    let Some(page) = value.get(field) else {
        return;
    };
    let items = page_items(page);
    if page_has_no_display_entries(page, items) {
        return;
    }
    out.push_str(&format!("\n## {heading}\n\n"));
    write_page_stats(out, page, items);
    for item in items.into_iter().flatten() {
        out.push_str(&format!("- {}\n", fmt_item(item)));
    }
}

fn sym_ref(item: &Value) -> String {
    let name = text_field(item, "qualname")
        .or_else(|| text_field(item, "id"))
        .or_else(|| text_field(item, "name"))
        .unwrap_or("symbol");
    match format_repo_location(item) {
        Some(loc) => format!("{} ({})", inline_code(name), inline_code(&loc)),
        None => inline_code(name),
    }
}

fn render_hotspots(out: &mut String, v: &Value) {
    write_summary(out, v, &[("target", "target")]);
    write_obj_summary(
        out,
        v,
        "summary",
        &[
            ("total_symbols", "symbols"),
            ("tested_symbols", "tested"),
            ("untested_symbols", "untested"),
        ],
    );
    write_obj_summary(
        out,
        v,
        "pagerank",
        &[
            ("damping", "damping"),
            ("iterations", "iters"),
            ("converged", "converged"),
            ("graph_nodes", "nodes"),
            ("graph_edges", "edges"),
        ],
    );
    render_section(out, v, "hotspots", "Hotspots", |i| {
        let cov = if i["test_covered"].as_bool() == Some(true) {
            "tested"
        } else {
            "untested"
        };
        let (sc, pr) = (
            scalar_text(&i["hotspot_score"]),
            scalar_text(&i["pagerank"]),
        );
        let (fi, fo, cx) = (
            u64_field(i, "fan_in").unwrap_or(0),
            u64_field(i, "fan_out").unwrap_or(0),
            u64_field(i, "complexity_proxy").unwrap_or(0),
        );
        format!(
            "{} — score `{sc}`, pr `{pr}`, in `{fi}`/out `{fo}`, cx `{cx}`, `{cov}`",
            sym_ref(i)
        )
    });
}

fn render_boundary(out: &mut String, v: &Value) {
    write_summary(out, v, &[("target", "target")]);
    write_obj_summary(
        out,
        v,
        "summary",
        &[
            ("scoped_symbols", "symbols"),
            ("public_symbols", "public"),
            ("inbound_external_callers", "inbound"),
            ("internal_only_public_count", "internal_only_pub"),
        ],
    );
    render_section(out, v, "inbound_callers", "Inbound external callers", |i| {
        let loc = text_field(i, "caller_path")
            .map(|p| match u64_field(i, "caller_line") {
                Some(l) => format!(" ({})", inline_code(&format!("{p}:{l}"))),
                None => format!(" ({})", inline_code(p)),
            })
            .unwrap_or_default();
        let (c, k, t) = (
            text_field(i, "caller").unwrap_or("?"),
            text_field(i, "edge_kind").unwrap_or("edge"),
            text_field(i, "target").unwrap_or("?"),
        );
        format!("{} --[{k}]--> {}{loc}", inline_code(c), inline_code(t))
    });
    render_section(
        out,
        v,
        "outbound_dependencies",
        "Outbound dependencies",
        |i| {
            let (t, s, n) = (
                text_field(i, "target").unwrap_or("?"),
                text_field(i, "scope").unwrap_or("external"),
                u64_field(i, "edge_count").unwrap_or(0),
            );
            format!("{} [{s}] (`{n}` edges)", inline_code(t))
        },
    );
    render_section(
        out,
        v,
        "internal_only_public",
        "Internal-only public symbols",
        |i| {
            format!(
                "{} — external `0`, internal `{}`",
                sym_ref(i),
                u64_field(i, "internal_caller_count").unwrap_or(0)
            )
        },
    );
}

fn render_path(out: &mut String, v: &Value) {
    for (k, lbl) in [("from", "From"), ("to", "To")] {
        if let Some(node) = v.get(k) {
            out.push_str(&format!("- **{lbl}:** {}\n", sym_ref(node)));
        }
    }
    write_summary(
        out,
        v,
        &[
            ("reachable", "reachable"),
            ("shortest_hops", "shortest_hops"),
            ("total_paths", "paths"),
        ],
    );
    let Some(paths) = v.get("paths").and_then(Value::as_array) else {
        return;
    };
    out.push_str("\n## Paths\n\n");
    if paths.is_empty() {
        out.push_str("No path found within depth budget.\n");
    }
    for (idx, p) in paths.iter().enumerate() {
        let hops = u64_field(p, "hops").unwrap_or(0);
        let chain: Vec<String> = p
            .get("edges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|e| {
                format!(
                    "{} --[{}]--> {}",
                    inline_code(text_field(e, "src").unwrap_or("?")),
                    text_field(e, "kind").unwrap_or("edge"),
                    inline_code(text_field(e, "dst").unwrap_or("?"))
                )
            })
            .collect();
        if chain.is_empty() {
            out.push_str(&format!("{}. (`{hops}` hops, self)\n", idx + 1));
        } else {
            out.push_str(&format!(
                "{}. (`{hops}` hops) {}\n",
                idx + 1,
                chain.join(" -> ")
            ));
        }
    }
}

fn render_entrypoints(out: &mut String, v: &Value) {
    if let Some(t) = v.get("target") {
        out.push_str(&format!("- **Target:** {}\n", sym_ref(t)));
    }
    write_obj_summary(
        out,
        v,
        "summary",
        &[
            ("total_roots", "roots"),
            ("explicit_entrypoints", "explicit"),
            ("uncalled_public_roots", "public_roots"),
        ],
    );
    render_section(out, v, "roots", "Entrypoint roots", |i| {
        let chain: Vec<&str> = i
            .get("call_path")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let suffix = if chain.is_empty() {
            String::new()
        } else {
            format!(" — {}", chain.join(" -> "))
        };
        format!(
            "[{}] {} (`{}` hops){suffix}",
            text_field(i, "category").unwrap_or("root"),
            sym_ref(i),
            u64_field(i, "hops").unwrap_or(0)
        )
    });
}

fn render_diff_impact(out: &mut String, v: &Value) {
    write_summary(
        out,
        v,
        &[
            ("target", "target"),
            ("base", "base"),
            ("complete", "complete"),
        ],
    );
    render_section(out, v, "changed_symbols", "Changed symbols", |i| {
        let sig = if i["signature_touched"].as_bool() == Some(true) {
            " `[signature_touched]`"
        } else {
            ""
        };
        format!("{}{sig}", sym_ref(i))
    });
    render_section(out, v, "uncovered_symbols", "Uncovered symbols", sym_ref);
    render_section(out, v, "tests", "Affected tests", |i| {
        format!(
            "{} (`{}` hops)",
            sym_ref(i),
            u64_field(i, "hops").unwrap_or(0)
        )
    });
    render_section(out, v, "affected_docs", "Governed documents", |i| {
        let loc = format_repo_location(i)
            .map(|l| inline_code(&l))
            .unwrap_or_default();
        let (inv, title) = (
            text_field(i, "invariant_id").unwrap_or("INV"),
            text_field(i, "section_title").unwrap_or(""),
        );
        format!("{} {loc} — {title}", inline_code(inv))
    });
    render_section(out, v, "nodes", "Reached nodes", |i| {
        format!(
            "{} (d=`{}`)",
            sym_ref(i),
            u64_field(i, "distance").unwrap_or(0)
        )
    });
    render_section(out, v, "unmapped_changes", "Unmapped changes", |i| {
        format!(
            "{} — `{}`",
            inline_code(text_field(i, "path").unwrap_or("?")),
            text_field(i, "reason").unwrap_or("unmapped")
        )
    });
}

fn render_cochange(out: &mut String, v: &Value) {
    write_summary(out, v, &[("target", "target"), ("base", "base")]);
    write_obj_summary(
        out,
        v,
        "summary",
        &[
            ("commits_scanned", "commits_scanned"),
            ("commits_skipped_large", "skipped_large"),
            ("unlinked_pairs", "unlinked_pairs"),
        ],
    );
    render_section(out, v, "pairs", "Unlinked co-change pairs", |i| {
        let (a, b) = (
            text_field(i, "file_a").unwrap_or("?"),
            text_field(i, "file_b").unwrap_or("?"),
        );
        let (c, conf) = (
            u64_field(i, "cochange_count").unwrap_or(0),
            scalar_text(&i["confidence"]),
        );
        format!(
            "{} <-> {} — `{c}` co-changes (confidence `{conf}`)",
            inline_code(a),
            inline_code(b)
        )
    });
}
