use super::{escape_text, inline_code, line_number, text_field, u64_field};
use serde_json::Value;

pub(super) fn render(value: &Value, title: &str) -> String {
    let mut out = format!("# {}\n\n", escape_text(title));
    out.push_str("## Summary\n\n");

    let target = text_field(value, "target").filter(|s| !s.is_empty());
    match target {
        Some(t) => out.push_str(&format!("- **Target:** {}\n", inline_code(t))),
        None => out.push_str("- **Target:** workspace\n"),
    }
    if let Some(cats) = value.get("categories").and_then(Value::as_array) {
        let joined = cats
            .iter()
            .filter_map(Value::as_str)
            .map(inline_code)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            out.push_str(&format!("- **Categories:** {joined}\n"));
        }
    }
    if let Some(sev) = text_field(value, "minimum_severity") {
        out.push_str(&format!("- **Minimum severity:** {}\n", inline_code(sev)));
    }
    if let Some(summary) = value.get("summary") {
        let total = u64_field(summary, "total").unwrap_or(0);
        let errors = u64_field(summary, "errors").unwrap_or(0);
        let warnings = u64_field(summary, "warnings").unwrap_or(0);
        let rules = u64_field(summary, "rules_evaluated").unwrap_or(0);
        out.push_str(&format!(
            "- **Total:** `{total}` (`{errors}` error(s), `{warnings}` warning(s), `{rules}` rule(s) evaluated)\n"
        ));
        if let Some(by_cat) = summary.get("by_category").and_then(Value::as_object) {
            let counts = by_cat
                .iter()
                .map(|(k, v)| format!("{}: `{}`", inline_code(k), v.as_u64().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(", ");
            if !counts.is_empty() {
                out.push_str(&format!("- **By category:** {counts}\n"));
            }
        }
    }

    out.push_str("\n## Violations\n\n");
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if items.is_empty() {
        out.push_str("No lint violations found.\n");
    } else {
        for item in items {
            let sev = text_field(item, "severity")
                .unwrap_or("error")
                .to_ascii_uppercase();
            let cat = text_field(item, "category").unwrap_or("unknown");
            let rule_id = text_field(item, "rule_id").unwrap_or("unknown");
            let loc = format_item_location(item);
            let msg = text_field(item, "message").unwrap_or("");
            out.push_str(&format!(
                "- **[{sev}]** ({} / {}) {loc}: {}\n",
                inline_code(cat),
                inline_code(rule_id),
                escape_text(msg)
            ));
            if let Some(inv) = text_field(item, "invariant") {
                out.push_str(&format!("  - **Invariant:** {}\n", inline_code(inv)));
            }
            if let Some(sym) = text_field(item, "symbol") {
                out.push_str(&format!("  - **Symbol:** {}\n", inline_code(sym)));
            }
            if let Some(tgt) = text_field(item, "target") {
                out.push_str(&format!("  - **Target:** {}\n", inline_code(tgt)));
            }
            if let Some(src) = item.get("rule_source") {
                let src_loc = format_item_location(src);
                if !src_loc.is_empty() {
                    out.push_str(&format!("  - **Rule source:** {src_loc}\n"));
                }
            }
            if let Some(sug) = text_field(item, "suggestion") {
                out.push_str(&format!("  - **Suggestion:** {}\n", escape_text(sug)));
            }
        }
    }

    if value.get("has_more") == Some(&Value::Bool(true)) {
        let next_offset = u64_field(value, "next_offset").unwrap_or(0);
        let gen = text_field(value, "generation").unwrap_or("");
        out.push_str(&format!(
            "\n- **Continuation:** `offset={next_offset}` {}\n",
            inline_code(&format!("generation={gen}"))
        ));
    }
    if value.get("byte_limited") == Some(&Value::Bool(true)) {
        out.push_str("- **Budget:** truncated to response byte budget; narrow target/rules or page with `offset`.\n");
    }
    out
}

fn format_item_location(item: &Value) -> String {
    let Some(path) = text_field(item, "path") else {
        return String::new();
    };
    let label = match line_number(item) {
        Some(line) => inline_code(&format!("{path}:{line}")),
        None => inline_code(path),
    };
    match text_field(item, "uri") {
        Some(uri) => format!("[{label}]({uri})"),
        None => label,
    }
}
