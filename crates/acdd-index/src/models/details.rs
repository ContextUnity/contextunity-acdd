use serde_json::Value;

/// Keys used in the persisted node details projection.
pub struct NodeDetailKey;
impl NodeDetailKey {
    pub const SIGNATURE: &'static str = "signature";
    pub const DOC: &'static str = "doc";
    pub const DOCSTRING: &'static str = "docstring";
    #[cfg(test)]
    pub(crate) const VISIBILITY: &'static str = "visibility";
    #[cfg(test)]
    pub(crate) const PARAMS: &'static str = "params";
    #[cfg(test)]
    pub(crate) const RETURNS: &'static str = "returns";
    pub const ASYNC: &'static str = "async";
    #[cfg(test)]
    pub(crate) const EXPORT: &'static str = "export";
    #[cfg(test)]
    pub(crate) const STATIC: &'static str = "static";
    #[cfg(test)]
    pub(crate) const CATEGORY: &'static str = "category";
    pub const DEFAULT_EXPORT: &'static str = "default_export";
    pub const RETURN_TYPE: &'static str = "return_type";
    pub const RULE: &'static str = "rule";
    pub const ENTRYPOINT: &'static str = "entrypoint";
    pub const ENTRYPOINT_BASE: &'static str = "entrypoint_base";
    pub const ENTRYPOINT_ORIGINAL_JSON: &'static str = "entrypoint_original_json";
    pub const IMPL_TRAIT: &'static str = "impl_trait";
    pub(crate) const TRAIT_DEFAULT: &'static str = "trait_default";
    pub const DECORATORS: &'static str = "decorators";
    pub(crate) const IS_STUB: &'static str = "is_stub";
    pub(crate) const IS_OVERLOAD: &'static str = "is_overload";
    pub const INCLUDED_FROM: &'static str = "included_from";
    pub const SOURCE_PATH: &'static str = "source_path";
    pub const ARCH_RULES: &'static str = "arch_rules";
    pub const COGNITIVE_COMPLEXITY: &'static str = "cognitive_complexity";
    pub const MAX_NESTING_DEPTH: &'static str = "max_nesting_depth";
    pub const FACT_LIMIT_EXCEEDED: &'static str = "fact_limit_exceeded";
}

/// Boolean properties projected from node details.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeDetailFlags {
    pub has_entrypoint: bool,
    pub has_impl_trait: bool,
    pub has_trait_default: bool,
    pub has_cfg_test: bool,
    pub has_tool: bool,
    pub is_stub: bool,
    pub is_overload: bool,
}
fn is_cfg_test_attribute(dec: &str) -> bool {
    let Some((_, rest)) = dec.split_once("cfg(") else {
        return false;
    };
    let mut in_quotes = false;
    let mut escaped = false;
    let mut ident = String::new();
    for ch in rest.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            continue;
        }
        if ch == '"' {
            in_quotes = true;
            ident.clear();
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
        } else {
            if ident == "test" {
                return true;
            }
            ident.clear();
        }
    }
    ident == "test"
}

impl NodeDetailFlags {
    pub fn from_value(value: &Value) -> Self {
        let present = |key| value.get(key).is_some_and(|v| !v.is_null());
        let mut flags = Self {
            has_entrypoint: present(NodeDetailKey::ENTRYPOINT),
            has_impl_trait: present(NodeDetailKey::IMPL_TRAIT),
            has_trait_default: present(NodeDetailKey::TRAIT_DEFAULT),
            is_stub: value
                .get(NodeDetailKey::IS_STUB)
                .and_then(Value::as_bool)
                .unwrap_or(false),
            is_overload: value
                .get(NodeDetailKey::IS_OVERLOAD)
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Self::default()
        };
        if let Some(decorators) = value
            .get(NodeDetailKey::DECORATORS)
            .and_then(Value::as_array)
        {
            for dec in decorators.iter().filter_map(Value::as_str) {
                flags.has_cfg_test |= is_cfg_test_attribute(dec);
                flags.has_tool |= dec.starts_with("#[tool") || dec.starts_with("#[rmcp::tool");
            }
        }
        flags
    }
    pub fn from_json(raw: &str) -> Self {
        if !raw.starts_with('{')
            || ![
                NodeDetailKey::ENTRYPOINT,
                NodeDetailKey::IMPL_TRAIT,
                NodeDetailKey::TRAIT_DEFAULT,
                NodeDetailKey::DECORATORS,
                NodeDetailKey::IS_STUB,
                NodeDetailKey::IS_OVERLOAD,
            ]
            .iter()
            .any(|&key| raw.contains(key))
        {
            return Self::default();
        }
        serde_json::from_str::<Value>(raw)
            .map_or_else(|_| Self::default(), |v| Self::from_value(&v))
    }
}
