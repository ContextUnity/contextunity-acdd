use super::core::{
    append_fenced_block, code_span, escape_text, inline_code, page_items, scalar_text, text_field,
    u64_field,
};
use serde_json::Value;
use std::{collections::HashSet, path::Path};

/// Semantic discovery operations with a domain-specific Markdown layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryKind {
    /// Workspace inventory and indexed health.
    Overview,
    /// One selected symbol and its verified context.
    Inspect,
    /// Ranked symbol matches and bounded graph context.
    Search,
    /// AST matches or a profile-provided syntax diagnostic.
    Ast,
}

/// Builds a file URI from a path already resolved by the owning adapter.
pub(super) fn file_uri_for_path(path: &Path, line: Option<usize>) -> Option<String> {
    let path = path.to_str()?.replace('\\', "/");
    if !path.starts_with('/') {
        return None;
    }
    let mut uri = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(char::from(b))
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    if let Some(l) = line {
        uri.push_str(&format!("#L{l}"));
    }
    Some(uri)
}

pub(super) fn render(value: &Value, title: &str, kind: DiscoveryKind) -> String {
    if kind == DiscoveryKind::Ast && value.get("diagnostic").is_some() {
        return render_ast_diagnostic(&value["diagnostic"], title);
    }
    let mut out = format!("# {}\n\n", escape_text(title));
    match kind {
        DiscoveryKind::Overview => render_overview(&mut out, value),
        DiscoveryKind::Inspect => render_inspect(&mut out, value),
        DiscoveryKind::Search => render_search(&mut out, value),
        DiscoveryKind::Ast => render_ast_matches(&mut out, value),
    }
    let scope = operation_scope(value, kind);
    for key in [
        "languages",
        "coverage_by_workspace",
        "documented_in",
        "analysis_gaps",
    ] {
        if let Some(p) = value
            .get(key)
            .filter(|p| p.get("has_more") == Some(&Value::Bool(true)))
        {
            render_page_continuation(&mut out, p, &key.replace('_', " "), scope.as_deref());
        }
    }
    out
}

pub(super) fn ast_legacy_error(d: &Value) -> String {
    let (code, lang) = (
        text_field(d, "code").unwrap_or("unsupported_ast_pattern"),
        text_field(d, "language").unwrap_or("unknown"),
    );
    let (sl, sc) = (
        u64_field(&d["span"], "start_line").unwrap_or(1),
        u64_field(&d["span"], "start_column").unwrap_or(1),
    );
    let (el, ec) = (
        u64_field(&d["span"], "end_line").unwrap_or(sl),
        u64_field(&d["span"], "end_column").unwrap_or(sc),
    );
    let hint = text_field(d, "hint").unwrap_or("No profile hint was supplied.");
    let examples = strings(&d["examples"])
        .into_iter()
        .map(|e| format!("  - {}", e.replace('\n', "\n    ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{code}: Unsupported AST pattern for language '{lang}' at {sl}:{sc} to {el}:{ec}.\nHint: {hint}\nExamples of valid patterns for {lang}:\n{examples}")
}

fn render_ast_diagnostic(d: &Value, title: &str) -> String {
    let mut out = format!(
        "# {}\n\n> [!CAUTION]\n> **{}** for `{}`.\n",
        escape_text(title),
        escape_text(text_field(d, "code").unwrap_or("unsupported_ast_pattern")),
        escape_text(text_field(d, "language").unwrap_or("unknown"))
    );
    let coords: Vec<String> = [
        "start_byte",
        "end_byte",
        "start_line",
        "start_column",
        "end_line",
        "end_column",
    ]
    .into_iter()
    .filter_map(|f| u64_field(&d["span"], f).map(|v| format!("{f}={v}")))
    .collect();
    if !coords.is_empty() {
        out.push_str(&format!("> **Pattern span:** {}.\n", coords.join(", ")));
    }
    if let Some(hint) = text_field(d, "hint") {
        let quoted: String = hint
            .lines()
            .map(|l| format!("> {}\n", escape_text(l)))
            .collect();
        out.push_str(&format!("> **Hint:** {quoted}\n"));
    }
    let examples = strings(&d["examples"]);
    if !examples.is_empty() {
        out.push_str("\n**Valid patterns for this profile**\n\n");
        for ex in examples {
            out.push_str(&format!("- `{}`\n", code_span(&ex)));
        }
    }
    out
}

fn render_overview_counts_and_languages(out: &mut String, value: &Value) {
    if let Some(counts) = value.get("counts").and_then(Value::as_object) {
        let parts: Vec<String> = [
            ("files", "files"),
            ("nodes", "nodes"),
            ("edges", "edges"),
            ("doc_sections", "doc sections"),
            ("parse_errors", "parse errors"),
            ("unresolved", "unresolved"),
            ("external_imports", "external"),
        ]
        .into_iter()
        .filter_map(|(k, lbl)| counts.get(k).map(|c| format!("{lbl}: {}", scalar_text(c))))
        .collect();
        if !parts.is_empty() {
            out.push_str(&format!("\n## Indexed counts\n\n- {}\n", parts.join(" | ")));
        }
    }
    if let Some(languages) = value
        .get("languages")
        .and_then(page_items)
        .filter(|l| !l.is_empty())
    {
        out.push_str("\n## Languages\n\n");
        for lang in languages {
            let name = text_field(lang, "language").unwrap_or("unknown");
            out.push_str(&format!(
                "- `{}`: {} files",
                escape_text(name),
                lang.get("files")
                    .map(scalar_text)
                    .unwrap_or_else(|| "unavailable".into())
            ));
            for (f, d) in [
                ("resolved", "resolved"),
                ("unresolved", "unresolved"),
                ("ambiguous", "ambiguous"),
                ("external_imports", "external references"),
                ("parse_errors", "parse errors"),
            ] {
                if let Some(c) = lang.get(f) {
                    out.push_str(&format!("; {d}: {}", scalar_text(c)));
                }
            }
            out.push('\n');
        }
    }
}

const OVERVIEW_HUB_CAP: usize = 8;

fn render_overview_discovery(out: &mut String, container: &Value, scope: Option<&str>) {
    if let Some(m) = container
        .get("manifest_components")
        .filter(|v| !v.is_null())
    {
        render_manifest_components(out, m, scope);
    }
    if let Some(hubs) = container.get("documentation_hubs").filter(|v| !v.is_null()) {
        out.push_str("\n## Discovery documentation hub\n\n");
        if hubs.get("available") == Some(&Value::Bool(false)) {
            out.push_str(&format!(
                "Documentation hub unavailable: {}\n",
                text_field(hubs, "cause").unwrap_or("indexed document inventory unavailable")
            ));
        } else {
            let items = page_items(hubs).unwrap_or(&[]);
            for item in items.iter().take(OVERVIEW_HUB_CAP) {
                let (path, sec) = (
                    text_field(item, "path").unwrap_or("unknown document"),
                    text_field(item, "section_title").unwrap_or("document"),
                );
                let anchor = u64_field(item, "line")
                    .map(|l| format!("#L{l}"))
                    .unwrap_or_default();
                match text_field(item, "uri") {
                    Some(u) => out.push_str(&format!(
                        "- [{} — {}{anchor}]({u})\n",
                        escape_text(path),
                        escape_text(sec)
                    )),
                    None => out.push_str(&format!(
                        "- `{}` — {}{anchor}\n",
                        escape_text(path),
                        escape_text(sec)
                    )),
                }
            }
            if items.len() > OVERVIEW_HUB_CAP {
                out.push_str(&format!(
                    "- … and {} more hubs\n",
                    items.len() - OVERVIEW_HUB_CAP
                ));
            }
            render_page_continuation(out, hubs, "documentation hubs", scope);
        }
    }
    if let Some(runs) = container.get("active_runs").filter(|v| !v.is_null()) {
        out.push_str("\n## Discovery active runs\n\n");
        if text_field(runs, "availability") == Some("unavailable") {
            out.push_str(&format!(
                "Active run inventory unavailable: {}\n",
                text_field(runs, "cause").unwrap_or("state inventory unavailable")
            ));
        } else if page_items(runs).unwrap_or(&[]).is_empty() {
            out.push_str("No active runs.\n");
        } else {
            for item in page_items(runs).unwrap_or(&[]) {
                let rid = text_field(item, "run_id").unwrap_or("run");
                let rname = text_field(item, "run_name").unwrap_or(rid);
                let meta = format!(
                    "`{}`; status=`{}`; profile=`{}`; isolation=`{}`",
                    escape_text(rid),
                    escape_text(text_field(item, "status").unwrap_or("active")),
                    escape_text(text_field(item, "profile_id").unwrap_or("default")),
                    escape_text(text_field(item, "isolation").unwrap_or("worktree"))
                );
                match (text_field(item, "spec_path"), text_field(item, "uri")) {
                    (Some(_), Some(u)) => {
                        out.push_str(&format!("- [{}]({u}) ({meta})\n", escape_text(rname)))
                    }
                    (Some(s), None) => out.push_str(&format!(
                        "- **{}** ({meta}; spec=`{}`)\n",
                        escape_text(rname),
                        escape_text(s)
                    )),
                    (None, _) => out.push_str(&format!("- **{}** ({meta})\n", escape_text(rname))),
                }
            }
        }
    }
}

fn render_overview(out: &mut String, value: &Value) {
    out.push_str("## Workspace\n\n");
    let root = text_field(value, "workspace_root")
        .or_else(|| text_field(&value["workspace"], "root"))
        .or_else(|| text_field(&value["metadata"], "workspace_root"));
    if let Some(r) = root {
        out.push_str(&format!("- Root: `{}`\n", escape_text(r)));
    }
    if let Some(p) = text_field(value, "path") {
        out.push_str(&format!("- Path: `{}`\n", escape_text(p)));
    }
    let scope = operation_scope(value, DiscoveryKind::Overview)
        .or_else(|| root.map(|r| format!("workspace_root={r}")));
    for (k, lbl) in [("worktree", "Worktree"), ("branch", "Branch")] {
        if let Some(v) = text_field(&value["workspace"], k) {
            out.push_str(&format!("- {lbl}: `{}`\n", escape_text(v)));
        }
    }
    if let Some(f) = value.get("freshness") {
        let st = text_field(f, "status").unwrap_or("unavailable");
        if !matches!(st, "matched" | "source_inventory_matched") {
            out.push_str(&format!(
                "- Freshness warning: {} ({})\n",
                escape_text(st),
                escape_text(
                    text_field(f, "cause")
                        .or_else(|| text_field(f, "refresh"))
                        .unwrap_or("freshness evidence unavailable")
                )
            ));
        }
    }
    let compiled = strings(&value["compiled_profiles"]);
    render_overview_counts_and_languages(out, value);
    if let Some(files) = value.get("files") {
        let items = page_items(files).unwrap_or(&[]);
        if !items.is_empty() {
            out.push_str("\n## Files\n\n");
            for item in items {
                let p = text_field(item, "path").unwrap_or("file");
                let lang = text_field(item, "language").unwrap_or("unknown");
                let syms = u64_field(item, "symbols").unwrap_or(0);
                out.push_str(&format!(
                    "- `{}` ({lang}, {syms} symbols)\n",
                    escape_text(p)
                ));
            }
            render_page_continuation(out, files, "files", scope.as_deref());
        }
    }
    if let Some(comp) = value.get("components") {
        out.push_str("\n## Directory components\n\n");
        let items = page_items(comp).unwrap_or(&[]);
        if items.is_empty() && u64_field(comp, "total") == Some(0) {
            out.push_str("No directory components were indexed.\n");
        }
        for item in items {
            let id = text_field(item, "id")
                .map(|i| format!(" (`{}`)", escape_text(i)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{}**{id} — `{}`\n",
                escape_text(text_field(item, "name").unwrap_or("component")),
                escape_text(text_field(item, "path").unwrap_or("path unavailable"))
            ));
        }
        render_page_continuation(out, comp, "directory components", scope.as_deref());
    }
    if value.get("compiled_profiles").is_some() {
        if compiled.is_empty() {
            out.push_str("\n- Compiled profiles: none\n");
        } else {
            let inline: Vec<String> = compiled
                .iter()
                .map(|p| format!("`{}`", escape_text(p)))
                .collect();
            out.push_str(&format!(
                "\n- **Compiled profiles:** {}\n",
                inline.join(", ")
            ));
        }
    }
    if let Some(scopes) = value.get("coverage_by_workspace").and_then(page_items) {
        out.push_str("\n## Resolution health\n\n");
        for ws in scopes {
            out.push_str(&format!(
                "### {}\n\n",
                escape_text(text_field(ws, "name").unwrap_or("workspace"))
            ));
            render_resolution_health(out, ws);
        }
    }
    for c in [value.get("discovery"), Some(value)].into_iter().flatten() {
        render_overview_discovery(out, c, scope.as_deref());
    }
    if let Some(c) = value.get("cycles") {
        render_json_section(out, "Dependency cycles", c);
    }
    if let Some(t) = value.get("topology") {
        render_json_section(out, "Module topology", t);
    }
    if let Some(meta) = value.get("metadata") {
        if let Some(entries) = meta.as_array() {
            if !entries.is_empty() {
                out.push_str("\n## Workspace metadata\n\n");
                for e in entries {
                    out.push_str(&format!(
                        "- `{}`: `{}`\n",
                        escape_text(text_field(e, "key").unwrap_or("metadata key unavailable")),
                        e.get("value")
                            .map(scalar_text)
                            .unwrap_or_else(|| "unavailable".into())
                    ));
                }
            }
        } else {
            render_json_section(out, "Workspace metadata", meta);
        }
    }
}

fn render_json_section(out: &mut String, title: &str, value: &Value) {
    out.push_str(&format!("\n## {}\n\n", escape_text(title)));
    append_fenced_block(
        out,
        "json",
        &serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into()),
        0,
    );
}

fn format_health_metric(health: &Value) -> String {
    if text_field(health, "availability") == Some("available") {
        let pct = health
            .get("percent")
            .and_then(Value::as_f64)
            .map(|v| format!("{v:.1}%"))
            .unwrap_or_else(|| "unavailable".into());
        format!(
            "{pct} ({} / {} non-external references)",
            u64_field(health, "resolved").unwrap_or(0),
            u64_field(health, "denominator").unwrap_or(0)
        )
    } else {
        format!(
            "unavailable ({})",
            escape_text(text_field(health, "cause").unwrap_or("insufficient indexed evidence"))
        )
    }
}

fn render_resolution_health(out: &mut String, scope: &Value) {
    let health = &scope["resolution_health"];
    out.push_str(&format!(
        "Resolution Health: {}\n",
        format_health_metric(health)
    ));
    if let (Some(r), Some(u), Some(a)) = (
        u64_field(scope, "resolved"),
        u64_field(scope, "unresolved"),
        u64_field(scope, "ambiguous"),
    ) {
        out.push_str(&format!("Resolved: {r}; unresolved: {u}; ambiguous: {a}\n"));
    }
    out.push_str(&format!(
        "External references: {} (excluded from health); external crate count: unavailable\n",
        scope
            .get("external_imports")
            .map(scalar_text)
            .unwrap_or_else(|| "unavailable".into())
    ));
    let mut has_partition = false;
    for (key, label) in [("prod", "Prod"), ("tests", "Tests")] {
        if let Some(bucket) = health.get(key).filter(|v| v.is_object()) {
            if !has_partition {
                out.push('\n');
                has_partition = true;
            }
            out.push_str(&format!("- {label}: {}", format_health_metric(bucket)));
            if let (Some(u), Some(a), Some(ext)) = (
                u64_field(bucket, "unresolved"),
                u64_field(bucket, "ambiguous"),
                u64_field(bucket, "external_imports"),
            ) {
                out.push_str(&format!(
                    " — unresolved: {u}; ambiguous: {a}; external: {ext}"
                ));
            }
            out.push('\n');
        }
    }
    let reasons = &scope["diagnostic_reasons"];
    let sep = if has_partition { "\n" } else { "" };
    if text_field(reasons, "availability") != Some("available") {
        out.push_str(&format!(
            "{sep}Diagnostic reasons: unavailable ({})\n",
            escape_text(text_field(reasons, "cause").unwrap_or("diagnostic aggregate unavailable"))
        ));
        return;
    }
    let Some(counts) = reasons.get("counts").and_then(Value::as_object) else {
        out.push_str(&format!(
            "{sep}Diagnostic reasons: unavailable (reason matrix not supplied)\n"
        ));
        return;
    };
    let reason_label = |r: &str| match r {
        "dynamic_callback" => "Dynamic callbacks",
        "unresolved_receiver_type" => "Unresolved receiver type",
        "framework_magic" => "Framework magic",
        "external_unindexed" => "Unindexed external references",
        "ambiguous_candidates" => "Ambiguous candidates",
        "shadowed_binding" => "Shadowed binding",
        "unknown" => "Unknown reason",
        _ => "Recorded reason",
    };
    let mut non_ext_lines = Vec::new();
    let mut external_rows = Vec::new();
    for (reason, statuses) in counts {
        let Some(statuses) = statuses.as_object() else {
            continue;
        };
        let total: u64 = statuses
            .iter()
            .filter(|(s, _)| s.as_str() != "external")
            .filter_map(|(_, c)| c.as_u64())
            .sum();
        if total > 0 {
            non_ext_lines.push(format!("- {} ({reason}): {total}\n", reason_label(reason)));
            for (status, count) in statuses {
                let Some(n) = count.as_u64().filter(|&n| n > 0) else {
                    continue;
                };
                if status != "external" {
                    non_ext_lines.push(format!("  - Reason ({reason}, status={status}): {n}\n"));
                }
            }
        }
        if let Some(n) = statuses
            .get("external")
            .and_then(Value::as_u64)
            .filter(|&n| n > 0)
        {
            external_rows.push((reason.as_str(), n));
        }
    }
    if !non_ext_lines.is_empty() {
        out.push_str("\nNon-external diagnostic references:\n");
        for line in &non_ext_lines {
            out.push_str(line);
        }
    }
    if !external_rows.is_empty() {
        if non_ext_lines.is_empty() {
            out.push_str(sep);
        }
        out.push_str("External-status diagnostic references:\n");
        for (reason, n) in external_rows {
            out.push_str(&format!(
                "- {} (reason={reason}, status=external): {n}\n",
                reason_label(reason)
            ));
        }
    }
}

fn render_manifest_components(out: &mut String, page: &Value, scope: Option<&str>) {
    out.push_str("\n## Manifest components\n\n");
    if text_field(page, "availability") == Some("unavailable")
        || page.get("available") == Some(&Value::Bool(false))
    {
        out.push_str(&format!(
            "Manifest inventory unavailable: {}\n",
            text_field(page, "cause").unwrap_or("inventory bounds prevented a complete scan")
        ));
        return;
    }
    let items = page_items(page).unwrap_or(&[]);
    if items.is_empty() {
        out.push_str(
            "No declared package components were found in the bounded manifest inventory.\n",
        );
    }
    for c in items {
        let unit = text_field(c, "unit").unwrap_or("package");
        let name = text_field(c, "name").unwrap_or(if unit == "workspace" {
            "workspace"
        } else {
            "unnamed component"
        });
        out.push_str(&format!(
            "- **{}** (`{}`, {unit})",
            escape_text(name),
            text_field(c, "ecosystem").unwrap_or("unknown ecosystem")
        ));
        if let Some(ws) = text_field(c, "workspace").filter(|w| *w != "primary") {
            out.push_str(&format!(" · workspace `{}`", escape_text(ws)));
        }
        if let Some(d) = text_field(c, "description") {
            out.push_str(&format!(": {}", escape_text(d)));
        }
        if let Some(r) = text_field(c, "root").filter(|r| !r.is_empty()) {
            match text_field(c, "root_uri") {
                Some(u) => out.push_str(&format!(" — [root `{}`]({u})", escape_text(r))),
                None => out.push_str(&format!(" — root `{}`", escape_text(r))),
            }
        }
        if let (Some(p), Some(u)) = (
            text_field(c, "manifest_path"),
            text_field(c, "manifest_uri"),
        ) {
            out.push_str(&format!(" · [manifest `{}`]({u})", escape_text(p)));
        }
        let members: Vec<String> = c
            .get("workspace_members")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(escape_text)
            .collect();
        if !members.is_empty() {
            out.push_str(&format!(
                " · declared workspace members: {}",
                members.join(", ")
            ));
        }
        out.push('\n');
    }
    for d in strings(&page["diagnostics"]) {
        out.push_str(&format!("- Manifest diagnostic: {}\n", escape_text(&d)));
    }
    for d in page
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let path = d
            .get("manifest_path")
            .or_else(|| d.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("manifest");
        let code = d
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("manifest_diagnostic");
        let reason = d
            .get("reason")
            .or_else(|| d.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("could not establish package identity");
        out.push_str(&format!(
            "- Manifest diagnostic at `{}` (`{}`): {}\n",
            escape_text(path),
            escape_text(code),
            escape_text(reason)
        ));
    }
    if page.get("diagnostics_truncated") == Some(&Value::Bool(true)) {
        out.push_str(&format!(
            "- Manifest diagnostics truncated; {} found.\n",
            page.get("diagnostic_total")
                .map(scalar_text)
                .unwrap_or_else(|| "unknown".into())
        ));
    } else if page.get("diagnostics_available") == Some(&Value::Bool(false)) {
        out.push_str(&format!(
            "- Manifest diagnostics unavailable: {}\n",
            text_field(page, "diagnostics_cause")
                .unwrap_or("inventory bounds prevented a complete scan")
        ));
    }
    render_page_continuation(out, page, "manifest components", scope);
}

fn render_inspect_invariants(out: &mut String, inv: &Value) {
    let direct = inv["direct"].as_array().cloned().unwrap_or_default();
    if !direct.is_empty() {
        out.push_str("\n> [!IMPORTANT] Governing Invariants\n> Direct governing invariants\n");
        for rule in direct {
            let src = &rule["source"];
            let link = match (text_field(src, "path"), u64_field(src, "line")) {
                (Some(p), Some(l)) => {
                    format!(" — {}", format_link(p, Some(l), text_field(src, "uri")))
                }
                _ => String::new(),
            };
            out.push_str(&format!(
                "\n> - `{}`: {}{link}",
                escape_text(text_field(&rule, "id").unwrap_or("invariant")),
                escape_text(text_field(&rule, "rule").unwrap_or(""))
            ));
        }
        out.push('\n');
    }
    for (f, lbl) in [
        ("omitted_direct", "Omitted direct invariants"),
        ("omitted_transitive", "Omitted one-hop invariant references"),
    ] {
        if let Some(c) = inv.get(f).filter(|v| v.as_u64().is_some_and(|n| n > 0)) {
            out.push_str(&format!("\n- {lbl}: {}\n", scalar_text(c)));
        }
    }
    if let Some(tr) = inv
        .get("transitive_ids")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
    {
        out.push_str("\n### One-hop or transitive invariant references\n\n");
        for id in tr.iter().filter_map(Value::as_str) {
            out.push_str(&format!("- `{}`\n", escape_text(id)));
        }
    }
    if let Some(srcs) = inv
        .get("omitted_sources")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
    {
        out.push_str("\n### One-hop invariant source references\n\n");
        for r in srcs {
            let src = &r["source"];
            let coord = match (
                text_field(src, "path"),
                u64_field(src, "line"),
                text_field(src, "uri"),
            ) {
                (Some(p), l, u) => format_link(p, l, u),
                (None, _, Some(u)) => format!("[source]({u})"),
                _ => "source coordinate unavailable".into(),
            };
            out.push_str(&format!(
                "- `{}`: {coord}\n",
                escape_text(text_field(r, "id").unwrap_or("invariant"))
            ));
        }
    }
}

fn render_inspect_docs_and_source(out: &mut String, value: &Value) {
    let doc_items = value.get("documents").and_then(page_items).unwrap_or(&[]);
    let mut seen_docs = HashSet::new();
    for key in ["documented_in", "analysis_gaps"] {
        if let Some(items) = value
            .get(key)
            .and_then(page_items)
            .filter(|i| !i.is_empty())
        {
            out.push_str(&format!("\n### {}\n\n", key.replace('_', " ")));
            for item in items {
                let content = (key == "documented_in")
                    .then(|| text_field(item, "doc_id"))
                    .flatten()
                    .filter(|id| seen_docs.insert((*id).to_owned()))
                    .and_then(|id| {
                        text_field(item, "content").or_else(|| {
                            doc_items
                                .iter()
                                .find(|d| text_field(d, "doc_id") == Some(id))
                                .and_then(|d| text_field(d, "content"))
                        })
                    });
                render_evidence_row(out, item, content, key == "documented_in");
            }
        }
    }
    for key in ["documents", "coverage"] {
        if let Some(page) = value.get(key) {
            if key == "coverage"
                && (page_items(page).is_some_and(|i| !i.is_empty())
                    || page.get("statuses").is_some())
            {
                render_json_section(out, key, page);
            }
            render_page_continuation(
                out,
                page,
                key,
                operation_scope(value, DiscoveryKind::Inspect).as_deref(),
            );
        }
    }
    if let Some(source) = text_field(value, "source") {
        out.push_str("\n### Source preview\n\n");
        let preview = &value["source_preview"];
        if !preview.is_null() {
            out.push_str(&format!(
                "Source range: lines {}-{}.\n\n",
                preview
                    .get("start_line")
                    .map(scalar_text)
                    .unwrap_or_else(|| "unavailable".into()),
                preview
                    .get("end_line")
                    .map(scalar_text)
                    .unwrap_or_else(|| "unavailable".into())
            ));
        }
        append_fenced_block(out, "text", source, 0);
        if !preview.is_null() {
            let flags: Vec<String> = [
                ("body_offset", true),
                ("body_lines", false),
                ("total_body_lines", false),
                ("has_more", false),
                ("truncated_line", true),
                ("omitted_leading_lines", true),
                ("omitted_body_lines", true),
            ]
            .into_iter()
            .filter_map(|(f, skip_inactive)| {
                let v = preview.get(f)?;
                if skip_inactive && (v.as_u64() == Some(0) || v.as_bool() == Some(false)) {
                    return None;
                }
                Some(format!("{f}={}", scalar_text(v)))
            })
            .collect();
            if !flags.is_empty() {
                out.push_str(&format!("\nSource preview state: {}.\n", flags.join(", ")));
            }
            if let Some(hint) = text_field(preview, "continuation_hint") {
                out.push_str(&format!("Source preview note: {}\n", escape_text(hint)));
            }
            if preview.get("has_more") == Some(&Value::Bool(true))
                && preview.get("truncated_line") != Some(&Value::Bool(true))
            {
                out.push_str(&format!(
                    "Continue source for {} in {} with source_offset {} and generation {}.\n",
                    inline_code(
                        text_field(&value["node"], "id").unwrap_or("the same selected symbol")
                    ),
                    inline_code(text_field(&value["node"], "path").unwrap_or("the selected path")),
                    preview
                        .get("next_source_offset")
                        .map(scalar_text)
                        .unwrap_or_else(|| "unavailable".into()),
                    inline_code(text_field(value, "generation").unwrap_or("unavailable"))
                ));
            }
        }
    }
}

fn render_inspect(out: &mut String, value: &Value) {
    let (node, summary) = (&value["node"], &value["summary"]);
    out.push_str("## Symbol\n\n");
    if let Some(name) = text_field(node, "qualname").or_else(|| text_field(node, "name")) {
        out.push_str(&format!("- **{}**", escape_text(name)));
        if let Some(k) = text_field(node, "kind") {
            out.push_str(&format!(" (`{}`)", escape_text(k)));
        }
        if let Some(s) = text_field(node, "signature")
            .or_else(|| text_field(summary, "signature"))
            .or_else(|| text_field(&node["details"], "signature"))
        {
            let sig = code_span(s);
            out.push_str(&format!(" — `{sig}`"));
        }
        out.push('\n');
    }
    if let (Some(p), Some(l)) = (text_field(node, "path"), u64_field(node, "line")) {
        out.push_str(&format!(
            "- Source: {}\n",
            format_link(p, Some(l), text_field(node, "uri"))
        ));
    }
    if let Some(doc) = text_field(node, "docstring")
        .or_else(|| text_field(summary, "docstring"))
        .or_else(|| text_field(&node["details"], "doc"))
    {
        out.push_str(&format!(
            "\n{}\n",
            doc.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        ));
    }
    if let Some(c) = text_field(node, "container") {
        out.push_str(&format!("- Container: `{}`\n", escape_text(c)));
    } else if let Some(c) = summary.get("container").filter(|v| !v.is_null()) {
        let name = text_field(c, "qualname")
            .or_else(|| text_field(c, "name"))
            .unwrap_or("container");
        let path = text_field(c, "path").unwrap_or("");
        let coord = u64_field(c, "line")
            .map(|l| format!("{path}#L{l}"))
            .unwrap_or_else(|| path.to_owned());
        match text_field(c, "uri") {
            Some(u) => out.push_str(&format!("- Container: [{}]({u})\n", escape_text(name))),
            None => out.push_str(&format!(
                "- Container: `{}` at `{}`\n",
                escape_text(name),
                escape_text(&coord)
            )),
        }
    }
    if let Some(calls) = summary.get("calls").and_then(Value::as_object) {
        let mut hdr = false;
        for (dir, title, role) in [
            ("inbound", "Callers", "caller"),
            ("outbound", "Calls", "callee"),
        ] {
            if let Some(count) = calls.get(dir).and_then(|r| r.get("count")) {
                if !std::mem::replace(&mut hdr, true) {
                    out.push_str("\n### Available relation categories\n\n");
                }
                out.push_str(&format!("- {title}: {}\n", scalar_text(count)));
                render_graph_items(out, calls[dir].get("preview"), role);
            }
        }
    }
    render_inspect_invariants(out, &value["governing_invariants"]);
    render_inspect_docs_and_source(out, value);
}

fn render_search(out: &mut String, value: &Value) {
    let query = text_field(&value["request_scope"], "pattern")
        .or_else(|| text_field(value, "query"))
        .or_else(|| text_field(value, "pattern"));
    let page = &value["nodes"];
    let items = page_items(page).unwrap_or(&[]);
    let groups = page.get("grouped_by_file").and_then(Value::as_object);
    let grouped_count: usize = groups
        .into_iter()
        .flat_map(|g| g.values())
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum();
    let scope = operation_scope(value, DiscoveryKind::Search);
    if items.is_empty() && grouped_count == 0 {
        if u64_field(page, "total") == Some(0) {
            out.push_str(&format!("## Search\n\nNo indexed matches for `{}`. This result is limited to the request and does not establish global absence. Try a narrower symbol name or widen the kind/path scope.\n", escape_text(query.unwrap_or("the requested pattern"))));
        } else {
            out.push_str("No matches are present on this page; the total or remaining search scope is unavailable.\n");
        }
        render_page_continuation(out, page, "search results", scope.as_deref().or(query));
        return;
    }
    let total_label = match u64_field(page, "total") {
        Some(t) if t > 1000 => "1000+".to_owned(),
        Some(t) => t.to_string(),
        None if page.get("has_more") == Some(&Value::Bool(true)) => "1000+".to_owned(),
        None => "unavailable".to_owned(),
    };
    out.push_str(&format!(
        "## Search for `{}` — {total_label} indexed matches\n\n",
        escape_text(query.unwrap_or("the requested pattern"))
    ));
    let mut index = 1;
    for item in items {
        render_search_item(out, item, text_field(item, "path").unwrap_or(""), index);
        index += 1;
    }
    for (path, entries) in groups.into_iter().flatten() {
        for item in entries.as_array().into_iter().flatten() {
            render_search_item(out, item, path, index);
            index += 1;
        }
    }
    render_page_continuation(out, page, "search results", scope.as_deref().or(query));
}

fn compact_inline(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let mut s: String = normalized.chars().take(max_chars).collect();
        s.push('…');
        s
    }
}

fn render_search_item(out: &mut String, item: &Value, path: &str, index: usize) {
    let name = text_field(item, "qualname")
        .or_else(|| text_field(item, "name"))
        .unwrap_or("indexed symbol");
    out.push_str(&format!("{index}. **{}**", escape_text(name)));
    let tags: Vec<&str> = ["kind", "classification"]
        .into_iter()
        .filter_map(|k| text_field(item, k))
        .collect();
    if !tags.is_empty() {
        out.push_str(&format!(" (`{}`)", tags.join("`, `")));
    }
    if !path.is_empty() || text_field(item, "uri").is_some() {
        out.push_str(&format!(
            " — {}",
            format_link(path, u64_field(item, "line"), text_field(item, "uri"))
        ));
    }
    let full = text_field(item, "uri").is_some();
    if let Some(reason) = text_field(item, "match_reason") {
        let lbl = match reason {
            "exact_name" => "exact symbol name",
            "name_prefix" => "name prefix",
            "exact_qualified_name" => "exact qualified name",
            "name_fragment" => "name fragment",
            "indexed_text" => "indexed text",
            "name_pattern" => "name pattern",
            "qualified_pattern" => "qualified-name pattern",
            _ => reason,
        };
        out.push_str(&format!(
            "{}`{}`",
            if full { " · Match reason: " } else { " · " },
            escape_text(lbl)
        ));
    }
    out.push('\n');
    let mut details = Vec::new();
    if let Some(s) = text_field(item, "signature") {
        details.push(format!("`{}`", code_span(&compact_inline(s, 120))));
    }
    if let Some(d) = text_field(item, "docstring_first_sentence") {
        let doc = escape_text(&compact_inline(d, 120));
        details.push(if full {
            format!("Documentation: {doc}")
        } else {
            doc
        });
    }
    if let Some(s) =
        text_field(item, "match_snippet").filter(|_| text_field(item, "source").is_none())
    {
        details.push(format!("match: `{}`", code_span(&compact_inline(s, 120))));
    }
    let pulse = &item["graph_pulse"];
    if !pulse.is_null() {
        let mut p_parts = Vec::new();
        for (k, desc) in [("callers", "callers"), ("implementations", "impls")] {
            if let Some(n) = pulse.get(k) {
                if let Some(c) = u64_field(n, "count").filter(|&c| c > 0) {
                    p_parts.push(format!("{c} {desc}"));
                } else if text_field(n, "availability") == Some("unavailable") {
                    p_parts.push(format!("{desc}: unavailable"));
                }
            }
        }
        if let Some(tests) = pulse.get("covering_tests") {
            let total = u64_field(tests, "total").unwrap_or(0);
            match text_field(tests, "discovery_method") {
                Some("graph") if total > 0 => p_parts.push(if full {
                    format!("verified covering tests: {total}")
                } else {
                    format!("{total} tests")
                }),
                Some(m) if total > 0 => p_parts.push(format!("tests: {m}")),
                None if text_field(tests, "availability") == Some("unavailable") => {
                    p_parts.push("tests: unavailable".to_owned())
                }
                _ => {}
            }
        }
        if !p_parts.is_empty() {
            details.push(format!("pulse: {}", p_parts.join(", ")));
        }
    }
    if !details.is_empty() {
        out.push_str(&format!("   {}\n", details.join(" | ")));
    }
    if full && !pulse.is_null() {
        render_graph_items(out, pulse.pointer("/callers/preview"), "caller");
        render_graph_items(out, pulse.pointer("/covering_tests/items"), "test");
    }
    if let Some(source) = text_field(item, "source") {
        append_fenced_block(out, "text", source, 0);
    }
}

fn render_graph_items(out: &mut String, items: Option<&Value>, role: &str) {
    for item in items.and_then(Value::as_array).into_iter().flatten() {
        let name = text_field(item, "qualname")
            .or_else(|| text_field(item, "name"))
            .unwrap_or("indexed symbol");
        let path = text_field(item, "path").unwrap_or("");
        let coord = u64_field(item, "line")
            .map(|l| format!("{path}#L{l}"))
            .unwrap_or_else(|| path.to_owned());
        match text_field(item, "uri") {
            Some(uri) => out.push_str(&format!(
                "    - {role}: [{} — `{}`]({uri})\n",
                escape_text(name),
                escape_text(&coord)
            )),
            None if coord.is_empty() => {
                out.push_str(&format!("    - {role}: {}\n", escape_text(name)))
            }
            None => out.push_str(&format!(
                "    - {role}: {} at `{}`\n",
                escape_text(name),
                escape_text(&coord)
            )),
        }
    }
}

fn render_ast_matches(out: &mut String, value: &Value) {
    let page = &value["matches"];
    let matches = page_items(page).unwrap_or(&[]);
    let (pattern, language) = (
        text_field(value, "pattern").unwrap_or("the requested pattern"),
        text_field(value, "language").unwrap_or("language unavailable"),
    );
    out.push_str(&format!(
        "## AST search for `{}` in `{}`\n\n",
        escape_text(pattern),
        escape_text(language)
    ));
    let total_unavail = text_field(page, "total_status") == Some("unavailable")
        || page.get("computation_truncated") == Some(&Value::Bool(true))
        || value.get("truncated") == Some(&Value::Bool(true));
    if let Some(total) = u64_field(page, "total").filter(|_| !total_unavail) {
        out.push_str(&format!("{total} total matches\n\n"));
    } else if total_unavail {
        out.push_str(&format!(
            "{} matches on this page; total unavailable\n\n",
            matches.len()
        ));
    } else {
        out.push_str(&format!("{} matches\n\n", matches.len()));
    }
    if matches.is_empty() {
        out.push_str("No AST matches were returned for the bounded search.\n");
    }
    for m in matches {
        out.push_str("- Match");
        if let Some(path) = text_field(m, "path") {
            let lbl = match (u64_field(m, "line"), u64_field(m, "end_line")) {
                (Some(l), Some(e)) if e != l => format!("{path}#L{l}-L{e}"),
                (Some(l), _) => format!("{path}#L{l}"),
                _ => path.to_owned(),
            };
            match text_field(m, "uri") {
                Some(u) => out.push_str(&format!(" in [{}]({u})", escape_text(&lbl))),
                None => out.push_str(&format!(" in `{}`", escape_text(&lbl))),
            }
        }
        out.push('\n');
        if let Some(snippet) = text_field(m, "snippet").or_else(|| text_field(m, "text")) {
            out.push_str("  - Matched syntax:\n\n");
            append_fenced_block(out, "text", snippet, 0);
        }
        for k in ["captures", "enclosing"] {
            if let Some(f) = m.get(k) {
                out.push_str(&format!(
                    "  - {k}: `{}`\n",
                    f.as_str().map(code_span).unwrap_or_else(|| f.to_string())
                ));
            }
        }
    }
    if total_unavail {
        out.push_str("\n- Total: unavailable for this bounded search.\n");
    } else if let Some(t) = page.get("total") {
        out.push_str(&format!("\n- Total: {}\n", scalar_text(t)));
    }
    if page.is_object() {
        let scope = operation_scope(value, DiscoveryKind::Ast)
            .unwrap_or_else(|| format!("pattern={pattern}, language={language}"));
        render_page_continuation(out, page, "AST matches", Some(&scope));
    }
    if let Some(t) = value.get("truncated") {
        out.push_str(&format!("\n- Search truncated: {}\n", scalar_text(t)));
    }
}

fn render_evidence_row(out: &mut String, item: &Value, content: Option<&str>, include_meta: bool) {
    let path = text_field(item, "path").unwrap_or("indexed evidence");
    out.push_str(&format!(
        "- {}",
        format_link(
            path,
            u64_field(item, "doc_line").or_else(|| u64_field(item, "line")),
            text_field(item, "uri")
        )
    ));
    let section = text_field(item, "section_title")
        .or_else(|| {
            (!include_meta)
                .then(|| text_field(item, "reason"))
                .flatten()
        })
        .unwrap_or("");
    if !section.is_empty() {
        out.push_str(&format!(" — {}", escape_text(section)));
    }
    if include_meta {
        let mut meta = Vec::new();
        if let Some(id) = text_field(item, "doc_id") {
            meta.push(format!("id {}", inline_code(id)));
        }
        if let Some(dt) = text_field(item, "doc_type") {
            meta.push(format!("type {}", inline_code(dt)));
        }
        if let Some(l) = u64_field(item, "line") {
            meta.push(format!("citation line {l}"));
        }
        if !meta.is_empty() {
            out.push_str(&format!(" ({})", meta.join(", ")));
        }
    }
    out.push('\n');
    if let Some(c) = content.filter(|c| !c.is_empty()) {
        out.push('\n');
        for l in c.lines() {
            out.push_str(&format!("> {}\n", escape_text(l)));
        }
        out.push('\n');
    }
}

fn render_page_continuation(out: &mut String, page: &Value, name: &str, scope: Option<&str>) {
    if page.get("has_more") == Some(&Value::Bool(true)) {
        let next = page
            .get("next_offset")
            .map(scalar_text)
            .unwrap_or_else(|| "unknown".into());
        match text_field(page, "generation") {
            Some(gen) => {
                let gen = if gen.len() == 64 && gen.bytes().all(|b| b.is_ascii_hexdigit()) {
                    &gen[..8]
                } else {
                    gen
                };
                out.push_str(&format!("\nContinue {name}{} at offset {next} with generation {}.\n", scope.map(|s| format!(" for {}", inline_code(s))).unwrap_or_else(|| " for the same original request scope".into()), inline_code(gen)));
            }
            None => out.push_str(&format!("\nMore {name} remain at offset {next}, but the accepted generation is unavailable; continuation cannot be safely replayed.\n")),
        }
    }
    if page.get("computation_truncated") == Some(&Value::Bool(true)) {
        out.push_str("\nThe bounded computation was truncated; totals may be unavailable.\n");
    }
}

fn operation_scope(value: &Value, kind: DiscoveryKind) -> Option<String> {
    if let Some(scope) = value.get("request_scope").and_then(Value::as_object) {
        let entries: Vec<String> = scope
            .iter()
            .filter(|(k, v)| {
                k.as_str() != "workspace_root" && !v.is_null() && v.as_bool() != Some(false)
            })
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                )
            })
            .collect();
        if !entries.is_empty() {
            return Some(entries.join(", "));
        }
    }
    match kind {
        DiscoveryKind::Overview => text_field(value, "path")
            .map(|p| format!("path={p}"))
            .or_else(|| text_field(value, "workspace_root").map(|r| format!("workspace_root={r}"))),
        DiscoveryKind::Inspect => match (
            text_field(&value["node"], "id"),
            text_field(&value["node"], "path"),
        ) {
            (Some(id), Some(p)) => Some(format!("selector={id}, path={p}")),
            (Some(id), None) => Some(format!("selector={id}")),
            _ => None,
        },
        DiscoveryKind::Search => text_field(value, "pattern").map(|p| format!("pattern={p}")),
        DiscoveryKind::Ast => Some(format!(
            "pattern={}, language={}",
            text_field(value, "pattern")?,
            text_field(value, "language").unwrap_or("language unavailable")
        )),
    }
}

fn format_link(path: &str, line: Option<u64>, uri: Option<&str>) -> String {
    let label = line
        .map(|l| format!("{path}#L{l}"))
        .unwrap_or_else(|| path.to_owned());
    match uri {
        Some(u) => format!("[{}]({u})", escape_text(&label)),
        None => format!("`{}`", escape_text(&label)),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
