use super::core::{
    escape_label, escape_text, inline_code, line_number, page_items, render_markdown_fields,
    scalar_text, text_field, u64_field,
};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) enum TraversalKind {
    Impact,
    Explain,
    Tests,
    DeadTails,
    Duplicates,
}

pub(super) fn kind_for(title: &str, value: &Value) -> Option<TraversalKind> {
    match title {
        "code_map_impact" => Some(TraversalKind::Impact),
        "code_map_explain" => Some(TraversalKind::Explain),
        "code_map_tests" => Some(TraversalKind::Tests),
        "code_map_query" if value.get("incoming").is_some() || value.get("outgoing").is_some() => {
            Some(TraversalKind::Explain)
        }
        "code_map_query"
            if value.get("direction").is_some()
                && value.get("depth").is_some()
                && value.get("nodes").is_some() =>
        {
            Some(TraversalKind::Impact)
        }
        "code_map_query" if text_field(value, "operation") == Some("dead_tails") => {
            Some(TraversalKind::DeadTails)
        }
        "code_map_query"
            if value
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.first().is_some_and(|first| {
                        first.get("duplicate_count").is_some() && first.get("locations").is_some()
                    })
                }) =>
        {
            Some(TraversalKind::Duplicates)
        }
        _ => None,
    }
}

pub(super) fn render(value: &Value, title: &str, kind: TraversalKind) -> String {
    let mut out = format!("# {}\n\n", escape_text(title));
    match kind {
        TraversalKind::Impact => render_impact(&mut out, value),
        TraversalKind::Explain => render_explain(&mut out, value),
        TraversalKind::Tests => render_tests(&mut out, value),
        TraversalKind::DeadTails => render_dead_tails(&mut out, value),
        TraversalKind::Duplicates => render_duplicates(&mut out, value),
    }
    out
}

fn render_summary_fields(out: &mut String, heading: &str, value: &Value, fields: &[&str]) {
    out.push_str(&format!("## {heading}\n\n"));
    for field in fields {
        if let Some(v) = value.get(*field) {
            let lbl: String = field
                .split('_')
                .map(|p| {
                    let mut c = p.chars();
                    c.next()
                        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
                })
                .collect::<Vec<_>>()
                .join(" ");
            let rendered = scalar_text(v);
            let formatted = if v.is_string() {
                rendered
            } else {
                inline_code(&rendered)
            };
            out.push_str(&format!("- **{lbl}:** {formatted}\n"));
        }
    }
}

fn render_impact(out: &mut String, value: &Value) {
    render_summary_fields(
        out,
        "Traversal",
        value,
        &["direction", "depth", "mode", "complete"],
    );
    if value.get("complete") == Some(&Value::Bool(false)) {
        out.push_str("\nTraversal is incomplete within the reported bounds; review unmapped changes and continuation details below.\n");
    }
    if let Some(sel) = value.get("selector") {
        out.push_str("## Selected target\n\n");
        render_record(out, sel, "symbol", &[]);
    }
    if let Some(inv) = value.get("governing_invariants") {
        render_invariants(out, inv);
    }
    let mut consumed = vec![
        "direction",
        "depth",
        "mode",
        "complete",
        "selector",
        "governing_invariants",
    ];
    for (field, heading) in [
        ("nodes", "Reached nodes"),
        ("changed_symbols", "Changed symbols"),
        ("tests", "Affected tests"),
        ("unmapped_changes", "Unmapped changes"),
        ("affected_docs", "Affected documents"),
    ] {
        if let Some(page) = value.get(field) {
            render_page(out, heading, page, |o, item| match field {
                "nodes" => render_impact_node(o, item),
                "tests" => render_test_item(o, item, value),
                _ => render_record(o, item, "item", &[]),
            });
            consumed.push(field);
        }
    }
    if let Some(changes) = value.get("changes") {
        out.push_str("## Git change view\n\n");
        if let Some(files) = changes.get("files").and_then(Value::as_array) {
            out.push_str("### Changed files\n\n");
            if files.is_empty() {
                out.push_str("No changed files in this Git view.\n");
            }
            for f in files {
                render_record(out, f, "changed file", &[]);
            }
            render_markdown_fields(out, changes, 0, &["files"]);
        } else {
            render_markdown_fields(out, changes, 0, &[]);
        }
        consumed.push("changes");
    }
    render_markdown_fields(out, value, 0, &consumed);
}

fn render_explain(out: &mut String, value: &Value) {
    if let Some(node) = value.get("node") {
        out.push_str("## Selected symbol\n\n");
        render_record(out, node, "symbol", &[]);
    }
    if let Some(inv) = value.get("governing_invariants") {
        render_invariants(out, inv);
    }
    let mut consumed = vec!["node", "governing_invariants"];
    for (field, heading, is_edge) in [
        ("incoming", "Inbound relationships", true),
        ("outgoing", "Outbound relationships", true),
        ("documents", "Verified documentation", false),
        ("affected_docs", "Affected documents", false),
        ("analysis_gaps", "Analysis gaps", false),
    ] {
        if let Some(page) = value.get(field) {
            render_page(out, heading, page, |o, item| {
                if is_edge {
                    render_edge(o, item);
                } else {
                    render_record(o, item, "document", &[]);
                }
            });
            consumed.push(field);
        }
    }
    render_markdown_fields(out, value, 0, &consumed);
}

fn render_tests(out: &mut String, value: &Value) {
    render_summary_fields(
        out,
        "Test mapping",
        value,
        &[
            "direction",
            "discovery_method",
            "unresolved_references",
            "scope",
        ],
    );
    match text_field(value, "discovery_method") {
        Some("lexical_fallback") => out.push_str(
            "\n> [!NOTE]\n> Lexical fallback is unverified and supplies no graph call path.\n",
        ),
        Some("graph") => out.push_str("\nDiscovery uses verified exact-call graph paths.\n"),
        _ => {}
    }
    if let Some(c) = u64_field(value, "unresolved_references").filter(|c| *c > 0) {
        out.push_str(&format!("\nThe selected symbol scope contains {c} unresolved reference(s), which may conceal dynamic test callers.\n"));
    }
    if let Some(sel) = value.get("selector") {
        out.push_str("## Selected symbol\n\n");
        render_record(out, sel, "symbol", &[]);
    }
    let mut consumed = vec![
        "direction",
        "discovery_method",
        "unresolved_references",
        "scope",
        "selector",
    ];
    if let Some(page) = value.get("nodes") {
        render_page(out, "Related tests and dependencies", page, |o, item| {
            render_test_item(o, item, value)
        });
        consumed.push("nodes");
    }
    render_markdown_fields(out, value, 0, &consumed);
}

fn format_repo_location(item: &Value) -> Option<String> {
    let path = text_field(item, "path")?;
    let line = u64_field(item, "line");
    let end_line = u64_field(item, "end_line");
    Some(match (line, end_line) {
        (Some(l), Some(e)) if e > l => format!("{path}:{l}-{e}"),
        (Some(l), _) => format!("{path}:{l}"),
        (None, _) => path.to_owned(),
    })
}

fn page_has_no_display_entries(page: &Value, items: Option<&[Value]>) -> bool {
    items.is_none_or(<[Value]>::is_empty)
        && u64_field(page, "total") == Some(0)
        && page.get("has_more") != Some(&Value::Bool(true))
        && page.get("next_offset").is_none_or(Value::is_null)
        && page.get("omitted") != Some(&Value::Bool(true))
}

fn write_page_stats(out: &mut String, page: &Value, items: Option<&[Value]>) {
    let total = u64_field(page, "total").unwrap_or_else(|| items.map_or(0, |a| a.len() as u64));
    let has_more = page
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    out.push_str(&format!(
        "- **total**: `{total}` | **has_more**: `{has_more}`"
    ));
    if let Some(next_offset) = u64_field(page, "next_offset") {
        out.push_str(&format!(" | **next_offset**: `{next_offset}`"));
    }
    out.push_str("\n\n");
}

fn render_duplicates(out: &mut String, value: &Value) {
    out.push_str("## Duplicate code blocks\n\n");
    let items = value.get("items").and_then(Value::as_array);
    write_page_stats(out, value, items.map(Vec::as_slice));
    for item in items.into_iter().flatten() {
        let name = text_field(item, "name").unwrap_or("unnamed");
        let kind = text_field(item, "kind").unwrap_or("block");
        let match_type = text_field(item, "match_type").unwrap_or("exact");
        let line_count = u64_field(item, "line_count").unwrap_or(0);
        let dup_count = u64_field(item, "duplicate_count").unwrap_or(0);
        let dup_lines = u64_field(item, "duplicated_lines").unwrap_or(0);
        let locs: Vec<String> = item
            .get("locations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|loc| format_repo_location(loc).map(|s| inline_code(&s)))
            .collect();
        out.push_str(&format!(
            "- {} ({kind}, {match_type}, {line_count}L x {dup_count} = {dup_lines} dup lines): {}\n",
            inline_code(name),
            locs.join(", ")
        ));
    }
}

fn format_dead_tail_symbol(item: &Value) -> String {
    let qn = text_field(item, "qualname")
        .or_else(|| text_field(item, "name"))
        .unwrap_or("symbol");
    let kind = text_field(item, "kind").unwrap_or("symbol");
    let lines = u64_field(item, "lines").unwrap_or(0);
    let loc = format_repo_location(item)
        .map(|l| format!(" — {}", inline_code(&l)))
        .unwrap_or_default();
    format!("{} ({kind}, {lines}L){loc}", inline_code(qn))
}

fn render_dead_tails(out: &mut String, value: &Value) {
    out.push_str("## Dead tails & phantom-tested code\n\n");
    if let Some(scope) = value.get("scope").filter(|v| !v.is_null()) {
        let s = scalar_text(scope);
        let fmt = if scope.is_string() {
            s
        } else {
            inline_code(&s)
        };
        out.push_str(&format!("- **Scope:** {fmt}\n"));
    }
    if let Some(summary) = value.get("summary").filter(|v| v.is_object()) {
        let p = u64_field(summary, "phantom_tested_count").unwrap_or(0);
        let d = u64_field(summary, "dead_tails_count").unwrap_or(0);
        let c = u64_field(summary, "dangling_calls_count").unwrap_or(0);
        out.push_str(&format!(
            "- **Phantom tested:** `{p}` | **Dead tails:** `{d}` | **Dangling calls:** `{c}`\n"
        ));
    }
    let mut consumed = vec!["operation", "scope", "summary"];
    for (field, heading) in [
        ("phantom_tested", "Phantom-tested production code"),
        ("dead_tails", "Dead tails"),
        ("dangling_calls", "Dangling internal calls"),
    ] {
        if let Some(page) = value.get(field) {
            let items = page_items(page);
            if page_has_no_display_entries(page, items) {
                consumed.push(field);
                continue;
            }
            out.push_str(&format!("\n## {}\n\n", escape_text(heading)));
            write_page_stats(out, page, items);
            for item in items.into_iter().flatten() {
                match field {
                    "phantom_tested" => {
                        let base = format_dead_tail_symbol(item);
                        let callers: Vec<String> = item
                            .get("test_callers")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .map(|c| {
                                let name = text_field(c, "name").unwrap_or("test");
                                match format_repo_location(c) {
                                    Some(l) => format!("{name} (`{l}`)"),
                                    None => name.to_owned(),
                                }
                            })
                            .collect();
                        if callers.is_empty() {
                            out.push_str(&format!("- {base}\n"));
                        } else {
                            out.push_str(&format!("- {base} — tests: {}\n", callers.join(", ")));
                        }
                    }
                    "dangling_calls" => {
                        let sym = text_field(item, "symbol").unwrap_or("unknown symbol");
                        let kind = text_field(item, "kind").unwrap_or("unknown");
                        let res = text_field(item, "resolution").unwrap_or("unresolved");
                        let loc = format_repo_location(item)
                            .map(|l| format!(" — {}", inline_code(&l)))
                            .unwrap_or_default();
                        out.push_str(&format!("- {} ({kind}, {res}){loc}\n", inline_code(sym)));
                    }
                    _ => out.push_str(&format!("- {}\n", format_dead_tail_symbol(item))),
                }
            }
            consumed.push(field);
        }
    }
    render_markdown_fields(out, value, 0, &consumed);
}

fn render_name(out: &mut String, name: &str, uri: Option<&str>, bold: bool) {
    match uri {
        Some(u) => out.push_str(&format!("[{}]({u})", escape_label(name))),
        None if bold => out.push_str(&format!("**{}**", escape_text(name))),
        None => out.push_str(&escape_text(name)),
    }
}

fn render_record(out: &mut String, value: &Value, fallback: &str, extra_excluded: &[&str]) {
    if !value.is_object() {
        render_markdown_fields(out, value, 1, &[]);
        return;
    }
    render_record_heading(out, value, fallback, true);
    let mut excl = record_exclusions(value);
    excl.extend_from_slice(extra_excluded);
    render_markdown_fields(out, value, 1, &excl);
}

fn render_record_heading(out: &mut String, item: &Value, fallback: &str, include_role: bool) {
    let name = text_field(item, "name")
        .or_else(|| text_field(item, "qualname"))
        .unwrap_or(fallback);
    out.push_str("- ");
    render_name(out, name, text_field(item, "uri"), true);
    if include_role {
        if let Some(r) = text_field(item, "test_role") {
            out.push_str(&format!(" ({})", inline_code(r)));
        }
    }
    if let Some(id) = text_field(item, "id") {
        out.push_str(&format!(" — id {}", inline_code(id)));
    }
    append_location(out, item, " — ");
    out.push('\n');
}

fn record_exclusions(value: &Value) -> Vec<&'static str> {
    let mut excl = Vec::new();
    let (name, qualname) = (text_field(value, "name"), text_field(value, "qualname"));
    if name.is_some() {
        excl.push("name");
        if qualname == name {
            excl.push("qualname");
        }
    } else if qualname.is_some() {
        excl.push("qualname");
    }
    for key in ["id", "path", "test_role", "uri"] {
        if text_field(value, key).is_some() {
            excl.push(key);
        }
    }
    if text_field(value, "path").is_some() && line_number(value).is_some() {
        excl.push("line");
    }
    excl
}

fn render_impact_node(out: &mut String, value: &Value) {
    let consumed: Vec<&str> = ["edge", "via"]
        .into_iter()
        .filter(|k| value.get(*k).is_some())
        .collect();
    render_record(out, value, "reached node", &consumed);
    if let Some(edge) = value.get("edge") {
        render_edge(out, edge);
    }
    if let Some(via) = value.get("via") {
        if via.is_object() {
            out.push_str(&format!(
                "  - Via {} using {}; this depth-grouped path has no call-site coordinate.\n",
                inline_code(text_field(via, "id").unwrap_or("unknown predecessor")),
                inline_code(text_field(via, "kind").unwrap_or("unknown relation"))
            ));
            render_markdown_fields(out, via, 2, &["id", "kind"]);
        } else {
            out.push_str("  - Via details: ");
            render_markdown_fields(out, via, 2, &[]);
        }
    }
}

fn render_edge(out: &mut String, edge: &Value) {
    let (kind, confidence) = (
        text_field(edge, "kind").unwrap_or("relationship"),
        text_field(edge, "confidence"),
    );
    let badge = match (kind, confidence) {
        ("calls", Some("exact")) => "[proven call] ",
        ("possible_calls", _) => "[heuristic / candidate] ",
        _ => "",
    };
    out.push_str(&format!("  - {badge}{}", inline_code(kind)));
    if let Some(s) = text_field(edge, "src_public_id") {
        out.push_str(&format!(" from {}", inline_code(s)));
    }
    if let Some(t) = text_field(edge, "dst_public_id") {
        out.push_str(&format!(" to {}", inline_code(t)));
    }
    if let Some(d) = text_field(edge, "direction") {
        out.push_str(&format!(" ({})", escape_text(d)));
    }
    append_location(out, edge, " at ");
    if let Some(c) = confidence {
        out.push_str(&format!("; confidence {}", inline_code(c)));
    }
    if let Some(n) = edge.get("occurrence_count") {
        let s = scalar_text(n);
        out.push_str(&format!(
            "; occurrences {}",
            if n.is_string() { s } else { inline_code(&s) }
        ));
    }
    out.push('\n');
    render_markdown_fields(
        out,
        edge,
        2,
        &[
            "kind",
            "confidence",
            "src_public_id",
            "dst_public_id",
            "direction",
            "path",
            "line",
            "uri",
            "occurrence_count",
        ],
    );
}

fn render_test_item(out: &mut String, item: &Value, response: &Value) {
    render_record_heading(out, item, "test result", false);
    if let Some(r) = text_field(item, "test_role") {
        out.push_str(&format!("  - Supplied role: {}\n", inline_code(r)));
    }
    let Some(conn) = item.get("connection") else {
        render_markdown_fields(out, item, 1, &record_exclusions(item));
        return;
    };
    let relation = text_field(conn, "relation");
    if let Some(rel) = relation {
        out.push_str(&format!("  - Relationship: {}\n", inline_code(rel)));
    }
    let mut conn_excl: Vec<&str> = relation.map(|_| "relation").into_iter().collect();
    if text_field(response, "direction") == Some("inbound")
        && text_field(response, "discovery_method") == Some("graph")
    {
        if let (Some(ids), Some(nodes)) = (
            conn.get("call_path").and_then(Value::as_array),
            conn.get("verified_call_path_nodes")
                .and_then(Value::as_array),
        ) {
            if ids.len() == nodes.len()
                && !nodes.is_empty()
                && ids
                    .iter()
                    .zip(nodes)
                    .all(|(id, n)| id.as_str().is_some() && n.get("id") == Some(id))
            {
                out.push_str("  - Verified call path: ");
                for (i, node) in nodes.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" -> ");
                    }
                    let name = text_field(node, "name")
                        .or_else(|| text_field(node, "qualname"))
                        .or_else(|| text_field(node, "id"))
                        .unwrap_or("unavailable node");
                    render_name(out, name, text_field(node, "uri"), false);
                    if let Some(id) = text_field(node, "id") {
                        out.push_str(&format!(" {}", inline_code(id)));
                    }
                    append_location(out, node, " at ");
                    if let Some(r) = text_field(node, "test_role") {
                        out.push_str(&format!(" ({})", inline_code(r)));
                    }
                    if i + 1 == nodes.len()
                        && text_field(node, "id")
                            .is_some_and(|id| response["selector"]["id"].as_str() == Some(id))
                    {
                        out.push_str(" (target)");
                    }
                    render_markdown_fields(out, node, 2, &record_exclusions(node));
                }
                out.push('\n');
                conn_excl.extend(["call_path", "verified_call_path_nodes"]);
            }
        }
    }
    if conn
        .as_object()
        .is_some_and(|f| f.keys().any(|k| !conn_excl.contains(&k.as_str())))
    {
        out.push_str("  - Connection details:\n");
        render_markdown_fields(out, conn, 2, &conn_excl);
    } else if !conn.is_object() {
        render_markdown_fields(out, conn, 2, &[]);
    } else if conn.as_object().is_some_and(|f| f.is_empty()) {
        out.push_str("  - Connection is an empty object.\n");
    }
    let mut excluded = record_exclusions(item);
    excluded.push("connection");
    render_markdown_fields(out, item, 1, &excluded);
}

fn render_invariants(out: &mut String, value: &Value) {
    let mut excl = Vec::new();
    if let Some(rules) = value
        .get("direct")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
    {
        excl.push("direct");
        out.push_str("\n> [!IMPORTANT] Governing Invariants\n");
        for rule in rules {
            let (id, text) = (
                text_field(rule, "id").unwrap_or("identifier unavailable"),
                text_field(rule, "rule").unwrap_or("rule text unavailable"),
            );
            for (i, line) in text.lines().enumerate() {
                if i == 0 {
                    out.push_str(&format!("> - {}: {}", inline_code(id), escape_text(line)));
                } else {
                    out.push_str(&format!("\n>   {}", escape_text(line)));
                }
            }
            append_location(out, &rule["source"], " — ");
            out.push('\n');
            render_markdown_fields(out, rule, 2, &["id", "rule", "source"]);
            if let Some(src) = rule.get("source") {
                render_markdown_fields(out, src, 2, &["path", "line", "uri"]);
            }
        }
        out.push('\n');
    }
    render_markdown_fields(out, value, 0, &excl);
}

fn render_page(
    out: &mut String,
    heading: &str,
    page: &Value,
    mut render_item: impl FnMut(&mut String, &Value),
) {
    let items = page_items(page);
    if page_has_no_display_entries(page, items) {
        return;
    }
    out.push_str(&format!("\n## {}\n\n", escape_text(heading)));
    let Some(items) = items else {
        render_markdown_fields(out, page, 0, &[]);
        return;
    };
    render_markdown_fields(out, page, 0, &["items"]);
    for item in items {
        render_item(out, item);
    }
}

fn append_location(out: &mut String, item: &Value, prefix: &str) {
    let Some(path) = text_field(item, "path") else {
        return;
    };
    if let Some(line) = line_number(item) {
        let loc = inline_code(&format!("{path}:{line}"));
        match text_field(item, "uri") {
            Some(u) => out.push_str(&format!("{prefix}[{loc}]({u})")),
            None => out.push_str(&format!("{prefix}{loc}")),
        }
    } else {
        out.push_str(&format!("{prefix}{}", inline_code(path)));
    }
}
