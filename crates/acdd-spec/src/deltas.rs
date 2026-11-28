//! Pure `spec_deltas` schema, validation, and deterministic target Spec verification.

use acdd_core::errors::{AcddError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Expected before/after text change for an id-keyed entry in `spec_deltas[].modified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecDeltaModification {
    /// Optional prior text expected before the edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previously: Option<String>,
    /// Required post-edit text that the target entry must carry.
    pub now: String,
}

/// Declarative edit declared by one Spec against an id-keyed list or map on another Spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecDelta {
    /// Target Spec ID (`SpecDocument.id`).
    pub target: String,
    /// Target field or named item collection on the target Spec.
    pub field: String,
    /// Entry IDs that must exist in `target.field`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// Entry IDs mapped to expected post-edit `now` text in `target.field`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modified: BTreeMap<String, SpecDeltaModification>,
    /// Entry IDs that must be absent from `target.field`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

/// Deterministic outcome of verifying `spec_deltas` across a workspace Spec set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeltaVerificationReport {
    /// Number of source Spec documents declaring non-empty `spec_deltas` evaluated.
    pub specs_checked: usize,
    /// Number of individual `SpecDelta` entries evaluated.
    pub deltas_checked: usize,
    /// Human-readable per-delta failure diagnostics (empty on pass).
    pub diagnostics: Vec<String>,
}

impl DeltaVerificationReport {
    /// Return `true` when all evaluated `spec_deltas` entries satisfied their target Specs.
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Read-only projection of a Spec document used by [`verify_spec_deltas`].
pub trait DeltaSpecView {
    /// Specification document identifier (`id`).
    fn delta_spec_id(&self) -> &str;
    /// Content digest used as a deterministic tie-breaker.
    fn delta_spec_digest(&self) -> &str;
    /// Declared `spec_deltas` entries on this document.
    fn delta_spec_deltas(&self) -> &[SpecDelta];
    /// Lookup an extra frontmatter field by name.
    fn delta_field(&self, field: &str) -> Option<&Value>;
    /// Lookup a named item or task collection by name as `(item_ref, searchable_payload)` pairs.
    fn delta_named_entries(&self, field: &str) -> Option<Vec<(&str, Value)>>;
}

/// Return `true` when `segment` is a safe single path-component identifier (no separators, `..`, or control/placeholder chars).
pub fn is_safe_path_segment_id(segment: &str) -> bool {
    let trimmed = segment.trim();
    !trimmed.is_empty()
        && trimmed == segment
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains(['/', '\\', '\0'])
        && !trimmed.contains("..")
}

fn delta_err(msg: impl Into<String>) -> AcddError {
    AcddError::new(ErrorCode::SpecSchemaInvalid, msg)
}

fn validate_non_empty_id(id: &str, label: &str) -> Result<(), AcddError> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed != id {
        return Err(delta_err(format!(
            "{label} entry id '{id}' must be non-empty without surrounding whitespace"
        )));
    }
    Ok(())
}

/// Validate structural and non-contradiction invariants for parsed `spec_deltas` entries.
pub fn validate_spec_deltas(deltas: &[SpecDelta]) -> Result<(), AcddError> {
    for (idx, delta) in deltas.iter().enumerate() {
        let ctx = format!("spec_deltas[{idx}]");
        if !is_safe_path_segment_id(&delta.target) {
            return Err(delta_err(format!(
                "{ctx}.target '{}' must be a safe non-empty spec identifier",
                delta.target
            )));
        }
        if delta.field.trim().is_empty() || delta.field.trim() != delta.field {
            return Err(delta_err(format!(
                "{ctx}.field '{}' must be non-empty without surrounding whitespace",
                delta.field
            )));
        }
        let mut added_set = BTreeSet::new();
        for id in &delta.added {
            validate_non_empty_id(id, &format!("{ctx}.added"))?;
            added_set.insert(id.as_str());
        }
        for (id, mod_spec) in &delta.modified {
            validate_non_empty_id(id, &format!("{ctx}.modified"))?;
            if mod_spec.now.trim().is_empty() {
                return Err(delta_err(format!(
                    "{ctx}.modified.{id}.now must be non-empty"
                )));
            }
        }
        for id in &delta.removed {
            validate_non_empty_id(id, &format!("{ctx}.removed"))?;
            if added_set.contains(id.as_str()) || delta.modified.contains_key(id.as_str()) {
                return Err(delta_err(format!(
                    "{ctx}: entry '{id}' cannot appear in both 'removed' and 'added'/'modified'"
                )));
            }
        }
    }
    Ok(())
}

/// Verify all `spec_deltas` declared on `sources` against `all_specs`, optionally filtered by `target_filter`.
pub fn verify_spec_deltas<S: DeltaSpecView>(
    sources: &[&S],
    all_specs: &[&S],
    target_filter: Option<&str>,
) -> DeltaVerificationReport {
    let mut specs_by_id: BTreeMap<&str, Vec<&S>> = BTreeMap::new();
    for spec in all_specs {
        specs_by_id
            .entry(spec.delta_spec_id())
            .or_default()
            .push(*spec);
    }
    let mut sorted_sources: Vec<&S> = sources.to_vec();
    sorted_sources.sort_by(|a, b| {
        a.delta_spec_id()
            .cmp(b.delta_spec_id())
            .then_with(|| a.delta_spec_digest().cmp(b.delta_spec_digest()))
    });

    let mut report = DeltaVerificationReport::default();
    for src in sorted_sources {
        let deltas = src.delta_spec_deltas();
        if deltas.is_empty() {
            continue;
        }
        let mut matched_source = false;
        for (idx, delta) in deltas.iter().enumerate() {
            if target_filter.is_some_and(|tf| delta.target != tf) {
                continue;
            }
            if !matched_source {
                report.specs_checked += 1;
                matched_source = true;
            }
            report.deltas_checked += 1;
            let prefix = format!(
                "{} spec_deltas[{idx}] ({}.{})",
                src.delta_spec_id(),
                delta.target,
                delta.field
            );
            let Some(candidates) = specs_by_id.get(delta.target.as_str()) else {
                report.diagnostics.push(format!(
                    "{prefix}: target spec '{}' not found",
                    delta.target
                ));
                continue;
            };
            if candidates.len() > 1 {
                report.diagnostics.push(format!(
                    "{prefix}: target spec '{}' is ambiguous ({} active specs share this id)",
                    delta.target,
                    candidates.len()
                ));
                continue;
            }
            let target_doc = candidates[0];
            let entries = match extract_target_entries(target_doc, &delta.field, &prefix) {
                Ok(entries) => entries,
                Err(diag) => {
                    report.diagnostics.push(diag);
                    continue;
                }
            };
            for id in &delta.added {
                if !entries.contains_key(id.as_str()) {
                    report.diagnostics.push(format!(
                        "{prefix}: added entry '{id}' is missing from '{}.{}'",
                        delta.target, delta.field
                    ));
                }
            }
            for id in &delta.removed {
                if entries.contains_key(id.as_str()) {
                    report.diagnostics.push(format!(
                        "{prefix}: removed entry '{id}' is still present in '{}.{}'",
                        delta.target, delta.field
                    ));
                }
            }
            for (id, mod_spec) in &delta.modified {
                match entries.get(id.as_str()) {
                    None => report.diagnostics.push(format!(
                        "{prefix}: modified entry '{id}' is missing from '{}.{}'",
                        delta.target, delta.field
                    )),
                    Some(vals) if !vals.iter().any(|v| value_carries_now(v, &mod_spec.now)) => {
                        report.diagnostics.push(format!(
                            "{prefix}: modified entry '{id}' in '{}.{}' does not carry expected now text '{}'",
                            delta.target, delta.field, mod_spec.now
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
    }
    report
}

fn extract_target_entries<S: DeltaSpecView>(
    doc: &S,
    field: &str,
    prefix: &str,
) -> Result<BTreeMap<String, Vec<Value>>, String> {
    let mut entries: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let target_id = doc.delta_spec_id();
    if let Some(val) = doc.delta_field(field) {
        match val {
            Value::Array(items) => {
                for (pos, item) in items.iter().enumerate() {
                    match item {
                        Value::String(s) => {
                            let (id, text) = split_string_entry(s);
                            entries.entry(id.to_owned()).or_default().push(json!(text));
                        }
                        Value::Object(map) => {
                            let Some((id_key, id_val)) = extract_object_entry_id(map) else {
                                return Err(format!(
                                    "{prefix}: entry [{pos}] in '{target_id}.{field}' must be a string or object with string 'id'/'ref'/'name'/'key'",
                                ));
                            };
                            let mut payload = map.clone();
                            payload.remove(id_key);
                            entries
                                .entry(id_val.to_owned())
                                .or_default()
                                .push(Value::Object(payload));
                        }
                        other => {
                            return Err(format!(
                                "{prefix}: entry [{pos}] in '{target_id}.{field}' has unsupported type {}",
                                json_type_name(other)
                            ));
                        }
                    }
                }
                return Ok(entries);
            }
            Value::Object(map) => {
                for (k, v) in map {
                    entries.entry(k.clone()).or_default().push(v.clone());
                }
                return Ok(entries);
            }
            other => {
                return Err(format!(
                    "{prefix}: target field '{target_id}.{field}' must be a list or mapping, got {}",
                    json_type_name(other)
                ));
            }
        }
    }
    if let Some(named) = doc.delta_named_entries(field) {
        for (id, payload) in named {
            entries.entry(id.to_owned()).or_default().push(payload);
        }
        return Ok(entries);
    }
    Err(format!(
        "{prefix}: target field '{field}' not found on '{target_id}'"
    ))
}

fn split_string_entry(s: &str) -> (&str, &str) {
    let trimmed = s.trim();
    if let Some((head, tail)) = trimmed.split_once(':') {
        let id = head.trim();
        if !id.is_empty() && !id.contains(char::is_whitespace) {
            return (id, tail.trim());
        }
    }
    if let Some((head, tail)) = trimmed.split_once(char::is_whitespace) {
        let id = head.trim();
        if !id.is_empty() {
            return (id, tail.trim());
        }
    }
    (trimmed, "")
}

fn extract_object_entry_id(map: &Map<String, Value>) -> Option<(&'static str, &str)> {
    for key in ["id", "ref", "name", "key"] {
        if let Some(id) = map.get(key).and_then(Value::as_str).map(str::trim) {
            if !id.is_empty() {
                return Some((key, id));
            }
        }
    }
    None
}

fn value_carries_now(val: &Value, now: &str) -> bool {
    match val {
        Value::String(s) => s.contains(now),
        Value::Bool(b) => (if *b { "true" } else { "false" }) == now,
        Value::Number(n) => n.to_string() == now,
        Value::Array(items) => items.iter().any(|item| value_carries_now(item, now)),
        Value::Object(map) => map.values().any(|v| value_carries_now(v, now)),
        Value::Null => false,
    }
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
