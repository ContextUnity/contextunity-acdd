use super::core::{
    append_fenced_block, escape_label, escape_text, inline_code, is_freshness_envelope,
    page_items, render_field, render_markdown, render_markdown_fields, text_field, u64_field,
};
use acdd_core::ports::DomainToolOutput;
use serde_json::Value;

/// Render a blackboard page or posted message JSON payload as Markdown.
pub fn render_blackboard_markdown(value: &Value, title: &str) -> String {
    render_markdown(value, title)
}

/// Render a [`DomainToolOutput`] returned by an external `DomainProvider` plugin tool.
pub fn render_plugin_tool_markdown(tool_name: &str, output: &DomainToolOutput) -> String {
    match output {
        DomainToolOutput::Markdown(text) => text.clone(),
        DomainToolOutput::Json(value) => render_markdown(value, tool_name),
    }
}

/// Renders search results and document pages from the sections and relation pages supplied by
/// the reader. URI links are attached by the selected workspace adapter before this is called.
pub(super) fn render_docs(value: &Value, title: &str) -> String {
    let mut output = format!("# {}\n\n", escape_text(title));
    if title == "search_docs" {
        output.push_str("## Search results\n\n");
        render_search_section_page(&mut output, &value["sections"]);
    } else {
        render_section_page(&mut output, "Document sections", &value["sections"], false);
        for (field, heading, line_is_coord) in [
            ("cites_doc", "Verified document citations", false),
            ("cited_by_doc", "Verified incoming document citations", false),
            ("referenced_symbols", "Verified referenced symbols", true),
        ] {
            if let Some(page) = value.get(field) {
                if u64_field(page, "total") == Some(0) && page.get("omitted") != Some(&Value::Bool(true)) {
                    continue;
                }
                render_page_metadata(&mut output, heading, page);
                for (i, item) in page_items(page).unwrap_or(&[]).iter().enumerate() {
                    output.push_str(&format!("### Entry {}\n\n", i + 1));
                    render_entity_link(&mut output, item, line_is_coord);
                    let excl: &[&str] = if line_is_coord { &["uri", "path", "line", "end_line"] } else { &["uri", "path"] };
                    render_markdown_fields(&mut output, item, 0, excl);
                }
            }
        }
        if let Some(candidates) = value.get("referenced_symbol_candidates") {
            output.push_str("## Ambiguous symbol candidates (navigation only; unverified)\n\n");
            render_page_metadata(&mut output, "Candidate page", candidates);
            for (i, cand) in page_items(candidates).unwrap_or(&[]).iter().enumerate() {
                output.push_str(&format!("### Candidate {}\n\n", i + 1));
                for (field, label) in [("source", "Claim source"), ("target", "Possible target")] {
                    if let Some(entity) = cand.get(field) {
                        output.push_str(&format!("**{label}**\n\n"));
                        render_entity_link(&mut output, entity, true);
                        render_markdown_fields(&mut output, entity, 0, &["uri", "path", "line", "end_line"]);
                    }
                }
                render_markdown_fields(&mut output, cand, 0, &["source", "target"]);
            }
        }
    }
    if let Some(freshness) = value.get("freshness") {
        if freshness.as_object().is_some_and(|o| is_freshness_envelope(o, true)) {
            render_field(&mut output, "freshness", freshness, 0);
        }
    }
    output
}

fn render_search_section_page(output: &mut String, page: &Value) {
    output.push_str("## Sections\n\n");
    let total = u64_field(page, "total").unwrap_or(0);
    let limit = u64_field(page, "limit").unwrap_or(0);
    let offset = u64_field(page, "offset").unwrap_or(0);
    let has_more = page.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    output.push_str(&format!(
        "- **total**: `{total}` | **limit**: `{limit}` | **offset**: `{offset}` | **has_more**: `{has_more}`"
    ));
    if let Some(next_offset) = u64_field(page, "next_offset") {
        output.push_str(&format!(" | **next_offset**: `{next_offset}`"));
    }
    output.push_str("\n\n");

    for section in page_items(page).unwrap_or(&[]) {
        let title = text_field(section, "section_title")
            .or_else(|| text_field(section, "path"))
            .unwrap_or("Document section");
        let path = text_field(section, "path").unwrap_or_default();
        let doc_id = text_field(section, "doc_id").unwrap_or_default();
        let doc_type = text_field(section, "doc_type").unwrap_or_default();
        let is_inv = u64_field(section, "is_invariant") == Some(1)
            || section.get("is_invariant").and_then(Value::as_bool) == Some(true);
        output.push_str(&format!("- **{}** — `{path}`", escape_text(title)));
        if !doc_id.is_empty() || !doc_type.is_empty() || is_inv {
            output.push_str(" (");
            let mut first = true;
            if !doc_id.is_empty() {
                output.push_str(&inline_code(doc_id));
                first = false;
            }
            if !doc_type.is_empty() {
                if !first {
                    output.push_str(", ");
                }
                output.push_str(doc_type);
                first = false;
            }
            if is_inv {
                if !first {
                    output.push_str(", ");
                }
                output.push_str("invariant");
            }
            output.push(')');
        }
        output.push('\n');
        if let Some(excerpt) = text_field(section, "excerpt") {
            output.push_str("\n  **Match excerpt**\n\n");
            append_fenced_block(output, "text", excerpt, 2);
            output.push('\n');
        }
    }
}

fn render_section_page(output: &mut String, heading: &str, page: &Value, excerpts: bool) {
    render_page_metadata(output, heading, page);
    for section in page_items(page).unwrap_or(&[]) {
        let title = text_field(section, "section_title").or_else(|| text_field(section, "path")).unwrap_or("Document section");
        output.push_str(&format!("### {}\n\n", escape_text(title)));
        render_entity_link(output, section, true);
        render_markdown_fields(output, section, 0, &["content", "excerpt", "uri", "path", "line", "end_line"]);
        if let Some(content) = text_field(section, "content") {
            output.push_str("\n#### Source Markdown\n\n");
            output.push_str(content);
            if !content.ends_with('\n') { output.push('\n'); }
            output.push('\n');
        }
        if excerpts {
            if let Some(excerpt) = text_field(section, "excerpt") {
                output.push_str("**Match excerpt**\n\n");
                append_fenced_block(output, "text", excerpt, 0);
                output.push('\n');
            }
        }
    }
}

fn render_page_metadata(output: &mut String, heading: &str, page: &Value) {
    output.push_str(&format!("## {}\n\n", escape_text(heading)));
    render_markdown_fields(output, page, 0, &["items"]);
    if page.get("items").is_some() { output.push('\n'); }
}

fn render_entity_link(output: &mut String, entity: &Value, include_lines: bool) {
    let Some(path) = text_field(entity, "path") else { return };
    output.push_str(if include_lines { "**Location**: " } else { "**Document**: " });
    match text_field(entity, "uri") {
        Some(uri) => output.push_str(&format!("[{}]({uri})", escape_label(path))),
        None => output.push_str(&inline_code(path)),
    }
    if include_lines {
        let line = u64_field(entity, "line");
        if let Some(l) = line { output.push_str(&format!(" line {l}")); }
        if let Some(end) = u64_field(entity, "end_line") {
            output.push_str(&if line.is_some() { format!("–{end}") } else { format!(" through line {end}") });
        }
    }
    output.push_str("\n\n");
}
