use super::context::{has_node_binding, PhaseContext};
use crate::linker::{
    python, rust,
    types::{qualified, receiver_head, receiver_type_name, ReceiverType},
};
use crate::models::{
    tags::{LanguageId, NodeKind},
    Node,
};
use hashbrown::HashMap;

fn resolve_parameter_annotation_receiver<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    node: &'a Node,
    raw_annotation: &'a str,
    is_rust: bool,
    type_candidates: &mut Vec<&'a Node>,
    lookup_key: &mut String,
) -> Option<ReceiverType<'res>>
where
    'a: 'file,
{
    let annotation = receiver_type_name(raw_annotation)?;
    let raw_head = receiver_head(raw_annotation);
    let (head, member) = annotation.split_once('.').unwrap_or((&annotation, ""));
    type_candidates.clear();
    let mut scope = node
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    let mut annotation_shadowed = false;
    while !scope.is_empty() {
        if !is_rust
            && ctx
                .input
                .by_qual
                .get(scope)
                .is_some_and(|scope_nodes| {
                    scope_nodes.iter().any(|scope_node| {
                        scope_node.path == *ctx.path
                            && has_node_binding(
                                ctx.input.interner,
                                ctx.input.node_rebindings,
                                scope_node.id.as_str(),
                                head,
                            )
                    })
                })
        {
            annotation_shadowed = true;
            break;
        }
        type_candidates.extend(
            ctx.lookup(qualified(lookup_key, scope, &annotation))
                .iter()
                .copied()
                .filter(|candidate| candidate.path == *ctx.path),
        );
        if !type_candidates.is_empty()
            && ctx
                .alias_import_positions
                .get(&(scope, head))
                .is_some_and(|position| {
                    position.line <= node.line
                        && type_candidates
                            .iter()
                            .any(|candidate| candidate.line < position.line)
                })
        {
            type_candidates.clear();
            annotation_shadowed = true;
            break;
        }
        if !type_candidates.is_empty() {
            break;
        }
        if is_rust
            && !ctx
                .aliases
                .get(scope)
                .is_some_and(|entries| entries.contains_key(head))
            && ctx
                .lookup(qualified(lookup_key, scope, head))
                .iter()
                .any(|candidate| {
                    candidate.path == *ctx.path
                        && matches!(
                            candidate.kind.as_str(),
                            k if k == NodeKind::Module.as_str() || k == "module_declaration"
                        )
                })
        {
            annotation_shadowed = true;
            break;
        }
        if !is_rust
            && ctx
                .input
                .by_qual
                .get(scope)
                .is_some_and(|scope_nodes| {
                    scope_nodes
                        .iter()
                        .filter(|scope_node| scope_node.path == *ctx.path)
                        .any(|scope_node| {
                            has_node_binding(
                                ctx.input.interner,
                                ctx.input.node_bindings,
                                scope_node.id.as_str(),
                                head,
                            )
                        })
                })
        {
            annotation_shadowed = true;
            break;
        }
        if ctx
            .aliases
            .get(scope)
            .is_some_and(|entries| entries.contains_key(head))
        {
            break;
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    let imported_type = if annotation_shadowed || !type_candidates.is_empty() {
        None
    } else {
        ctx.alias_for(head, Some(node), true)
    };
    if let Some(imported) = imported_type {
        if member.is_empty() {
            type_candidates.extend_from_slice(imported);
        } else {
            for target in imported {
                type_candidates.extend(
                    ctx.lookup(qualified(lookup_key, &target.qualname, member))
                        .iter()
                        .copied()
                        .filter(|candidate| candidate.path == target.path),
                );
            }
        }
    }
    if type_candidates.is_empty()
        && imported_type.is_some_and(Vec::is_empty)
        && !annotation_shadowed
        && ctx.python_nullable_parameter_wrapper(raw_annotation, node)
    {
        return None;
    }
    type_candidates.retain(|candidate| {
        matches!(
            candidate.kind.as_str(),
            k if k == NodeKind::Class.as_str()
                || k == NodeKind::Struct.as_str()
                || k == NodeKind::Enum.as_str()
                || k == NodeKind::Type.as_str()
                || k == NodeKind::Interface.as_str()
        )
    });
    type_candidates.sort_by(|a, b| a.id.cmp(&b.id));
    type_candidates.dedup_by_key(|candidate| candidate.id.as_str());
    Some(resolve_parameter_receiver(
        ctx,
        ParameterAnnotationSpec {
            node,
            raw_annotation,
            annotation: &annotation,
            raw_head,
            head,
            member,
            is_rust,
            annotation_shadowed,
            imported_type,
        },
        type_candidates,
    ))
}

pub(super) fn collect_typed_parameters<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    lookup_key: &mut String,
) -> HashMap<&'a str, HashMap<&'a str, ReceiverType<'res>>>
where
    'a: 'file,
{
    let mut typed_parameters: HashMap<&'a str, HashMap<&'a str, ReceiverType<'res>>> =
        HashMap::new();
    let mut type_candidates = Vec::new();
    let is_rust = ctx
        .profile
        .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str());
    for node in &ctx.facts.nodes {
        for (parameter, annotation) in node.parameter_types() {
            if has_node_binding(
                ctx.input.interner,
                ctx.input.node_rebindings,
                node.id.as_str(),
                parameter,
            ) {
                continue;
            }
            let Some(raw_annotation) = annotation else {
                continue;
            };
            let Some(receiver) = resolve_parameter_annotation_receiver(
                ctx,
                node,
                raw_annotation,
                is_rust,
                &mut type_candidates,
                lookup_key,
            ) else {
                continue;
            };
            if !matches!(receiver, ReceiverType::Unknown) {
                typed_parameters
                    .entry(node.id.as_str())
                    .or_default()
                    .insert(parameter, receiver);
            }
        }
    }
    typed_parameters
}

struct ParameterAnnotationSpec<'a, 'res, 'p> {
    node: &'a Node,
    raw_annotation: &'a str,
    annotation: &'p str,
    raw_head: &'a str,
    head: &'p str,
    member: &'p str,
    is_rust: bool,
    annotation_shadowed: bool,
    imported_type: Option<&'res Vec<&'a Node>>,
}

fn resolve_parameter_receiver<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    spec: ParameterAnnotationSpec<'a, 'res, '_>,
    type_candidates: &[&'a Node],
) -> ReceiverType<'res>
where
    'a: 'file,
{
    #[cfg(feature = "lang-rust")]
    let qualified_external_annotation = || {
        rust::semantic_context::qualified_external_annotation(
            ctx.profile,
            spec.raw_annotation,
            spec.head,
            ctx.path,
            ctx.input.dependency_registry,
        )
    };
    #[cfg(not(feature = "lang-rust"))]
    let qualified_external_annotation = || None;
    #[cfg(feature = "lang-rust")]
    let external_annotation = || {
        rust::semantic_context::external_annotation_type(
            ctx.profile,
            spec.raw_head,
            spec.head,
            ctx.path,
            ctx.input.dependency_registry,
        )
    };
    #[cfg(not(feature = "lang-rust"))]
    let external_annotation = || None;

    match type_candidates {
        [target] => ReceiverType::Local(target),
        [] => {
            if spec.annotation_shadowed {
                ReceiverType::Unknown
            } else if spec.imported_type.is_some_and(|targets| targets.is_empty()) {
                ctx.external_for(spec.head, Some(spec.node), true)
                    .filter(|(module, import_line)| {
                        *import_line <= spec.node.line
                            && !python::resolution::is_dynamic_typing_annotation(
                                spec.head,
                                spec.member,
                                spec.node,
                                ctx.aliases,
                                &ctx.dynamic_type_aliases,
                            )
                            && !(spec.is_rust
                                && rust_has_non_lifetime_generics(spec.raw_annotation)
                                && !matches!(
                                    module.split("::").next(),
                                    Some("std" | "core" | "alloc")
                                )
                                && !ctx.profile.is_some_and(|p| p.builtin_generic(spec.head)))
                    })
                    .map(|(module, line)| ReceiverType::External(module.as_str(), *line))
                    .unwrap_or(ReceiverType::Unknown)
            } else if matches!(spec.head, "std" | "core" | "alloc") {
                match spec.head {
                    "std" => ReceiverType::External("std", 0),
                    "core" => ReceiverType::External("core", 0),
                    "alloc" => ReceiverType::External("alloc", 0),
                    _ => ReceiverType::Unknown,
                }
            } else if !spec.annotation_shadowed
                && spec.imported_type.is_none()
                && !spec.member.is_empty()
                && ctx.input.by_qual.get(spec.annotation).is_none()
            {
                qualified_external_annotation()
                    .or_else(external_annotation)
                    .unwrap_or(ReceiverType::Unknown)
            } else {
                external_annotation().unwrap_or(ReceiverType::Unknown)
            }
        }
        _ => ReceiverType::Ambiguous,
    }
}

fn rust_has_non_lifetime_generics(annotation: &str) -> bool {
    let Some(name) = crate::linker::types::strip_rust_references(annotation, true) else {
        return false;
    };
    let Some((_, rest)) = name.split_once('<') else {
        return false;
    };
    let Some(inner) = rest.strip_suffix('>') else {
        return false;
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| !part.starts_with('\''))
}
