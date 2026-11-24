use crate::ast::{relations, syntax::text};
use crate::models::Facts;
use crate::semantic::{CallableParameter, CallableSignature, TypeExpr};
use std::collections::HashSet;
use tree_sitter::Node;

pub(in super::super) fn type_expr(node: Node<'_>, source: &str) -> TypeExpr {
    let names = generic_names(node, source);
    type_expr_inner(node, source, &names, 0)
}

pub(super) fn type_expr_inner(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        return TypeExpr::Unknown;
    }
    match node.kind() {
        "reference_type" => reference_type_expr(node, source, generic_names, depth),
        "tuple_type" => tuple_type_expr(node, source, generic_names, depth),
        "array_type" | "slice_type" => array_slice_type_expr(node, source, generic_names, depth),
        "generic_type" => generic_type_expr(node, source, generic_names, depth),
        "bounded_type" => bounded_type_expr(node, source, generic_names, depth),
        "abstract_type" | "dynamic_type" => {
            if let Some(name) = trait_object_name(node, source) {
                return TypeExpr::Named { name };
            }
            abstract_or_dynamic_type_expr(node, source, generic_names, depth)
        }
        "type_identifier" | "scoped_type_identifier" | "primitive_type" => {
            if let Some(name) = trait_object_name(node, source) {
                return TypeExpr::Named { name };
            }
            let name = text(node, source).trim();
            if name != "_" && !generic_root(node, source, generic_names) {
                TypeExpr::Named {
                    name: name.to_owned(),
                }
            } else {
                TypeExpr::Unknown
            }
        }
        _ => trait_object_name(node, source)
            .map_or(TypeExpr::Unknown, |name| TypeExpr::Named { name }),
    }
}

fn reference_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
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
    TypeExpr::Applied {
        base: if mutable { "&mut" } else { "&" }.to_owned(),
        args: vec![type_expr_inner(inner, source, generic_names, depth + 1)],
    }
}

fn tuple_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    let mut cursor = node.walk();
    let args = node
        .named_children(&mut cursor)
        .map(|child| type_expr_inner(child, source, generic_names, depth + 1))
        .collect();
    TypeExpr::Applied {
        base: "()".to_owned(),
        args,
    }
}

fn array_slice_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    let Some(element) = node
        .child_by_field_name("element")
        .or_else(|| node.named_child(0))
    else {
        return TypeExpr::Unknown;
    };
    let fixed_array = node.kind() == "array_type" && node.child_by_field_name("length").is_some();
    TypeExpr::Applied {
        base: if fixed_array {
            "__rust_array_type"
        } else {
            "[]"
        }
        .to_owned(),
        args: vec![type_expr_inner(element, source, generic_names, depth + 1)],
    }
}

fn generic_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    let Some(base) = node
        .child_by_field_name("type")
        .and_then(|ty| static_name(ty, source))
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
    TypeExpr::Unknown
}

fn bounded_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "lifetime" {
            let ty = type_expr_inner(child, source, generic_names, depth + 1);
            if !matches!(ty, TypeExpr::Unknown) {
                return ty;
            }
        }
    }
    TypeExpr::Unknown
}

fn iterator_item_type(
    child: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> Option<TypeExpr> {
    let base_ok = child
        .child_by_field_name("type")
        .and_then(|ty| static_name(ty, source))
        .is_some_and(|name| {
            name == "Iterator"
                || name.ends_with("::Iterator")
                || name == "IntoIterator"
                || name.ends_with("::IntoIterator")
        });
    if !base_ok {
        return None;
    }
    let arguments = child.child_by_field_name("type_arguments")?;
    let mut arg_cursor = arguments.walk();
    for arg in arguments.named_children(&mut arg_cursor) {
        if arg.kind() != "type_binding" {
            continue;
        }
        let is_item = arg
            .child_by_field_name("name")
            .is_some_and(|n| text(n, source).trim() == "Item");
        if is_item {
            if let Some(ty) = arg.child_by_field_name("type") {
                return Some(TypeExpr::Applied {
                    base: "Iterator".to_owned(),
                    args: vec![type_expr_inner(ty, source, generic_names, depth + 1)],
                });
            }
        }
    }
    None
}

fn abstract_or_dynamic_type_expr(
    node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    depth: usize,
) -> TypeExpr {
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
        let Some(child) = check else {
            continue;
        };
        if child.kind() == "generic_type" {
            if let Some(iter_ty) = iterator_item_type(child, source, generic_names, depth) {
                return iter_ty;
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

pub(super) fn generic_root(mut node: Node<'_>, source: &str, names: &HashSet<&str>) -> bool {
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

pub(super) fn static_name(node: Node<'_>, source: &str) -> Option<String> {
    matches!(
        node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier" | "scoped_type_identifier"
    )
    .then(|| text(node, source).trim().to_owned())
}

pub(super) fn type_expr_has_unknown(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Unknown => true,
        TypeExpr::Named { .. } => false,
        TypeExpr::Applied { args, .. } => args.iter().any(type_expr_has_unknown),
    }
}

pub(in super::super) fn is_async(node: Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    let is_async = node.named_children(&mut cursor).any(|child| {
        child.kind() == "function_modifiers"
            && text(child, source)
                .split_whitespace()
                .any(|modifier| modifier == "async")
    });
    is_async
}

pub(super) fn generic_names<'a>(node: Node<'_>, source: &'a str) -> HashSet<&'a str> {
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

pub(in super::super) fn type_references(
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
            "lifetime" | "lifetime_parameter" | "attribute_item" | "line_comment" | "block_comment" => {}
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

/// Scans generic trait bounds declared on `param_name` across `type_parameters`
/// and `where_clause` of `fn_node`.
fn find_generic_param_trait_bounds<'a, R>(
    fn_node: Node<'a>,
    param_name: &str,
    source: &str,
    mut visit: impl FnMut(Node<'a>) -> Option<R>,
) -> Option<R> {
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
            if name == Some(param_name) {
                if let Some(res) = visit_bounds_container(param, &mut visit) {
                    return Some(res);
                }
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
            let left = pred
                .child_by_field_name("left")
                .or_else(|| pred.named_child(0));
            if left.is_some_and(|l| text(l, source).trim() == param_name) {
                if let Some(res) = visit_bounds_container(pred, &mut visit) {
                    return Some(res);
                }
            }
        }
    }
    None
}

fn visit_bounds_container<'a, R>(
    node: Node<'a>,
    visit: &mut impl FnMut(Node<'a>) -> Option<R>,
) -> Option<R> {
    if let Some(bounds) = node.child_by_field_name("bounds") {
        return visit(bounds);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "trait_bounds" {
            if let Some(res) = visit(child) {
                return Some(res);
            }
        }
    }
    None
}

pub(super) fn generic_param_serializer_bound(
    fn_node: Node<'_>,
    param_name: &str,
    source: &str,
) -> bool {
    find_generic_param_trait_bounds(fn_node, param_name, source, |bounds| {
        let mut cursor = bounds.walk();
        let found = bounds.named_children(&mut cursor).any(|bound| {
            let txt = text(bound, source).trim();
            txt == "Serializer" || txt.ends_with("::Serializer")
        });
        found.then_some(())
    })
    .is_some()
}

fn extract_function_type_node<'a>(ty: Node<'a>, source: &str) -> Option<Node<'a>> {
    match ty.kind() {
        "function_type" => Some(ty),
        "reference_type" | "higher_ranked_trait_bound" => ty
            .child_by_field_name("type")
            .and_then(|inner| extract_function_type_node(inner, source)),
        "abstract_type" | "dynamic_type" => ty
            .child_by_field_name("trait")
            .and_then(|inner| extract_function_type_node(inner, source))
            .or_else(|| {
                let mut cursor = ty.walk();
                let found = ty
                    .named_children(&mut cursor)
                    .find_map(|child| extract_function_type_node(child, source));
                found
            }),
        "bounded_type" | "trait_bounds" | "parenthesized_type" => {
            let mut cursor = ty.walk();
            let found = ty
                .named_children(&mut cursor)
                .filter(|c| c.kind() != "lifetime")
                .find_map(|child| extract_function_type_node(child, source));
            found
        }
        "generic_type" => {
            let base = ty
                .child_by_field_name("type")
                .and_then(|base| static_name(base, source))?;
            let short = base.rsplit("::").next().unwrap_or(base.as_str());
            if !matches!(short, "Box" | "Arc" | "Rc" | "Option" | "Fn" | "FnMut" | "FnOnce") {
                return None;
            }
            let args = ty.child_by_field_name("type_arguments")?;
            let mut cursor = args.walk();
            let found = args
                .named_children(&mut cursor)
                .find_map(|arg| extract_function_type_node(arg, source));
            found
        }
        _ => None,
    }
}

fn collect_function_type_params(
    fn_type: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
) -> Vec<TypeExpr> {
    let Some(params) = fn_type.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut cursor = params.walk();
    params
        .named_children(&mut cursor)
        .take(32)
        .map(|param| {
            let ty_node = if param.kind() == "parameter" {
                param.child_by_field_name("type")
            } else {
                Some(param)
            };
            ty_node.map_or(TypeExpr::Unknown, |node| {
                type_expr_inner(node, source, generic_names, 0)
            })
        })
        .collect()
}

/// Extracts higher-order callback metadata (`is_callable`, `callback_params`)
/// for a parameter type node, resolving direct `fn(...)`, `impl/dyn Fn*`, boxed
/// trait objects, and `<F: Fn*>` / `where F: Fn*` bounds.
pub(super) fn inspect_callable_param_type(
    fn_node: Node<'_>,
    annotation: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
) -> (bool, Vec<TypeExpr>) {
    if let Some(fn_type) = extract_function_type_node(annotation, source) {
        return (true, collect_function_type_params(fn_type, source, generic_names));
    }
    let mut base = annotation;
    while base.kind() == "reference_type" {
        let Some(inner) = base.child_by_field_name("type") else {
            break;
        };
        base = inner;
    }
    if base.kind() == "type_identifier" {
        let param_ty_name = text(base, source).trim();
        if generic_names.contains(param_ty_name) {
            if let Some(cb_params) = find_generic_param_trait_bounds(
                fn_node,
                param_ty_name,
                source,
                |bounds| {
                    extract_function_type_node(bounds, source).map(|fn_type| {
                        collect_function_type_params(fn_type, source, generic_names)
                    })
                },
            ) {
                return (true, cb_params);
            }
        }
    }
    (false, Vec::new())
}

pub(super) fn extract_callable_signature(
    fn_node: Node<'_>,
    source: &str,
    generic_names: &HashSet<&str>,
    return_type: Option<TypeExpr>,
) -> Option<CallableSignature> {
    if !matches!(fn_node.kind(), "function_item" | "function_signature_item") {
        return None;
    }
    let parameters = fn_node.child_by_field_name("parameters")?;
    let mut has_receiver = false;
    let mut has_callback_param = false;
    let mut params = Vec::new();
    let mut cursor = parameters.walk();
    for param in parameters.named_children(&mut cursor) {
        if param.kind() == "self_parameter" {
            has_receiver = true;
            continue;
        }
        if params.len() >= 128 {
            break;
        }
        let name = param
            .child_by_field_name("pattern")
            .and_then(super::expr::pattern_identifier)
            .map(|id| text(id, source).trim().to_owned());
        let (type_expr, callback_params) = param
            .child_by_field_name("type")
            .map(|ty| {
                let (is_callable, cb) =
                    inspect_callable_param_type(fn_node, ty, source, generic_names);
                if is_callable && !cb.is_empty() {
                    has_callback_param = true;
                }
                (type_expr_inner(ty, source, generic_names, 0), cb)
            })
            .unwrap_or((TypeExpr::Unknown, Vec::new()));
        params.push(CallableParameter {
            name,
            type_expr,
            callback_params,
        });
    }
    if !has_callback_param {
        return None;
    }
    let sig = CallableSignature {
        has_receiver,
        params,
        return_type,
    };
    sig.valid().then_some(sig)
}

pub(crate) fn parse_rust_type_text(raw: &str, depth: usize) -> TypeExpr {
    if depth >= 6 {
        return TypeExpr::Unknown;
    }
    let s = raw.trim();
    if s.is_empty() {
        return TypeExpr::Unknown;
    }
    if let Some(inner) = s.strip_prefix('&') {
        let mut inner = inner.trim_start();
        if let Some(rest) = inner.strip_prefix('\'') {
            inner = rest
                .split_once(char::is_whitespace)
                .map_or("", |(_, tail)| tail)
                .trim_start();
        }
        let mutable = inner.starts_with("mut ");
        let inner = inner.strip_prefix("mut ").unwrap_or(inner).trim_start();
        return TypeExpr::Applied {
            base: if mutable { "&mut" } else { "&" }.to_owned(),
            args: vec![parse_rust_type_text(inner, depth + 1)],
        };
    }
    if let Some((base, rest)) = s.split_once('<') {
        if let Some(inner) = rest.strip_suffix('>') {
            let base = base.trim();
            let args = parse_generic_text_args(inner, depth);
            if !base.is_empty() {
                return if args.is_empty() {
                    TypeExpr::Named {
                        name: base.to_owned(),
                    }
                } else {
                    TypeExpr::Applied {
                        base: base.to_owned(),
                        args,
                    }
                };
            }
        }
    }
    let s = s.strip_prefix("dyn ").map_or(s, str::trim_start);
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':'))
    {
        TypeExpr::Named { name: s.to_owned() }
    } else {
        TypeExpr::Unknown
    }
}

fn parse_generic_text_args(inner: &str, depth: usize) -> Vec<TypeExpr> {
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
    args
}
