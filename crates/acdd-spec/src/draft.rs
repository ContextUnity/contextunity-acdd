//! Specification drafting and slug sanitization (`draft_spec`).

use crate::discovery::{effective_spec_collections, next_spec_id_for_prefix};
use crate::overlay::SpecRuntimeOverlay;
use crate::schema::resolve_spec_schema;
use crate::spec::parse_spec_with_collections;
use crate::types::{DraftReq, DraftResp};
use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::schema::SchemaDef;
use std::collections::BTreeMap;
use std::path::Path;

/// Normalize a user-supplied draft slug into a bounded lowercase `[a-z0-9_-]` identifier.
fn sanitize_slug(raw: &str) -> Result<String, AcddError> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let slug: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() || slug.len() > 64 {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("invalid draft slug '{raw}'"),
        ));
    }
    Ok(slug)
}

/// Draft a new sequentially numbered specification Markdown document inside `primary`.
pub fn draft_spec(
    primary: &Path,
    configured_specs: &BTreeMap<String, SpecConfig>,
    req: &DraftReq,
    overlay: &dyn SpecRuntimeOverlay,
) -> Result<DraftResp, AcddError> {
    let collections = effective_spec_collections(configured_specs);
    let col = collections.get(&req.spec).ok_or_else(|| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("unknown spec collection '{}'", req.spec),
        )
    })?;

    let slug = sanitize_slug(&req.slug)?;
    let draft_dir = col.default_draft_dir().ok_or_else(|| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "spec collection '{}' has no literal non-glob directory for drafting",
                req.spec
            ),
        )
    })?;

    let (spec_id, spec_rel) = if col.prefixes.is_empty() && req.prefix.is_none() {
        (slug.clone(), format!("{draft_dir}/{slug}.md"))
    } else {
        let prefix = match req.prefix {
            Some(ref p) => {
                if !col.prefixes.is_empty() && !col.prefixes.contains(p) {
                    return Err(AcddError::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!(
                            "prefix '{p}' is not allowed for collection '{}' (allowed: {:?})",
                            req.spec, col.prefixes
                        ),
                    ));
                }
                p.clone()
            }
            None => col
                .prefixes
                .first()
                .cloned()
                .unwrap_or_else(|| "SPEC".to_owned()),
        };

        let db_max = overlay.max_spec_number(&req.spec, &prefix)?;
        let spec_id = next_spec_id_for_prefix(primary, &req.spec, col, &prefix, db_max)?;
        let spec_rel = format!("{draft_dir}/{spec_id}-{slug}.md");
        (spec_id, spec_rel)
    };

    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| slug.replace('-', " "));
    let schema = resolve_spec_schema(None, Some((req.spec.as_str(), col)), &collections)?;
    let workflow_line = req
        .workflow
        .as_deref()
        .or(col.workflow.as_deref())
        .map(|w| format!("workflow: {w}\n"))
        .unwrap_or_default();

    let mut frontmatter = format!(
        "---\nid: {spec_id}\ntitle: \"{}\"\nspec: {}\n{workflow_line}status: planned\n",
        title.replace('"', "\\\""),
        req.spec
    );
    let mut emitted_fields = std::collections::BTreeSet::from([
        "id".to_owned(),
        "title".to_owned(),
        "spec".to_owned(),
        "workflow".to_owned(),
        "status".to_owned(),
    ]);
    for key in ["invariants", "acceptance_criteria"] {
        if schema.fields.properties.contains_key(key) && emitted_fields.insert(key.to_owned()) {
            frontmatter.push_str(&format!("{key}: []\n"));
        }
    }
    for req_field in &schema.fields.required {
        if emitted_fields.insert(req_field.clone()) {
            let val = default_yaml_value(schema.fields.properties.get(req_field));
            frontmatter.push_str(&format!("{req_field}: {val}\n"));
        }
    }
    frontmatter.push_str("---\n\n");

    let mut rendered = format!("{frontmatter}# {spec_id}: {title}\n");
    if let Some((group_key, item_schema)) = schema.items.iter().next() {
        let token = item_schema.item.as_deref().unwrap_or(group_key.as_str());
        let item_ref = if token == "task" {
            "T1".to_owned()
        } else {
            let prefix = token
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('I');
            format!("{prefix}1")
        };
        rendered.push_str(&format!("\n### {token}: {item_ref}\n\n```yaml\n"));
        if token == "task" {
            rendered.push_str(
                "target: \"Describe task T1 target\"\nscope: []\ntags: [default]\ninvariants: []\nacceptance_criteria: []\n",
            );
            let preset = ["target", "scope", "tags", "invariants", "acceptance_criteria"];
            for req_field in &item_schema.fields.required {
                if !preset.contains(&req_field.as_str()) {
                    let val = default_yaml_value(item_schema.fields.properties.get(req_field));
                    rendered.push_str(&format!("{req_field}: {val}\n"));
                }
            }
        } else {
            let mut item_emitted = std::collections::BTreeSet::new();
            for req_field in &item_schema.fields.required {
                if item_emitted.insert(req_field.clone()) {
                    let val = default_yaml_value(item_schema.fields.properties.get(req_field));
                    rendered.push_str(&format!("{req_field}: {val}\n"));
                }
            }
            if item_emitted.is_empty() {
                rendered.push_str(&format!("title: \"Describe {token} {item_ref}\"\n"));
            }
        }
        rendered.push_str("```\n");
    }

    let _ = parse_spec_with_collections(&rendered, Some((req.spec.as_str(), col)), &collections)?;

    match acdd_core::fs::write_new_confined(primary, Path::new(&spec_rel), rendered.as_bytes()) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("draft target '{spec_rel}' already exists"),
            ));
        }
        Err(err) => {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("failed to write drafted spec '{spec_rel}': {err}"),
            ));
        }
    }

    Ok(DraftResp {
        spec_id,
        spec_path: spec_rel,
        spec: req.spec.clone(),
    })
}

fn default_yaml_value(schema: Option<&SchemaDef>) -> String {
    let Some(s) = schema else {
        return "\"TBD\"".to_owned();
    };
    if let Some(ref c) = s.const_value {
        return yaml_scalar(c);
    }
    if let Some(first) = s.enum_values.as_ref().and_then(|v| v.first()) {
        return yaml_scalar(first);
    }
    match s.schema_type.as_str() {
        "array" => {
            let count = s.min_items.unwrap_or(0);
            if count == 0 {
                "[]".to_owned()
            } else {
                let elem = default_yaml_value(s.items.as_deref());
                let elems: Vec<String> = (0..count).map(|_| elem.clone()).collect();
                format!("[{}]", elems.join(", "))
            }
        }
        "object" => {
            if s.required.is_empty() {
                "{}".to_owned()
            } else {
                let entries: Vec<String> = s
                    .required
                    .iter()
                    .map(|k| format!("{k}: {}", default_yaml_value(s.properties.get(k))))
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
        }
        "boolean" => "false".to_owned(),
        "integer" => {
            let v = s.minimum.map(|m| m.ceil() as i64).unwrap_or(0).max(0);
            v.to_string()
        }
        "number" => {
            let v = s.minimum.unwrap_or(0.0);
            if v.fract() == 0.0 {
                format!("{}", v as i64)
            } else {
                v.to_string()
            }
        }
        _ => "\"TBD\"".to_owned(),
    }
}

fn yaml_scalar(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(st) => format!("\"{}\"", st.replace('"', "\\\"")),
        other => other.to_string(),
    }
}
