//! Invariant collection helpers for `docs.invariants` context lens.

use super::super::common::{
    truncate_single_line, DOCS_INVARIANT_FALLBACK_ITEM_LIMIT, DOCS_INVARIANT_MAX_RULE_CHARS,
};
use super::ranking::{any_glob_matches, is_archived_doc_rel};
use super::walk::collect_matching_worktree_files_where;
use acdd_core::config::{Adapter, SpecConfig};
use acdd_core::fs::is_doc_path;
use acdd_index::db::reader;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn collect_scoped_db_invariants(
    conn: &Connection,
    admitted_scope: &[String],
    docs_globs: &[String],
    effective_cols: &BTreeMap<String, SpecConfig>,
    enforced_ids: &BTreeSet<String>,
    seen: &mut BTreeSet<(String, String)>,
    items: &mut Vec<Value>,
) {
    let Ok(scoped) = reader::governing_invariants_for_scope(conn, admitted_scope) else {
        return;
    };
    for bucket in ["direct", "omitted_sources"] {
        let Some(arr) = scoped.get(bucket).and_then(Value::as_array) else {
            continue;
        };
        for entry in arr {
            let id = entry["id"].as_str().unwrap_or_default().to_owned();
            let path = entry["source"]["path"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if id.is_empty()
                || path.is_empty()
                || !is_docs_invariant_eligible(&path, docs_globs, effective_cols)
            {
                continue;
            }
            let raw_rule = strip_invariant_prefix(entry["rule"].as_str().unwrap_or_default(), &id);
            let rule = truncate_single_line(raw_rule, DOCS_INVARIANT_MAX_RULE_CHARS);
            if seen.insert((path.clone(), id.clone())) {
                let enforced = enforced_ids.contains(&id);
                items.push(build_invariant_item(
                    id,
                    path,
                    Some(entry["source"]["line"].clone()),
                    rule,
                    Some(true),
                    enforced,
                ));
            }
        }
    }
}

pub(super) fn collect_fallback_db_invariants(
    conn: &Connection,
    docs_globs: &[String],
    effective_cols: &BTreeMap<String, SpecConfig>,
    enforced_ids: &BTreeSet<String>,
    seen: &mut BTreeSet<(String, String)>,
    items: &mut Vec<Value>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT path, section_title, invariants
         FROM doc_sections
         WHERE is_invariant = 1
         ORDER BY CASE WHEN path LIKE 'docs/adr/%' OR path LIKE 'docs/architecture/%' THEN 0 ELSE 1 END,
                  path,
                  section_title
         LIMIT 64",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows.flatten() {
        if items.len() >= DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
            break;
        }
        let (path, section_title, inv_raw) = row;
        if !is_docs_invariant_eligible(&path, docs_globs, effective_cols) {
            continue;
        }
        let parsed_entries = parse_doc_section_invariants(
            inv_raw.as_deref(),
            &section_title,
            DOCS_INVARIANT_MAX_RULE_CHARS,
        );
        for (id, rule) in parsed_entries {
            if items.len() >= DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
                break;
            }
            if seen.insert((path.clone(), id.clone())) {
                let enforced = enforced_ids.contains(&id);
                items.push(build_invariant_item(
                    id,
                    path.clone(),
                    None,
                    rule,
                    Some(false),
                    enforced,
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn collect_configured_enforced_ids(
    worktree_adapter: &Adapter,
    enforced_ids: &mut BTreeSet<String>,
) {
    for spec in &worktree_adapter.architecture.rules {
        if let Some(inv) = spec
            .invariant
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| spec.id.starts_with("INV-").then_some(spec.id.as_str()))
        {
            enforced_ids.insert(inv.to_owned());
        }
    }
}

pub(super) fn collect_filesystem_invariants(
    worktree: &Path,
    docs_globs: &[String],
    effective_cols: &BTreeMap<String, SpecConfig>,
    enforced_ids: &BTreeSet<String>,
    seen: &mut BTreeSet<(String, String)>,
    items: &mut Vec<Value>,
) {
    for (_, rel) in collect_matching_worktree_files_where(worktree, |rel| {
        is_doc_path(rel) && is_docs_invariant_eligible(rel, docs_globs, effective_cols)
    }) {
        let Ok(text) = acdd_core::fs::read_confined_utf8(worktree, std::path::Path::new(&rel))
        else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let Some(inv_id) = extract_invariant_id(line) else {
                continue;
            };
            if !seen.insert((rel.clone(), inv_id.clone())) {
                continue;
            }
            let enforced = enforced_ids.contains(&inv_id);
            items.push(build_invariant_item(
                inv_id,
                rel.clone(),
                Some(json!(idx + 1)),
                truncate_single_line(line.trim(), DOCS_INVARIANT_MAX_RULE_CHARS),
                None,
                enforced,
            ));
            if items.len() >= DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
                return;
            }
        }
    }
}

fn build_invariant_item(
    id: String,
    path: String,
    line: Option<Value>,
    rule: String,
    scope_match: Option<bool>,
    enforced: bool,
) -> Value {
    let mut item = serde_json::Map::new();
    item.insert("id".to_owned(), json!(id));
    item.insert("path".to_owned(), json!(path));
    if let Some(line) = line.filter(|v| !v.is_null()) {
        item.insert("line".to_owned(), line);
    }
    item.insert("rule".to_owned(), json!(rule));
    if let Some(scope_match) = scope_match {
        item.insert("scope_match".to_owned(), json!(scope_match));
    }
    if enforced {
        item.insert("enforced".to_owned(), json!(true));
    }
    Value::Object(item)
}

fn is_docs_invariant_eligible(
    rel_path: &str,
    docs_globs: &[String],
    effective_cols: &BTreeMap<String, SpecConfig>,
) -> bool {
    !is_archived_doc_rel(rel_path)
        && any_glob_matches(docs_globs, rel_path)
        && acdd_spec::match_spec_collection(effective_cols, rel_path).is_none()
}

fn extract_invariant_id(line: &str) -> Option<String> {
    let idx = line.find("INV-")?;
    let tail = &line[idx..];
    let end = tail
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(tail.len());
    let candidate = &tail[..end];
    (candidate.len() > 4).then(|| candidate.to_owned())
}

fn strip_invariant_prefix<'a>(text: &'a str, id: &str) -> &'a str {
    let trimmed = text.trim();
    let without_id = trimmed
        .strip_prefix(id)
        .or_else(|| trimmed.strip_prefix("Invariant"))
        .unwrap_or(trimmed);
    without_id
        .trim_start_matches([':', '-', '—', '`', '*', ' '])
        .trim()
}

fn parse_doc_section_invariants(
    inv_raw: Option<&str>,
    section_title: &str,
    max_rule_chars: usize,
) -> Vec<(String, String)> {
    let texts: Vec<String> = match inv_raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) if raw.starts_with('[') => serde_json::from_str::<Vec<String>>(raw)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect(),
        Some(raw) => vec![raw.to_owned()],
        None => Vec::new(),
    };
    if texts.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for block in texts {
        let mut cursor = block.as_str();
        let mut found_id = false;
        while let Some(idx) = cursor.find("INV-") {
            if let Some(id) = extract_invariant_id(&cursor[idx..]) {
                let after_id = &cursor[idx + id.len()..];
                let next_inv = after_id.find("INV-").unwrap_or(after_id.len());
                let raw_rule = strip_invariant_prefix(&after_id[..next_inv], &id);
                let rule = truncate_single_line(raw_rule, max_rule_chars);
                out.push((id, rule));
                found_id = true;
                cursor = &after_id[next_inv..];
            } else {
                cursor = &cursor[idx + 4..];
            }
        }
        if !found_id {
            let clean = strip_invariant_prefix(
                block.trim_start_matches("[!IMPORTANT]").trim(),
                section_title.trim(),
            );
            let id = if section_title.trim().is_empty() {
                "INV-SECTION".to_owned()
            } else {
                section_title.trim().to_owned()
            };
            out.push((id, truncate_single_line(clean, max_rule_chars)));
        }
    }
    out
}
