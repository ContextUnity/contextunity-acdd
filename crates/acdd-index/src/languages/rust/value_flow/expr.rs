use super::macros::{collect_ancestor_shadows, file_macro_facts};
use super::types::{static_name, type_expr, type_expr_has_unknown, type_expr_inner};
use crate::ast::syntax::text;
use crate::semantic::{CallSiteEvidence, SourcePosition, TypeExpr, ValueExpr};
use std::borrow::Cow;
use std::collections::HashSet;
use tree_sitter::Node;

const MAX_ADAPTER_CHAIN_DEPTH: usize = 7;

pub(in super::super) fn callee_name<'a>(
    mut node: Node<'_>,
    source: &'a str,
) -> Option<Cow<'a, str>> {
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
                "generic_type" => {
                    self.collect(node.child_by_field_name("type")?, source, depth + 1)
                }
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

pub(super) fn pattern_identifier<'t>(node: Node<'t>) -> Option<Node<'t>> {
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

pub(super) fn strip_pattern_wrappers(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "reference_pattern" | "ref_pattern" | "mut_pattern" | "match_pattern"
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

pub(in super::super) fn value(node: Node<'_>, source: &str) -> ValueExpr {
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

pub(super) fn value_depth(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    if (depth >= MAX_ADAPTER_CHAIN_DEPTH && node.kind() == "call_expression")
        || depth > MAX_ADAPTER_CHAIN_DEPTH
    {
        return ValueExpr::Unknown;
    }
    match node.kind() {
        "array_expression" | "tuple_expression" => {
            compound_sequence_expr(node, source, depth, shadowed_macros)
        }
        "reference_expression" => reference_value_expr(node, source, depth, shadowed_macros),
        "closure_expression" => ValueExpr::Construct {
            callee: "__rust_closure!".to_owned(),
        },
        "string_literal" | "raw_string_literal" => named_annotated_expr("str"),
        "byte_string_literal" | "raw_byte_string_literal" => ValueExpr::Annotated {
            type_expr: TypeExpr::Applied {
                base: "[]".to_owned(),
                args: vec![TypeExpr::Named {
                    name: "u8".to_owned(),
                }],
            },
        },
        "boolean_literal" => named_annotated_expr("bool"),
        "char_literal" => named_annotated_expr("char"),
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
        "type_cast_expression" => node
            .child_by_field_name("type")
            .map(|ty| type_expr_inner(ty, source, &HashSet::new(), 0))
            .filter(|expr| !matches!(expr, TypeExpr::Unknown))
            .map_or(ValueExpr::Unknown, |type_expr| ValueExpr::Annotated {
                type_expr,
            }),
        "try_expression" => node.named_child(0).map_or(ValueExpr::Unknown, |inner| {
            ValueExpr::Project {
                value: Box::new(value_depth(inner, source, depth + 1, shadowed_macros)),
                member: "?".to_owned(),
            }
        }),
        "parenthesized_expression" => node.named_child(0).map_or(ValueExpr::Unknown, |inner| {
            value_depth(inner, source, depth, shadowed_macros)
        }),
        "unary_expression" => unary_value_expr(node, source, depth, shadowed_macros),
        "struct_expression" => node
            .child_by_field_name("name")
            .and_then(|name| static_name(name, source))
            .map_or(ValueExpr::Unknown, |callee| ValueExpr::Construct { callee }),
        "macro_invocation" => macro_invocation_expr(node, source, shadowed_macros),
        "call_expression" => call_expression_expr(node, source, depth, shadowed_macros),
        "index_expression" => index_value_expr(node, source, depth, shadowed_macros),
        "field_expression" => field_value_expr(node, source, depth, shadowed_macros),
        "identifier" | "self" | "scoped_identifier" => ValueExpr::Alias {
            name: text(node, source).to_owned(),
        },
        "block" => (!block_has_let(node))
            .then(|| block_tail_expr(node))
            .flatten()
            .map_or(ValueExpr::Unknown, |tail| {
                value_depth(tail, source, depth + 1, shadowed_macros)
            }),
        "match_expression" => match_expression_expr(node, source, depth, shadowed_macros),
        "if_expression" => if_expression_expr(node, source, depth, shadowed_macros),
        "binary_expression" => binary_value_expr(node, source),
        _ => ValueExpr::Unknown,
    }
}

fn named_annotated_expr(name: &str) -> ValueExpr {
    ValueExpr::Annotated {
        type_expr: TypeExpr::Named {
            name: name.to_owned(),
        },
    }
}

fn compound_sequence_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
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

fn reference_value_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
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

fn unary_value_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    let trimmed = text(node, source).trim_start();
    if trimmed.starts_with('*') {
        node.child_by_field_name("argument")
            .or_else(|| node.named_child(0))
            .map_or(ValueExpr::Unknown, |inner| ValueExpr::Project {
                value: Box::new(value_depth(inner, source, depth + 1, shadowed_macros)),
                member: "__rust_deref".to_owned(),
            })
    } else if trimmed.starts_with('!') {
        named_annotated_expr("bool")
    } else {
        ValueExpr::Unknown
    }
}

fn macro_invocation_expr(
    node: Node<'_>,
    source: &str,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    let callee = node
        .child_by_field_name("macro")
        .map(|m| text(m, source).trim());
    match callee {
        Some("vec") if !shadowed_macros.contains("vec") => vec_macro_expr(node, source)
            .unwrap_or_else(|| ValueExpr::Construct {
                callee: "Vec".to_string(),
            }),
        Some("format") if !shadowed_macros.contains("format") => ValueExpr::Construct {
            callee: "String".to_string(),
        },
        Some("concat" | "stringify" | "include_str" | "env") => named_annotated_expr("str"),
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
        Some("format_args") => named_annotated_expr("std::fmt::Arguments"),
        _ => ValueExpr::Unknown,
    }
}

fn resolve_mapped_member_tag<'a>(
    node: Node<'_>,
    member_name: &'a str,
    source: &str,
) -> &'a str {
    if member_name == "unwrap_or" && has_string_literal_first_arg(node, 1) {
        return "__rust_unwrap_or_str";
    }
    if member_name == "map_or" && has_string_literal_first_arg(node, 2) {
        return "__rust_map_or_str";
    }
    if member_name == "map" || member_name == "and_then" {
        return single_arg_node(node)
            .and_then(|arg| match arg.kind() {
                "closure_expression" if member_name == "and_then" => {
                    is_identity_some_closure(arg, source).then_some("__rust_identity_some")
                }
                "scoped_identifier" | "identifier" => {
                    static_fn_map_tag(text(arg, source).trim())
                }
                _ => None,
            })
            .unwrap_or(member_name);
    }
    if member_name == "filter_map" && is_filter_map_ok_arg(node, source) {
        return "__rust_filter_map_ok";
    }
    if member_name == "get" && single_arg_node(node).is_some_and(|arg| arg.kind() == "range_expression")
    {
        return "__rust_get_range";
    }
    member_name
}

fn single_arg_node(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("arguments")
        .filter(|args| args.named_child_count() == 1)
        .and_then(|args| args.named_child(0))
}

fn is_identity_some_closure(arg: Node<'_>, source: &str) -> bool {
    (|| -> Option<bool> {
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
        Some(
            body.kind() == "call_expression"
                && text(function, source) == "Some"
                && value.kind() == "identifier"
                && text(value, source) == text(parameter, source),
        )
    })()
    .unwrap_or(false)
}

fn static_fn_map_tag(path: &str) -> Option<&'static str> {
    match path {
        "str::trim" | "str::trim_start" | "str::trim_end" | "String::as_str" => {
            Some("__rust_map_str_trim")
        }
        "str::to_owned"
        | "str::to_string"
        | "ToOwned::to_owned"
        | "ToString::to_string"
        | "Into::into"
        | "String::from" => Some("__rust_map_to_string"),
        "Value::as_str" | "serde_json::Value::as_str" | "toml::Value::as_str" => {
            Some("__rust_map_as_str")
        }
        "toml::Value::as_table" => Some("__rust_map_as_toml_table"),
        "toml::Value::as_integer" | "Value::as_i64" | "serde_json::Value::as_i64" => {
            Some("__rust_map_i64")
        }
        "toml::Value::as_bool" | "Value::as_bool" | "serde_json::Value::as_bool" => {
            Some("__rust_map_bool")
        }
        "Value::as_array"
        | "Value::as_array_mut"
        | "serde_json::Value::as_array"
        | "serde_json::Value::as_array_mut" => Some("__rust_map_as_array"),
        "Value::as_object"
        | "Value::as_object_mut"
        | "serde_json::Value::as_object"
        | "serde_json::Value::as_object_mut" => Some("__rust_map_as_object"),
        "Value::as_u64" | "serde_json::Value::as_u64" => Some("__rust_map_u64"),
        "Path::to_str"
        | "std::path::Path::to_str"
        | "OsStr::to_str"
        | "std::ffi::OsStr::to_str" => Some("__rust_map_path_to_str"),
        "Path::to_path_buf" | "std::path::Path::to_path_buf" => Some("__rust_map_to_path_buf"),
        "Clone::clone" => Some("__rust_map_cloned"),
        _ => None,
    }
}

fn is_filter_map_ok_arg(node: Node<'_>, source: &str) -> bool {
    single_arg_node(node).is_some_and(|arg| {
        if arg.kind() != "closure_expression" {
            return matches!(
                text(arg, source).trim(),
                "Result::ok" | "std::result::Result::ok" | "core::result::Result::ok"
            );
        }
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
    })
}

fn is_known_projection_member(member_name: &str) -> bool {
    matches!(
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
    )
}

fn call_field_expression_expr(
    node: Node<'_>,
    function: Node<'_>,
    generic_result: &Option<TypeExpr>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> Option<ValueExpr> {
    let receiver = function.child_by_field_name("value")?;
    let member = function.child_by_field_name("field")?;
    let raw_member = text(member, source);
    let member_name = resolve_mapped_member_tag(node, raw_member, source);
    if let Some(projected) =
        closure_projection_expr(node, member_name, receiver, source, depth, shadowed_macros)
    {
        return Some(projected);
    }
    if member_name == "collect" {
        if let Some(TypeExpr::Applied { args, .. }) = generic_result {
            if let [target_ty] = args.as_slice() {
                let inner = value_depth(receiver, source, depth + 1, shadowed_macros);
                if matches!(inner, ValueExpr::Unknown) || !type_expr_has_unknown(target_ty) {
                    return Some(ValueExpr::Annotated {
                        type_expr: target_ty.clone(),
                    });
                }
            }
        }
    }
    if member_name == "parse" {
        if let Some(TypeExpr::Applied { args, .. }) = generic_result {
            if let [target_ty] = args.as_slice() {
                return Some(ValueExpr::Annotated {
                    type_expr: TypeExpr::Applied {
                        base: "Result".to_owned(),
                        args: vec![
                            target_ty.clone(),
                            TypeExpr::Named {
                                name: "String".to_owned(),
                            },
                        ],
                    },
                });
            }
        }
    }
    if !is_known_projection_member(member_name) {
        return None;
    }
    let inner = value_depth(receiver, source, depth + 1, shadowed_macros);
    Some(if matches!(inner, ValueExpr::Unknown) {
        ValueExpr::Unknown
    } else {
        ValueExpr::Project {
            value: Box::new(inner),
            member: member_name.to_owned(),
        }
    })
}

fn call_expression_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    let Some(mut function) = node.child_by_field_name("function") else {
        return ValueExpr::Unknown;
    };
    let mut generic_result = None;
    if function.kind() == "generic_function" {
        generic_result = function
            .child_by_field_name("type_arguments")
            .filter(|arguments| (1..=4).contains(&arguments.named_child_count()))
            .map(|arguments| {
                let mut cursor = arguments.walk();
                TypeExpr::Applied {
                    base: "__rust_call_arguments".to_owned(),
                    args: arguments
                        .named_children(&mut cursor)
                        .map(|argument| type_expr(argument, source))
                        .collect(),
                }
            });
        let Some(inner) = function.child_by_field_name("function") else {
            return ValueExpr::Unknown;
        };
        function = inner;
    }
    if function.kind() == "field_expression" {
        if let Some(expr) = call_field_expression_expr(
            node,
            function,
            &generic_result,
            source,
            depth,
            shadowed_macros,
        ) {
            return expr;
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
            if let Some(arg) = single_arg_node(node) {
                let inner = value_depth(arg, source, depth + 1, shadowed_macros);
                if !matches!(inner, ValueExpr::Unknown) {
                    return ValueExpr::Project {
                        value: Box::new(inner),
                        member: if callee.ends_with("Some") {
                            "__rust_wrap_some"
                        } else {
                            "__rust_wrap_ok"
                        }
                        .to_owned(),
                    };
                }
            }
        }
        let call = ValueExpr::Call {
            callee: callee.to_string(),
            site: None,
        };
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

fn index_value_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    let base = node
        .child_by_field_name("value")
        .or_else(|| node.named_child(0));
    let index = node
        .child_by_field_name("index")
        .or_else(|| node.named_child(1));
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

fn field_value_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
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

fn is_diverging_expr(n: Node<'_>, source: &str) -> bool {
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
}

fn match_arm_unwraps_scrutinee(arm: Node<'_>, source: &str) -> bool {
    let Some(mut pat) = arm.child_by_field_name("pattern") else {
        return false;
    };
    let Some(arm_val) = arm
        .child_by_field_name("value")
        .or_else(|| arm.named_child(1))
    else {
        return false;
    };
    if pat.kind() == "match_pattern" {
        if let Some(inner_pat) = pat.named_child(0) {
            pat = inner_pat;
        }
    }
    if pat.kind() != "tuple_struct_pattern" {
        return false;
    }
    let type_name = pat
        .child_by_field_name("type")
        .map(|t| text(t, source).trim().to_owned());
    if !matches!(type_name.as_deref(), Some("Ok" | "Some")) {
        return false;
    }
    let pat_id = pat
        .named_children(&mut pat.walk())
        .find(|c| pat.child_by_field_name("type") != Some(*c))
        .and_then(pattern_identifier)
        .map(|id| text(id, source));
    pat_id.is_some_and(|id| id == text(arm_val, source).trim())
}

fn match_expression_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
    let block = node.child_by_field_name("body").unwrap_or(node);
    if let Some(scrutinee) = node.child_by_field_name("value") {
        let inner = value_depth(scrutinee, source, depth + 1, shadowed_macros);
        if !matches!(inner, ValueExpr::Unknown) {
            let mut cursor = block.walk();
            if block
                .named_children(&mut cursor)
                .any(|child| child.kind() == "match_arm" && match_arm_unwraps_scrutinee(child, source))
            {
                return ValueExpr::Project {
                    value: Box::new(inner),
                    member: "unwrap".to_string(),
                };
            }
        }
    }
    let mut alternatives = Vec::new();
    let mut cursor = block.walk();
    for arm in block.named_children(&mut cursor) {
        if arm.kind() != "match_arm" {
            continue;
        }
        let Some(tail) = arm
            .child_by_field_name("value")
            .or_else(|| arm.named_child(1))
            .and_then(block_tail_expr)
        else {
            return ValueExpr::Unknown;
        };
        if is_diverging_expr(tail, source) {
            continue;
        }
        let arm_val = value_depth(tail, source, depth + 1, shadowed_macros);
        if matches!(arm_val, ValueExpr::Unknown) {
            return ValueExpr::Unknown;
        }
        if !alternatives.contains(&arm_val) {
            alternatives.push(arm_val);
        }
        if alternatives.len() > 4 {
            return ValueExpr::Unknown;
        }
    }
    match alternatives.len() {
        0 => ValueExpr::Unknown,
        1 => alternatives.pop().unwrap_or(ValueExpr::Unknown),
        _ => ValueExpr::BranchJoin { alternatives },
    }
}

fn if_expression_expr(
    node: Node<'_>,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> ValueExpr {
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
    let (Some(then_node), Some(else_node)) = (then_expr, else_expr) else {
        return ValueExpr::Unknown;
    };
    if is_diverging_expr(else_node, source) {
        return value_depth(then_node, source, depth + 1, shadowed_macros);
    }
    if is_diverging_expr(then_node, source) {
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

fn binary_value_expr(node: Node<'_>, source: &str) -> ValueExpr {
    if node.child_by_field_name("operator").is_some_and(|op| {
        matches!(
            text(op, source).trim(),
            "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||"
        )
    }) {
        named_annotated_expr("bool")
    } else {
        ValueExpr::Unknown
    }
}

fn vec_token_expr(node: Node<'_>, source: &str) -> Option<ValueExpr> {
    match node.kind() {
        "identifier" => Some(ValueExpr::Alias {
            name: text(node, source).to_owned(),
        }),
        "string_literal" | "raw_string_literal" => Some(named_annotated_expr("str")),
        "boolean_literal" => Some(named_annotated_expr("bool")),
        "char_literal" => Some(named_annotated_expr("char")),
        "token_tree" if text(node, source).trim_start().starts_with('(') => {
            let mut items = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                items.push(vec_token_expr(child, source)?);
            }
            (!items.is_empty()).then_some(ValueExpr::Project {
                value: Box::new(ValueExpr::BranchJoin {
                    alternatives: items,
                }),
                member: "__rust_tuple".to_owned(),
            })
        }
        _ => None,
    }
}

fn vec_chain_expr(children: &[Node<'_>], source: &str) -> Option<ValueExpr> {
    let end = children
        .iter()
        .position(|c| matches!(text(*c, source).trim(), "," | ";"))
        .unwrap_or(children.len());
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
        let callee = format!(
            "{}::{}",
            text(slice[0], source).trim(),
            text(slice[2], source).trim()
        );
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
    let children: Vec<_> = tree
        .children(&mut inner_cursor)
        .filter(|c| !matches!(c.kind(), "[" | "]"))
        .collect();
    if children.is_empty() {
        return None;
    }
    let elem = vec_chain_expr(&children, source)?;
    Some(ValueExpr::Project {
        value: Box::new(elem),
        member: "__rust_vec_of".to_owned(),
    })
}

fn block_has_let(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let has_let = node
        .named_children(&mut cursor)
        .any(|child| child.kind() == "let_declaration");
    has_let
}

fn block_tail_expr<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if node.kind() != "block" {
        return Some(node);
    }
    let last = node.named_child(node.named_child_count().checked_sub(1)?)?;
    if last.kind() == "expression_statement" {
        return last.named_child(0).filter(|inner| {
            matches!(
                inner.kind(),
                "return_expression" | "break_expression" | "continue_expression" | "macro_invocation"
            )
        });
    }
    (last.kind() != "let_declaration").then_some(last)
}

pub(super) fn value_expr_within_depth(expr: &ValueExpr, depth: usize) -> bool {
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

pub(super) fn sanitize_binding_value(value: ValueExpr) -> ValueExpr {
    if value_expr_within_depth(&value, 0) {
        value
    } else {
        ValueExpr::Unknown
    }
}

pub(super) fn unwrap_option_result_member(pattern: Node<'_>, source: &str) -> Option<&'static str> {
    if pattern.kind() != "tuple_struct_pattern" {
        return None;
    }
    let type_name = pattern
        .child_by_field_name("type")
        .map(|t| text(t, source).trim().to_owned());
    match type_name.as_deref().and_then(|n| n.rsplit("::").next()) {
        Some("Ok" | "Some") => Some("unwrap"),
        Some("Err") => Some("__rust_err"),
        Some("Vacant" | "Occupied") => Some(""),
        _ => None,
    }
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

fn static_closure_projection(closure_node: Node<'_>, member_name: &str, item_expr: ValueExpr, source: &str) -> ValueExpr {
    let full = text(closure_node, source).trim();
    let projected_member = match full {
        "Some" | "std::option::Option::Some" | "core::option::Option::Some"
            if member_name == "map" =>
        {
            Some("__rust_wrap_some")
        }
        "Ok" | "std::result::Result::Ok" | "core::result::Result::Ok" if member_name == "map" => {
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
        "Path::to_str" | "std::path::Path::to_str" | "OsStr::to_str" | "std::ffi::OsStr::to_str" => {
            Some("to_str")
        }
        "OsStr::to_string_lossy" | "std::ffi::OsStr::to_string_lossy" => {
            Some("to_string_lossy")
        }
        _ => None,
    };
    if let Some(method) = projected_member {
        ValueExpr::Project {
            value: Box::new(item_expr),
            member: method.to_owned(),
        }
    } else {
        ValueExpr::Call {
            callee: full.to_owned(),
            site: None,
        }
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
        "with" => "__rust_local_key",
        _ => return None,
    };
    let args = node.child_by_field_name("arguments")?;
    let (default_node, closure_node) = match member_name {
        "map" | "and_then" | "filter_map" | "flat_map" | "with"
            if args.named_child_count() == 1 =>
        {
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
        member: if member_name == "with" {
            "__rust_local_key"
        } else {
            "__rust_iter_item"
        }
        .to_owned(),
    };
    let (body_sub, default_val, default_ok) = if closure_node.kind() == "closure_expression" {
        eval_closure_projection_body(
            closure_node,
            default_node,
            member_name,
            &item_expr,
            source,
            depth,
            shadowed_macros,
        )?
    } else if matches!(member_name, "map" | "and_then" | "filter_map")
        && matches!(closure_node.kind(), "scoped_identifier" | "identifier")
    {
        (
            static_closure_projection(closure_node, member_name, item_expr, source),
            None,
            false,
        )
    } else {
        return None;
    };
    if member_name == "with" {
        return Some(body_sub);
    }
    if matches!(member_name, "filter_map" | "flat_map" | "and_then") {
        if matches!(body_sub, ValueExpr::Unknown) || !value_expr_within_depth(&body_sub, 0) {
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

fn eval_closure_projection_body(
    closure_node: Node<'_>,
    default_node: Option<Node<'_>>,
    member_name: &str,
    item_expr: &ValueExpr,
    source: &str,
    depth: usize,
    shadowed_macros: &HashSet<&'static str>,
) -> Option<(ValueExpr, Option<ValueExpr>, bool)> {
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
    let body_sub = substitute_closure_param(body_val, item_expr, &bindings);
    let default_val = default_node.map(|d| value_depth(d, source, depth + 2, shadowed_macros));
    let start_depth = if member_name == "with" { 0 } else { 2 };
    let body_ok = !matches!(body_sub, ValueExpr::Unknown)
        && value_expr_within_depth(&body_sub, start_depth);
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
    Some((
        if body_ok { body_sub } else { ValueExpr::Unknown },
        default_val,
        default_ok,
    ))
}
