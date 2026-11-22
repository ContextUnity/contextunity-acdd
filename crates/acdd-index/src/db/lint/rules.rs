use super::{is_archived_path, IndexedEdge, IndexedNode, LintViolation, RuleSource};
use crate::db::commitments::{read_metadata_optional, MetadataKey};
use crate::models::details::NodeDetailKey;
use acdd_core::lint::{
    external_symbol_matches_prefix, ArchRuleKind, ArchRuleSpec, ArchitectureConfig, LintCategory,
    LintSeverity, ZoneSelector,
};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(super) struct LoadedRule {
    pub(super) spec: ArchRuleSpec,
    pub(super) source: RuleSource,
}

pub(super) struct RuleLoadResult {
    pub(super) rules: Vec<LoadedRule>,
    pub(super) violations: Vec<LintViolation>,
}

pub(super) fn resolve_architecture<'a>(
    conn: &Connection,
    explicit: Option<&'a ArchitectureConfig>,
) -> Result<Cow<'a, ArchitectureConfig>> {
    if let Some(cfg) = explicit {
        return Ok(Cow::Borrowed(cfg));
    }
    if let Some(raw) = read_metadata_optional(conn, MetadataKey::Adapter)? {
        let value: Value = serde_json::from_str(&raw).context("invalid persisted adapter JSON")?;
        if let Some(arch_val) = value.get("architecture").filter(|v| !v.is_null()) {
            let cfg: ArchitectureConfig = serde_json::from_value(arch_val.clone())
                .context("invalid persisted adapter architecture")?;
            cfg.validate()?;
            return Ok(Cow::Owned(cfg));
        }
    }
    Ok(Cow::Owned(ArchitectureConfig::default()))
}

pub(super) fn load_indexed_doc_rules(
    conn: &Connection,
) -> Result<Vec<(RuleSource, Value)>> {
    let like_pattern = format!("%{}%", NodeDetailKey::ARCH_RULES);
    let mut stmt = conn.prepare(
        "SELECT p.path, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind = 'document'
           AND n.details LIKE ?1
           AND p.path NOT LIKE 'archive/%'
           AND p.path NOT LIKE '%/archive/%'
         ORDER BY p.path, n.line, n.id",
    )?;
    let mut rows = stmt.query([like_pattern])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if is_archived_path(&path) {
            continue;
        }
        let details_raw: String = row.get(1)?;
        let Ok(details) = serde_json::from_str::<Value>(&details_raw) else {
            continue;
        };
        let Some(rules_arr) = details
            .get(NodeDetailKey::ARCH_RULES)
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in rules_arr {
            let line = item.get("line").and_then(Value::as_u64).unwrap_or(1) as usize;
            let rule_val = item.get("rule").unwrap_or(item).clone();
            entries.push((RuleSource { path: path.clone(), line }, rule_val));
        }
    }
    Ok(entries)
}

fn doc_rule_violation(
    rule_id: &str,
    source: RuleSource,
    message: String,
    invariant: Option<String>,
) -> LintViolation {
    LintViolation {
        category: LintCategory::Docs,
        rule_id: rule_id.to_owned(),
        severity: LintSeverity::Error,
        path: source.path.clone(),
        line: Some(source.line),
        message,
        invariant,
        rule_source: Some(source),
        symbol: None,
        target: None,
        suggestion: None,
    }
}

pub(super) fn load_all_rules(
    conn: &Connection,
    arch: &ArchitectureConfig,
) -> Result<RuleLoadResult> {
    let known_zones = arch.zone_names();
    let mut seen_ids: BTreeMap<String, RuleSource> = BTreeMap::new();
    let mut rules = Vec::new();
    let mut violations = Vec::new();

    for spec in &arch.rules {
        let source = RuleSource {
            path: "acdd.yaml".to_owned(),
            line: 1,
        };
        seen_ids.insert(spec.id.clone(), source.clone());
        rules.push(LoadedRule {
            spec: spec.clone(),
            source,
        });
    }

    for (source, rule_val) in load_indexed_doc_rules(conn)? {
        let spec = match serde_json::from_value::<ArchRuleSpec>(rule_val) {
            Ok(spec) => spec,
            Err(err) => {
                violations.push(doc_rule_violation(
                    "docs.invalid_arch_rule",
                    source,
                    format!("invalid persisted acdd-rule: {err}"),
                    None,
                ));
                continue;
            }
        };
        if let Err(err) = spec.validate(Some(&known_zones)) {
            violations.push(doc_rule_violation(
                "docs.invalid_arch_rule",
                source,
                format!("invalid acdd-rule '{}': {err}", spec.id),
                rule_invariant_id(&spec),
            ));
            continue;
        }
        if let Some(first_source) = seen_ids.get(&spec.id) {
            violations.push(doc_rule_violation(
                "docs.duplicate_rule_id",
                source,
                format!(
                    "duplicate arch rule id '{}' (already declared at {}:{})",
                    spec.id, first_source.path, first_source.line
                ),
                rule_invariant_id(&spec),
            ));
            continue;
        }
        seen_ids.insert(spec.id.clone(), source.clone());
        rules.push(LoadedRule { spec, source });
    }

    Ok(RuleLoadResult { rules, violations })
}

/// Returns all invariant IDs backed by `acdd.yaml` `architecture.rules` or indexed `acdd-rule` blocks.
pub fn enforced_invariant_ids(
    conn: &Connection,
    config_rules: &[ArchRuleSpec],
) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for spec in config_rules {
        if let Some(inv) = rule_invariant_id(spec) {
            ids.insert(inv);
        }
    }
    for (_, rule_val) in load_indexed_doc_rules(conn)? {
        if let Ok(spec) = serde_json::from_value::<ArchRuleSpec>(rule_val) {
            if let Some(inv) = rule_invariant_id(&spec) {
                ids.insert(inv);
            }
        }
    }
    Ok(ids)
}

pub(super) fn rule_invariant_id(spec: &ArchRuleSpec) -> Option<String> {
    spec.invariant.clone().or_else(|| {
        spec.id
            .starts_with("INV-")
            .then(|| spec.id.clone())
    })
}

pub(super) fn selector_matches_file(
    selector: &ZoneSelector,
    path: &str,
    zones: &BTreeMap<String, Vec<String>>,
    file_lang: Option<&str>,
) -> bool {
    if selector.is_excluded(path) {
        return false;
    }
    if !selector.languages.is_empty()
        && !file_lang.is_some_and(|lang| {
            selector
                .languages
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(lang))
        })
    {
        return false;
    }
    if selector.has_path_scope() {
        return selector.matches_path(path, zones);
    }
    true
}

pub(super) fn selector_has_internal_criteria(selector: &ZoneSelector) -> bool {
    selector.has_path_scope()
        || !selector.symbols.is_empty()
        || !selector.decorators.is_empty()
        || !selector.languages.is_empty()
}

pub(super) fn selector_matches_node(
    selector: &ZoneSelector,
    node: &IndexedNode,
    zones: &BTreeMap<String, Vec<String>>,
    file_lang: Option<&str>,
) -> bool {
    if !selector_has_internal_criteria(selector) {
        return false;
    }
    let effective_lang = if node.language.is_empty() {
        file_lang
    } else {
        Some(node.language.as_str())
    };
    if !selector_matches_file(selector, &node.path, zones, effective_lang) {
        return false;
    }
    if !selector.symbols.is_empty() && !node_matches_symbols(node, &selector.symbols) {
        return false;
    }
    if !selector.decorators.is_empty() && !node_matches_decorators(node, &selector.decorators) {
        return false;
    }
    true
}

pub(super) fn node_matches_symbols(node: &IndexedNode, symbols: &[String]) -> bool {
    symbols.iter().any(|sym| {
        node.name == *sym
            || node.qualname == *sym
            || node.id == *sym
            || external_symbol_matches_prefix(&node.qualname, sym)
            || node.qualname.ends_with(&format!(".{sym}"))
    })
}

fn strip_decorator(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches('@')
        .trim_start_matches("#[")
        .trim_end_matches(']')
}

fn node_matches_decorators(node: &IndexedNode, decorators: &[String]) -> bool {
    node.decorators.iter().any(|raw| {
        let stripped = strip_decorator(raw);
        decorators.iter().any(|wanted| {
            let clean = strip_decorator(wanted);
            raw == wanted || stripped == clean || external_symbol_matches_prefix(stripped, clean)
        })
    })
}

pub(super) fn edge_matches_rule(edge: &IndexedEdge, rule: &ArchRuleSpec) -> bool {
    let kind_ok = if rule.edge_kinds.is_empty() {
        match rule.kind {
            ArchRuleKind::ForbidCycles => matches!(edge.kind.as_str(), "calls" | "imports"),
            ArchRuleKind::MustImplement => {
                matches!(edge.kind.as_str(), "implements" | "inherits" | "bases" | "extends")
            }
            _ => matches!(
                edge.kind.as_str(),
                "imports"
                    | "calls"
                    | "possible_calls"
                    | "references"
                    | "inherits"
                    | "implements"
                    | "bases"
                    | "overrides"
                    | "extends"
            ),
        }
    } else {
        rule.edge_kinds
            .iter()
            .any(|k| k.as_str() == edge.kind)
    };
    if !kind_ok {
        return false;
    }
    if rule.confidences.is_empty() {
        matches!(edge.confidence.as_str(), "exact" | "inferred")
    } else {
        rule.confidences
            .iter()
            .any(|c| c.as_str() == edge.confidence)
    }
}
