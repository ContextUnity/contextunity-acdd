use crate::ast::{relations, text};
use crate::languages::support::value_flow_core::{ScopeFrameFlags, ValueFlowCollectorCore};
use crate::models::Facts;
use crate::semantic::{
    BindingFact, CallSiteEvidence, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};
use tree_sitter::Node;

pub(super) fn type_expr(node: Node<'_>, source: &str) -> TypeExpr {
    let generic_names = generic_names(node, source);
    type_expr_inner(node, source, &generic_names, 0)
}

fn type_expr_inner(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    if depth >= crate::languages::MAX_VALUE_FLOW_TYPE_DEPTH {
        return TypeExpr::Unknown;
    }
    if node.kind() == "reference_type" {
        let Some(inner) = node.child_by_field_name("type") else {
            return TypeExpr::Unknown;
        };
        let mut cursor = node.walk();
        let mutable = node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "mutable_specifier")
            || text(node, source)
                .split_whitespace()
                .any(|part| part == "mut");
        return TypeExpr::Applied {
            base: if mutable { "&mut" } else { "&" }.to_owned(),
            args: vec![type_expr_inner(inner, source, generic_names, depth + 1)],
        };
    }
    if node.kind() == "tuple_type" {
        let mut cursor = node.walk();
        let mut args = Vec::new();
        for child in node.named_children(&mut cursor) {
            args.push(type_expr_inner(child, source, generic_names, depth + 1));
        }
        return TypeExpr::Applied {
            base: "()".to_owned(),
            args,
        };
    }
    if matches!(node.kind(), "array_type" | "slice_type") {
        let Some(element) = node
            .child_by_field_name("element")
            .or_else(|| node.named_child(0))
        else {
            return TypeExpr::Unknown;
        };
        let fixed_array =
            node.kind() == "array_type" && node.child_by_field_name("length").is_some();
        return TypeExpr::Applied {
            base: if fixed_array {
                "__rust_array_type"
            } else {
                "[]"
            }
            .to_owned(),
            args: vec![type_expr_inner(element, source, generic_names, depth + 1)],
        };
    }
    if let Some(name) = trait_object_name(node, source) {
        return TypeExpr::Named { name };
    }
    if matches!(
        node.kind(),
        "type_identifier" | "scoped_type_identifier" | "primitive_type"
    ) {
        let name = text(node, source).trim();
        if name != "_" && !generic_root(node, source, generic_names) {
            return TypeExpr::Named {
                name: name.to_owned(),
            };
        }
    }
    if node.kind() == "generic_type" {
        let Some(base) = node
            .child_by_field_name("type")
            .and_then(|node| static_name(node, source))
        else {
            return TypeExpr::Unknown;
        };
        if generic_root(node, source, generic_names) {
            return TypeExpr::Unknown;
        }
        let Some(arguments) = node.child_by_field_name("type_arguments") else {
            return TypeExpr::Unknown;
        };
        let mut cursor = arguments.walk();
        let mut args = Vec::new();
        let mut has_lifetime = false;
        for argument in arguments.named_children(&mut cursor) {
            if argument.kind() == "lifetime" {
                has_lifetime = true;
                continue;
            }
            if args.len() == 4 {
                return TypeExpr::Unknown;
            }
            args.push(type_expr_inner(argument, source, generic_names, depth + 1));
        }
        if !args.is_empty() {
            return TypeExpr::Applied { base, args };
        }
        if has_lifetime {
            return TypeExpr::Named { name: base };
        }
    }
    if node.kind() == "bounded_type" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "lifetime" {
                let ty = type_expr_inner(child, source, generic_names, depth + 1);
                if !matches!(ty, TypeExpr::Unknown) {
                    return ty;
                }
            }
        }
        return TypeExpr::Unknown;
    }
    if matches!(node.kind(), "abstract_type" | "dynamic_type") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let check = if child.kind() == "trait_bounds" {
                let mut tc = child.walk();
                let found = child.named_children(&mut tc).find(|c| {
                    c.kind() == "generic_type"
                        || matches!(c.kind(), "type_identifier" | "scoped_type_identifier")
                });
                found
            } else {
                Some(child)
            };
            let Some(child) = check else { continue };
            if child.kind() == "generic_type" {
                let base_ok = child
                    .child_by_field_name("type")
                    .and_then(|ty| static_name(ty, source))
                    .is_some_and(|name| {
                        name == "Iterator"
                            || name.ends_with("::Iterator")
                            || name == "IntoIterator"
                            || name.ends_with("::IntoIterator")
                    });
                if base_ok {
                    if let Some(arguments) = child.child_by_field_name("type_arguments") {
                        let mut arg_cursor = arguments.walk();
                        for arg in arguments.named_children(&mut arg_cursor) {
                            if arg.kind() == "type_binding" {
                                let is_item = arg
                                    .child_by_field_name("name")
                                    .is_some_and(|n| text(n, source).trim() == "Item");
                                if is_item {
                                    if let Some(ty) = arg.child_by_field_name("type") {
                                        return TypeExpr::Applied {
                                            base: "Iterator".to_owned(),
                                            args: vec![type_expr_inner(
                                                ty,
                                                source,
                                                generic_names,
                                                depth + 1,
                                            )],
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
                let ty = type_expr_inner(child, source, generic_names, depth + 1);
                if !matches!(ty, TypeExpr::Unknown) {
                    return ty;
                }
            } else if matches!(child.kind(), "type_identifier" | "scoped_type_identifier") {
                if let Some(name) = static_name(child, source) {
                    if !generic_names.contains(name.as_str()) {
                        return TypeExpr::Named { name };
                    }
                }
            }
        }
    }
    TypeExpr::Unknown
}

fn trait_object_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut syntax = text(node, source).trim();
    while syntax.starts_with('(') && syntax.ends_with(')') {
        syntax = syntax[1..syntax.len().checked_sub(1)?].trim();
    }
    let bounds = syntax.strip_prefix("dyn ")?;
    let principal = bounds.split('+').next()?.trim();
    if principal.is_empty()
        || !principal
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | ':'))
    {
        return None;
    }
    Some(principal.to_owned())
}

fn generic_root(mut node: Node<'_>, source: &str, names: &HashSet<&str>) -> bool {
    if names.is_empty() {
        return false;
    }
    for _ in 0..8 {
        node = match node.kind() {
            "generic_type" => match node.child_by_field_name("type") {
                Some(base) => base,
                None => return true,
            },
            "scoped_type_identifier" | "scoped_identifier" => {
                match node.child_by_field_name("path") {
                    Some(path) => path,
                    None => return false,
                }
            }
            "type_identifier" | "identifier" | "primitive_type" => {
                return names.contains(text(node, source));
            }
            _ => return false,
        };
    }
    true
}

fn static_name(node: Node<'_>, source: &str) -> Option<String> {
    matches!(
        node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier" | "scoped_type_identifier"
    )
    .then(|| text(node, source).trim().to_owned())
}

pub(super) fn callee_name<'a>(mut node: Node<'_>, source: &'a str) -> Option<Cow<'a, str>> {
    const MAX_SEGMENTS: usize = 8;
    const MAX_BYTES: usize = 512;

    struct Parts<'a> {
        names: [Option<(&'static str, &'a str)>; MAX_SEGMENTS],
        len: usize,
        bytes: usize,
    }

    impl<'a> Parts<'a> {
        fn push(&mut self, separator: &'static str, name: &'a str) -> Option<()> {
            if self.len == MAX_SEGMENTS || name.is_empty() {
                return None;
            }
            self.bytes = self
                .bytes
                .checked_add(separator.len())?
                .checked_add(name.len())?;
            if self.bytes > MAX_BYTES
                || !name
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
            {
                return None;
            }
            self.names[self.len] = Some((separator, name));
            self.len += 1;
            Some(())
        }

        fn collect(&mut self, node: Node<'_>, source: &'a str, depth: usize) -> Option<()> {
            if depth >= MAX_SEGMENTS || node.has_error() || node.is_missing() {
                return None;
            }
            match node.kind() {
                "identifier" | "type_identifier" | "self" | "super" | "crate" => {
                    self.push("", text(node, source))
                }
                "generic_type" => self.collect(
                    node.child_by_field_name("type")?,
                    source,
                    depth + 1,
                ),
                "scoped_identifier" | "scoped_type_identifier" => {
                    if let Some(path) = node.child_by_field_name("path") {
                        self.collect(path, source, depth + 1)?;
                    }
                    let name = node.child_by_field_name("name")?;
                    if !matches!(name.kind(), "identifier" | "type_identifier" | "super") {
                        return None;
                    }
                    self.push("::", text(name, source))
                }
                "field_expression" => {
                    self.collect(node.child_by_field_name("value")?, source, depth + 1)?;
                    let field = node.child_by_field_name("field")?;
                    if field.kind() != "field_identifier" {
                        return None;
                    }
                    self.push(".", text(field, source))
                }
                _ => None,
            }
        }
    }

    if node.has_error() || node.is_missing() {
        return None;
    }
    if node.kind() == "generic_function" {
        node = node.child_by_field_name("function")?;
    }
    let mut parts = Parts {
        names: [None; MAX_SEGMENTS],
        len: 0,
        bytes: 0,
    };
    parts.collect(node, source, 0)?;
    let original = text(node, source);
    if original.len() == parts.bytes {
        return Some(Cow::Borrowed(original));
    }
    let mut canonical = String::with_capacity(parts.bytes);
    for (separator, name) in parts.names.into_iter().take(parts.len).flatten() {
        canonical.push_str(separator);
        canonical.push_str(name);
    }
    Some(Cow::Owned(canonical))
}

fn pattern_identifier<'t>(node: Node<'t>) -> Option<Node<'t>> {
    match node.kind() {
        "identifier" => Some(node),
        "mut_pattern" | "ref_pattern" | "reference_pattern" => node
            .child_by_field_name("pattern")
            .or_else(|| {
                node.named_child(0)
                    .filter(|c| c.kind() != "mutable_specifier")
                    .or_else(|| node.named_child(1))
            })
            .and_then(pattern_identifier),
        _ => None,
    }
}

const MAX_ADAPTER_CHAIN_DEPTH: usize = 7;

pub(super) fn value(node: Node<'_>, source: &str) -> ValueExpr {
    let file_facts = file_macro_facts(node, source);
    let shadows = collect_ancestor_shadows(node, source, &file_facts);
    value_depth(node, source, 0, &shadows)
}

fn literal_suffix_expr(node: Node<'_>, source: &str, suffixes: &[&str]) -> ValueExpr {
    let raw = text(node, source).trim();
    suffixes
        .iter()
        .find(|suffix| raw.ends_with(*suffix))
        .map_or(ValueExpr::Unknown, |name| ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: (*name).to_owned(),
            },
        })
}

fn has_string_literal_first_arg(node: Node<'_>, expected_argc: usize) -> bool {
    node.child_by_field_name("arguments")
        .filter(|args| args.named_child_count() == expected_argc)
        .and_then(|args| args.named_child(0))
        .is_some_and(|arg| matches!(arg.kind(), "string_literal" | "raw_string_literal"))
}

fn range_element_type(node: Node<'_>, source: &str) -> Option<TypeExpr> {
    let mut cursor = node.walk();
    let mut saw_unsuffixed_int = false;
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "char_literal" => {
                return Some(TypeExpr::Named {
                    name: "char".to_owned(),
                });
            }
            "integer_literal" => {
                if let ValueExpr::Annotated { type_expr } = literal_suffix_expr(
                    child,
                    source,
                    &[
                        "usize", "isize", "u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16",
                        "u8", "i8",
                    ],
                ) {
                    return Some(type_expr);
                }
                saw_unsuffixed_int = true;
            }
            _ => {}
        }
    }
    saw_unsuffixed_int.then(|| TypeExpr::Named {
        name: "usize".to_owned(),
    })
}

fn value_depth(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    if depth >= MAX_ADAPTER_CHAIN_DEPTH && node.kind() == "call_expression" {
        return ValueExpr::Unknown;
    }
    if depth > MAX_ADAPTER_CHAIN_DEPTH {
        return ValueExpr::Unknown;
    }
    match node.kind() {
        "array_expression" | "tuple_expression" => {
            if node.named_child_count() == 0 || node.named_child_count() > 8 {
                return ValueExpr::Unknown;
            }
            let mut cursor = node.walk();
            ValueExpr::Project {
                value: Box::new(ValueExpr::BranchJoin {
                    alternatives: node
                        .named_children(&mut cursor)
                        .map(|child| value_depth(child, source, depth + 1, shadowed_macros))
                        .collect(),
                }),
                member: if node.kind() == "array_expression" {
                    "__rust_array"
                } else {
                    "__rust_tuple"
                }
                .to_owned(),
            }
        }
        "reference_expression" => {
            node.child_by_field_name("value")
                .map_or(ValueExpr::Unknown, |inner| ValueExpr::Project {
                    value: Box::new(value_depth(inner, source, depth + 1, shadowed_macros)),
                    member: if text(node, source).trim_start().starts_with("&mut ") {
                        "__rust_ref_mut"
                    } else {
                        "__rust_ref"
                    }
                    .to_owned(),
                })
        }
        "closure_expression" => ValueExpr::Construct { callee: "__rust_closure!".to_owned() },
        "string_literal" | "raw_string_literal" => ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "str".to_owned(),
            },
        },
        "byte_string_literal" | "raw_byte_string_literal" => ValueExpr::Annotated {
            type_expr: TypeExpr::Applied {
                base: "[]".to_owned(),
                args: vec![TypeExpr::Named {
                    name: "u8".to_owned(),
                }],
            },
        },
        "boolean_literal" => ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "bool".to_owned(),
            },
        },
        "char_literal" => ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "char".to_owned(),
            },
        },
        "integer_literal" => literal_suffix_expr(
            node,
            source,
            &[
                "usize", "isize", "u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16", "u8",
                "i8",
            ],
        ),
        "float_literal" => literal_suffix_expr(node, source, &["f64", "f32"]),
        "range_expression" => range_element_type(node, source).map_or(ValueExpr::Unknown, |item| {
            ValueExpr::Annotated {
                type_expr: TypeExpr::Applied {
                    base: "__rust_range".to_owned(),
                    args: vec![item],
                },
            }
        }),
        "type_cast_expression" => {
            let ty = node
                .child_by_field_name("type")
                .map(|ty| type_expr_inner(ty, source, &HashSet::new(), 0))
                .filter(|expr| !matches!(expr, TypeExpr::Unknown));
            ty.map_or(ValueExpr::Unknown, |type_expr| ValueExpr::Annotated { type_expr })
        }
        "try_expression" => {
            node.named_child(0)
                .map_or(ValueExpr::Unknown, |inner| ValueExpr::Project {
                    value: Box::new(value_depth(inner, source, depth + 1, shadowed_macros)),
                    member: "?".to_owned(),
                })
        }
        "parenthesized_expression" => node.named_child(0).map_or(ValueExpr::Unknown, |inner| {
            value_depth(inner, source, depth, shadowed_macros)
        }),
        "unary_expression" => {
            let trimmed = text(node, source).trim_start();
            if trimmed.starts_with('*') {
                node.child_by_field_name("argument")
                    .or_else(|| node.named_child(0))
                    .map_or(ValueExpr::Unknown, |inner| ValueExpr::Project {
                        value: Box::new(value_depth(inner, source, depth + 1, shadowed_macros)),
                        member: "__rust_deref".to_owned(),
                    })
            } else if trimmed.starts_with('!') {
                ValueExpr::Annotated {
                    type_expr: TypeExpr::Named {
                        name: "bool".to_owned(),
                    },
                }
            } else {
                ValueExpr::Unknown
            }
        }
        "struct_expression" => node
            .child_by_field_name("name")
            .and_then(|name| static_name(name, source))
            .map_or(ValueExpr::Unknown, |callee| ValueExpr::Construct { callee }),
        "macro_invocation" => {
            let callee = node
                .child_by_field_name("macro")
                .map(|m| text(m, source).trim());
            match callee {
                Some("vec") => {
                    if shadowed_macros.contains("vec") {
                        ValueExpr::Unknown
                    } else {
                        vec_macro_expr(node, source).unwrap_or_else(|| ValueExpr::Construct {
                            callee: "Vec".to_string(),
                        })
                    }
                }
                Some("format") => {
                    if shadowed_macros.contains("format") {
                        ValueExpr::Unknown
                    } else {
                        ValueExpr::Construct {
                            callee: "String".to_string(),
                        }
                    }
                }
                Some("concat" | "stringify" | "include_str" | "env") => ValueExpr::Annotated {
                    type_expr: TypeExpr::Named {
                        name: "str".to_string(),
                    },
                },
                Some("option_env") => ValueExpr::Annotated {
                    type_expr: TypeExpr::Applied {
                        base: "Option".to_string(),
                        args: vec![TypeExpr::Named {
                            name: "str".to_string(),
                        }],
                    },
                },
                Some("include_bytes") => ValueExpr::Annotated {
                    type_expr: TypeExpr::Applied {
                        base: "[]".to_string(),
                        args: vec![TypeExpr::Named {
                            name: "u8".to_string(),
                        }],
                    },
                },
                Some("format_args") => ValueExpr::Annotated {
                    type_expr: TypeExpr::Named {
                        name: "std::fmt::Arguments".to_string(),
                    },
                },
                _ => ValueExpr::Unknown,
            }
        }
        "call_expression" => {
            let Some(mut function) = node.child_by_field_name("function") else {
                return ValueExpr::Unknown;
            };
            let mut generic_result = None;
            if function.kind() == "generic_function" {
                generic_result = function.child_by_field_name("type_arguments")
                    .filter(|arguments| (1..=4).contains(&arguments.named_child_count()))
                    .map(|arguments| {
                        let mut cursor = arguments.walk();
                        TypeExpr::Applied {
                            base: "__rust_call_arguments".to_owned(),
                            args: arguments.named_children(&mut cursor)
                                .map(|argument| type_expr(argument, source)).collect(),
                        }
                    });
                let Some(inner) = function.child_by_field_name("function") else {
                    return ValueExpr::Unknown;
                };
                function = inner;
            }
            if function.kind() == "field_expression" {
                if let (Some(receiver), Some(member)) = (
                    function.child_by_field_name("value"),
                    function.child_by_field_name("field"),
                ) {
                    let member_name = text(member, source);
                    let member_name = if member_name == "unwrap_or"
                        && has_string_literal_first_arg(node, 1)
                    {
                        "__rust_unwrap_or_str"
                    } else if member_name == "map_or" && has_string_literal_first_arg(node, 2) {
                        "__rust_map_or_str"
                    } else if member_name == "map" || member_name == "and_then" {
                        let mapped = node
                            .child_by_field_name("arguments")
                            .filter(|args| args.named_child_count() == 1)
                            .and_then(|args| args.named_child(0))
                            .and_then(|arg| match arg.kind() {
                                "closure_expression" if member_name == "and_then" => {
                                    let params = arg.child_by_field_name("parameters")?;
                                    let parameter = params
                                        .named_child(0)
                                        .filter(|_| params.named_child_count() == 1)?;
                                    if parameter.kind() != "identifier" {
                                        return None;
                                    }
                                    let body = arg.child_by_field_name("body")?;
                                    let function = body.child_by_field_name("function")?;
                                    let args = body.child_by_field_name("arguments")?;
                                    let value = args
                                        .named_child(0)
                                        .filter(|_| args.named_child_count() == 1)?;
                                    (body.kind() == "call_expression"
                                        && text(function, source) == "Some"
                                        && value.kind() == "identifier"
                                        && text(value, source) == text(parameter, source))
                                    .then_some("__rust_identity_some")
                                }
                                "scoped_identifier" | "identifier" => {
                                    match text(arg, source).trim() {
                                        "str::trim" | "str::trim_start" | "str::trim_end" => {
                                            Some("__rust_map_str_trim")
                                        }
                                        "String::as_str" => Some("__rust_map_str_trim"),
                                        "str::to_owned"
                                        | "str::to_string"
                                        | "ToOwned::to_owned"
                                        | "ToString::to_string"
                                        | "Into::into"
                                        | "String::from" => Some("__rust_map_to_string"),
                                        "Value::as_str"
                                        | "serde_json::Value::as_str"
                                        | "toml::Value::as_str" => Some("__rust_map_as_str"),
                                        "toml::Value::as_table" => Some("__rust_map_as_toml_table"),
                                        "toml::Value::as_integer" => Some("__rust_map_i64"),
                                        "toml::Value::as_bool" => Some("__rust_map_bool"),
                                        "Value::as_array"
                                        | "Value::as_array_mut"
                                        | "serde_json::Value::as_array"
                                        | "serde_json::Value::as_array_mut" => {
                                            Some("__rust_map_as_array")
                                        }
                                        "Value::as_object"
                                        | "Value::as_object_mut"
                                        | "serde_json::Value::as_object"
                                        | "serde_json::Value::as_object_mut" => {
                                            Some("__rust_map_as_object")
                                        }
                                        "Value::as_u64" | "serde_json::Value::as_u64" => {
                                            Some("__rust_map_u64")
                                        }
                                        "Value::as_i64" | "serde_json::Value::as_i64" => {
                                            Some("__rust_map_i64")
                                        }
                                        "Value::as_bool" | "serde_json::Value::as_bool" => {
                                            Some("__rust_map_bool")
                                        }
                                        "Path::to_str"
                                        | "std::path::Path::to_str"
                                        | "OsStr::to_str"
                                        | "std::ffi::OsStr::to_str" => {
                                            Some("__rust_map_path_to_str")
                                        }
                                        "Path::to_path_buf" | "std::path::Path::to_path_buf" => {
                                            Some("__rust_map_to_path_buf")
                                        }
                                        "Clone::clone" => Some("__rust_map_cloned"),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            });
                        mapped.unwrap_or(member_name)
                    } else if member_name == "filter_map" {
                        let result_ok = node
                            .child_by_field_name("arguments")
                            .filter(|args| args.named_child_count() == 1)
                            .and_then(|args| args.named_child(0))
                            .is_some_and(|arg| {
                                if arg.kind() == "closure_expression" {
                                    (|| -> Option<()> {
                                        let params = arg.child_by_field_name("parameters")?;
                                        let parameter = params
                                            .named_child(0)
                                            .filter(|_| params.named_child_count() == 1)?;
                                        let body = arg.child_by_field_name("body")?;
                                        let function = body.child_by_field_name("function")?;
                                        let args = body.child_by_field_name("arguments")?;
                                        let receiver = function.child_by_field_name("value")?;
                                        let field = function.child_by_field_name("field")?;
                                        (parameter.kind() == "identifier"
                                            && receiver.kind() == "identifier"
                                            && text(parameter, source) == text(receiver, source)
                                            && text(field, source) == "ok"
                                            && args.named_child_count() == 0)
                                            .then_some(())
                                    })()
                                    .is_some()
                                } else {
                                    matches!(
                                        text(arg, source).trim(),
                                        "Result::ok"
                                            | "std::result::Result::ok"
                                            | "core::result::Result::ok"
                                    )
                                }
                            });
                        if result_ok {
                            "__rust_filter_map_ok"
                        } else {
                            member_name
                        }
                    } else if member_name == "get"
                        && node
                            .child_by_field_name("arguments")
                            .filter(|args| args.named_child_count() == 1)
                            .and_then(|args| args.named_child(0))
                            .is_some_and(|arg| arg.kind() == "range_expression")
                    {
                        "__rust_get_range"
                    } else {
                        member_name
                    };
                    if let Some(projected) = closure_projection_expr(
                        node,
                        member_name,
                        receiver,
                        source,
                        depth,
                        shadowed_macros,
                    ) {
                        return projected;
                    }
                    if member_name == "collect" {
                        if let Some(TypeExpr::Applied { args, .. }) = &generic_result {
                            if let [target_ty] = args.as_slice() {
                                let inner = value_depth(receiver, source, depth + 1, shadowed_macros);
                                if matches!(inner, ValueExpr::Unknown)
                                    || !type_expr_has_unknown(target_ty)
                                {
                                    return ValueExpr::Annotated {
                                        type_expr: target_ty.clone(),
                                    };
                                }
                            }
                        }
                    }
                    if member_name == "parse" {
                        if let Some(TypeExpr::Applied { args, .. }) = &generic_result {
                            if let [target_ty] = args.as_slice() {
                                return ValueExpr::Annotated {
                                    type_expr: TypeExpr::Applied {
                                        base: "Result".to_owned(),
                                        args: vec![
                                            target_ty.clone(),
                                            TypeExpr::Named {
                                                name: "String".to_owned(),
                                            },
                                        ],
                                    },
                                };
                            }
                        }
                    }
                    if matches!(
                        member_name,
                        "unwrap"
                            | "expect"
                            | "ok"
                            | "err"
                            | "as_ref"
                            | "as_mut"
                            | "as_deref"
                            | "as_deref_mut"
                            | "cloned"
                            | "copied"
                            | "clone"
                            | "iter"
                            | "iter_mut"
                            | "into_iter"
                            | "par_iter"
                            | "par_iter_mut"
                            | "into_par_iter"
                            | "next"
                            | "map"
                            | "map_or"
                            | "map_or_else"
                            | "map_err"
                            | "filter_map"
                            | "__rust_filter_map_ok"
                            | "__rust_map_str_trim"
                            | "__rust_map_to_string"
                            | "__rust_map_as_str"
                            | "__rust_map_as_array"
                            | "__rust_map_as_object"
                            | "__rust_map_u64"
                            | "__rust_map_i64"
                            | "__rust_map_bool"
                            | "__rust_map_path_to_str"
                            | "__rust_map_to_path_buf"
                            | "__rust_map_cloned"
                            | "__rust_get_range"
                            | "flat_map"
                            | "flatten"
                            | "inspect"
                            | "inspect_err"
                            | "fuse"
                            | "as_slice"
                            | "query_row"
                            | "enumerate"
                            | "filter"
                            | "filter_entry"
                            | "take"
                            | "skip"
                            | "step_by"
                            | "zip"
                            | "chain"
                            | "rev"
                            | "peekable"
                            | "collect"
                            | "trim"
                            | "trim_start"
                            | "trim_end"
                            | "trim_matches"
                            | "trim_start_matches"
                            | "trim_end_matches"
                            | "strip_prefix"
                            | "strip_suffix"
                            | "replace"
                            | "replacen"
                            | "to_lowercase"
                            | "to_uppercase"
                            | "repeat"
                            | "rsplit"
                            | "rsplitn"
                            | "splitn"
                            | "split_whitespace"
                            | "split_once"
                            | "rsplit_once"
                            | "find"
                            | "rfind"
                            | "args"
                            | "to_owned"
                            | "to_string"
                            | "into_owned"
                            | "into_bytes"
                            | "into_inner"
                            | "file_type"
                            | "elapsed"
                            | "prefix"
                            | "suffix"
                            | "tempdir"
                            | "tempdir_in"
                            | "tempfile"
                            | "tempfile_in"
                            | "head"
                            | "peel_to_commit"
                            | "peel_to_tree"
                            | "tree"
                            | "index"
                            | "path"
                            | "depth"
                            | "timestamp"
                            | "into"
                            | "as_bytes"
                            | "lines"
                            | "split"
                            | "split_inclusive"
                            | "split_terminator"
                            | "rsplit_terminator"
                            | "split_first"
                            | "split_last"
                            | "chars"
                            | "char_indices"
                            | "bytes"
                            | "join"
                            | "to_str"
                            | "to_string_lossy"
                            | "to_path_buf"
                            | "as_path"
                            | "as_os_str"
                            | "with_extension"
                            | "with_file_name"
                            | "components"
                            | "ancestors"
                            | "canonicalize"
                            | "file_name"
                            | "extension"
                            | "current_dir"
                            | "output"
                            | "status"
                            | "get"
                            | "get_mut"
                            | "get_ref"
                            | "get_or_insert"
                            | "get_or_insert_with"
                            | "get_or_init"
                            | "first"
                            | "first_mut"
                            | "last"
                            | "last_mut"
                            | "pop"
                            | "pop_front"
                            | "pop_back"
                            | "front"
                            | "back"
                            | "remove"
                            | "swap_remove"
                            | "shift_remove"
                            | "insert"
                            | "get_key_value"
                            | "entry"
                            | "keys"
                            | "values"
                            | "values_mut"
                            | "into_keys"
                            | "into_values"
                            | "or_insert"
                            | "or_insert_with"
                            | "or_insert_with_key"
                            | "or_default"
                            | "lock"
                            | "try_lock"
                            | "read"
                            | "try_read"
                            | "write"
                            | "try_write"
                            | "borrow"
                            | "try_borrow"
                            | "borrow_mut"
                            | "try_borrow_mut"
                            | "get_or_try_init"
                            | "as_str"
                            | "as_table"
                            | "as_integer"
                            | "as_float"
                            | "as_array"
                            | "as_array_mut"
                            | "as_object"
                            | "as_object_mut"
                            | "as_u64"
                            | "as_i64"
                            | "as_bool"
                            | "arg"
                            | "and_then"
                            | "or"
                            | "or_else"
                            | "and"
                            | "transpose"
                            | "ok_or"
                            | "ok_or_else"
                            | "unwrap_or"
                            | "unwrap_or_else"
                            | "unwrap_or_default"
                            | "__rust_identity_some"
                            | "prepare"
                            | "prepare_cached"
                            | "transaction"
                            | "unchecked_transaction"
                            | "transaction_with_behavior"
                            | "query"
                            | "parse"
                            | "parse_with"
                            | "child"
                            | "named_child"
                            | "child_by_field_name"
                            | "parent"
                            | "next_sibling"
                            | "prev_sibling"
                            | "next_named_sibling"
                            | "prev_named_sibling"
                            | "children"
                            | "named_children"
                            | "context"
                            | "with_context"
                            | "rposition"
                            | "then"
                            | "then_some"
                            | "then_with"
                            | "partition"
                            | "try_fold"
                            | "try_for_each"
                            | "find_map"
                            | "file_stem"
                            | "metadata"
                            | "symlink_metadata"
                            | "open"
                            | "create"
                            | "append"
                            | "truncate"
                            | "create_new"
                            | "mode"
                            | "custom_flags"
                            | "cmp"
                            | "partial_cmp"
                            | "checked_add"
                            | "checked_sub"
                            | "checked_mul"
                            | "checked_div"
                            | "checked_rem"
                            | "saturating_add"
                            | "saturating_sub"
                            | "as_file"
                            | "permissions"
                            | "is_ok_and"
                            | "is_err_and"
                            | "is_some"
                            | "is_none"
                            | "is_ok"
                            | "is_err"
                            | "is_empty"
                            | "is_some_and"
                            | "is_none_or"
                            | "next_if"
                            | "next_if_eq"
                    ) {
                        let inner = value_depth(receiver, source, depth + 1, shadowed_macros);
                        if matches!(inner, ValueExpr::Unknown) {
                            return ValueExpr::Unknown;
                        }
                        return ValueExpr::Project {
                            value: Box::new(inner),
                            member: member_name.to_owned(),
                        };
                    }
                }
            }
            if let Some(callee) = callee_name(function, source) {
                if matches!(
                    callee.as_ref(),
                    "Some"
                        | "std::option::Option::Some"
                        | "core::option::Option::Some"
                        | "Ok"
                        | "std::result::Result::Ok"
                        | "core::result::Result::Ok"
                ) {
                    if let Some(arg) = node
                        .child_by_field_name("arguments")
                        .filter(|args| args.named_child_count() == 1)
                        .and_then(|args| args.named_child(0))
                    {
                        let inner = value_depth(arg, source, depth + 1, shadowed_macros);
                        if !matches!(inner, ValueExpr::Unknown) {
                            let member = if callee.ends_with("Some") {
                                "__rust_wrap_some"
                            } else {
                                "__rust_wrap_ok"
                            };
                            return ValueExpr::Project {
                                value: Box::new(inner),
                                member: member.to_owned(),
                            };
                        }
                    }
                }
                let call = ValueExpr::Call { callee: callee.to_string(), site: None };
                let is_deserializer = callee.ends_with("from_str")
                    || callee.ends_with("from_slice")
                    || callee.ends_with("from_reader")
                    || callee.ends_with("from_value")
                    || callee.contains("yaml")
                    || callee.contains("json")
                    || callee.contains("serde");
                return match generic_result {
                    Some(type_expr) if is_deserializer => ValueExpr::Project {
                        value: Box::new(ValueExpr::BranchJoin {
                            alternatives: vec![call, ValueExpr::Annotated { type_expr }],
                        }),
                        member: "__rust_generic_call".to_owned(),
                    },
                    _ => call,
                };
            }
            if function.kind() == "field_expression" {
                if let (Some(receiver), Some(member)) = (
                    function.child_by_field_name("value"),
                    function.child_by_field_name("field"),
                ) {
                    let inner = value_depth(receiver, source, depth + 1, shadowed_macros);
                    if !matches!(inner, ValueExpr::Unknown) {
                        let pos = node.start_position();
                        return ValueExpr::Call {
                            callee: text(member, source).to_owned(),
                            site: Some(CallSiteEvidence {
                                position: SourcePosition {
                                    line: pos.row + 1,
                                    column: pos.column,
                                },
                                receiver: Some(Box::new(inner)),
                                arguments: Vec::new(),
                            }),
                        };
                    }
                }
            }
            ValueExpr::Unknown
        }
        "index_expression" => {
            let base = node.child_by_field_name("value").or_else(|| node.named_child(0));
            let index = node.child_by_field_name("index").or_else(|| node.named_child(1));
            let member = match index.map(|idx| idx.kind()) {
                Some("range_expression") => "__rust_range_slice",
                Some("string_literal" | "raw_string_literal") | None => "__rust_index",
                Some(_) => "__rust_elem_index",
            };
            base.map_or(ValueExpr::Unknown, |base| ValueExpr::Project {
                value: Box::new(value_depth(base, source, depth + 1, shadowed_macros)),
                member: member.to_owned(),
            })
        }
        "field_expression" => {
            let Some(receiver) = node.child_by_field_name("value") else {
                return ValueExpr::Unknown;
            };
            let Some(member) = node.child_by_field_name("field") else {
                return ValueExpr::Unknown;
            };
            ValueExpr::Project {
                value: Box::new(value_depth(receiver, source, depth + 1, shadowed_macros)),
                member: format!("__rust_field:{}", text(member, source)),
            }
        }
        "identifier" | "self" => ValueExpr::Alias {
            name: text(node, source).to_owned(),
        },
        "scoped_identifier" => ValueExpr::Alias {
            name: text(node, source).to_owned(),
        },
        "match_expression" => {
            let scrutinee = node.child_by_field_name("value");
            if let Some(scrutinee) = scrutinee {
                let inner = value_depth(scrutinee, source, depth + 1, shadowed_macros);
                if !matches!(inner, ValueExpr::Unknown) {
                    let block = node.child_by_field_name("body").unwrap_or(node);
                    let mut cursor = block.walk();
                    let has_unwrap_arm = block.named_children(&mut cursor).any(|child| {
                        if child.kind() == "match_arm" {
                            let pat_opt = child.child_by_field_name("pattern");
                            let arm_val_opt = child
                                .child_by_field_name("value")
                                .or_else(|| child.named_child(1));
                            if let (Some(mut pat), Some(arm_val)) = (pat_opt, arm_val_opt) {
                                if pat.kind() == "match_pattern" {
                                    if let Some(inner_pat) = pat.named_child(0) {
                                        pat = inner_pat;
                                    }
                                }
                                if pat.kind() == "tuple_struct_pattern" {
                                    let type_name = pat
                                        .child_by_field_name("type")
                                        .map(|t| text(t, source).trim().to_owned());
                                    if matches!(type_name.as_deref(), Some("Ok" | "Some")) {
                                        let pat_id = pat
                                            .named_children(&mut pat.walk())
                                            .find(|c| pat.child_by_field_name("type") != Some(*c))
                                            .and_then(pattern_identifier)
                                            .map(|id| text(id, source));
                                        let val_text = text(arm_val, source).trim();
                                        if let Some(p_id) = pat_id {
                                            if p_id == val_text {
                                                return true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        false
                    });
                    if has_unwrap_arm {
                        return ValueExpr::Project {
                            value: Box::new(inner),
                            member: "unwrap".to_string(),
                        };
                    }
                }
            }
            ValueExpr::Unknown
        }
        "if_expression" => {
            let then_expr = node
                .child_by_field_name("consequence")
                .and_then(block_tail_expr);
            let else_expr = node
                .child_by_field_name("alternative")
                .and_then(|alt| {
                    if alt.kind() == "else_clause" {
                        alt.named_child(0)
                    } else {
                        Some(alt)
                    }
                })
                .and_then(block_tail_expr);
            match (then_expr, else_expr) {
                (Some(then_node), Some(else_node)) => {
                    let diverges = |n: Node<'_>| {
                        matches!(
                            n.kind(),
                            "return_expression" | "break_expression" | "continue_expression"
                        ) || (n.kind() == "macro_invocation"
                            && n.child_by_field_name("macro").is_some_and(|m| {
                                matches!(
                                    text(m, source).trim(),
                                    "bail" | "panic" | "unreachable" | "todo" | "unimplemented"
                                )
                            }))
                    };
                    if diverges(else_node) {
                        return value_depth(then_node, source, depth + 1, shadowed_macros);
                    }
                    if diverges(then_node) {
                        return value_depth(else_node, source, depth + 1, shadowed_macros);
                    }
                    let then_val = value_depth(then_node, source, depth + 1, shadowed_macros);
                    let else_val = value_depth(else_node, source, depth + 1, shadowed_macros);
                    if matches!(then_val, ValueExpr::Unknown) || matches!(else_val, ValueExpr::Unknown) {
                        ValueExpr::Unknown
                    } else if then_val == else_val {
                        then_val
                    } else {
                        ValueExpr::Project {
                            value: Box::new(ValueExpr::BranchJoin {
                                alternatives: vec![then_val, else_val],
                            }),
                            member: "__rust_if_join".to_owned(),
                        }
                    }
                }
                _ => ValueExpr::Unknown,
            }
        }
        "binary_expression" => {
            if node
                .child_by_field_name("operator")
                .is_some_and(|op| {
                    matches!(
                        text(op, source).trim(),
                        "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||"
                    )
                })
            {
                ValueExpr::Annotated {
                    type_expr: TypeExpr::Named {
                        name: "bool".to_owned(),
                    },
                }
            } else {
                ValueExpr::Unknown
            }
        }
        _ => ValueExpr::Unknown,
    }
}

fn vec_token_expr(node: Node<'_>, source: &str) -> Option<ValueExpr> {
    match node.kind() {
        "identifier" => Some(ValueExpr::Alias {
            name: text(node, source).to_owned(),
        }),
        "string_literal" | "raw_string_literal" => Some(ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "str".to_owned(),
            },
        }),
        "boolean_literal" => Some(ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "bool".to_owned(),
            },
        }),
        "char_literal" => Some(ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "char".to_owned(),
            },
        }),
        "token_tree" if text(node, source).trim_start().starts_with('(') => {
            let mut items = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                items.push(vec_token_expr(child, source)?);
            }
            (!items.is_empty()).then_some(ValueExpr::Project {
                value: Box::new(ValueExpr::BranchJoin { alternatives: items }),
                member: "__rust_tuple".to_owned(),
            })
        }
        _ => None,
    }
}

fn vec_chain_expr(children: &[Node<'_>], source: &str) -> Option<ValueExpr> {
    let mut end = children.len();
    for (idx, child) in children.iter().enumerate() {
        if matches!(text(*child, source).trim(), "," | ";") {
            end = idx;
            break;
        }
    }
    let slice = &children[..end];
    if slice.is_empty() {
        return None;
    }
    if slice.len() == 1 {
        return vec_token_expr(slice[0], source);
    }
    let (mut expr, mut pos) = if slice[0].kind() == "scoped_identifier"
        && slice.get(1).is_some_and(|n| n.kind() == "token_tree")
        && text(slice[1], source).trim_start().starts_with('(')
    {
        (
            ValueExpr::Call {
                callee: text(slice[0], source).trim().to_owned(),
                site: None,
            },
            2usize,
        )
    } else if matches!(slice[0].kind(), "identifier" | "self")
        && slice.get(1).is_some_and(|n| text(*n, source).trim() == "::")
        && slice.get(2).is_some_and(|n| n.kind() == "identifier")
        && slice.get(3).is_some_and(|n| n.kind() == "token_tree")
        && text(slice[3], source).trim_start().starts_with('(')
    {
        let callee = format!("{}::{}", text(slice[0], source).trim(), text(slice[2], source).trim());
        (ValueExpr::Call { callee, site: None }, 4usize)
    } else {
        (vec_token_expr(slice[0], source)?, 1usize)
    };
    while pos < slice.len() {
        if text(slice[pos], source).trim() != "." {
            return None;
        }
        let member_node = slice.get(pos + 1).filter(|n| n.kind() == "identifier")?;
        let member = text(*member_node, source).trim();
        let called = slice
            .get(pos + 2)
            .is_some_and(|n| n.kind() == "token_tree" && text(*n, source).trim_start().starts_with('('));
        expr = if called {
            pos += 3;
            ValueExpr::Call {
                callee: member.to_owned(),
                site: Some(CallSiteEvidence {
                    position: SourcePosition {
                        line: member_node.start_position().row + 1,
                        column: member_node.start_position().column,
                    },
                    receiver: Some(Box::new(expr)),
                    arguments: Vec::new(),
                }),
            }
        } else {
            pos += 2;
            ValueExpr::Project {
                value: Box::new(expr),
                member: format!("__rust_field:{member}"),
            }
        };
    }
    Some(expr)
}

fn vec_macro_expr(node: Node<'_>, source: &str) -> Option<ValueExpr> {
    let mut cursor = node.walk();
    let tree = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let mut inner_cursor = tree.walk();
    let children: Vec<_> = tree.children(&mut inner_cursor).filter(|c| !matches!(c.kind(), "[" | "]")).collect();
    if children.is_empty() {
        return None;
    }
    let elem = vec_chain_expr(&children, source)?;
    Some(ValueExpr::Project {
        value: Box::new(elem),
        member: "__rust_vec_of".to_owned(),
    })
}

fn block_tail_expr<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if node.kind() != "block" {
        return Some(node);
    }
    let last = node.named_child(node.named_child_count().checked_sub(1)?)?;
    if last.kind() == "expression_statement" {
        return last
            .named_child(0)
            .filter(|inner| {
                matches!(
                    inner.kind(),
                    "return_expression"
                        | "break_expression"
                        | "continue_expression"
                        | "macro_invocation"
                )
            });
    }
    (last.kind() != "let_declaration").then_some(last)
}

fn value_expr_within_depth(expr: &ValueExpr, depth: usize) -> bool {
    if depth >= 8 {
        return false;
    }
    match expr {
        ValueExpr::Alias { name } | ValueExpr::Construct { callee: name } => {
            !name.is_empty() && name.len() <= 4_096
        }
        ValueExpr::Field { receiver, member } => {
            !receiver.is_empty()
                && receiver.len() <= 4_096
                && !member.is_empty()
                && member.len() <= 4_096
        }
        ValueExpr::Call { callee, site } => {
            !callee.is_empty()
                && callee.len() <= 4_096
                && expr.valid_call_sites(depth)
                && site
                    .as_ref()
                    .and_then(|s| s.receiver.as_deref())
                    .is_none_or(|receiver| value_expr_within_depth(receiver, depth + 1))
        }
        ValueExpr::Await { value: inner } => value_expr_within_depth(inner, depth + 1),
        ValueExpr::Scoped {
            value: inner,
            body_end,
        } => body_end.line > 0 && value_expr_within_depth(inner, depth + 1),
        ValueExpr::Project {
            value: inner,
            member,
        } => {
            !member.is_empty()
                && member.len() <= 4_096
                && value_expr_within_depth(inner, depth + 1)
        }
        ValueExpr::BranchJoin { alternatives } => {
            !alternatives.is_empty()
                && alternatives
                    .iter()
                    .all(|alt| value_expr_within_depth(alt, depth + 1))
        }
        _ => true,
    }
}

fn type_expr_has_unknown(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Unknown => true,
        TypeExpr::Named { .. } => false,
        TypeExpr::Applied { args, .. } => args.iter().any(type_expr_has_unknown),
    }
}

fn unwrap_option_result_pattern(
    pattern: Node<'_>,
    val_node: Node<'_>,
    source: &str,
    shadows: &HashSet<&'static str>,
) -> Option<ValueExpr> {
    if pattern.kind() != "tuple_struct_pattern" {
        return None;
    }
    let type_name = pattern
        .child_by_field_name("type")
        .map(|t| text(t, source).trim().to_owned());
    let short = type_name
        .as_deref()
        .and_then(|name| name.rsplit("::").next());
    if !matches!(short, Some("Ok" | "Some" | "Err" | "Vacant" | "Occupied")) {
        return None;
    }
    let base_val = value_depth(val_node, source, 0, shadows);
    Some(if matches!(base_val, ValueExpr::Unknown) {
        ValueExpr::Unknown
    } else if matches!(short, Some("Vacant" | "Occupied")) {
        base_val
    } else if short == Some("Err") {
        ValueExpr::Project {
            value: Box::new(base_val),
            member: "__rust_err".to_string(),
        }
    } else {
        ValueExpr::Project {
            value: Box::new(base_val),
            member: "unwrap".to_string(),
        }
    })
}

fn strip_pattern_wrappers(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "reference_pattern" | "ref_pattern" | "mut_pattern"
    ) {
        let Some(inner) = node.child_by_field_name("pattern").or_else(|| {
            node.named_child(0)
                .filter(|child| child.kind() != "mutable_specifier")
                .or_else(|| node.named_child(1))
        }) else {
            break;
        };
        node = inner;
    }
    node
}

fn extract_closure_bindings(param: Node<'_>, source: &str) -> Option<Vec<(String, Option<usize>)>> {
    let param = if param.kind() == "parameter" {
        param.child_by_field_name("pattern")?
    } else {
        param
    };
    let inner = strip_pattern_wrappers(param);
    if inner.kind() == "identifier" {
        let name = text(inner, source).trim();
        return Some(if name == "_" {
            Vec::new()
        } else {
            vec![(name.to_owned(), None)]
        });
    }
    if inner.kind() == "tuple_pattern" {
        let mut out = Vec::new();
        let mut cursor = inner.walk();
        for (idx, elem) in inner.named_children(&mut cursor).enumerate() {
            if let Some(id_node) = pattern_identifier(elem) {
                let name = text(id_node, source).trim();
                if name != "_" {
                    out.push((name.to_owned(), Some(idx)));
                }
            }
        }
        return Some(out);
    }
    None
}

fn substitute_closure_param(
    expr: ValueExpr,
    item_expr: &ValueExpr,
    bindings: &[(String, Option<usize>)],
) -> ValueExpr {
    match expr {
        ValueExpr::Alias { ref name } => bindings
            .iter()
            .find(|(bound, _)| bound == name)
            .map(|(_, field_idx)| match field_idx {
                Some(idx) => ValueExpr::Project {
                    value: Box::new(item_expr.clone()),
                    member: format!("__rust_field:{idx}"),
                },
                None => item_expr.clone(),
            })
            .unwrap_or(expr),
        ValueExpr::Project { value, member } => ValueExpr::Project {
            value: Box::new(substitute_closure_param(*value, item_expr, bindings)),
            member,
        },
        ValueExpr::Call { callee, site } => ValueExpr::Call {
            callee,
            site: site.map(|mut site| {
                site.receiver = site
                    .receiver
                    .map(|recv| Box::new(substitute_closure_param(*recv, item_expr, bindings)));
                site
            }),
        },
        ValueExpr::BranchJoin { alternatives } => ValueExpr::BranchJoin {
            alternatives: alternatives
                .into_iter()
                .map(|alt| substitute_closure_param(alt, item_expr, bindings))
                .collect(),
        },
        ValueExpr::Scoped { value, body_end } => ValueExpr::Scoped {
            value: Box::new(substitute_closure_param(*value, item_expr, bindings)),
            body_end,
        },
        other => other,
    }
}

fn closure_projection_expr(
    node: Node<'_>,
    member_name: &str,
    receiver: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> Option<ValueExpr> {
    let tag = match member_name {
        "map" => "__rust_closure_map",
        "and_then" => "__rust_closure_and_then",
        "filter_map" => "__rust_closure_filter_map",
        "flat_map" => "__rust_closure_flat_map",
        "map_or" | "map_or_else" => "__rust_closure_map_or",
        _ => return None,
    };
    let args = node.child_by_field_name("arguments")?;
    let (default_node, closure_node) = match member_name {
        "map" | "and_then" | "filter_map" | "flat_map" if args.named_child_count() == 1 => {
            (None, args.named_child(0)?)
        }
        "map_or" if args.named_child_count() == 2 => (args.named_child(0), args.named_child(1)?),
        "map_or_else" if args.named_child_count() == 2 => {
            let def = args
                .named_child(0)
                .filter(|n| n.kind() == "closure_expression")
                .and_then(|c| c.child_by_field_name("body"));
            (def, args.named_child(1)?)
        }
        _ => return None,
    };
    let recv_val = value_depth(receiver, source, depth + 1, shadowed_macros);
    if matches!(recv_val, ValueExpr::Unknown) {
        return None;
    }
    let item_expr = ValueExpr::Project {
        value: Box::new(recv_val.clone()),
        member: "__rust_iter_item".to_owned(),
    };
    let (body_sub, default_val, default_ok) = if closure_node.kind() == "closure_expression" {
        let params = closure_node.child_by_field_name("parameters")?;
        if params.named_child_count() != 1 {
            return None;
        }
        let bindings = extract_closure_bindings(params.named_child(0)?, source)?;
        let body_node = closure_node.child_by_field_name("body")?;
        if !matches!(member_name, "map_or" | "map_or_else")
            && body_node.kind() == "identifier"
            && bindings
                .iter()
                .any(|(bound, field)| field.is_none() && bound == text(body_node, source).trim())
        {
            return None;
        }
        let body_val = value_depth(body_node, source, depth + 2, shadowed_macros);
        let body_sub = substitute_closure_param(body_val, &item_expr, &bindings);
        let default_val = default_node.map(|d| value_depth(d, source, depth + 2, shadowed_macros));
        let body_ok =
            !matches!(body_sub, ValueExpr::Unknown) && value_expr_within_depth(&body_sub, 2);
        let default_ok = default_val
            .as_ref()
            .is_some_and(|v| !matches!(v, ValueExpr::Unknown) && value_expr_within_depth(v, 2));
        if matches!(member_name, "map_or" | "map_or_else") {
            if !body_ok && !default_ok {
                return None;
            }
        } else if !body_ok {
            return None;
        }
        (
            if body_ok { body_sub } else { ValueExpr::Unknown },
            default_val,
            default_ok,
        )
    } else if matches!(member_name, "map" | "and_then" | "filter_map")
        && matches!(closure_node.kind(), "scoped_identifier" | "identifier")
    {
        let full = text(closure_node, source).trim();
        let projected_member = match full {
            "Some" | "std::option::Option::Some" | "core::option::Option::Some"
                if member_name == "map" =>
            {
                Some("__rust_wrap_some")
            }
            "Ok" | "std::result::Result::Ok" | "core::result::Result::Ok"
                if member_name == "map" =>
            {
                Some("__rust_wrap_ok")
            }
            "str::trim" => Some("trim"),
            "str::trim_start" => Some("trim_start"),
            "str::trim_end" => Some("trim_end"),
            "str::to_owned" | "ToOwned::to_owned" | "std::borrow::ToOwned::to_owned" => {
                Some("to_owned")
            }
            "str::to_string" | "ToString::to_string" | "std::string::ToString::to_string" => {
                Some("to_string")
            }
            "String::as_str" | "String::as_ref" => Some("as_str"),
            "Option::as_deref" | "std::option::Option::as_deref" => Some("as_deref"),
            "Option::as_ref" | "std::option::Option::as_ref" => Some("as_ref"),
            "Option::cloned" | "std::option::Option::cloned" => Some("cloned"),
            "Option::copied" | "std::option::Option::copied" => Some("copied"),
            "Result::ok" | "std::result::Result::ok" => Some("ok"),
            "Result::err" | "std::result::Result::err" => Some("err"),
            "Path::to_path_buf" | "std::path::Path::to_path_buf" => Some("to_path_buf"),
            "Path::to_str" | "std::path::Path::to_str" | "OsStr::to_str"
            | "std::ffi::OsStr::to_str" => Some("to_str"),
            "OsStr::to_string_lossy" | "std::ffi::OsStr::to_string_lossy" => {
                Some("to_string_lossy")
            }
            _ => None,
        };
        let sub = if let Some(method) = projected_member {
            ValueExpr::Project {
                value: Box::new(item_expr),
                member: method.to_owned(),
            }
        } else {
            ValueExpr::Call {
                callee: full.to_owned(),
                site: None,
            }
        };
        (sub, None, false)
    } else {
        return None;
    };
    if matches!(member_name, "filter_map" | "flat_map" | "and_then") {
        if matches!(body_sub, ValueExpr::Unknown) || !value_expr_within_depth(&body_sub, 1) {
            return None;
        }
        return Some(ValueExpr::Project {
            value: Box::new(body_sub),
            member: tag.to_owned(),
        });
    }
    let mut alternatives = vec![recv_val, body_sub];
    if let Some(default_expr) = default_val {
        alternatives.push(if default_ok {
            default_expr
        } else {
            ValueExpr::Unknown
        });
    }
    Some(ValueExpr::Project {
        value: Box::new(ValueExpr::Project {
            value: Box::new(ValueExpr::BranchJoin { alternatives }),
            member: "__rust_tuple".to_owned(),
        }),
        member: tag.to_owned(),
    })
}

fn sanitize_binding_value(value: ValueExpr) -> ValueExpr {
    if value_expr_within_depth(&value, 0) {
        value
    } else {
        ValueExpr::Unknown
    }
}

pub(crate) struct Collector<'source, 'tree> {
    root: Node<'tree>,
    body: Option<Node<'tree>>,
    source: &'source str,
    file_facts: std::sync::Arc<FileMacroFacts>,
    core: ValueFlowCollectorCore,
    generic_names: Option<HashSet<&'source str>>,
    block_shadows: Vec<HashSet<&'static str>>,
    closure_depth: usize,
}

fn extract_function_return_tuple_elements<'a>(func_item: Node<'a>) -> Option<Vec<Node<'a>>> {
    let ret_type = func_item.child_by_field_name("return_type")?;
    let target_tuple = if ret_type.kind() == "tuple_type" {
        Some(ret_type)
    } else {
        (0..ret_type.named_child_count()).find_map(|i| {
            let c = ret_type.named_child(i)?;
            (c.kind() == "tuple_type").then_some(c)
        })
    };
    let tup = target_tuple?;
    Some(
        (0..tup.named_child_count())
            .filter_map(|i| tup.named_child(i))
            .collect(),
    )
}

fn resolve_function_return_tuple<'a>(
    mut curr: Node<'a>,
    fn_name: &str,
    source: &str,
    bindings: &[BindingFact],
) -> Option<Vec<Node<'a>>> {
    if bindings.iter().any(|b| b.name == fn_name) {
        return None;
    }
    while let Some(parent) = curr.parent() {
        let count = parent.named_child_count();
        for i in 0..count {
            if let Some(child) = parent.named_child(i) {
                if child.kind() == "function_item" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if text(name_node, source).trim() == fn_name {
                            return extract_function_return_tuple_elements(child);
                        }
                    }
                }
            }
        }
        curr = parent;
    }
    None
}

fn generic_param_serializer_bound(fn_node: Node<'_>, param_name: &str, source: &str) -> bool {
    let has_serializer = |node: Node<'_>| {
        let find_bounds = |bounds: Node<'_>| {
            let mut cursor = bounds.walk();
            let found = bounds.named_children(&mut cursor).any(|bound| {
                let txt = text(bound, source).trim();
                txt == "Serializer" || txt.ends_with("::Serializer")
            });
            found
        };
        if let Some(bounds) = node.child_by_field_name("bounds") {
            return find_bounds(bounds);
        }
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "trait_bounds" && find_bounds(child));
        found
    };
    if let Some(params) = fn_node.child_by_field_name("type_parameters") {
        let mut cursor = params.walk();
        for mut param in params.named_children(&mut cursor) {
            if param.kind() == "type_parameter" {
                if let Some(inner) = param
                    .child_by_field_name("name")
                    .or_else(|| param.named_child(0))
                    .filter(|n| n.kind() == "constrained_type_parameter")
                {
                    param = inner;
                }
            }
            let name = param
                .child_by_field_name("left")
                .or_else(|| param.child_by_field_name("name"))
                .or_else(|| {
                    let mut c = param.walk();
                    let found = param
                        .named_children(&mut c)
                        .find(|n| n.kind() == "type_identifier");
                    found
                })
                .map(|n| text(n, source).trim());
            if name == Some(param_name) && has_serializer(param) {
                return true;
            }
        }
    }
    let mut cursor = fn_node.walk();
    for child in fn_node.named_children(&mut cursor) {
        if child.kind() != "where_clause" {
            continue;
        }
        let mut wc = child.walk();
        for pred in child.named_children(&mut wc) {
            let left = pred.child_by_field_name("left").or_else(|| pred.named_child(0));
            if left.is_some_and(|l| text(l, source).trim() == param_name) && has_serializer(pred) {
                return true;
            }
        }
    }
    false
}

impl<'source, 'tree> Collector<'source, 'tree> {
    pub(crate) fn new(node: Node<'tree>, source: &'source str) -> Self {
        let file_facts = file_macro_facts(node, source);
        let root_shadows = collect_ancestor_shadows(node, source, &file_facts);
        let block_shadows = vec![root_shadows];
        let mut core = ValueFlowCollectorCore::with_scope_capacity(16);
        let mut scope_ids = Vec::new();
        let mut anc = node.parent();
        while let Some(curr) = anc {
            if curr.kind() == "declaration_list" || curr.kind() == "source_file" {
                scope_ids.push(curr.id());
            }
            anc = curr.parent();
        }
        for (scope_id, name, _pos, ty_expr) in &file_facts.thread_locals {
            if scope_ids.contains(scope_id) {
                core.push_binding(
                    name.clone(),
                    SourcePosition { line: 1, column: 0 },
                    ValueExpr::Annotated {
                        type_expr: TypeExpr::Applied {
                            base: "std::thread::LocalKey".to_owned(),
                            args: vec![ty_expr.clone()],
                        },
                    },
                    false,
                );
            }
        }
        let mut generics = None;
        let mut resolve = |ty: Node<'_>| {
            let names = generics.get_or_insert_with(|| generic_names(node, source));
            type_expr_inner(ty, source, names, 0)
        };
        if matches!(node.kind(), "type_item" | "const_item" | "static_item") {
            core.set_alias_type_if_none(node.child_by_field_name("type").map(&mut resolve));
        }
        if matches!(node.kind(), "function_item" | "function_signature_item") {
            core.set_return_type_if_none(node.child_by_field_name("return_type").map(|ty| {
                if is_async(node, source) {
                    TypeExpr::Unknown
                } else {
                    resolve(ty)
                }
            }));
            if let Some(parameters) = node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    if parameter.kind() == "self_parameter" {
                        let syntax = text(parameter, source).trim();
                        let mut type_expr = TypeExpr::Named { name: "Self".to_owned() };
                        if syntax.starts_with('&') {
                            type_expr = TypeExpr::Applied {
                                base: if syntax.split_whitespace().any(|part| matches!(part, "mut" | "&mut")) { "&mut" } else { "&" }.to_owned(),
                                args: vec![type_expr],
                            };
                        }
                        let position = parameter.end_position();
                        core.push_binding(
                            "self".to_owned(),
                            SourcePosition { line: position.row + 1, column: position.column },
                            ValueExpr::Annotated { type_expr },
                            false,
                        );
                        continue;
                    }
                    let (Some(pattern), Some(annotation)) = (
                        parameter.child_by_field_name("pattern"),
                        parameter.child_by_field_name("type"),
                    ) else {
                        continue;
                    };
                    let resolved = match resolve(annotation) {
                        TypeExpr::Unknown if annotation.kind() == "type_identifier"
                            && generic_param_serializer_bound(
                                node,
                                text(annotation, source).trim(),
                                source,
                            ) =>
                        {
                            TypeExpr::Named {
                                name: "serde::Serializer".to_owned(),
                            }
                        }
                        other => other,
                    };
                    let mut nominal = annotation;
                    while nominal.kind() == "reference_type" {
                        let Some(inner) = nominal.child_by_field_name("type") else {
                            break;
                        };
                        nominal = inner;
                    }
                    if nominal.kind() == "primitive_type" && text(nominal, source) != "str" {
                        continue;
                    }
                    match &resolved {
                        TypeExpr::Applied { .. } => {}
                        TypeExpr::Named { .. } => {}
                        TypeExpr::Unknown => continue,
                    }
                    let Some(pattern) = pattern_identifier(pattern) else {
                        continue;
                    };
                    let position = parameter.end_position();
                    core.push_binding(
                        text(pattern, source).to_owned(),
                        SourcePosition {
                            line: position.row + 1,
                            column: position.column,
                        },
                        ValueExpr::Annotated {
                            type_expr: resolved,
                        },
                        false,
                    );
                }
            }
        }
        let mut out_body = node.child_by_field_name("body");
        if let Some(body) = out_body {
            if node.kind() == "struct_item" {
                let mut push_field = |name, at: Node<'_>, ty| {
                    let position = at.start_position();
                    core.push_field(
                        name,
                        SourcePosition {
                            line: position.row + 1,
                            column: position.column,
                        },
                        ValueExpr::Annotated {
                            type_expr: resolve(ty),
                        },
                        false,
                    );
                };
                let mut cursor = body.walk();
                if body.kind() == "ordered_field_declaration_list" {
                    for (ordinal, ty) in body.children_by_field_name("type", &mut cursor).enumerate()
                    {
                        push_field(ordinal.to_string(), ty, ty);
                    }
                } else {
                    for field in body.named_children(&mut cursor) {
                        if field.kind() != "field_declaration" {
                            continue;
                        }
                        if let (Some(name), Some(ty)) = (
                            field.child_by_field_name("name"),
                            field.child_by_field_name("type"),
                        ) {
                            push_field(text(name, source).to_owned(), field, ty);
                        }
                    }
                }
                out_body = None;
            }
        }
        Self {
            root: node,
            body: out_body,
            source,
            file_facts,
            core,
            generic_names: generics,
            block_shadows,
            closure_depth: 0,
        }
    }

    fn current_shadows(&self) -> HashSet<&'static str> {
        let mut shadows = HashSet::new();
        for scope in &self.block_shadows {
            shadows.extend(scope);
        }
        shadows
    }

    fn push_binding_checked(
        &mut self,
        name: String,
        position: SourcePosition,
        value: ValueExpr,
        conditional: bool,
    ) {
        if name.is_empty() || name.len() > 4_096 || position.line == 0 {
            return;
        }
        self.core
            .push_binding(name, position, sanitize_binding_value(value), conditional);
    }

    fn push_tuple_struct_bindings(
        &mut self,
        pattern: Node<'_>,
        position: SourcePosition,
        unwrapped_val: ValueExpr,
        body_end: Option<SourcePosition>,
        conditional: bool,
    ) {
        let wrap_scoped = |val: ValueExpr| -> ValueExpr {
            if let Some(end) = body_end {
                if matches!(val, ValueExpr::Unknown) {
                    ValueExpr::Unknown
                } else {
                    ValueExpr::Scoped {
                        value: Box::new(val),
                        body_end: end,
                    }
                }
            } else {
                val
            }
        };
        let mut cursor = pattern.walk();
        for child in pattern.named_children(&mut cursor) {
            if pattern.child_by_field_name("type") == Some(child) {
                continue;
            }
            let mut inner_child = child;
            while matches!(
                inner_child.kind(),
                "reference_pattern" | "ref_pattern" | "mut_pattern"
            ) {
                let Some(next) = inner_child.child_by_field_name("pattern").or_else(|| {
                    inner_child
                        .named_child(0)
                        .filter(|c| c.kind() != "mutable_specifier")
                        .or_else(|| inner_child.named_child(1))
                }) else {
                    break;
                };
                inner_child = next;
            }
            if let Some(id_node) = pattern_identifier(inner_child) {
                let name = text(id_node, self.source).to_owned();
                self.push_binding_checked(
                    name,
                    position,
                    wrap_scoped(unwrapped_val.clone()),
                    conditional,
                );
            } else if matches!(inner_child.kind(), "tuple_pattern" | "slice_pattern") {
                let is_tuple = inner_child.kind() == "tuple_pattern";
                let mut inner = inner_child.walk();
                for (idx, elem) in inner_child.named_children(&mut inner).enumerate() {
                    if let Some(id_node) = pattern_identifier(elem) {
                        let name = text(id_node, self.source).to_owned();
                        let elem_val = if matches!(unwrapped_val, ValueExpr::Unknown) {
                            ValueExpr::Unknown
                        } else {
                            ValueExpr::Project {
                                value: Box::new(unwrapped_val.clone()),
                                member: if is_tuple {
                                    format!("__rust_field:{idx}")
                                } else {
                                    "__rust_elem_index".to_owned()
                                },
                            }
                        };
                        self.push_binding_checked(
                            name,
                            position,
                            wrap_scoped(elem_val),
                            conditional,
                        );
                    }
                }
            } else if inner_child.kind() == "tuple_struct_pattern" {
                let inner_val = if matches!(unwrapped_val, ValueExpr::Unknown) {
                    ValueExpr::Unknown
                } else {
                    ValueExpr::Project {
                        value: Box::new(unwrapped_val.clone()),
                        member: "unwrap".to_owned(),
                    }
                };
                self.push_tuple_struct_bindings(
                    inner_child,
                    position,
                    inner_val,
                    body_end,
                    conditional,
                );
            }
        }
    }

    pub(crate) fn enter(&mut self, current: Node<'tree>) {
        let source = self.source;
        let body = self.body;
        let parent = self.core.scope_state().current_or_default();
        let active = body == Some(current)
            || (parent.is_active()
                && !matches!(
                    current.kind(),
                    "function_item" | "impl_item" | "trait_item" | "mod_item"
                ));
        let branch_conditional = parent.is_branch_conditional()
            || (body != Some(current)
                && matches!(
                    current.kind(),
                    "if_expression"
                        | "match_expression"
                        | "while_expression"
                        | "for_expression"
                        | "loop_expression"
                ));
        self.core.scope_state_mut().push(
            ScopeFrameFlags::new()
                .with_active(active)
                .with_branch_conditional(branch_conditional)
                .with_collect_facts(active),
        );
        if !active {
            return;
        }
        let is_conditional = self.core.scope_state().is_conditional();
        if current.kind() == "macro_invocation" {
            if let Some(m_name) = current.child_by_field_name("macro") {
                let macro_text = text(m_name, source).trim();
                if matches!(macro_text, "thread_local" | "std::thread_local") {
                    if let Some(tree) = current
                        .named_children(&mut current.walk())
                        .find(|c| c.kind() == "token_tree")
                    {
                        for (name, pos, ty_expr) in parse_thread_local_statics(tree, source) {
                            self.push_binding_checked(
                                name,
                                pos,
                                ValueExpr::Annotated {
                                    type_expr: TypeExpr::Applied {
                                        base: "std::thread::LocalKey".to_owned(),
                                        args: vec![ty_expr],
                                    },
                                },
                                self.core.scope_state().is_conditional(),
                            );
                        }
                    }
                }
            }
        }
        if current.kind() == "block" {
            let block_set = self
                .file_facts
                .blocks
                .get(&current.id())
                .map(|b| b.imports.clone())
                .unwrap_or_default();
            self.block_shadows.push(block_set);
        }
        if current.kind() == "macro_definition" {
            if let Some(m_name) = current.child_by_field_name("name") {
                let name = text(m_name, source).trim();
                if name == "vec" || name == "format" {
                    if let Some(last) = self.block_shadows.last_mut() {
                        if name == "vec" {
                            last.insert("vec");
                        } else {
                            last.insert("format");
                        }
                    }
                }
            }
        }
        if current.kind() == "for_expression" {
            if let (Some(pattern), Some(iterable), Some(body)) = (
                current.child_by_field_name("pattern"),
                current.child_by_field_name("value"),
                current.child_by_field_name("body"),
            ) {
                let start = body.start_position();
                let end = body.end_position();
                let body_pos = SourcePosition {
                    line: start.row + 1,
                    column: start.column,
                };
                let body_end = SourcePosition {
                    line: end.row + 1,
                    column: end.column,
                };
                let iter_expr = value_depth(iterable, source, 0, &self.current_shadows());
                let iter_item = match iter_expr {
                    ValueExpr::Project { value, member }
                        if member == "__rust_closure_flat_map" =>
                    {
                        ValueExpr::Project {
                            value,
                            member: "__rust_flat_map_item".to_owned(),
                        }
                    }
                    ValueExpr::Project { ref member, .. }
                        if member == "__rust_closure_filter_map" =>
                    {
                        ValueExpr::Project {
                            value: Box::new(iter_expr),
                            member: "__rust_iter_item".to_owned(),
                        }
                    }
                    other => ValueExpr::Project {
                        value: Box::new(ValueExpr::Project {
                            value: Box::new(other),
                            member: "into_iter".to_owned(),
                        }),
                        member: "__rust_iter_item".to_owned(),
                    },
                };
                let unwrapped_pat = strip_pattern_wrappers(pattern);
                if let Some(id_node) = pattern_identifier(unwrapped_pat) {
                    self.push_binding_checked(
                        text(id_node, source).to_owned(),
                        body_pos,
                        ValueExpr::Scoped {
                            value: Box::new(iter_item),
                            body_end,
                        },
                        false,
                    );
                } else if unwrapped_pat.kind() == "tuple_pattern" {
                    let mut pat_cursor = unwrapped_pat.walk();
                    for (idx, pat_node) in unwrapped_pat.named_children(&mut pat_cursor).enumerate() {
                        if let Some(id_node) = pattern_identifier(pat_node) {
                            self.push_binding_checked(
                                text(id_node, source).to_owned(),
                                body_pos,
                                ValueExpr::Scoped {
                                    value: Box::new(ValueExpr::Project {
                                        value: Box::new(iter_item.clone()),
                                        member: format!("__rust_field:{idx}"),
                                    }),
                                    body_end,
                                },
                                false,
                            );
                        }
                    }
                }
            }
        }
        if current.kind() == "closure_expression" {
            self.closure_depth += 1;
            let body_end_pos = current.end_position();
            let body_end = SourcePosition {
                line: body_end_pos.row + 1,
                column: body_end_pos.column,
            };

            let mut inferred_param_types: Vec<Option<TypeExpr>> = Vec::new();
            let mut inferred_param_values: Vec<Option<ValueExpr>> = Vec::new();
            if let Some(parent) = current.parent() {
                if parent.kind() == "arguments" {
                    if let Some(call) = parent.parent() {
                        if call.kind() == "call_expression" {
                            if let Some(func) = call.child_by_field_name("function") {
                                let func = if func.kind() == "generic_function" {
                                    func.child_by_field_name("function").unwrap_or(func)
                                } else {
                                    func
                                };
                                if func.kind() == "field_expression" {
                                    let recv_opt = func.child_by_field_name("value");
                                    if let Some(field) = func.child_by_field_name("field") {
                                        let method_name = text(field, source);
                                        let recv_projection = |member: &str| -> Option<ValueExpr> {
                                            let recv = recv_opt?;
                                            let shadows = self.current_shadows();
                                            let recv_val = value_depth(recv, source, 0, &shadows);
                                            (!matches!(recv_val, ValueExpr::Unknown)).then(|| {
                                                ValueExpr::Project {
                                                    value: Box::new(recv_val),
                                                    member: member.to_string(),
                                                }
                                            })
                                        };
                                        let simple_proj = match method_name {
                                            "query_map"
                                            | "query_row"
                                            | "query_and_then"
                                            | "query_row_and_then" => Some("__rust_closure_row"),
                                            "map"
                                            | "filter"
                                            | "find"
                                            | "position"
                                            | "rposition"
                                            | "any"
                                            | "all"
                                            | "for_each"
                                            | "flat_map"
                                            | "filter_map"
                                            | "inspect"
                                            | "is_some_and"
                                            | "is_none_or"
                                            | "is_ok_and"
                                            | "is_err_and"
                                            | "and_then"
                                            | "find_map"
                                            | "try_for_each"
                                            | "skip_while"
                                            | "take_while"
                                            | "partition"
                                            | "max_by_key"
                                            | "min_by_key"
                                            | "sort_by_key" => Some("__rust_iter_item"),
                                            "map_or" | "map_or_else"
                                                if parent.named_child(1) == Some(current) =>
                                            {
                                                Some("__rust_iter_item")
                                            }
                                            "filter_entry" => Some("__rust_filter_entry_item"),
                                            "with" | "try_with" => Some("__rust_local_key"),
                                            "map_err"
                                            | "inspect_err"
                                            | "unwrap_or_else"
                                            | "ok_or_else"
                                            | "or_else" => Some("__rust_err"),
                                            _ => None,
                                        };
                                        if let Some(member) = simple_proj {
                                            if let Some(proj) = recv_projection(member) {
                                                inferred_param_values.push(Some(proj));
                                            }
                                        } else if method_name == "retain" {
                                            if let Some(item) = recv_projection("__rust_iter_item")
                                            {
                                                let param_count = current
                                                    .child_by_field_name("parameters")
                                                    .map_or(1, |p| p.named_child_count());
                                                if param_count >= 2 {
                                                    inferred_param_values.push(Some(
                                                        ValueExpr::Project {
                                                            value: Box::new(item.clone()),
                                                            member: "__rust_field:0".to_string(),
                                                        },
                                                    ));
                                                    inferred_param_values.push(Some(
                                                        ValueExpr::Project {
                                                            value: Box::new(item),
                                                            member: "__rust_field:1".to_string(),
                                                        },
                                                    ));
                                                } else {
                                                    inferred_param_values.push(Some(item));
                                                }
                                            }
                                        } else if matches!(
                                            method_name,
                                            "sort_by" | "max_by" | "min_by" | "reduce"
                                        ) {
                                            if let Some(item) = recv_projection("__rust_iter_item")
                                            {
                                                inferred_param_values.push(Some(item.clone()));
                                                inferred_param_values.push(Some(item));
                                            }
                                        } else if matches!(method_name, "fold" | "try_fold") {
                                            if let Some(item) = recv_projection("__rust_iter_item")
                                            {
                                                inferred_param_values.push(None);
                                                inferred_param_values.push(Some(item));
                                            }
                                        }
                                    }
                                } else if func.kind() == "identifier"
                                    || func.kind() == "scoped_identifier"
                                {
                                    let callee = text(func, source);
                                    if callee.ends_with("walk") || callee.ends_with("visit") {
                                        inferred_param_types.push(Some(TypeExpr::Named {
                                            name: "tree_sitter::Node".to_string(),
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(params) = current.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for (idx, param) in params.named_children(&mut cursor).enumerate() {
                    let (param_node, explicit_type) = if param.kind() == "parameter" {
                        (
                            param.child_by_field_name("pattern"),
                            param.child_by_field_name("type").map(|ty| {
                                type_expr_inner(
                                    ty,
                                    source,
                                    self.generic_names
                                        .get_or_insert_with(|| generic_names(self.root, source)),
                                    0,
                                )
                            }),
                        )
                    } else {
                        (Some(param), None)
                    };

                    let Some(param_node) = param_node else {
                        continue;
                    };
                    if let Some(id_node) = pattern_identifier(param_node) {
                        let name = text(id_node, source);
                        let pos = param.end_position();
                        let position = SourcePosition {
                            line: pos.row + 1,
                            column: pos.column,
                        };

                        let val_expr = if let Some(ty) = explicit_type {
                            ValueExpr::Annotated { type_expr: ty }
                        } else if let Some(Some(inferred_ty)) = inferred_param_types.get(idx) {
                            ValueExpr::Annotated {
                                type_expr: inferred_ty.clone(),
                            }
                        } else if let Some(Some(inferred_val)) = inferred_param_values.get(idx) {
                            inferred_val.clone()
                        } else {
                            ValueExpr::Unknown
                        };

                        let scoped_val = if matches!(val_expr, ValueExpr::Unknown) {
                            ValueExpr::Unknown
                        } else {
                            ValueExpr::Scoped {
                                value: Box::new(val_expr),
                                body_end,
                            }
                        };

                        self.push_binding_checked(
                            name.to_owned(),
                            position,
                            scoped_val,
                            false,
                        );
                    } else {
                        let mut tuple_node = param_node;
                        while matches!(
                            tuple_node.kind(),
                            "reference_pattern" | "ref_pattern" | "mut_pattern"
                        ) {
                            let Some(inner) = tuple_node.child_by_field_name("pattern").or_else(|| {
                                tuple_node
                                    .named_child(0)
                                    .filter(|c| c.kind() != "mutable_specifier")
                                    .or_else(|| tuple_node.named_child(1))
                            }) else {
                                break;
                            };
                            tuple_node = inner;
                        }
                        if tuple_node.kind() == "tuple_pattern" {
                            let base_val = inferred_param_values
                                .get(idx)
                                .and_then(|val| val.as_ref())
                                .cloned()
                                .unwrap_or(ValueExpr::Unknown);
                            let pos = param.end_position();
                            let position = SourcePosition {
                                line: pos.row + 1,
                                column: pos.column,
                            };
                            let mut tcursor = tuple_node.walk();
                            for (field_idx, elem) in
                                tuple_node.named_children(&mut tcursor).enumerate()
                            {
                                if let Some(id_node) = pattern_identifier(elem) {
                                    let elem_val = if matches!(base_val, ValueExpr::Unknown) {
                                        ValueExpr::Unknown
                                    } else {
                                        ValueExpr::Scoped {
                                            value: Box::new(ValueExpr::Project {
                                                value: Box::new(base_val.clone()),
                                                member: format!("__rust_field:{field_idx}"),
                                            }),
                                            body_end,
                                        }
                                    };
                                    self.push_binding_checked(
                                        text(id_node, source).to_owned(),
                                        position,
                                        elem_val,
                                        false,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        if (current.kind() == "let_declaration" && self.closure_depth == 0)
            || matches!(
                current.kind(),
                "assignment_expression" | "compound_assignment_expr"
            )
        {
            let declaration = current.kind() == "let_declaration";
            let lexical_end = if declaration {
                let mut parent = current.parent();
                let mut end = None;
                while let Some(node) = parent {
                    if node.kind() == "block" {
                        if Some(node) != self.body {
                            let position = node.end_position();
                            end = Some(SourcePosition {
                                line: position.row + 1,
                                column: position.column,
                            });
                        }
                        break;
                    }
                    parent = node.parent();
                }
                end
            } else {
                None
            };
            let pattern = current
                .child_by_field_name("pattern")
                .or_else(|| current.child_by_field_name("left"));
            if let Some(pattern) = pattern {
                if current.kind() == "let_declaration" && pattern.kind() == "tuple_pattern" {
                    let mut pat_cursor = pattern.walk();
                    let pat_elements: Vec<Node<'_>> =
                        pattern.named_children(&mut pat_cursor).collect();

                    fn tuple_children<'a>(
                        node_opt: Option<Node<'a>>,
                        kind: &str,
                    ) -> Option<Vec<Node<'a>>> {
                        let node = node_opt.filter(|n| n.kind() == kind)?;
                        let mut cursor = node.walk();
                        Some(node.named_children(&mut cursor).collect())
                    }
                    let val_opt = current.child_by_field_name("value");
                    let val_elements = tuple_children(val_opt, "tuple_expression");
                    let mut type_elements =
                        tuple_children(current.child_by_field_name("type"), "tuple_type");

                    if val_elements.is_none() && type_elements.is_none() {
                        if let Some(val_node) = val_opt {
                            if val_node.kind() == "call_expression" {
                                if let Some(func_node) = val_node.child_by_field_name("function") {
                                    let fn_name = text(func_node, source).trim();
                                    if let Some(types) = resolve_function_return_tuple(
                                        current,
                                        fn_name,
                                        source,
                                        &self.core.facts().bindings,
                                    ) {
                                        type_elements = Some(types);
                                    }
                                }
                            }
                        }
                    }

                    let rest_idx = pat_elements.iter().position(|p| {
                        p.kind() == "rest_pattern" || text(*p, source).trim() == ".."
                    });

                    let shadows = self.current_shadows();
                    let position = current.end_position();

                    let resolve_tuple_idx = |i: usize, len: usize| -> Option<usize> {
                        if let Some(r) = rest_idx {
                            if i < r {
                                Some(i)
                            } else {
                                let from_end = pat_elements.len() - 1 - i;
                                len.checked_sub(from_end + 1)
                            }
                        } else {
                            Some(i)
                        }
                    };

                    for (i, pat_node) in pat_elements.iter().enumerate() {
                        if pat_node.kind() == "rest_pattern"
                            || text(*pat_node, source).trim() == ".."
                        {
                            continue;
                        }
                        if let Some(id_node) = pattern_identifier(*pat_node) {
                            let name = text(id_node, source).to_owned();
                            let inferred = if let Some(ref types) = type_elements {
                                resolve_tuple_idx(i, types.len())
                                    .and_then(|idx| types.get(idx).copied())
                                    .map_or(ValueExpr::Unknown, |ty_node| ValueExpr::Annotated {
                                        type_expr: type_expr_inner(
                                            ty_node,
                                            source,
                                            self.generic_names.get_or_insert_with(|| {
                                                generic_names(self.root, source)
                                            }),
                                            0,
                                        ),
                                    })
                            } else if let Some(ref vals) = val_elements {
                                resolve_tuple_idx(i, vals.len())
                                    .and_then(|idx| vals.get(idx).copied())
                                    .map_or(ValueExpr::Unknown, |val_node| {
                                        value_depth(val_node, source, 0, &shadows)
                                    })
                            } else if rest_idx.is_none() {
                                if let Some(val_node) = val_opt {
                                    let base_val = value_depth(val_node, source, 0, &shadows);
                                    if matches!(base_val, ValueExpr::Unknown) {
                                        ValueExpr::Unknown
                                    } else {
                                        ValueExpr::Project {
                                            value: Box::new(base_val),
                                            member: format!("__rust_field:{i}"),
                                        }
                                    }
                                } else {
                                    ValueExpr::Unknown
                                }
                            } else {
                                ValueExpr::Unknown
                            };

                            self.push_binding_checked(
                                name,
                                SourcePosition {
                                    line: position.row + 1,
                                    column: position.column,
                                },
                                if let Some(body_end) = lexical_end {
                                    ValueExpr::Scoped {
                                        value: Box::new(inferred),
                                        body_end,
                                    }
                                } else {
                                    inferred
                                },
                                is_conditional,
                            );
                        }
                    }
                } else if current.kind() == "let_declaration" && pattern.kind() == "struct_pattern"
                {
                    let position = current.end_position();
                    let pos = SourcePosition {
                        line: position.row + 1,
                        column: position.column,
                    };
                    let base_val = current
                        .child_by_field_name("value")
                        .map(|val_node| {
                            let shadows = self.current_shadows();
                            value_depth(val_node, source, 0, &shadows)
                        })
                        .unwrap_or(ValueExpr::Unknown);
                    self.push_struct_pattern_bindings(
                        pattern,
                        pos,
                        base_val,
                        lexical_end,
                        is_conditional,
                    );
                } else if let Some(unwrapped_val) = (current.kind() == "let_declaration")
                    .then(|| current.child_by_field_name("value"))
                    .flatten()
                    .and_then(|val| {
                        unwrap_option_result_pattern(pattern, val, source, &self.current_shadows())
                    })
                {
                    let position = current.end_position();
                    self.push_tuple_struct_bindings(
                        pattern,
                        SourcePosition {
                            line: position.row + 1,
                            column: position.column,
                        },
                        unwrapped_val,
                        lexical_end,
                        is_conditional,
                    );
                } else {
                    let emit = |pattern: Node<'_>| {
                        let position = current.end_position();
                        let inferred = if current.kind() == "let_declaration"
                            && current.child_by_field_name("value").is_some()
                            && current
                                .child_by_field_name("pattern")
                                .is_some_and(|pattern| {
                                    matches!(pattern.kind(), "identifier" | "mut_pattern")
                                }) {
                            let explicit_type = current.child_by_field_name("type").map(|ty| {
                                ValueExpr::Annotated {
                                    type_expr: type_expr_inner(
                                        ty,
                                        source,
                                        self.generic_names.get_or_insert_with(|| {
                                            generic_names(self.root, source)
                                        }),
                                        0,
                                    ),
                                }
                            });
                            let complete_type = explicit_type.filter(|value| match value {
                                ValueExpr::Annotated { type_expr } => {
                                    !type_expr_has_unknown(type_expr)
                                }
                                _ => false,
                            });
                            complete_type.unwrap_or_else(|| {
                                let shadows = self.current_shadows();
                                value_depth(
                                    current
                                        .child_by_field_name("value")
                                        .expect("value checked"),
                                    source,
                                    0,
                                    &shadows,
                                )
                            })
                        } else if current.kind() == "assignment_expression"
                            && matches!(pattern.kind(), "identifier" | "mut_pattern")
                        {
                            current
                                .child_by_field_name("right")
                                .or_else(|| current.child_by_field_name("value"))
                                .map_or(ValueExpr::Unknown, |val| {
                                    let shadows = self.current_shadows();
                                    let expr = value_depth(val, source, 0, &shadows);
                                    if matches!(expr, ValueExpr::Project { .. } | ValueExpr::Alias { .. })
                                        || matches!(
                                            &expr,
                                            ValueExpr::Call { callee, .. }
                                                if matches!(
                                                    callee.as_str(),
                                                    "Some" | "Ok" | "Err" | "None"
                                                )
                                        )
                                    {
                                        expr
                                    } else {
                                        ValueExpr::Unknown
                                    }
                                })
                        } else {
                            ValueExpr::Unknown
                        };
                        self.push_binding_checked(
                            pattern_identifier(pattern).map_or_else(
                                || text(pattern, source).to_owned(),
                                |identifier| text(identifier, source).to_owned(),
                            ),
                            SourcePosition {
                                line: position.row + 1,
                                column: position.column,
                            },
                            if let Some(body_end) = lexical_end {
                                ValueExpr::Scoped {
                                    value: Box::new(inferred),
                                    body_end,
                                }
                            } else {
                                inferred
                            },
                            is_conditional,
                        );
                    };
                    if current.kind() == "let_declaration"
                        || matches!(
                            pattern.kind(),
                            "identifier" | "tuple_pattern" | "mut_pattern"
                        )
                    {
                        super::patterns::bindings(pattern, emit);
                    }
                }
            }
        }
        if current.kind() == "let_condition" {
            let pattern = current.child_by_field_name("pattern");
            let value_node = current.child_by_field_name("value");
            if let (Some(pattern), Some(val_node)) = (pattern, value_node) {
                let shadows = self.current_shadows();
                let body_end = {
                    let mut p = current.parent();
                    while let Some(parent) = p {
                        if parent.kind() == "if_expression" || parent.kind() == "while_expression" {
                            break;
                        }
                        if parent.kind() == "block" || parent.kind() == "function_item" {
                            p = None;
                            break;
                        }
                        p = parent.parent();
                    }
                    p.and_then(|parent| {
                        if parent.kind() == "if_expression" {
                            parent.child_by_field_name("consequence")
                        } else if parent.kind() == "while_expression" {
                            parent.child_by_field_name("body")
                        } else {
                            None
                        }
                    })
                    .map(|b| {
                        let pos = b.end_position();
                        SourcePosition {
                            line: pos.row + 1,
                            column: pos.column,
                        }
                    })
                };
                let is_conditional = body_end.is_none() || self.core.scope_state().is_conditional();

                if pattern.kind() == "tuple_pattern" {
                    let mut p_cursor = pattern.walk();
                    let pat_els: Vec<_> = pattern.named_children(&mut p_cursor).collect();
                    let val_els: Option<Vec<_>> = (val_node.kind() == "tuple_expression").then(|| {
                        let mut v_cursor = val_node.walk();
                        val_node.named_children(&mut v_cursor).collect()
                    });
                    let base_val = val_els
                        .is_none()
                        .then(|| value_depth(val_node, source, 0, &shadows));
                    let pos = current.end_position();
                    let bind_pos = SourcePosition {
                        line: pos.row + 1,
                        column: pos.column,
                    };
                    for (idx, pat_el) in pat_els.into_iter().enumerate() {
                        if let Some(el_val_node) = val_els.as_ref().and_then(|els| els.get(idx)).copied() {
                            if let Some(unwrapped) =
                                unwrap_option_result_pattern(pat_el, el_val_node, source, &shadows)
                            {
                                self.push_tuple_struct_bindings(
                                    pat_el, bind_pos, unwrapped, body_end, is_conditional,
                                );
                                continue;
                            }
                        }
                        let raw_val = if let Some(el_val_node) =
                            val_els.as_ref().and_then(|els| els.get(idx)).copied()
                        {
                            value_depth(el_val_node, source, 0, &shadows)
                        } else if let Some(base) = base_val.as_ref().filter(|v| !matches!(v, ValueExpr::Unknown)) {
                            ValueExpr::Project {
                                value: Box::new(base.clone()),
                                member: format!("__rust_field:{idx}"),
                            }
                        } else {
                            ValueExpr::Unknown
                        };
                        let scoped_val = match body_end {
                            Some(end) if !matches!(raw_val, ValueExpr::Unknown) => ValueExpr::Scoped {
                                value: Box::new(raw_val),
                                body_end: end,
                            },
                            _ => raw_val,
                        };
                        if let Some(id_node) = pattern_identifier(pat_el) {
                            self.push_binding_checked(
                                text(id_node, source).to_owned(),
                                bind_pos,
                                scoped_val,
                                is_conditional,
                            );
                        } else if pat_el.kind() == "tuple_struct_pattern" {
                            self.push_tuple_struct_bindings(
                                pat_el, bind_pos, scoped_val, body_end, is_conditional,
                            );
                        }
                    }
                } else if let Some(unwrapped_val) =
                    unwrap_option_result_pattern(pattern, val_node, source, &shadows)
                {
                    let pos = current.end_position();
                    self.push_tuple_struct_bindings(
                        pattern,
                        SourcePosition {
                            line: pos.row + 1,
                            column: pos.column,
                        },
                        unwrapped_val,
                        body_end,
                        is_conditional,
                    );
                } else if let Some(id_node) = pattern_identifier(pattern) {
                    let base_val = value_depth(val_node, source, 0, &shadows);
                    let name = text(id_node, source).to_owned();
                    let pos = current.end_position();
                    let scoped_val = if let Some(end) = body_end {
                        if matches!(base_val, ValueExpr::Unknown) {
                            ValueExpr::Unknown
                        } else {
                            ValueExpr::Scoped {
                                value: Box::new(base_val),
                                body_end: end,
                            }
                        }
                    } else {
                        base_val
                    };
                    self.push_binding_checked(
                        name,
                        SourcePosition {
                            line: pos.row + 1,
                            column: pos.column,
                        },
                        scoped_val,
                        is_conditional,
                    );
                }
            }
        }
        if current.kind() == "match_arm" {
            let scrutinee = current
                .parent()
                .and_then(|block| block.parent())
                .filter(|m| m.kind() == "match_expression")
                .and_then(|m| m.child_by_field_name("value"));
            let mut pattern_opt = current.child_by_field_name("pattern");
            if let Some(pat) = pattern_opt {
                if pat.kind() == "match_pattern" {
                    pattern_opt = pat.named_child(0);
                }
            }
            if let (Some(mut pat), Some(val_node)) = (pattern_opt, scrutinee) {
                while pat.kind() == "or_pattern" && pat.named_child_count() == 1 {
                    if let Some(inner) = pat.named_child(0) {
                        pat = inner;
                    } else {
                        break;
                    }
                }
                let shadows = self.current_shadows();
                let end = current.end_position();
                let body_end = SourcePosition {
                    line: end.row + 1,
                    column: end.column,
                };
                let pos = pat.end_position();
                let position = SourcePosition {
                    line: pos.row + 1,
                    column: pos.column,
                };
                if let Some(unwrapped_val) =
                    unwrap_option_result_pattern(pat, val_node, source, &shadows)
                {
                    self.push_tuple_struct_bindings(
                        pat,
                        position,
                        unwrapped_val,
                        Some(body_end),
                        true,
                    );
                } else if pat.kind() == "struct_pattern" {
                    let base_val = value_depth(val_node, source, 0, &shadows);
                    self.push_struct_pattern_bindings(
                        pat,
                        position,
                        base_val,
                        Some(body_end),
                        true,
                    );
                } else if let Some(id_node) = pattern_identifier(pat) {
                    let name = text(id_node, source);
                    if name != "_"
                        && name.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_')
                    {
                        let base_val = value_depth(val_node, source, 0, &shadows);
                        let scoped_val = if matches!(base_val, ValueExpr::Unknown) {
                            ValueExpr::Unknown
                        } else {
                            ValueExpr::Scoped {
                                value: Box::new(base_val),
                                body_end,
                            }
                        };
                        self.push_binding_checked(name.to_owned(), position, scoped_val, true);
                    }
                }
            }
        }
    }

    fn push_struct_pattern_bindings(
        &mut self,
        pattern: Node<'_>,
        pos: SourcePosition,
        base_val: ValueExpr,
        lexical_end: Option<SourcePosition>,
        is_conditional: bool,
    ) {
        let source = self.source;
        let mut pat_cursor = pattern.walk();
        for field_node in pattern.named_children(&mut pat_cursor) {
            let (bind_name, field_name) = match field_node.kind() {
                "shorthand_field_identifier" => {
                    let name = text(field_node, source).to_owned();
                    (Some(name.clone()), Some(name))
                }
                "field_pattern" => {
                    let f = field_node
                        .child_by_field_name("name")
                        .map(|n| text(n, source).to_owned());
                    let b = field_node
                        .child_by_field_name("pattern")
                        .and_then(pattern_identifier)
                        .map(|n| text(n, source).to_owned());
                    match (b, f) {
                        (Some(b), Some(f)) => (Some(b), Some(f)),
                        _ => {
                            let mut direct = None;
                            super::patterns::bindings(field_node, |id| {
                                if direct.is_none() {
                                    direct = Some(text(id, source).to_owned());
                                }
                            });
                            (direct.clone(), direct)
                        }
                    }
                }
                _ => (None, None),
            };
            if let (Some(bind_name), Some(field_name)) = (bind_name, field_name) {
                let proj = if matches!(base_val, ValueExpr::Unknown) {
                    ValueExpr::Unknown
                } else {
                    ValueExpr::Project {
                        value: Box::new(base_val.clone()),
                        member: format!("__rust_field:{field_name}"),
                    }
                };
                let val = if let Some(body_end) = lexical_end {
                    if matches!(proj, ValueExpr::Unknown) {
                        ValueExpr::Unknown
                    } else {
                        ValueExpr::Scoped {
                            value: Box::new(proj),
                            body_end,
                        }
                    }
                } else {
                    proj
                };
                self.push_binding_checked(bind_name, pos, val, is_conditional);
            } else {
                super::patterns::bindings(field_node, |id_node| {
                    self.push_binding_checked(
                        text(id_node, source).to_owned(),
                        pos,
                        ValueExpr::Unknown,
                        is_conditional,
                    );
                });
            }
        }
    }

    pub(crate) fn exit(&mut self, node: Node<'tree>) {
        if node.kind() == "closure_expression"
            && self.core.scope_state().current_or_default().is_active()
        {
            self.closure_depth = self.closure_depth.saturating_sub(1);
        }
        if node.kind() == "block" && self.block_shadows.len() > 1 {
            self.block_shadows.pop();
        }
        self.core.scope_state_mut().pop();
    }

    pub(crate) fn finish(self) -> ValueFlowFacts {
        self.core.finish()
    }
}

impl<'tree> crate::ast::visitor::SyntaxCollector<'tree> for Collector<'_, 'tree> {
    type Output = ValueFlowFacts;
    fn enter(&mut self, node: Node<'tree>) {
        Collector::enter(self, node);
    }
    fn exit(&mut self, node: Node<'tree>) {
        Collector::exit(self, node);
    }
    fn finish(self) -> ValueFlowFacts {
        Collector::finish(self)
    }
}

pub(super) fn extract(node: Node<'_>, source: &str) -> ValueFlowFacts {
    crate::ast::visitor::collect(node, Collector::new(node, source))
}

fn parse_thread_local_statics(tree: Node<'_>, source: &str) -> Vec<(String, SourcePosition, TypeExpr)> {
    let raw = text(tree, source);
    let base_pos = tree.start_position();
    let mut out = Vec::new();
    for stmt in raw.split(';') {
        let Some(static_pos) = stmt.find("static ") else {
            continue;
        };
        let rest = stmt[static_pos + "static ".len()..].trim_start();
        let Some((name_part, rhs)) = rest.split_once(':') else {
            continue;
        };
        let name = name_part.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some((ty_part, _)) = rhs.split_once('=') else {
            continue;
        };
        let ty = parse_rust_type_text(ty_part.trim(), 0);
        if !matches!(ty, TypeExpr::Unknown) {
            out.push((
                name.to_owned(),
                SourcePosition {
                    line: base_pos.row + 1,
                    column: base_pos.column,
                },
                ty,
            ));
        }
    }
    out
}

fn parse_rust_type_text(raw: &str, depth: usize) -> TypeExpr {
    if depth >= 6 {
        return TypeExpr::Unknown;
    }
    let s = raw.trim();
    if s.is_empty() {
        return TypeExpr::Unknown;
    }
    if let Some(inner) = s.strip_prefix('&') {
        let inner = inner.trim_start();
        let inner = inner.strip_prefix("mut ").unwrap_or(inner).trim_start();
        return TypeExpr::Applied {
            base: "&".to_owned(),
            args: vec![parse_rust_type_text(inner, depth + 1)],
        };
    }
    if let Some((base, rest)) = s.split_once('<') {
        if let Some(inner) = rest.strip_suffix('>') {
            let base = base.trim();
            let mut args = Vec::new();
            let mut start = 0usize;
            let mut angle = 0usize;
            let mut paren = 0usize;
            for (idx, ch) in inner.char_indices() {
                match ch {
                    '<' => angle += 1,
                    '>' => angle = angle.saturating_sub(1),
                    '(' => paren += 1,
                    ')' => paren = paren.saturating_sub(1),
                    ',' if angle == 0 && paren == 0 => {
                        let arg = inner[start..idx].trim();
                        if !arg.starts_with('\'') {
                            args.push(parse_rust_type_text(arg, depth + 1));
                        }
                        start = idx + 1;
                    }
                    _ => {}
                }
            }
            let last = inner[start..].trim();
            if !last.is_empty() && !last.starts_with('\'') {
                args.push(parse_rust_type_text(last, depth + 1));
            }
            if !base.is_empty() && !args.is_empty() {
                return TypeExpr::Applied {
                    base: base.to_owned(),
                    args,
                };
            }
        }
    }
    if s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | ':')) {
        TypeExpr::Named { name: s.to_owned() }
    } else {
        TypeExpr::Unknown
    }
}

pub(super) fn is_async(node: Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    let is_async = node.named_children(&mut cursor).any(|child| {
        child.kind() == "function_modifiers"
            && text(child, source)
                .split_whitespace()
                .any(|modifier| modifier == "async")
    });
    is_async
}

fn generic_names<'a>(node: Node<'_>, source: &'a str) -> HashSet<&'a str> {
    let mut generic_names = HashSet::new();
    let mut ancestor = Some(node);
    while let Some(scope) = ancestor {
        if let Some(parameters) = scope.child_by_field_name("type_parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                let name = match parameter.kind() {
                    "type_identifier" => Some(parameter),
                    "type_parameter" | "const_parameter" => parameter.child_by_field_name("name"),
                    "constrained_type_parameter" | "optional_type_parameter" => parameter
                        .child_by_field_name("left")
                        .or_else(|| parameter.child_by_field_name("name")),
                    _ => None,
                };
                if let Some(name) = name {
                    let name = if name.kind() == "constrained_type_parameter" {
                        name.child_by_field_name("left")
                    } else {
                        Some(name)
                    };
                    if let Some(name) = name {
                        generic_names.insert(text(name, source));
                    }
                }
            }
        }
        ancestor = scope.parent();
    }
    generic_names
}

pub(super) fn type_references(
    facts: &mut Facts,
    owner: &str,
    node: Node<'_>,
    source: &str,
    line: usize,
) {
    let generic_names = generic_names(node, source);
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "lifetime" | "lifetime_parameter" => {}
            "scoped_type_identifier" => {
                let raw = text(node, source);
                let head = raw.split("::").next().unwrap_or("");
                if !generic_names.contains(head) {
                    relations::reference(facts, owner, raw, "references", line);
                }
            }
            "type_identifier" | "identifier" => {
                let is_type_binding_name = node.parent().is_some_and(|parent| {
                    parent.kind() == "type_binding"
                        && parent.child_by_field_name("name") == Some(node)
                });
                let name = text(node, source);
                if !is_type_binding_name
                    && name != "_"
                    && !name.starts_with('\'')
                    && !generic_names.contains(name)
                {
                    relations::reference(facts, owner, name, "references", line);
                }
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
    }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct BlockMacroFacts {
    pub(crate) defs: Vec<(usize, &'static str)>,
    pub(crate) imports: HashSet<&'static str>,
}

#[derive(Clone, Default, Debug)]
pub(crate) struct ModuleMacroFacts {
    pub(crate) defs: Vec<(usize, &'static str)>,
    pub(crate) imports: HashSet<&'static str>,
}

#[derive(Clone, Default, Debug)]
pub(crate) struct FileMacroFacts {
    pub(crate) root_macro_defs: Vec<(usize, &'static str)>,
    pub(crate) root_macro_imports: HashSet<&'static str>,
    pub(crate) modules: HashMap<usize, ModuleMacroFacts>,
    pub(crate) blocks: HashMap<usize, BlockMacroFacts>,
    pub(crate) thread_locals: Vec<(usize, String, SourcePosition, TypeExpr)>,
}

fn get_file_root(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
    }
    node
}

fn module_of<'a>(mut node: Node<'a>) -> Node<'a> {
    if node.kind() == "declaration_list" {
        if let Some(parent) = node.parent() {
            if parent.kind() == "mod_item" {
                return node;
            }
        }
    }
    if node.kind() == "source_file" {
        return node;
    }
    while let Some(parent) = node.parent() {
        if parent.kind() == "declaration_list" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "mod_item" {
                    return parent;
                }
            }
        }
        node = parent;
    }
    node
}

fn parent_module<'a>(mod_node: Node<'a>, file_root: Node<'a>) -> Option<Node<'a>> {
    if mod_node == file_root {
        return None;
    }
    let mod_item = mod_node.parent()?;
    let enclosing = mod_item.parent()?;
    Some(module_of(enclosing))
}

fn find_child_module<'a>(body: Node<'a>, mod_name: &str, source: &str) -> Option<Node<'a>> {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "mod_item" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if text(name_node, source).trim() == mod_name {
                    return child.child_by_field_name("body");
                }
            }
        }
    }
    None
}

fn resolve_module_path<'a, 's>(
    context: Node<'a>,
    path: &'s str,
    source: &str,
    file_root: Node<'a>,
) -> Option<(Node<'a>, &'s str)> {
    let enc_mod = module_of(context);
    let (mut current, clean_path) = if let Some(stripped) = path.strip_prefix("crate::") {
        (file_root, stripped)
    } else if let Some(stripped) = path.strip_prefix("self::") {
        (enc_mod, stripped)
    } else if path.starts_with("super::") {
        let mut curr_mod = enc_mod;
        let mut remaining = path;
        while let Some(stripped) = remaining.strip_prefix("super::") {
            curr_mod = parent_module(curr_mod, file_root)?;
            remaining = stripped;
        }
        (curr_mod, remaining)
    } else {
        let first_seg = path.split("::").next().unwrap_or(path);
        if find_child_module(enc_mod, first_seg, source).is_some() || !path.contains("::") {
            (enc_mod, path)
        } else {
            (file_root, path)
        }
    };

    let parts: Vec<&str> = clean_path.split("::").collect();
    if parts.is_empty() {
        return None;
    }
    let target_name = parts[parts.len() - 1];
    let mod_parts = &parts[..parts.len() - 1];

    for &mod_name in mod_parts {
        current = find_child_module(current, mod_name, source)?;
    }

    Some((current, target_name))
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum MacroResolution {
    KnownMacro,
    KnownNonMacro,
    UnknownOrExhausted,
}

fn module_has_macro(
    body: Node<'_>,
    target_name: &str,
    source: &str,
    file_root: Node<'_>,
    visited: &mut HashSet<(usize, String)>,
    depth: usize,
) -> MacroResolution {
    if depth > 8 || !visited.insert((body.id(), target_name.to_string())) {
        return MacroResolution::UnknownOrExhausted;
    }

    let mut saw_exhausted = false;
    let mut saw_non_macro = false;
    let mut cursor = body.walk();
    for item in body.named_children(&mut cursor) {
        if item.kind() == "macro_definition" {
            if let Some(m_name) = item.child_by_field_name("name") {
                if text(m_name, source).trim() == target_name {
                    return MacroResolution::KnownMacro;
                }
            }
        } else if matches!(
            item.kind(),
            "function_item"
                | "function_signature_item"
                | "struct_item"
                | "enum_item"
                | "type_item"
                | "const_item"
                | "static_item"
                | "trait_item"
                | "union_item"
                | "mod_item"
        ) {
            if let Some(name_node) = item.child_by_field_name("name") {
                if text(name_node, source).trim() == target_name {
                    saw_non_macro = true;
                }
            }
        } else if item.kind() == "foreign_mod_item" {
            let mut f_cursor = item.walk();
            for f_child in item.named_children(&mut f_cursor) {
                if f_child.kind() == "declaration_list" {
                    let mut d_cursor = f_child.walk();
                    for d_child in f_child.named_children(&mut d_cursor) {
                        if let Some(name_node) = d_child.child_by_field_name("name") {
                            if text(name_node, source).trim() == target_name {
                                saw_non_macro = true;
                            }
                        }
                    }
                } else if let Some(name_node) = f_child.child_by_field_name("name") {
                    if text(name_node, source).trim() == target_name {
                        saw_non_macro = true;
                    }
                }
            }
        } else if item.kind() == "extern_crate_declaration" {
            let bound_name = item
                .child_by_field_name("alias")
                .or_else(|| item.child_by_field_name("name"));
            if let Some(name_node) = bound_name {
                if text(name_node, source).trim() == target_name {
                    saw_non_macro = true;
                }
            }
        } else if item.kind() == "use_declaration" {
            if let Some(arg) = item.child_by_field_name("argument") {
                let mut mod_imports = Vec::new();
                super::collect_use(arg, source, "", &mut mod_imports);
                for (re_path, re_alias) in mod_imports {
                    if re_alias == target_name {
                        let clean = re_path
                            .strip_prefix("crate::")
                            .or_else(|| re_path.strip_prefix("self::"))
                            .unwrap_or(&re_path);
                        if clean.starts_with("std::")
                            || clean.starts_with("core::")
                            || clean.starts_with("alloc::")
                        {
                            saw_non_macro = true;
                            continue;
                        }
                        if let Some((target_body, actual_target)) =
                            resolve_module_path(body, &re_path, source, file_root)
                        {
                            match module_has_macro(
                                target_body,
                                actual_target,
                                source,
                                file_root,
                                visited,
                                depth + 1,
                            ) {
                                MacroResolution::KnownMacro => return MacroResolution::KnownMacro,
                                MacroResolution::UnknownOrExhausted => saw_exhausted = true,
                                MacroResolution::KnownNonMacro => saw_non_macro = true,
                            }
                        } else {
                            saw_exhausted = true;
                        }
                    }
                }
            }
        }
    }

    if saw_non_macro && !saw_exhausted {
        MacroResolution::KnownNonMacro
    } else {
        MacroResolution::UnknownOrExhausted
    }
}

fn is_imported_target_macro(
    context: Node<'_>,
    path: &str,
    source: &str,
    file_root: Node<'_>,
) -> bool {
    let clean = path
        .strip_prefix("crate::")
        .or_else(|| path.strip_prefix("self::"))
        .unwrap_or(path);
    if clean.starts_with("std::") || clean.starts_with("core::") || clean.starts_with("alloc::") {
        return false;
    }
    if let Some((target_body, target_name)) = resolve_module_path(context, path, source, file_root)
    {
        let mut visited = HashSet::new();
        let res = module_has_macro(target_body, target_name, source, file_root, &mut visited, 0);
        return matches!(
            res,
            MacroResolution::KnownMacro | MacroResolution::UnknownOrExhausted
        );
    }
    true
}

fn collect_scope_macro_facts<'tree>(
    scope: Node<'tree>,
    file_root: Node<'tree>,
    source: &str,
    defs: &mut Vec<(usize, &'static str)>,
    imports: &mut HashSet<&'static str>,
    thread_locals: &mut Vec<(usize, String, SourcePosition, TypeExpr)>,
    stack: &mut Vec<Node<'tree>>,
) {
    let mut cursor = scope.walk();
    for child in scope.named_children(&mut cursor) {
        if child.kind() == "macro_invocation" {
            if let Some(m_name) = child.child_by_field_name("macro") {
                if matches!(
                    text(m_name, source).trim(),
                    "thread_local" | "std::thread_local"
                ) {
                    if let Some(tree) = child
                        .named_children(&mut child.walk())
                        .find(|c| c.kind() == "token_tree")
                    {
                        for (name, pos, ty_expr) in parse_thread_local_statics(tree, source) {
                            thread_locals.push((scope.id(), name, pos, ty_expr));
                        }
                    }
                }
            }
            stack.push(child);
        } else if child.kind() == "macro_definition" {
            if let Some(m_name) = child.child_by_field_name("name") {
                match text(m_name, source).trim() {
                    "vec" => defs.push((child.end_byte(), "vec")),
                    "format" => defs.push((child.end_byte(), "format")),
                    _ => {}
                }
            }
        } else if child.kind() == "use_declaration" {
            if let Some(argument) = child.child_by_field_name("argument") {
                let mut use_imports = Vec::new();
                super::collect_use(argument, source, "", &mut use_imports);
                for (path, alias) in use_imports {
                    if (alias == "vec" || alias == "format")
                        && !path.starts_with("std::")
                        && !path.starts_with("core::")
                        && !path.starts_with("alloc::")
                        && is_imported_target_macro(scope, &path, source, file_root)
                    {
                        imports.insert(if alias == "vec" { "vec" } else { "format" });
                    }
                }
            }
        } else {
            stack.push(child);
        }
    }
}

fn build_file_macro_facts(root: Node<'_>, source: &str) -> FileMacroFacts {
    let file_root = get_file_root(root);
    let mut facts = FileMacroFacts::default();

    let mut stack = vec![file_root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node == file_root {
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut facts.root_macro_defs,
                &mut facts.root_macro_imports,
                &mut facts.thread_locals,
                &mut stack,
            );
        } else if kind == "declaration_list" {
            let mut mod_facts = ModuleMacroFacts::default();
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut mod_facts.defs,
                &mut mod_facts.imports,
                &mut facts.thread_locals,
                &mut stack,
            );
            facts.modules.insert(node.id(), mod_facts);
        } else if kind == "block" {
            let mut block_facts = BlockMacroFacts::default();
            collect_scope_macro_facts(
                node,
                file_root,
                source,
                &mut block_facts.defs,
                &mut block_facts.imports,
                &mut facts.thread_locals,
                &mut stack,
            );
            facts.blocks.insert(node.id(), block_facts);
        } else {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    facts
}

std::thread_local! {
    static CACHED_FILE_FACTS: std::cell::RefCell<Option<(usize, std::sync::Arc<FileMacroFacts>)>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn clear_file_macro_facts() {
    CACHED_FILE_FACTS.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub(crate) fn file_macro_facts(root: Node<'_>, source: &str) -> std::sync::Arc<FileMacroFacts> {
    let file_root = get_file_root(root);
    let root_id = file_root.id();
    CACHED_FILE_FACTS.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if let Some((cached_id, ref facts)) = *borrow {
            if cached_id == root_id {
                return facts.clone();
            }
        }
        let facts = std::sync::Arc::new(build_file_macro_facts(file_root, source));
        *borrow = Some((root_id, facts.clone()));
        facts
    })
}

fn collect_ancestor_shadows(
    node: Node<'_>,
    _source: &str,
    file_facts: &FileMacroFacts,
) -> HashSet<&'static str> {
    let mut shadows = file_facts.root_macro_imports.clone();
    for (end_byte, name) in &file_facts.root_macro_defs {
        if *end_byte <= node.start_byte() {
            shadows.insert(name);
        }
    }

    let file_root = get_file_root(node);
    let mut curr = node;
    while let Some(parent) = curr.parent() {
        if parent == file_root {
            break;
        }
        if parent.kind() == "declaration_list" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "mod_item" {
                    if let Some(mod_facts) = file_facts.modules.get(&parent.id()) {
                        shadows.extend(&mod_facts.imports);
                        for (end_byte, name) in &mod_facts.defs {
                            if *end_byte <= node.start_byte() {
                                shadows.insert(name);
                            }
                        }
                    }
                }
            }
        } else if parent.kind() == "block" {
            if let Some(block_facts) = file_facts.blocks.get(&parent.id()) {
                shadows.extend(&block_facts.imports);
                for (end_byte, name) in &block_facts.defs {
                    if *end_byte <= node.start_byte() {
                        shadows.insert(name);
                    }
                }
            }
        }
        curr = parent;
    }

    shadows
}
