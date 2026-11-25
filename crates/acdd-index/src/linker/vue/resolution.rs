use super::super::resolution::input::ResolutionInput;
use super::super::semantic_context::Member;
use crate::languages::LanguageContext;
use crate::models::{compact_graph::CompactGraph, Node, ReceiverHint, Reference};
use crate::semantic::ComponentMacroRole;

pub(in crate::linker) fn component_macro_callable_origin(
    reference: &Reference,
    context: &LanguageContext,
) -> Option<String> {
    let ReceiverHint::ConfiguredMacroCallable { origin, producer } =
        reference.receiver_hint.as_ref()?
    else {
        return None;
    };
    context
        .component_macro(producer)
        .filter(|(rule, role)| {
            rule.pack == origin.pack && rule.id == origin.rule && *role == ComponentMacroRole::Emits
        })
        .map(|(rule, _)| {
            format!(
                "{}; rule:{}:{} callable from this file's Vue script setup binding",
                rule.display
                    .builtin
                    .as_deref()
                    .unwrap_or("configured macro"),
                rule.pack,
                rule.id
            )
        })
}

pub(in crate::linker) fn is_component_template_member(
    reference: &Reference,
    owner: Option<&Node>,
    tail: &str,
) -> bool {
    reference.kind == "references"
        && !tail.is_empty()
        && owner.is_some_and(|owner| {
            owner.kind == crate::models::tags::NodeKind::TemplateScope
                && owner
                    .detail_entry(crate::models::details::NodeDetailKey::VUE_SLOT)
                    .is_object()
        })
}

fn lookup_slot_template_member<'a, 'aliases, A>(
    input: &ResolutionInput<'a>,
    binding_scope: &'a Node,
    first: &str,
    tail: &str,
    alias_for: &A,
) -> Option<Member<'a>>
where
    'a: 'aliases,
    A: Fn(&str, Option<&'a Node>, bool) -> Option<&'aliases Vec<&'a Node>>,
{
    let component = binding_scope.detail_entry(crate::models::details::NodeDetailKey::VUE_SLOT)
        ["component"]
        .as_str()?;
    let slot = binding_scope.detail_entry(crate::models::details::NodeDetailKey::VUE_SLOT)["slot"]
        .as_str()?;
    let [module] = alias_for(component, Some(binding_scope), false)
        .map(Vec::as_slice)
        .filter(|targets| targets.len() == 1)?
    else {
        return None;
    };
    if module.kind != crate::models::tags::NodeKind::Module {
        return None;
    }
    let receiver = input.value_flow.component_slot_type(module, slot, first)?;
    let result = input
        .semantic_context
        .lookup_component_template_field(receiver, tail);
    matches!(result, Member::Local(_) | Member::Ambiguous).then_some(result)
}

pub(in crate::linker) fn component_slot_member<'a, 'aliases, A>(
    input: &ResolutionInput<'a>,
    path: &str,
    owner: &'a Node,
    first: &str,
    tail: &str,
    alias_for: A,
) -> Option<Member<'a>>
where
    'a: 'aliases,
    A: Fn(&str, Option<&'a Node>, bool) -> Option<&'aliases Vec<&'a Node>>,
{
    let mut scope = owner.qualname.as_str();
    while !scope.is_empty() {
        let mut scopes = input
            .by_qual
            .get(scope)
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                node.path == path && node.kind == crate::models::tags::NodeKind::TemplateScope
            });
        if let (Some(binding_scope), None) = (scopes.next(), scopes.next()) {
            if binding_scope.binding_names().any(|name| name == first) {
                return lookup_slot_template_member(input, binding_scope, first, tail, &alias_for);
            }
        }
        scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
    }
    None
}

pub(in crate::linker) fn resolve_component_tag<'a>(
    input: &ResolutionInput<'a>,
    path: &str,
    reference: &Reference,
    owner: Option<&Node>,
    graph: &mut CompactGraph,
) -> bool {
    if reference.kind != "references"
        || !owner.is_some_and(|owner| owner.kind == crate::models::tags::NodeKind::TemplateScope)
        || !matches!(
            reference.receiver_hint.as_ref(),
            Some(ReceiverHint::VueComponentTag)
        )
    {
        return false;
    }

    match input
        .semantic_context
        .component_candidates(path, &reference.expression)
        .as_slice()
    {
        [component] => {
            graph.reference_edge(
                path,
                reference,
                component.id.clone(),
                crate::models::tags::EdgeKind::References,
                crate::models::tags::EdgeConfidence::Exact,
            );
            graph.reference_coverage(
                path,
                reference,
                "resolved",
                format!("configured components directory: {}", component.path).into(),
            );
        }
        candidates if candidates.len() > 1 => {
            graph.reference_coverage(
                path,
                reference,
                "ambiguous",
                "multiple configured components share this registered name".into(),
            );
        }
        _ => {
            if let Some(origin) = input
                .semantic_context
                .component_builtin(path, &reference.expression)
            {
                graph.reference_coverage(
                    path,
                    reference,
                    "external",
                    format!("{origin} component registered for this project").into(),
                );
            } else {
                graph.reference_coverage(
                    path,
                    reference,
                    "unresolved",
                    "unregistered Vue template component tag".into(),
                );
            }
        }
    }
    true
}
