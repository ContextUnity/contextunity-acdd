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
    pub const SOURCE_PATH: &'static str = "source_path";
    pub const ARCH_RULES: &'static str = "arch_rules";
    pub const COGNITIVE_COMPLEXITY: &'static str = "cognitive_complexity";
    pub const MAX_NESTING_DEPTH: &'static str = "max_nesting_depth";
    pub const FACT_LIMIT_EXCEEDED: &'static str = "fact_limit_exceeded";
    pub const BINDINGS: &'static str = "bindings";
    pub const REBINDINGS: &'static str = "rebindings";
    pub const PARAM_TYPES: &'static str = "param_types";
    #[cfg(test)]
    pub(crate) const RECEIVER: &'static str = "receiver";
    #[cfg(test)]
    pub(crate) const RECEIVER_TYPE: &'static str = "receiver_type";
    #[cfg(test)]
    pub(crate) const RECEIVER_HINT: &'static str = "receiver_hint";
    pub const VALUE_FLOW: &'static str = "value_flow";
    pub const ROUTE_PATH: &'static str = "route_path";
    pub const ROUTE_NAMESPACE: &'static str = "route_namespace";
    pub const INCLUDE_NAMESPACE: &'static str = "include_namespace";
    pub const INCLUDE_MODULE: &'static str = "include_module";
    pub const INCLUDE_DEPTH: &'static str = "include_depth";
    pub const SOURCE_ID: &'static str = "source_id";
    pub const SOURCE_LINE: &'static str = "source_line";
    pub const SOURCE_QUALNAME: &'static str = "source_qualname";
    pub const METHOD: &'static str = "method";
    pub const NAME: &'static str = "name";
    pub const INCLUDED_FROM: &'static str = "included_from";
    pub const PYTHON_LOGGING_FACTORIES: &'static str = "python_logging_factories";
    pub const COMMONJS_BINDINGS: &'static str = "commonjs_bindings";
    pub const LAZY_EXPORTS: &'static str = "lazy_exports";
    pub const EXPORTS: &'static str = "exports";
    pub const SEMANTIC_FACTS: &'static str = "semantic_facts";
    pub const COLUMN: &'static str = "column";
    pub const ENTRYPOINT_DIRECT_MODULE: &'static str = "entrypoint_direct_module";
    pub const NAMED_DEFINITION_UNCERTAINTIES: &'static str = "named_definition_uncertainties";
    #[cfg(test)]
    pub(crate) const IS_STATIC: &'static str = "is_static";
    pub const NAMED_DEFINITION_UNCERTAINTY_FAULT: &'static str =
        "named_definition_uncertainty_fault";
    pub const CLASSIC_GLOBAL: &'static str = "classic_global";
    pub(crate) const IS_PUBLIC: &'static str = "is_public";
    pub(crate) const RUST_DERIVE_TRAIT: &'static str = "rust_derive_trait";
}

/// Boolean properties projected from node details.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeDetailFlags {
    pub has_entrypoint: bool,
    pub has_impl_trait: bool,
    pub has_trait_default: bool,
    pub has_cfg_test: bool,
    pub has_tool: bool,
    pub has_schema_derive: bool,
    pub is_public: bool,
    pub is_stub: bool,
    pub is_overload: bool,
}

const WIRE_CLI_DERIVES: &[&str] = &[
    "Serialize",
    "Deserialize",
    "JsonSchema",
    "Parser",
    "Args",
    "Subcommand",
    "ValueEnum",
];

fn split_top_level_cfg_args(expr: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (idx, ch) in expr.char_indices() {
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
        match ch {
            '"' => in_quotes = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(expr[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = expr[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn eval_cfg_is_test_only(expr: &str) -> bool {
    let e = expr.trim();
    if e == "test" {
        return true;
    }
    if let Some(inner) = e
        .strip_prefix("all")
        .map(str::trim)
        .and_then(|r| r.strip_prefix('('))
        .and_then(|r| r.strip_suffix(')'))
    {
        return split_top_level_cfg_args(inner)
            .into_iter()
            .any(eval_cfg_is_test_only);
    }
    if let Some(inner) = e
        .strip_prefix("any")
        .map(str::trim)
        .and_then(|r| r.strip_prefix('('))
        .and_then(|r| r.strip_suffix(')'))
    {
        let args = split_top_level_cfg_args(inner);
        return args.iter().any(|arg| arg.trim() == "test")
            || (!args.is_empty() && args.into_iter().all(eval_cfg_is_test_only));
    }
    false
}

pub(crate) fn is_cfg_test_attribute(dec: &str) -> bool {
    let compact: String = dec.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(body) = compact.strip_prefix("#[").and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let Some(inner) = body.strip_prefix("cfg(").and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    eval_cfg_is_test_only(inner)
}

fn has_schema_derive_decorator(dec: &str) -> bool {
    let d = dec.trim();
    if d.starts_with("#[serde(") || d.starts_with("#[schemars(") || d.starts_with("#[clap(") {
        return true;
    }
    let Some(inner) = d.strip_prefix("#[derive(").and_then(|s| s.strip_suffix(")]")) else {
        return false;
    };
    inner.split(',').any(|trait_path| {
        let tail = trait_path.trim().rsplit("::").next().unwrap_or("").trim();
        WIRE_CLI_DERIVES.contains(&tail)
    })
}

impl NodeDetailFlags {
    pub fn from_value(value: &Value) -> Self {
        let present = |key| value.get(key).is_some_and(|v| !v.is_null());
        let mut flags = Self {
            has_entrypoint: present(NodeDetailKey::ENTRYPOINT),
            has_impl_trait: present(NodeDetailKey::IMPL_TRAIT),
            has_trait_default: present(NodeDetailKey::TRAIT_DEFAULT),
            has_schema_derive: value
                .get(NodeDetailKey::RUST_DERIVE_TRAIT)
                .and_then(Value::as_str)
                .is_some_and(|t| WIRE_CLI_DERIVES.contains(&t)),
            is_public: value
                .get(NodeDetailKey::IS_PUBLIC)
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
                flags.has_schema_derive |= has_schema_derive_decorator(dec);
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
                NodeDetailKey::IS_PUBLIC,
                NodeDetailKey::RUST_DERIVE_TRAIT,
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

#[cfg(test)]
mod tests {
    use super::{is_cfg_test_attribute, NodeDetailFlags};

    #[test]
    fn cfg_test_attribute_and_schema_derive_flags_match_contract() {
        for (attr, expected) in [
            ("#[cfg(test)]", true),
            ("#[cfg(all(test, target_os = \"linux\"))]", true),
            ("#[cfg(any(test, feature = \"nonexistent\"))]", true),
            ("#[cfg(not(test))]", false),
            ("#[cfg(all(not(test), unix))]", false),
            ("#[cfg(feature = \"test\")]", false),
            ("#[cfg_attr(test, allow(dead_code))]", false),
        ] {
            assert_eq!(is_cfg_test_attribute(attr), expected, "{attr}");
        }
        let flags = NodeDetailFlags::from_json(
            r##"{"is_public":true,"decorators":["#[derive(serde::Serialize)]"]}"##,
        );
        assert!(flags.is_public && flags.has_schema_derive);
    }
}
