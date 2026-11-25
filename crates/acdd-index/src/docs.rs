use crate::models::DocSection;
use acdd_core::lint::ArchRuleSpec;
use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Default, Deserialize)]
struct Frontmatter {
    doc_type: Option<String>,
    title: Option<String>,
    status: Option<String>,
}

struct ActiveLink {
    line: usize,
    target_path: Option<String>,
    explicit_path: bool,
    range: Option<(usize, usize)>,
    occurrence: usize,
    claim_start: usize,
}

fn split_frontmatter(source: &str) -> (Option<&str>, &str, usize) {
    let Some(rest) = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
    else {
        return (None, source, 0);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let body_offset = source.len() - body.len();
            return (Some(yaml), body, body_offset);
        }
        offset += line.len();
    }
    (None, source, 0)
}

fn heading_level_depth(level: pulldown_cmark::HeadingLevel) -> usize {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

/// Render only parsed Markdown headings, preserving their hierarchy.
pub fn render_outline(_path: &str, source: &str, _spec_paths: &[String]) -> Result<String> {
    let (_, body, _) = split_frontmatter(source);
    let mut lines = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((heading_level_depth(level), String::new()));
            }
            Event::Text(t) | Event::Code(t) if current.is_some() => {
                if let Some((_, title)) = &mut current {
                    title.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, title)) = current.take() {
                    lines.push(format!("{} {}", "#".repeat(level), title));
                }
            }
            _ => {}
        }
    }
    Ok(lines.join("\n"))
}

struct SectionScanState<'a> {
    path: &'a str,
    spec_paths: &'a [String],
    known_zones: Option<&'a BTreeSet<String>>,
    line_starts: &'a [usize],
    body_offset: usize,
    meta: &'a Frontmatter,
    frontmatter_error: Option<&'a str>,
}

struct ScannedSectionEvents {
    symbols: Vec<String>,
    invariants: Vec<String>,
    citations: Vec<Value>,
    symbol_claims: Vec<Value>,
    arch_rules: Vec<Value>,
    arch_rule_errors: Vec<Value>,
    fenced_line_ranges: Vec<(usize, usize)>,
}

struct DocSectionEntryInput<'a> {
    index: usize,
    start: usize,
    end: usize,
    title: &'a str,
    content: &'a str,
    mtime: f64,
}

fn scan_section_events(
    path: &str,
    content: &str,
    section_line: usize,
    base_offset: usize,
    line_starts: &[usize],
    known_zones: Option<&BTreeSet<String>>,
    seen_rule_ids: &mut BTreeSet<String>,
) -> ScannedSectionEvents {
    let line_at = |offset: usize| -> usize {
        let pos = base_offset + offset;
        line_starts.partition_point(|&start_pos| start_pos <= pos)
    };
    let mut symbols = Vec::new();
    let mut invariants = Vec::new();
    let mut citations = Vec::new();
    let mut symbol_claims: Vec<Value> = Vec::new();
    let mut arch_rules: Vec<Value> = Vec::new();
    let mut arch_rule_errors: Vec<Value> = Vec::new();
    let mut fenced_line_ranges: Vec<(usize, usize)> = Vec::new();
    let mut active_fenced_block: Option<(usize, usize, bool, String)> = None;
    let mut quote = String::new();
    let mut in_quote = false;
    let mut active_link: Option<ActiveLink> = None;
    let mut next_link_occurrence = 0usize;
    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let is_acdd_rule = info.split_whitespace().next() == Some("acdd-rule");
                active_fenced_block = Some((
                    range.start,
                    line_at(range.start),
                    is_acdd_rule,
                    String::new(),
                ));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((block_start, start_line, is_acdd_rule, yaml)) =
                    active_fenced_block.take()
                {
                    let end_offset = range.end.saturating_sub(1).max(block_start);
                    fenced_line_ranges.push((start_line, line_at(end_offset)));
                    if is_acdd_rule {
                        parse_acdd_rule_block(
                            &yaml,
                            start_line,
                            known_zones,
                            seen_rule_ids,
                            &mut arch_rules,
                            &mut arch_rule_errors,
                        );
                    }
                }
            }
            Event::Text(value) => {
                if let Some((_, _, true, ref mut yaml)) = active_fenced_block {
                    yaml.push_str(&value);
                }
                if in_quote && active_fenced_block.is_none() {
                    quote.push_str(&value);
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let line = line_at(range.start);
                let target = dest_url.to_string();
                citations.push(json!({"target":target,"line":line}));
                let raw_path = target.split('#').next().unwrap_or_default();
                let explicit_path = !raw_path.is_empty();
                let target_path = explicit_path
                    .then(|| normalize_claim_path(path, raw_path))
                    .flatten();
                let range = line_anchor(&target).and_then(|(raw_path, first, last)| {
                    normalize_claim_path(path, &raw_path)
                        .map(|target_path| (target_path, first, last))
                });
                active_link = Some(ActiveLink {
                    line,
                    target_path,
                    explicit_path,
                    range: range.map(|(_, first, last)| (first, last)),
                    occurrence: next_link_occurrence,
                    claim_start: symbol_claims.len(),
                });
                next_link_occurrence = next_link_occurrence.saturating_add(1);
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = active_link.take() {
                    let label_count = symbol_claims.len().saturating_sub(link.claim_start);
                    for claim in &mut symbol_claims[link.claim_start..] {
                        claim["label_count"] = json!(label_count);
                    }
                }
            }
            Event::Code(value) => {
                let name = value.trim_end_matches("()").trim();
                if is_symbol_token(name) {
                    symbols.push(name.to_owned());
                    let mut claim = json!({
                        "name":name,
                        "line":line_at(range.start),
                        "explicit_path":active_link.as_ref().is_some_and(|link| link.explicit_path),
                        "label_count":1,
                    });
                    if let Some(link) = &active_link {
                        claim["link_occurrence"] = json!(link.occurrence);
                        claim["line"] = json!(link.line);
                        if let Some(target_path) = &link.target_path {
                            claim["target_path"] = json!(target_path);
                        }
                        if let Some((first, last)) = link.range {
                            claim["start_line"] = json!(first);
                            claim["end_line"] = json!(last);
                        }
                    }
                    symbol_claims.push(claim);
                }
                if in_quote {
                    quote.push_str(&value);
                }
            }
            Event::Start(Tag::BlockQuote) => {
                in_quote = true;
                quote.clear();
            }
            Event::End(TagEnd::BlockQuote) => {
                if quote.contains("Invariant:")
                    && ["[!IMPORTANT]", "[!WARNING]", "[!NOTE]"]
                        .iter()
                        .any(|alert| quote.contains(alert))
                {
                    invariants.push(quote.trim().to_owned());
                }
                in_quote = false;
            }
            Event::SoftBreak | Event::HardBreak if in_quote => quote.push('\n'),
            _ => {}
        }
    }
    append_wikilinks(content, section_line, &mut citations);
    symbols.sort();
    symbols.dedup();
    ScannedSectionEvents {
        symbols,
        invariants,
        citations,
        symbol_claims,
        arch_rules,
        arch_rule_errors,
        fenced_line_ranges,
    }
}

fn build_doc_section_entry(
    state: &SectionScanState<'_>,
    entry: DocSectionEntryInput<'_>,
    seen_rule_ids: &mut BTreeSet<String>,
) -> (DocSection, Value) {
    let DocSectionEntryInput {
        index,
        start,
        end,
        title,
        content,
        mtime,
    } = entry;
    let base_offset = state.body_offset + start;
    let section_line = state
        .line_starts
        .partition_point(|&start_pos| start_pos <= base_offset);
    let section_end_line = section_line + content.lines().count().saturating_sub(1);
    let ScannedSectionEvents {
        symbols,
        invariants,
        citations,
        symbol_claims,
        arch_rules,
        arch_rule_errors,
        fenced_line_ranges,
    } = scan_section_events(
        state.path,
        content,
        section_line,
        base_offset,
        state.line_starts,
        state.known_zones,
        seen_rule_ids,
    );
    let declared_invariants = content
        .lines()
        .enumerate()
        .filter_map(|(offset, line)| {
            let line_num = section_line + offset;
            if fenced_line_ranges
                .iter()
                .any(|&(block_start, block_end)| line_num >= block_start && line_num <= block_end)
            {
                return None;
            }
            let id = invariant_id(line)?;
            let rule = line.trim().trim_start_matches(['-', '*']).trim();
            (!rule.is_empty()).then(|| json!({"id":id,"rule":rule,"line":line_num}))
        })
        .collect::<Vec<_>>();
    let _ = end;
    let is_invariant = !invariants.is_empty() || !declared_invariants.is_empty();
    let doc = DocSection {
        doc_id: format!("doc:{}:{index}", state.path),
        path: state.path.into(),
        section_title: title.to_owned(),
        doc_type: state
            .meta
            .doc_type
            .clone()
            .unwrap_or_else(|| default_doc_type(state.path, state.spec_paths).into()),
        content: content.into(),
        is_invariant,
        invariants,
        referenced_symbols: symbols,
        mtime,
        size: content.len() as u64,
    };
    let mut details = json!({
        "line":section_line,
        "end_line":section_end_line,
        "status":state.meta.status,
        "citations":citations,
        "symbol_claims":symbol_claims,
        "declared_invariants":declared_invariants,
        "is_invariant":doc.is_invariant,
    });
    if let Some(error) = state.frontmatter_error {
        details["frontmatter_error"] = json!(error);
    }
    if !arch_rules.is_empty() {
        details["arch_rules"] = json!(arch_rules);
    }
    if !arch_rule_errors.is_empty() {
        details["arch_rule_errors"] = json!(arch_rule_errors);
    }
    (doc, details)
}

/// Source-derived metadata retained on the generic document node with optional known architecture zones.
pub fn extract_with_details_with_zones(
    path: &str,
    source: &str,
    mtime: f64,
    spec_paths: &[String],
    known_zones: Option<&BTreeSet<String>>,
) -> Result<Vec<(DocSection, Value)>> {
    let (frontmatter_yaml, body, body_offset) = split_frontmatter(source);
    let (meta, frontmatter_error) = match frontmatter_yaml {
        Some(yaml) => match serde_yaml::from_str::<Frontmatter>(yaml) {
            Ok(meta) => (meta, None),
            Err(err) => (Frontmatter::default(), Some(err.to_string())),
        },
        None => (Frontmatter::default(), None),
    };

    let mut headings = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => current = Some((range.start, String::new())),
            Event::Text(t) | Event::Code(t) if current.is_some() => {
                if let Some((_, title)) = &mut current {
                    title.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((start, title)) = current.take() {
                    headings.push((start, title));
                }
            }
            _ => {}
        }
    }
    if headings.first().is_none_or(|(start, _)| *start > 0) {
        headings.insert(0, (0, meta.title.clone().unwrap_or_else(|| path.into())));
    }

    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let state = SectionScanState {
        path,
        spec_paths,
        known_zones,
        line_starts: &line_starts,
        body_offset,
        meta: &meta,
        frontmatter_error: frontmatter_error.as_deref(),
    };
    let mut seen_rule_ids = BTreeSet::new();
    let mut output = Vec::new();
    for (index, (start, title)) in headings.iter().enumerate() {
        let end = headings
            .get(index + 1)
            .map_or(body.len(), |(start, _)| *start);
        let content = &body[*start..end];
        if content.trim().is_empty() {
            continue;
        }
        output.push(build_doc_section_entry(
            &state,
            DocSectionEntryInput {
                index,
                start: *start,
                end,
                title,
                content,
                mtime,
            },
            &mut seen_rule_ids,
        ));
    }
    Ok(output)
}

fn parse_acdd_rule_block(
    yaml: &str,
    start_line: usize,
    known_zones: Option<&BTreeSet<String>>,
    seen_rule_ids: &mut BTreeSet<String>,
    arch_rules: &mut Vec<Value>,
    arch_rule_errors: &mut Vec<Value>,
) {
    let raw_value = match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
        Ok(value) => value,
        Err(err) => {
            arch_rule_errors.push(json!({
                "line": start_line,
                "message": format!("invalid acdd-rule YAML: {err}"),
            }));
            return;
        }
    };
    let specs: Vec<ArchRuleSpec> = match raw_value {
        serde_yaml::Value::Sequence(items) => {
            if items.is_empty() {
                arch_rule_errors.push(json!({
                    "line": start_line,
                    "message": "invalid acdd-rule YAML: empty rule sequence",
                }));
                return;
            }
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                match serde_yaml::from_value::<ArchRuleSpec>(item) {
                    Ok(spec) => parsed.push(spec),
                    Err(err) => {
                        arch_rule_errors.push(json!({
                            "line": start_line,
                            "message": format!("invalid acdd-rule schema: {err}"),
                        }));
                        return;
                    }
                }
            }
            parsed
        }
        other => match serde_yaml::from_value::<ArchRuleSpec>(other) {
            Ok(spec) => vec![spec],
            Err(err) => {
                arch_rule_errors.push(json!({
                    "line": start_line,
                    "message": format!("invalid acdd-rule schema: {err}"),
                }));
                return;
            }
        },
    };
    for spec in specs {
        if let Err(err) = spec.validate(known_zones) {
            arch_rule_errors.push(json!({
                "line": start_line,
                "message": format!("invalid acdd-rule '{}': {err}", spec.id),
            }));
            continue;
        }
        if !seen_rule_ids.insert(spec.id.clone()) {
            arch_rule_errors.push(json!({
                "line": start_line,
                "message": format!("duplicate acdd-rule id '{}' in document", spec.id),
            }));
            continue;
        }
        arch_rules.push(json!({
            "line": start_line,
            "rule": spec,
        }));
    }
}

/// Returns the default `doc_type` inferred from path and configured `specs.*.path` when frontmatter omits `doc_type`.
fn default_doc_type(path: &str, spec_paths: &[String]) -> &'static str {
    let relative = path
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("]/").map(|(_, rel)| rel))
        .unwrap_or(path)
        .trim_start_matches("./")
        .replace('\\', "/");
    let rel_parts: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let Some(file_name) = rel_parts.last().copied() else {
        return "guide";
    };
    if file_name == "README.md" || file_name == "AGENTS.md" {
        return "guide";
    }
    for spec_path in spec_paths {
        let spec_norm = spec_path
            .trim()
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_matches('/')
            .to_owned();
        if spec_norm.is_empty() {
            continue;
        }
        let spec_parts: Vec<&str> = spec_norm
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if !spec_parts.is_empty()
            && rel_parts.len() > spec_parts.len()
            && rel_parts[..spec_parts.len()] == spec_parts[..]
        {
            let tail = &rel_parts[spec_parts.len()..];
            if !tail
                .iter()
                .any(|segment| segment.eq_ignore_ascii_case("archive"))
            {
                return "contract";
            }
        }
    }
    "guide"
}

fn is_symbol_token(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || "_.:".contains(c))
}

fn line_anchor(target: &str) -> Option<(String, usize, usize)> {
    let (path, fragment) = target.split_once('#')?;
    let fragment = fragment.strip_prefix('L')?;
    let (first, last) = fragment.split_once("-L").unwrap_or((fragment, fragment));
    let first = first.parse::<usize>().ok()?;
    let last = last.parse::<usize>().ok()?;
    (first > 0 && last >= first && !path.is_empty()).then(|| (path.to_owned(), first, last))
}

fn normalize_claim_path(source_path: &str, target_path: &str) -> Option<String> {
    if target_path.is_empty()
        || target_path.starts_with('/')
        || target_path.contains('\\')
        || target_path.contains(':')
    {
        return None;
    }
    let (workspace_prefix, source_relative) = if source_path.starts_with('[') {
        let close = source_path.find("]/")? + 2;
        (&source_path[..close], &source_path[close..])
    } else {
        ("", source_path)
    };
    let mut components = std::path::Path::new(source_relative)
        .parent()?
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    for component in target_path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value.to_owned()),
        }
    }
    (!components.is_empty()).then(|| format!("{workspace_prefix}{}", components.join("/")))
}

fn invariant_id(line: &str) -> Option<&str> {
    let start = line.find("INV-")?;
    let rest = &line[start..];
    let (id, _) = rest.split_once(':')?;
    (id.len() > 4
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        }))
    .then_some(id)
}

fn append_wikilinks(content: &str, start_line: usize, citations: &mut Vec<Value>) {
    for (line_offset, line) in content.lines().enumerate() {
        let mut rest = line;
        let mut consumed = 0;
        while let Some(open) = rest.find("[[") {
            let after_open = open + 2;
            let Some(close) = rest[after_open..].find("]]") else {
                break;
            };
            let target = rest[after_open..after_open + close].trim();
            if !target.is_empty() {
                citations
                    .push(json!({"target":target,"line":start_line + line_offset,"wiki":true}));
            }
            let advance = after_open + close + 2;
            consumed += advance;
            rest = &line[consumed..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_doc_type_classifies_configured_spec_paths_and_exclusions() {
        let specs = vec![
            "docs/milestones".to_owned(),
            "docs/plans".to_owned(),
            "docs/rfcs".to_owned(),
        ];
        let cases = [
            ("docs/milestones/AC-001.md", "contract"),
            ("[linked]/docs/plans/PLAN-002.md", "contract"),
            ("docs/rfcs/RFC-003.md", "contract"),
            ("docs/milestones/archive/AC-001.md", "guide"),
            ("docs/milestones/Archive/AC-001.md", "guide"),
            ("docs/milestones/AC-001-archive.md", "contract"),
            ("docs/milestones/README.md", "guide"),
            ("docs/plans/AGENTS.md", "guide"),
            ("docs/architecture/README.md", "guide"),
            ("docs/contracts/legacy.md", "guide"),
        ];
        for (path, expected) in cases {
            assert_eq!(default_doc_type(path, &specs), expected, "{path}");
        }

        // Lightweight outline strips frontmatter and handles both ATX and Setext headings.
        let md = "---\ntitle: Spec\n----\n---\nTop\n===\n\n## Child `fn_a`\n\nSub\n---\n";
        assert_eq!(
            render_outline("docs/milestones/AC-001.md", md, &specs).unwrap(),
            "# Top\n## Child fn_a\n## Sub"
        );

        // Malformed frontmatter surfaces frontmatter_error and declared INV- aligns is_invariant.
        let bad_fm = "---\ntitle: [unterminated\n---\n# Rules\n- INV-ONLY-DECLARED: must align.\n";
        let extracted =
            extract_with_details_with_zones("docs/rules.md", bad_fm, 0.0, &specs, None).unwrap();
        assert_eq!(extracted.len(), 1);
        let (doc, details) = &extracted[0];
        assert!(doc.is_invariant);
        assert_eq!(details["is_invariant"], json!(true));
        assert!(
            details["frontmatter_error"]
                .as_str()
                .is_some_and(|e| !e.is_empty()),
            "malformed frontmatter must record frontmatter_error: {details}"
        );
    }
}
