//! Declarative Spec schema definitions and embedded built-in `contract` / `memory` bases.

use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::schema::SchemaDef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

static CONTRACT_SCHEMA: OnceLock<SpecSchemaDef> = OnceLock::new();
static MEMORY_SCHEMA: OnceLock<SpecSchemaDef> = OnceLock::new();

fn default_roadmap_true() -> bool {
    true
}

fn ensure_object_schema(fields: &SchemaDef, path: &str) -> Result<(), AcddError> {
    if fields.schema_type != "object" {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("{path}.fields schema must have type 'object'"),
        ));
    }
    fields.validate_schema(&format!("{path}.fields"))
}

/// Declarative schema for a single named `items:` collection on a Spec type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecItemSchemaDef {
    /// Singular Markdown heading token (`### <item>: <ref>`). Defaults to the collection key when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// `SchemaDef` (`type: object`) validating extra fields on each item block.
    pub fields: SchemaDef,
}

/// Declarative schema definition for a Spec type and its named `items:` collections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecSchemaDef {
    /// Canonical Spec type or built-in base name (`contract`, `memory`, etc.).
    pub name: String,
    /// Default workflow reference when omitted in frontmatter / `acdd.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Whether specs of this type appear in `acdd roadmap` by default.
    #[serde(default = "default_roadmap_true")]
    pub roadmap: bool,
    /// `SchemaDef` (`type: object`) validating extra frontmatter fields beyond fixed core fields.
    pub fields: SchemaDef,
    /// Named item collections (`### <item>: <ref>`), e.g. `tasks`, `rules`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, SpecItemSchemaDef>,
}

impl SpecSchemaDef {
    /// Validate that `fields` and every declared `items` schema are structurally valid.
    pub fn validate_schema(&self, path: &str) -> Result<(), AcddError> {
        ensure_object_schema(&self.fields, path)?;
        for (collection, item_schema) in &self.items {
            ensure_object_schema(&item_schema.fields, &format!("{path}.items.{collection}"))?;
        }
        Ok(())
    }
}

fn load_embedded_schema(raw: &str, name: &str) -> SpecSchemaDef {
    match serde_yaml::from_str::<SpecSchemaDef>(raw) {
        Ok(schema) if schema.validate_schema(name).is_ok() => schema,
        _ => SpecSchemaDef {
            name: name.to_owned(),
            workflow: None,
            roadmap: true,
            fields: SchemaDef::object(),
            items: BTreeMap::new(),
        },
    }
}

/// Return the embedded built-in `contract` Spec schema definition.
pub fn builtin_contract_schema() -> &'static SpecSchemaDef {
    CONTRACT_SCHEMA.get_or_init(|| load_embedded_schema(include_str!("embedded/contract.yaml"), "contract"))
}

/// Look up an embedded built-in Spec schema by name (`"contract"` or `"memory"`).
pub fn builtin_spec_schema(name: &str) -> Option<&'static SpecSchemaDef> {
    match name {
        "contract" => Some(builtin_contract_schema()),
        "memory" => Some(MEMORY_SCHEMA.get_or_init(|| {
            load_embedded_schema(include_str!("embedded/memory.yaml"), "memory")
        })),
        _ => None,
    }
}

fn merge_object_schema(
    base: &mut SchemaDef,
    overlay: &SchemaDef,
    ctx: &str,
) -> Result<(), AcddError> {
    base.merge_object_overlay(overlay, ctx)
}

/// Resolve a concrete [`SpecSchemaDef`] from a configured [`SpecConfig`], merging built-in base
/// (`contract` / `memory` when applicable; skipped when `ref:` binds an external spec file) with
/// collection-level `fields`, `items`, `workflow`, and `roadmap` settings.
fn resolve_spec_schema_from_config(
    name: &str,
    cfg: &SpecConfig,
) -> Result<SpecSchemaDef, AcddError> {
    let mut schema = match cfg.effective_builtin_base(Some(name)) {
        Some(base_name) => {
            let mut base = builtin_spec_schema(base_name)
                .ok_or_else(|| {
                    AcddError::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!("specs.{name}.base '{base_name}' is not a known built-in spec"),
                    )
                })?
                .clone();
            base.name = name.to_owned();
            base
        }
        None => SpecSchemaDef {
            name: name.to_owned(),
            workflow: None,
            roadmap: true,
            fields: SchemaDef::object(),
            items: BTreeMap::new(),
        },
    };
    if cfg.workflow.is_some() {
        schema.workflow = cfg.workflow.clone();
    }
    schema.roadmap = cfg.effective_roadmap(Some(name));
    if let Some(ref overlay_fields) = cfg.fields {
        merge_object_schema(
            &mut schema.fields,
            overlay_fields,
            &format!("specs.{name}.fields"),
        )?;
    }
    for (col_key, item_cfg) in &cfg.items {
        if let Some(existing) = schema.items.get_mut(col_key) {
            if item_cfg.item.is_some() {
                existing.item = item_cfg.item.clone();
            }
            if let Some(ref overlay_fields) = item_cfg.fields {
                merge_object_schema(
                    &mut existing.fields,
                    overlay_fields,
                    &format!("specs.{name}.items.{col_key}.fields"),
                )?;
            }
        } else {
            schema.items.insert(
                col_key.clone(),
                SpecItemSchemaDef {
                    item: item_cfg.item.clone(),
                    fields: item_cfg.fields.clone().unwrap_or_else(SchemaDef::object),
                },
            );
        }
    }
    schema.validate_schema(&format!("specs.{name}"))?;
    Ok(schema)
}

/// Resolve the effective [`SpecSchemaDef`] for a document given its frontmatter `spec:` selector,
/// optional owning collection `(key, cfg)`, and all configured `specs:` collections.
pub fn resolve_spec_schema(
    spec_selector: Option<&str>,
    owning_col: Option<(&str, &SpecConfig)>,
    collections: &BTreeMap<String, SpecConfig>,
) -> Result<SpecSchemaDef, AcddError> {
    if let Some(sel) = spec_selector {
        if let Some(cfg) = collections.get(sel) {
            return resolve_spec_schema_from_config(sel, cfg);
        }
        if let Some((col_key, cfg)) = owning_col {
            if col_key == sel
                || cfg.effective_builtin_base(Some(col_key)) == Some(sel)
                || sel.chars().any(char::is_whitespace)
            {
                return resolve_spec_schema_from_config(col_key, cfg);
            }
        }
        if let Some(builtin) = builtin_spec_schema(sel) {
            return Ok(builtin.clone());
        }
        if owning_col.is_none() && collections.is_empty() {
            // Standalone `parse_spec` without workspace `specs:` context allows legacy narrative `spec:` labels
            // while still validating core/contract schema rules (validated strictly once collection registry is bound).
            return Ok(builtin_contract_schema().clone());
        }
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("unknown spec type '{sel}' (not registered in acdd.yaml specs: or built-ins)"),
        ));
    }
    if let Some((col_key, cfg)) = owning_col {
        return resolve_spec_schema_from_config(col_key, cfg);
    }
    Ok(builtin_contract_schema().clone())
}

/// Resolve whether a spec document matches a collection or built-in spec type `selector`
/// under ADR 0014 §2 precedence (explicit frontmatter `spec:` overrides directory ownership).
pub fn spec_matches_selector(
    doc_spec: Option<&str>,
    owning_col_key: Option<&str>,
    selector: &str,
    collections: &BTreeMap<String, SpecConfig>,
) -> bool {
    let owning_base = owning_col_key
        .and_then(|k| collections.get(k).and_then(|c| c.effective_builtin_base(Some(k))));
    match doc_spec {
        Some(sel) if collections.contains_key(sel) => {
            let sel_base = collections
                .get(sel)
                .and_then(|c| c.effective_builtin_base(Some(sel)));
            selector == sel || sel_base == Some(selector)
        }
        Some(sel) => {
            if let Some(k) = owning_col_key {
                if k == sel || owning_base == Some(sel) || sel.chars().any(char::is_whitespace) {
                    return selector == k || owning_base == Some(selector);
                }
            }
            if builtin_spec_schema(sel).is_some() {
                return selector == sel;
            }
            selector == sel
        }
        None => owning_col_key == Some(selector) || owning_base == Some(selector),
    }
}
