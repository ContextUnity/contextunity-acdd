use super::context::{has_node_binding, PhaseContext};
use crate::languages::profile::LanguageLinker;
use crate::linker::types::{qualified, receiver_head, receiver_type_name, ReceiverType};
use crate::models::{tags::NodeKind, Node};
use hashbrown::HashMap;

fn resolve_parameter_annotation_receiver<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    node: &'a Node,
    raw_annotation: &'a str,
    language_linker: &dyn LanguageLinker,
    type_candidates: &mut Vec<&'a Node>,
    lookup_key: &mut String,
) -> Option<ReceiverType<'res>>
where
    'a: 'file,
{
    let annotation = receiver_type_name(raw_annotation)?;
    let raw_head = receiver_head(raw_annotation);
    let (head, member) = annotation.split_once('.').unwrap_or((&annotation, ""));
    let check_binding_shadowing = language_linker.checks_annotation_binding_shadowing();
    let module_shadows = language_linker.module_shadows_annotation();
    type_candidates.clear();
    let mut scope = node
        .qualname
        .rsplit_once('.')
        .map_or("", |(parent, _)| parent);
    let mut annotation_shadowed = false;
    while !scope.is_empty() {
        if check_binding_shadowing
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
        if crate::linker::types::shadowed_by_late_import(&ctx.alias_import_positions, scope, head, node.line, type_candidates) {
            annotation_shadowed = true;
            break;
        }
        if !type_candidates.is_empty() {
            break;
        }
        if module_shadows
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
        if check_binding_shadowing
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
        && ctx.nullable_parameter_wrapper(raw_annotation, node)
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
            language_linker,
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
                ctx.language_linker,
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
    language_linker: &'p dyn LanguageLinker,
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
    match type_candidates {
        [target] => ReceiverType::Local(target),
        [] => {
            if spec.annotation_shadowed {
                ReceiverType::Unknown
            } else if spec.imported_type.is_some_and(|targets| targets.is_empty()) {
                ctx.external_for(spec.head, Some(spec.node), true)
                    .filter(|(module, import_line)| {
                        *import_line <= spec.node.line
                            && !spec.language_linker.is_dynamic_typing_annotation(
                                spec.head,
                                spec.member,
                                spec.node,
                                ctx.aliases,
                                &ctx.dynamic_type_aliases,
                            )
                            && spec.language_linker.admits_external_generic_annotation(
                                ctx.profile,
                                spec.raw_annotation,
                                spec.head,
                                module,
                            )
                    })
                    .map(|(module, line)| ReceiverType::External(module.as_str(), *line))
                    .unwrap_or(ReceiverType::Unknown)
            } else {
                let try_qualified = !spec.annotation_shadowed
                    && spec.imported_type.is_none()
                    && !spec.member.is_empty()
                    && ctx.input.by_qual.get(spec.annotation).is_none();
                spec.language_linker
                    .external_parameter_annotation(
                        ctx.profile,
                        spec.raw_annotation,
                        spec.raw_head,
                        spec.head,
                        spec.member,
                        ctx.path,
                        ctx.input.dependency_registry,
                        try_qualified,
                    )
                    .unwrap_or(ReceiverType::Unknown)
            }
        }
        _ => ReceiverType::Ambiguous,
    }
}
