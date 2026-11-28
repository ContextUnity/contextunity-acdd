use crate::errors::{AcddError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Policy for object properties not explicitly declared in `SchemaDef::properties`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AdditionalPropertiesDef {
    /// Allow (`true`) or reject (`false`) undeclared object properties.
    Boolean(bool),
    /// Validate each undeclared object property against this nested schema.
    Schema(Box<SchemaDef>),
}

/// Self-contained JSON Schema subset shared by proof schemas and `DomainProvider` capabilities/tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchemaDef {
    /// Expected JSON type (`object`, `array`, `string`, `integer`, `number`, `boolean`, `null`).
    #[serde(rename = "type")]
    pub schema_type: String,
    /// Named property schemas when `type == "object"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, SchemaDef>,
    /// Required property names when `type == "object"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Policy or schema for properties absent from `properties`.
    #[serde(
        default,
        rename = "additionalProperties",
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<AdditionalPropertiesDef>,
    /// Element schema required when `type == "array"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<SchemaDef>>,
    /// Closed set of allowed JSON values.
    #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<Value>>,
    /// Exact constant JSON value required at this node.
    #[serde(default, rename = "const", skip_serializing_if = "Option::is_none")]
    pub const_value: Option<Value>,
    /// Minimum Unicode scalar length allowed when `type == "string"`.
    #[serde(default, rename = "minLength", skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// Inclusive lower bound allowed when `type == "integer"` or `type == "number"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// Minimum number of own keys allowed when `type == "object"`.
    #[serde(default, rename = "minProperties", skip_serializing_if = "Option::is_none")]
    pub min_properties: Option<usize>,
    /// Minimum number of items allowed when `type == "array"`.
    #[serde(default, rename = "minItems", skip_serializing_if = "Option::is_none")]
    pub min_items: Option<usize>,
}

impl SchemaDef {
    /// Merge an object-schema overlay onto `self` (properties, required keys, `additionalProperties`, `minProperties`).
    pub fn merge_object_overlay(&mut self, overlay: &SchemaDef, ctx: &str) -> Result<(), AcddError> {
        if self.schema_type != "object" || overlay.schema_type != "object" {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("{ctx} schema must have type 'object'"),
            ));
        }
        for (k, v) in &overlay.properties {
            self.properties.insert(k.clone(), v.clone());
        }
        for req in &overlay.required {
            if !self.required.contains(req) {
                self.required.push(req.clone());
            }
        }
        if overlay.additional_properties.is_some() {
            self.additional_properties = overlay.additional_properties.clone();
        }
        if overlay.min_properties.is_some() {
            self.min_properties = overlay.min_properties;
        }
        Ok(())
    }

    /// Construct a minimal schema for the given JSON type name.
    pub fn of_type(schema_type: impl Into<String>) -> Self {
        Self {
            schema_type: schema_type.into(),
            properties: BTreeMap::new(),
            required: Vec::new(),
            additional_properties: None,
            items: None,
            enum_values: None,
            const_value: None,
            min_length: None,
            minimum: None,
            min_properties: None,
            min_items: None,
        }
    }

    /// Construct an `"object"` schema.
    pub fn object() -> Self {
        Self::of_type("object")
    }

    /// Construct a `"string"` schema.
    pub fn string() -> Self {
        Self::of_type("string")
    }

    /// Construct an `"array"` schema with the given item schema.
    pub fn array(items: SchemaDef) -> Self {
        let mut schema = Self::of_type("array");
        schema.items = Some(Box::new(items));
        schema
    }

    /// Validate that this `SchemaDef` itself is structurally sound.
    pub fn validate_schema(&self, path: &str) -> Result<(), AcddError> {
        if self.enum_values.as_ref().is_some_and(Vec::is_empty) {
            return Err(schema_err(format!("{path}.enum must not be empty")));
        }
        if self.min_length.is_some() && self.schema_type != "string" {
            return Err(schema_err(format!(
                "{path}.minLength is only valid when type is 'string'"
            )));
        }
        if let Some(min) = self.minimum {
            if self.schema_type != "integer" && self.schema_type != "number" {
                return Err(schema_err(format!(
                    "{path}.minimum is only valid when type is 'integer' or 'number'"
                )));
            }
            if !min.is_finite() {
                return Err(schema_err(format!("{path}.minimum must be finite")));
            }
        }
        if self.min_properties.is_some() && self.schema_type != "object" {
            return Err(schema_err(format!(
                "{path}.minProperties is only valid when type is 'object'"
            )));
        }
        if self.min_items.is_some() && self.schema_type != "array" {
            return Err(schema_err(format!(
                "{path}.minItems is only valid when type is 'array'"
            )));
        }

        match self.schema_type.as_str() {
            "object" => {
                if self.items.is_some() {
                    return Err(schema_err(format!("{path}.items is only valid for arrays")));
                }
                for req in &self.required {
                    if !self.properties.contains_key(req) {
                        return Err(schema_err(format!(
                            "{path}.required references undefined property '{req}'"
                        )));
                    }
                }
                if let Some(AdditionalPropertiesDef::Schema(extra)) = &self.additional_properties {
                    extra.validate_schema(&format!("{path}.additionalProperties"))?;
                }
                for (prop_name, prop_schema) in &self.properties {
                    prop_schema.validate_schema(&format!("{path}.properties.{prop_name}"))?;
                }
            }
            "array" => {
                if !self.properties.is_empty()
                    || !self.required.is_empty()
                    || self.additional_properties.is_some()
                {
                    return Err(schema_err(format!(
                        "{path} array schema cannot declare object fields"
                    )));
                }
                let Some(items) = &self.items else {
                    return Err(schema_err(format!("{path}.items is required for arrays")));
                };
                items.validate_schema(&format!("{path}.items"))?;
            }
            "string" | "integer" | "number" | "boolean" | "null" | "any" => {
                if !self.properties.is_empty()
                    || !self.required.is_empty()
                    || self.items.is_some()
                    || self.additional_properties.is_some()
                {
                    return Err(schema_err(format!(
                        "{path} scalar schema cannot declare container fields"
                    )));
                }
            }
            other => {
                return Err(schema_err(format!("{path}.type '{other}' is unsupported")));
            }
        }

        Ok(())
    }

    /// Validate a JSON value against this schema, failing closed with `ACDD_WORKFLOW_SCHEMA_INVALID`.
    pub fn validate_value(&self, value: &Value, path: &str) -> Result<(), AcddError> {
        self.validate_value_with_code(value, path, ErrorCode::WorkflowSchemaInvalid)
    }

    /// Validate a JSON value against this schema using `code` for any validation error.
    pub fn validate_value_with_code(
        &self,
        value: &Value,
        path: &str,
        code: ErrorCode,
    ) -> Result<(), AcddError> {
        let type_matches = match self.schema_type.as_str() {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "any" => true,
            _ => false,
        };
        if !type_matches {
            return Err(AcddError::new(
                code,
                format!("{path} must be {}", self.schema_type),
            ));
        }

        if let Some(allowed) = &self.enum_values {
            if !allowed.contains(value) {
                return Err(AcddError::new(
                    code,
                    format!("{path} is not an allowed enum value"),
                ));
            }
        }
        if let Some(expected) = &self.const_value {
            if expected != value {
                return Err(AcddError::new(
                    code,
                    format!("{path} does not match required const value"),
                ));
            }
        }
        if let (Some(min_len), Some(s)) = (self.min_length, value.as_str()) {
            if s.chars().count() < min_len {
                return Err(AcddError::new(
                    code,
                    format!("{path} must have at least {min_len} characters"),
                ));
            }
        }
        if let (Some(min), Some(num)) = (self.minimum, value.as_f64()) {
            if num < min {
                return Err(AcddError::new(code, format!("{path} must be >= {min}")));
            }
        }

        if let Some(object) = value.as_object() {
            self.validate_object_map(
                object.iter().map(|(k, v)| (k.as_str(), v)),
                object.len(),
                path,
                code,
                &[],
                false,
            )?;
        }

        if let Some(array) = value.as_array() {
            if let Some(min_items) = self.min_items {
                if array.len() < min_items {
                    return Err(AcddError::new(
                        code,
                        format!("{path} must have at least {min_items} items"),
                    ));
                }
            }
            if let Some(item_schema) = &self.items {
                for (idx, item) in array.iter().enumerate() {
                    item_schema.validate_value_with_code(item, &format!("{path}[{idx}]"), code)?;
                }
            }
        }

        Ok(())
    }

    /// Validate spec or item extra fields against this object schema, rejecting unknown keys
    /// unless `additionalProperties: true` or an `additionalProperties` schema is set, and attaching
    /// an edit-distance hint (`did you mean '...'?`) against declared + `core_field_names`.
    pub fn validate_spec_fields(
        &self,
        fields: &BTreeMap<String, Value>,
        core_field_names: &[&str],
        path: &str,
    ) -> Result<(), AcddError> {
        if self.schema_type != "object" {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("{path} fields schema must have type 'object'"),
            ));
        }
        self.validate_object_map(
            fields.iter().map(|(k, v)| (k.as_str(), v)),
            fields.len(),
            path,
            ErrorCode::SpecSchemaInvalid,
            core_field_names,
            true,
        )
    }

    fn validate_object_map<'a>(
        &self,
        entries: impl Iterator<Item = (&'a str, &'a Value)>,
        len: usize,
        path: &str,
        code: ErrorCode,
        core_field_names: &[&str],
        reject_undeclared_by_default: bool,
    ) -> Result<(), AcddError> {
        if let Some(min_props) = self.min_properties {
            if len < min_props {
                return Err(AcddError::new(
                    code,
                    format!("{path} must have at least {min_props} properties"),
                ));
            }
        }
        let entries_vec: Vec<(&str, &Value)> = entries.collect();
        for req in &self.required {
            if !entries_vec.iter().any(|(k, _)| k == req) {
                return Err(AcddError::new(code, format!("{path}.{req} is required")));
            }
        }
        for (key, item) in entries_vec {
            if let Some(prop_schema) = self.properties.get(key) {
                prop_schema.validate_value_with_code(item, &format!("{path}.{key}"), code)?;
            } else if let Some(AdditionalPropertiesDef::Schema(extra)) = &self.additional_properties {
                extra.validate_value_with_code(item, &format!("{path}.{key}"), code)?;
            } else if matches!(
                self.additional_properties,
                Some(AdditionalPropertiesDef::Boolean(false))
            ) || (self.additional_properties.is_none() && reject_undeclared_by_default)
            {
                return Err(unknown_field_error(
                    code,
                    path,
                    key,
                    self.properties.keys().map(String::as_str),
                    core_field_names,
                ));
            }
        }
        Ok(())
    }
}

fn unknown_field_error<'a>(
    code: ErrorCode,
    path: &str,
    unknown_key: &str,
    schema_keys: impl Iterator<Item = &'a str>,
    extra_keys: &[&'a str],
) -> AcddError {
    let mut err = AcddError::new(code, format!("{path}.{unknown_key} is not allowed"));
    if code == ErrorCode::SpecSchemaInvalid {
        err = err.with_diag_code(crate::errors::DIAG_SPEC_FIELD_UNKNOWN);
    }
    if let Some(suggestion) =
        suggest_similar_key(unknown_key, schema_keys.chain(extra_keys.iter().copied()))
    {
        err = err.with_hint(format!("did you mean '{suggestion}'?"));
    }
    err
}

/// Suggest the closest matching declared identifier using bounded Damerau-Levenshtein distance.
pub fn suggest_similar_key<'a>(
    unknown_key: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let max_dist = if unknown_key.chars().count() >= 6 { 3 } else { 2 };
    candidates
        .map(|cand| (damerau_levenshtein(unknown_key, cand), cand))
        .filter(|(dist, _)| *dist <= max_dist)
        .min()
        .map(|(_, suggestion)| suggestion)
}

fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (ac.len(), bc.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(ac[i - 1] != bc[j - 1]);
            let mut v = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && ac[i - 1] == bc[j - 2] && ac[i - 2] == bc[j - 1] {
                v = v.min(dp[i - 2][j - 2] + 1);
            }
            dp[i][j] = v;
        }
    }
    dp[n][m]
}

fn schema_err(message: String) -> AcddError {
    AcddError::new(ErrorCode::WorkflowSchemaInvalid, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_self_validation_matrix() {
        let valid_obj = SchemaDef {
            properties: BTreeMap::from([(
                "name".to_owned(),
                SchemaDef {
                    min_length: Some(1),
                    ..SchemaDef::string()
                },
            )]),
            required: vec!["name".to_owned()],
            additional_properties: Some(AdditionalPropertiesDef::Boolean(false)),
            ..SchemaDef::object()
        };

        let mut bad_required = SchemaDef::object();
        bad_required.required = vec!["missing".to_owned()];

        let mut bad_array_no_items = SchemaDef::of_type("array");
        bad_array_no_items.items = None;

        let mut bad_min_length = SchemaDef::of_type("integer");
        bad_min_length.min_length = Some(1);

        let mut bad_minimum = SchemaDef::string();
        bad_minimum.minimum = Some(0.0);

        let mut bad_minimum_nan = SchemaDef::of_type("number");
        bad_minimum_nan.minimum = Some(f64::NAN);

        let mut bad_empty_enum = SchemaDef::string();
        bad_empty_enum.enum_values = Some(vec![]);

        let mut bad_scalar_with_props = SchemaDef::string();
        bad_scalar_with_props
            .properties
            .insert("x".to_owned(), SchemaDef::of_type("boolean"));

        let mut bad_min_properties = SchemaDef::string();
        bad_min_properties.min_properties = Some(1);

        let mut bad_min_items = SchemaDef::object();
        bad_min_items.min_items = Some(1);

        let mut valid_obj_min_props = SchemaDef::object();
        valid_obj_min_props.min_properties = Some(1);

        let mut valid_arr_min_items = SchemaDef::array(SchemaDef::string());
        valid_arr_min_items.min_items = Some(1);

        let cases: &[(&str, &SchemaDef, bool)] = &[
            ("valid_obj", &valid_obj, true),
            ("valid_obj_min_props", &valid_obj_min_props, true),
            ("valid_arr_min_items", &valid_arr_min_items, true),
            ("bad_required", &bad_required, false),
            ("bad_array_no_items", &bad_array_no_items, false),
            ("bad_min_length", &bad_min_length, false),
            ("bad_minimum", &bad_minimum, false),
            ("bad_minimum_nan", &bad_minimum_nan, false),
            ("bad_min_properties", &bad_min_properties, false),
            ("bad_min_items", &bad_min_items, false),
            ("bad_empty_enum", &bad_empty_enum, false),
            ("bad_scalar_with_props", &bad_scalar_with_props, false),
        ];

        for &(label, schema, expect_ok) in cases {
            let res = schema.validate_schema("$");
            assert_eq!(res.is_ok(), expect_ok, "validate_schema({label}): {res:?}");
            if let Err(err) = res {
                assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid);
            }
        }
    }

    #[test]
    fn schema_value_validation_matrix() {
        let mut status_schema = SchemaDef::string();
        status_schema.enum_values = Some(vec![json!("pass"), json!("reject")]);

        let mut hops_schema = SchemaDef::of_type("integer");
        hops_schema.minimum = Some(1.0);

        let mut version_schema = SchemaDef::of_type("integer");
        version_schema.const_value = Some(json!(2));

        let mut summary_schema = SchemaDef::string();
        summary_schema.min_length = Some(3);

        let mut tags_schema = SchemaDef::array(SchemaDef::string());
        tags_schema.min_items = Some(1);

        let mut meta_schema = SchemaDef::object();
        meta_schema.min_properties = Some(1);

        let mut root = SchemaDef::object();
        root.properties.insert("status".to_owned(), status_schema);
        root.properties.insert("hops".to_owned(), hops_schema);
        root.properties.insert("version".to_owned(), version_schema);
        root.properties.insert("summary".to_owned(), summary_schema);
        root.properties.insert("tags".to_owned(), tags_schema);
        root.properties.insert("meta".to_owned(), meta_schema);
        root.required = vec!["status".to_owned(), "version".to_owned()];
        root.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

        root.validate_schema("$").unwrap();

        let cases: &[(&str, Value, bool)] = &[
            (
                "valid_full",
                json!({"status": "pass", "version": 2, "hops": 8, "summary": "ok!", "tags": ["a"], "meta": {"k": "v"}}),
                true,
            ),
            ("missing_required", json!({"status": "pass"}), false),
            (
                "invalid_enum",
                json!({"status": "maybe", "version": 2}),
                false,
            ),
            (
                "invalid_const",
                json!({"status": "pass", "version": 1}),
                false,
            ),
            (
                "below_minimum",
                json!({"status": "pass", "version": 2, "hops": 0}),
                false,
            ),
            (
                "float_for_integer",
                json!({"status": "pass", "version": 2, "hops": 1.5}),
                false,
            ),
            (
                "too_short_min_length",
                json!({"status": "pass", "version": 2, "summary": "ab"}),
                false,
            ),
            (
                "unicode_min_length_counts_scalars",
                json!({"status": "pass", "version": 2, "summary": "їжа"}),
                true,
            ),
            (
                "extra_property_rejected",
                json!({"status": "pass", "version": 2, "extra": true}),
                false,
            ),
            (
                "empty_array_rejected_by_min_items",
                json!({"status": "pass", "version": 2, "tags": []}),
                false,
            ),
            (
                "empty_object_rejected_by_min_properties",
                json!({"status": "pass", "version": 2, "meta": {}}),
                false,
            ),
            (
                "bad_array_item",
                json!({"status": "pass", "version": 2, "tags": [123]}),
                false,
            ),
        ];

        for (label, value, expect_ok) in cases {
            let res = root.validate_value(value, "$");
            assert_eq!(res.is_ok(), *expect_ok, "validate_value({label}): {res:?}");
            if let Err(err) = res {
                assert_eq!(err.code, ErrorCode::WorkflowSchemaInvalid);
            }
        }
        let mut s = SchemaDef::object();
        s.properties.insert("guidance".to_owned(), SchemaDef::of_type("any"));
        s.validate_schema("spec").unwrap();
        s.validate_spec_fields(&BTreeMap::from([("guidance".to_owned(), json!({"a": 1}))]), &["status"], "spec").unwrap();
        let err = s.validate_spec_fields(&BTreeMap::from([("stauts".to_owned(), json!("ok"))]), &["status"], "spec").unwrap_err();
        assert_eq!((err.code, err.hint.as_deref()), (ErrorCode::SpecSchemaInvalid, Some("did you mean 'status'?")));
    }
}
