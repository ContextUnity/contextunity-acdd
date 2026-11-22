use super::super::value_flow::{
    node_position, resolve_type, symbol_type, ReadTrace, SemanticLimits, SemanticResolver, Symbol,
    SymbolRole, TypeTarget, ValueFlowIndex,
};
use crate::ast::strip_outer_parentheses;
use crate::models::Node;
use crate::semantic::{SourcePosition, TypeExpr};

pub(in crate::linker) fn literal_type<'a>(
    owner: &Node,
    expression: &str,
) -> Option<TypeTarget<'a>> {
    if owner.language != "python" {
        return None;
    }
    match expression {
        "\"\"" => Some(TypeTarget::Builtin("str".to_owned())),
        "b\"\"" => Some(TypeTarget::Builtin("bytes".to_owned())),
        _ => crate::semantic::python_literal_type(expression)
            .map(|name| TypeTarget::Builtin(name.to_owned())),
    }
}

pub(in crate::linker) fn constructor_result<'a>(
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<TypeTarget<'a>> {
    if owner.language != "python" {
        return None;
    }
    if let Symbol::Type(target @ (TypeTarget::Local(_) | TypeTarget::Builtin(_))) =
        resolver.resolve_symbol(owner, callee, at, SymbolRole::Constructor)
    {
        return Some(target);
    }
    resolver.resolve_external_factory(owner, callee, at)
}

pub(in crate::linker) fn constructor_symbol<'a>(
    owner: &'a Node,
    expression: &str,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
) -> Option<Symbol<'a>> {
    (owner.language == "python")
        .then(|| resolver.resolve_symbol(owner, expression, at, SymbolRole::Constructor))
}

pub(in crate::linker) fn dotted_member_result<'a>(
    index: &ValueFlowIndex<'a>,
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
    trace: Option<&ReadTrace<'a, '_>>,
) -> Option<TypeTarget<'a>> {
    if owner.language != "python" {
        return None;
    }
    let mut split = callee.rsplit_once('.');
    while let Some((prefix, _member)) = split {
        let receiver = index
            .lookup_tracked(owner, prefix, at, trace)
            .cloned()
            .unwrap_or_else(|| {
                symbol_type(resolver.resolve_symbol(owner, prefix, at, SymbolRole::Type))
            });
        if !matches!(receiver, TypeTarget::Unknown | TypeTarget::Ambiguous) {
            return Some(
                resolver
                    .resolve_member_result(&receiver, &callee[prefix.len() + 1..], owner, at)
                    .unwrap_or(TypeTarget::Unknown),
            );
        }
        split = prefix.rsplit_once('.');
    }
    None
}

pub(in crate::linker) fn collection_return_element<'a>(
    owner: &'a Node,
    expression: &TypeExpr,
    resolver: &impl SemanticResolver<'a>,
    limits: SemanticLimits,
) -> Option<TypeTarget<'a>> {
    let TypeExpr::Applied { base, args } = expression else {
        return None;
    };
    if !matches!(base.as_str(), "list" | "List" | "Sequence" | "Iterable") || args.len() != 1 {
        return None;
    }
    let element = resolve_type(owner, &args[0], node_position(owner), resolver, limits);
    (!matches!(element, TypeTarget::Unknown | TypeTarget::Ambiguous)).then_some(element)
}

pub(in crate::linker) fn typed_dict_class<'a>(
    node: &'a Node,
    resolver: &impl SemanticResolver<'a>,
) -> bool {
    let Some(bases) = node.details["bases"]
        .as_str()
        .and_then(|bases| bases.strip_prefix('('))
        .and_then(|bases| bases.strip_suffix(')'))
    else {
        return false;
    };
    let (base, options) = bases
        .split_once(',')
        .map_or((bases, ""), |(base, options)| (base, options));
    if !options.is_empty()
        && !options.split_once('=').is_some_and(|(key, value)| {
            key.trim() == "total" && matches!(value.trim(), "False" | "True")
        })
    {
        return false;
    }
    let base = base.trim();
    if !matches!(
        base,
        "TypedDict" | "typing.TypedDict" | "typing_extensions.TypedDict"
    ) {
        return false;
    }
    matches!(
        resolver.resolve_symbol(node, base, node_position(node), SymbolRole::Type),
        Symbol::Type(TypeTarget::External { module, .. })
            if matches!(module.as_str(), "typing" | "typing_extensions")
    )
}

pub(in crate::linker) fn stored_type_expr(annotation: &str) -> Option<TypeExpr> {
    let annotation = strip_outer_parentheses(annotation.trim());
    #[cfg(feature = "lang-python")]
    let pipe_parts = crate::languages::python::value_flow::split_python_pipe_types(annotation);
    #[cfg(not(feature = "lang-python"))]
    let pipe_parts: Option<Vec<&str>> = None;
    if let Some(parts) = pipe_parts {
        if parts.len() >= 2 {
            let args = parts.into_iter().map(stored_type_argument).collect();
            return Some(TypeExpr::Applied {
                base: "Union".to_owned(),
                args,
            });
        }
    }
    let (base, rest) = annotation.split_once('[')?;
    let base = base.trim();
    let arguments = rest.strip_suffix(']')?;
    if base.is_empty() {
        return Some(TypeExpr::Unknown);
    }
    let mut depth = 0_usize;
    let mut start = 0_usize;
    let mut args = Vec::new();
    for (index, character) in arguments.char_indices() {
        match character {
            '[' => depth = depth.checked_add(1)?,
            ']' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                let argument = arguments[start..index].trim();
                if argument.is_empty() {
                    return None;
                }
                args.push(stored_type_argument(argument));
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let argument = arguments[start..].trim();
    if argument.is_empty() {
        return None;
    }
    args.push(stored_type_argument(argument));
    Some(TypeExpr::Applied {
        base: base.to_owned(),
        args,
    })
}

fn stored_type_argument(annotation: &str) -> TypeExpr {
    let unparenthesized = strip_outer_parentheses(annotation.trim());
    if unparenthesized.is_empty() {
        return TypeExpr::Unknown;
    }
    stored_type_expr(unparenthesized).unwrap_or_else(|| TypeExpr::Named {
        name: unparenthesized.to_owned(),
    })
}

fn is_pathlib_target(target: &TypeTarget<'_>) -> bool {
    match target {
        TypeTarget::Builtin(name) | TypeTarget::External { module: name, .. } => {
            name.starts_with("pathlib.")
        }
        _ => false,
    }
}

pub(in crate::linker) fn callable_target_return<'s, 'a>(
    index: &'s ValueFlowIndex<'a>,
    target: &'s TypeTarget<'a>,
    owner: &'a Node,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
    trace: Option<&ReadTrace<'a, '_>>,
) -> Option<&'s TypeTarget<'a>> {
    match target {
        TypeTarget::Callable(callable) => index.return_type_tracked(callable, trace),
        TypeTarget::CallableReturn(ret) => Some(ret.as_ref()),
        TypeTarget::ClassType(inner) => Some(inner.as_ref()),
        TypeTarget::Local(class) if owner.language == "python" && class.kind == "class" => {
            match resolver.resolve_member(target, "__call__", owner, at) {
                Symbol::Callable(call_fn) => index.return_type_tracked(call_fn, trace),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(in crate::linker) fn synthetic_property_result<'a>(
    owner: &'a Node,
    receiver: &TypeTarget<'a>,
    member: &str,
) -> Option<TypeTarget<'a>> {
    if owner.language != "python" {
        return None;
    }
    if is_pathlib_target(receiver) && member == "parent" {
        return Some(receiver.clone());
    }
    if let TypeTarget::ConfiguredReceiver {
        authority,
        origin,
        fallback,
        evidence,
    } = receiver
    {
        if let super::super::value_flow::ConfiguredReceiverAuthority::Hierarchy { pack, rule } =
            authority.as_ref()
        {
            let prefix = format!("{member}.");
            let members: Vec<String> = rule
                .members
                .iter()
                .filter_map(|m| m.strip_prefix(&prefix).map(str::to_owned))
                .collect();
            if !members.is_empty() {
                let returns = rule
                    .returns
                    .iter()
                    .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|rest| (rest.to_owned(), v.clone())))
                    .collect();
                let sub_rule = crate::languages::manifests::ClassHierarchyReceiverRule {
                    bases: rule.bases.clone(),
                    provider: rule.provider.clone(),
                    members,
                    returns,
                };
                return Some(TypeTarget::ConfiguredReceiver {
                    authority: std::sync::Arc::new(
                        super::super::value_flow::ConfiguredReceiverAuthority::Hierarchy {
                            pack: pack.clone(),
                            rule: std::sync::Arc::new(sub_rule),
                        },
                    ),
                    origin: origin.clone(),
                    fallback: fallback.clone(),
                    evidence: evidence.clone(),
                });
            }
        }
    }
    None
}

pub(in crate::linker) fn synthetic_member_result<'a>(
    index: &ValueFlowIndex<'a>,
    owner: &'a Node,
    receiver: &TypeTarget<'a>,
    callee: &str,
    at: SourcePosition,
    resolver: &impl SemanticResolver<'a>,
    trace: Option<&ReadTrace<'a, '_>>,
) -> Option<TypeTarget<'a>> {
    if owner.language != "python" {
        return None;
    }
    if callee == "__truediv__" && is_pathlib_target(receiver) {
        return Some(receiver.clone());
    }
    if callee == "__slice__"
        && matches!(
            receiver,
            TypeTarget::Builtin(name)
                if matches!(name.as_str(), "str" | "bytes" | "bytearray" | "list" | "tuple")
        )
    {
        return Some(receiver.clone());
    }
    if callee == "__call__" {
        return callable_target_return(index, receiver, owner, at, resolver, trace).cloned();
    }
    if matches!(callee, "__enter__" | "__aenter__")
        && !matches!(receiver, TypeTarget::Unknown | TypeTarget::Ambiguous)
    {
        if let Symbol::Callable(enter_fn) = resolver.resolve_member(receiver, callee, owner, at) {
            return Some(
                if callee == "__aenter__" {
                    index.async_return_type(enter_fn)
                } else {
                    index.return_type_tracked(enter_fn, trace)
                }
                .cloned()
                .unwrap_or(TypeTarget::Unknown),
            );
        }
        return Some(receiver.clone());
    }
    if is_pathlib_target(receiver) {
        let mut method = callee;
        while let Some(rest) = method.strip_prefix("parent.") {
            method = rest;
        }
        return match method {
            "parent" | "resolve" | "absolute" | "expanduser" | "joinpath" | "with_name"
            | "with_stem" | "with_suffix" => Some(receiver.clone()),
            "read_text" | "as_posix" | "as_uri" => Some(TypeTarget::Builtin("str".to_owned())),
            "read_bytes" => Some(TypeTarget::Builtin("bytes".to_owned())),
            _ => None,
        };
    }
    synthetic_property_result(owner, receiver, callee)
}
