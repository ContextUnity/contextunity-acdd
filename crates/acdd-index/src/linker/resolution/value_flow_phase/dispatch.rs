use super::super::context::*;
use super::candidates;
use super::context::{has_node_binding, PhaseContext};
use super::inferred::{self, RefQuery, ReferenceState};
use super::members;
use crate::languages::profile::{LanguageMemberResult, ProfileLinker};
use crate::linker::{
    semantic_context,
    types::{qualified, ReceiverType},
    value_flow,
};
use crate::models::{
    compact_graph::CompactGraph as Graph,
    tags::{CoverageStatus, EdgeConfidence, EdgeKind, NodeKind, ReferenceKind},
    Node, ReceiverHint, Reference,
};
use hashbrown::HashMap;
use std::borrow::Cow;

pub(super) fn resolve_reference<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    typed_parameters: &HashMap<&'a str, HashMap<&'a str, ReceiverType<'res>>>,
    r: &Reference,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    let owner = ctx
        .input
        .interner
        .lookup_node(ctx.input.by_id, r.source.as_str());
    let active_profile = ctx.reference_profile(r);
    let builtin_origin = active_profile.and_then(|profile| profile.linker().builtin_origin());
    let module_target = {
        let mut phase = ctx.reference_phase(graph, r, owner, active_profile);
        if active_profile.is_some_and(|profile| profile.linker().property_reference(&mut phase))
            || !configured_hint_is_effective(ctx, phase.graph, r)
            || active_profile.is_some_and(|profile| profile.linker().special_reference(&mut phase))
            || ctx.profile.is_some_and(|profile| {
                active_profile.is_none_or(|active| active.id() != profile.id())
                    && profile.linker().special_reference(&mut phase)
            })
        {
            return;
        }
        if members::resolve_computed_call(
            ctx.input,
            ctx.path,
            r,
            active_profile,
            builtin_origin,
            phase.graph,
        ) || resolve_configured_template_reference(ctx, phase.graph, r, owner)
        {
            return;
        }
        ctx.profile
            .and_then(|profile| profile.linker().explicit_module_target(profile, &phase))
    };

    let expression = if r.expression.contains("::") {
        Cow::Owned(r.expression.replace("::", "."))
    } else if ctx
        .profile
        .is_some_and(|profile| profile.linker().script_receiver_syntax())
        && r.expression.contains("?.")
    {
        Cow::Owned(r.expression.replace("?.", "."))
    } else {
        Cow::Borrowed(r.expression.as_str())
    };
    let keyword_explicit = ctx
        .profile
        .is_some_and(|profile| profile.linker().explicit_module_prefix(&r.expression));
    let (first, tail) = expression.split_once('.').unwrap_or((&expression, ""));
    if !r.dynamic
        && matches!(
            r.kind.as_str(),
            k if k == EdgeKind::Calls.as_str() || k == EdgeKind::References.as_str()
        )
    {
        if let Some(symbol) = owner.and_then(|owner| {
            ctx.input
                .semantic_context
                .named_member(owner, &expression, r.position())
        }) {
            let member = match symbol {
                value_flow::Symbol::Callable(node)
                    if r.kind != EdgeKind::Calls.as_str()
                        || matches!(
                            node.kind.as_str(),
                            k if k == NodeKind::Function.as_str() || k == NodeKind::Method.as_str()
                        ) =>
                {
                    semantic_context::Member::Local(node)
                }
                value_flow::Symbol::Type(value_flow::TypeTarget::Local(node))
                    if r.kind == EdgeKind::References.as_str() =>
                {
                    semantic_context::Member::Local(node)
                }
                value_flow::Symbol::Ambiguous
                | value_flow::Symbol::Type(value_flow::TypeTarget::Ambiguous) => {
                    semantic_context::Member::Ambiguous
                }
                _ => semantic_context::Member::Unknown,
            };
            emit_inferred(graph, ctx.path, r, member, None);
            return;
        }
    }
    let implicit_receiver = active_profile
        .filter(|profile| profile.linker().script_receiver_syntax())
        .and_then(|profile| {
            profile.linker().implicit_receiver_evidence(
                first,
                tail,
                ctx.language_context,
                ctx.path,
                &ctx.facts.activation_signals,
            )
        });
    let static_implicit_reference = r.dynamic && implicit_receiver.is_some();
    if r.dynamic && !static_implicit_reference {
        resolve_dynamic_reference(ctx, graph, r, owner, candidates, lookup_key);
        return;
    }

    let mut scope = owner.map(|n| reference_scope(n, ctx.path)).unwrap_or("");
    let definition_scope = matches!(
        r.kind.as_str(),
        k if k == EdgeKind::Decorates.as_str()
            || k == EdgeKind::Inherits.as_str()
            || k == EdgeKind::Implements.as_str()
            || k == EdgeKind::Bases.as_str()
    );
    if definition_scope {
        scope = scope.rsplit_once('.').map_or("", |(p, _)| p);
    }

    let query = RefQuery {
        r,
        owner,
        active_profile,
        builtin_origin,
        scope,
        expression: &expression,
        first,
        tail,
        keyword_explicit,
        module_target: module_target.as_ref(),
    };

    if resolve_early_language_specials(ctx, graph, &query) {
        return;
    }

    if matches!(
        r.kind.as_str(),
        k if k == EdgeKind::Calls.as_str()
            || k == EdgeKind::Mutates.as_str()
            || k == EdgeKind::Handles.as_str()
    ) && ctx.is_type_only(first, owner, definition_scope)
    {
        graph.reference_coverage(
            ctx.path,
            r,
            CoverageStatus::Unresolved.as_str(),
            "type-only import cannot be used as a runtime callable".into(),
        );
        return;
    }
    if !tail.is_empty() && ctx.is_type_only(first, owner, definition_scope) {
        graph.reference_coverage(
            ctx.path,
            r,
            CoverageStatus::Unresolved.as_str(),
            "type-only import member cannot be referenced at runtime".into(),
        );
        return;
    }
    if r.kind == EdgeKind::Calls.as_str() {
        if let Some(evidence) = owner.and_then(|owner| {
            ctx.input
                .value_flow
                .configured_callable(owner, &expression, r.position())
        }) {
            graph.reference_coverage(
                ctx.path,
                r,
                CoverageStatus::External.as_str(),
                evidence.into(),
            );
            return;
        }
    }

    let mut state = collect_lexical_scope_state(
        ctx,
        typed_parameters,
        &query,
        definition_scope,
        candidates,
        lookup_key,
    );

    if check_implicit_receiver_external(ctx, graph, &query, implicit_receiver, &state, candidates) {
        return;
    }

    if inferred::resolve_inferred_receiver_member(ctx, graph, &query, &state) {
        return;
    }

    state.receiver_member = ctx.language_linker.implicit_receiver_member(
        &mut crate::linker::traits::ImplicitReceiver {
            phase: ctx.reference_phase(graph, r, owner, ctx.profile),
            visibility: crate::linker::traits::ImportVisibility {
                input: ctx.input,
                path: ctx.path,
                aliases: ctx.aliases,
                alias_import_positions: &ctx.alias_import_positions,
            },
            typed_receiver: match state.typed_receiver {
                Some(ReceiverType::Local(node)) => Some(node),
                _ => None,
            },
            known_receiver: state.known_receiver,
            shadowed: state.shadowed,
            imported: state.imported,
            lookup: lookup_key,
        },
    );
    if state.shadowed
        && !state.known_receiver
        && !state
            .typed_receiver
            .is_some_and(|receiver| !matches!(receiver, ReceiverType::Unknown))
        && matches!(
            r.kind.as_str(),
            k if k == EdgeKind::Calls.as_str()
                || k == EdgeKind::Handles.as_str()
                || k == EdgeKind::Mutates.as_str()
        )
    {
        if resolve_language_reference(ctx, graph, r, owner, true) {
            return;
        }
        graph.reference_coverage(
            ctx.path,
            r,
            CoverageStatus::Unresolved.as_str(),
            "callee is shadowed by a parameter or local binding of unknown callable identity"
                .into(),
        );
        return;
    }

    if inferred::resolve_lexical_or_imported_candidates(
        ctx, graph, &query, &mut state, candidates, lookup_key,
    ) {
        return;
    }

    candidates::finalize_and_emit(ctx, graph, &query, &state, candidates, lookup_key);
}

fn configured_hint_is_effective(
    ctx: &PhaseContext<'_, '_, '_>,
    graph: &mut Graph,
    r: &Reference,
) -> bool {
    let Some(
        ReceiverHint::ConfiguredReference { origin }
        | ReceiverHint::ConfiguredTypedMember { origin, .. }
        | ReceiverHint::ConfiguredMacroCallable { origin, .. },
    ) = &r.receiver_hint
    else {
        return true;
    };
    let effective = if r.kind == ReferenceKind::Injects.as_str() {
        ctx.language_context
            .di_rule_origins
            .values()
            .any(|candidate| candidate == origin)
    } else {
        ctx.language_context.syntax_rules.iter().any(|rule| {
            rule.pack == origin.pack
                && rule.id == origin.rule
                && ctx.language_context.is_syntax_rule_active(
                    rule,
                    ctx.path,
                    &ctx.facts.activation_signals,
                )
        })
    };
    if !effective {
        graph.reference_coverage(
            ctx.path,
            r,
            CoverageStatus::Unresolved.as_str(),
            "extraction rule is no longer effective".into(),
        );
    }
    effective
}

fn resolve_configured_template_reference<'a>(
    ctx: &PhaseContext<'a, '_, '_>,
    graph: &mut Graph,
    reference: &Reference,
    owner: Option<&'a Node>,
) -> bool {
    let mut phase = ctx.reference_phase(graph, reference, owner, ctx.profile);
    ctx.language_linker.template_tag_reference(&mut phase)
        || ctx.language_linker.configured_template_reference(
            &mut phase,
            ctx.language_context,
            &ctx.facts.activation_signals,
        )
}

fn resolve_dynamic_reference<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    r: &Reference,
    _owner: Option<&'a Node>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    if resolve_language_reference(ctx, graph, r, _owner, true) {
        return;
    }
    let (computed, literal_builtin) =
        ctx.language_linker
            .computed_receiver(&mut crate::linker::traits::DynamicReceiver {
                visibility: crate::linker::traits::ImportVisibility {
                    input: ctx.input,
                    path: ctx.path,
                    aliases: ctx.aliases,
                    alias_import_positions: &ctx.alias_import_positions,
                },
                file_module: ctx.file_module,
                reference: r,
                constructor_aliases: &ctx.path_constructor_aliases,
                candidates,
                lookup: lookup_key,
                resolver: ctx,
            });
    let (status, evidence): (&str, Cow<'static, str>) = match computed {
        LanguageMemberResult::Local(method)
            if matches!(
                method.kind.as_str(),
                k if k == NodeKind::Method.as_str() || k == NodeKind::Function.as_str()
            ) =>
        {
            graph.reference_edge(
                ctx.path,
                r,
                method.id.clone(),
                crate::models::tags::EdgeKind::parse(&r.kind),
                EdgeConfidence::Inferred,
            );
            (
                CoverageStatus::Resolved.as_str(),
                format!(
                    "computed receiver has statically proven member {}",
                    method.qualname
                )
                .into(),
            )
        }
        LanguageMemberResult::External {
            module,
            import_line: line,
            provider: Some(provider),
            ..
        } => {
            let diagnostic_marker = if ctx
                .profile
                .is_some_and(|profile| profile.linker().framework_magic_diagnostic(profile, module))
            {
                "; diagnostic_reason=framework_magic"
            } else {
                ""
            };
            (
                CoverageStatus::External.as_str(),
                format!("{} of {provider}: {} via external import {module} at line {line}{diagnostic_marker}", ctx.language_linker.framework_member_label(), r.expression).into(),
            )
        }
        LanguageMemberResult::External {
            module,
            provider: None,
            ..
        } => (
            CoverageStatus::External.as_str(),
            format!("call through external import {module}; callable target unverified").into(),
        ),
        _ if literal_builtin => (
            CoverageStatus::Resolved.as_str(),
            format!("standard library or built-in callee: {}", r.expression).into(),
        ),
        _ => (
            CoverageStatus::Unresolved.as_str(),
            "computed receiver or dynamic callee; callable identity is unknown".into(),
        ),
    };
    graph.reference_coverage(ctx.path, r, status, evidence);
}

fn resolve_language_reference<'a>(
    ctx: &PhaseContext<'a, '_, '_>,
    graph: &mut Graph,
    reference: &Reference,
    owner: Option<&'a Node>,
    dynamic: bool,
) -> bool {
    let mut phase = ctx.reference_phase(graph, reference, owner, ctx.profile);
    if dynamic {
        ctx.language_linker.dynamic_reference(&mut phase)
    } else {
        ctx.language_linker.early_reference(&mut phase)
    }
}

fn resolve_early_language_specials<'a>(
    ctx: &PhaseContext<'a, '_, '_>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
) -> bool {
    if q.active_profile.is_some_and(|profile| {
        profile.linker().builtin_this_member(
            &ctx.reference_phase(graph, q.r, q.owner, q.active_profile),
            q.expression,
            &ctx.returned_object_field_writes,
        )
    }) {
        emit_inferred(
            graph,
            ctx.path,
            q.r,
            semantic_context::Member::Builtin,
            q.builtin_origin,
        );
        return true;
    }
    if q.active_profile.is_some_and(|profile| {
        profile
            .linker()
            .object_reference(&mut crate::linker::traits::ObjectReference {
                phase: ctx.reference_phase(graph, q.r, q.owner, q.active_profile),
                aliases: ctx.aliases,
                ownership: &|node| ctx.object_ownership(node),
                calls: &ctx.lexical_this_calls,
                language_context: ctx.language_context,
                activation_signals: &ctx.facts.activation_signals,
            })
    }) {
        return true;
    }
    if q.active_profile.is_some_and(|profile| {
        profile.linker().lexical_this_member(
            &mut ctx.reference_phase(graph, q.r, q.owner, q.active_profile),
            q.expression,
            &ctx.lexical_this_calls,
        )
    }) {
        return true;
    }
    if q.r.kind == EdgeKind::Calls.as_str() && q.tail.is_empty() {
        if let Some(evidence) = ctx
            .language_linker
            .macro_callable_origin(q.r, ctx.language_context)
        {
            graph.reference_coverage(
                ctx.path,
                q.r,
                CoverageStatus::External.as_str(),
                evidence.into(),
            );
            return true;
        }
    }
    if q.tail.is_empty()
        && ctx.language_linker.template_lexical_reference(
            &mut ctx.reference_phase(graph, q.r, q.owner, q.active_profile),
            q.first,
            ctx.component_module,
        )
    {
        return true;
    }
    if resolve_language_reference(ctx, graph, q.r, q.owner, false) {
        return true;
    }
    if q.r.kind != EdgeKind::Imports.as_str()
        && matches!(
            q.r.receiver_hint.as_ref(),
            Some(ReceiverHint::TypeOnlyImport)
        )
    {
        graph.reference_coverage(
            ctx.path,
            q.r,
            CoverageStatus::Unresolved.as_str(),
            "type-only import cannot be referenced at runtime".into(),
        );
        return true;
    }
    false
}

fn collect_lexical_scope_state<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    typed_parameters: &HashMap<&'a str, HashMap<&'a str, ReceiverType<'res>>>,
    q: &RefQuery<'a, '_>,
    definition_scope: bool,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) -> ReferenceState<'res>
where
    'a: 'file,
    'file: 'res,
{
    let mut scope = q.scope;
    let mut shadowed = false;
    let mut typed_receiver = None;
    let mut lexical_head_declared = false;
    let builtin_member_candidate = !q.tail.is_empty() && ctx.builtin_in_scope(q.expression, q.r);
    let imported = ctx
        .alias_for(q.first, q.owner, definition_scope)
        .filter(|_| {
            ctx.language_linker.import_visible_at(
                &crate::linker::traits::ImportVisibility {
                    input: ctx.input,
                    path: ctx.path,
                    aliases: ctx.aliases,
                    alias_import_positions: &ctx.alias_import_positions,
                },
                q.r,
                definition_scope,
            )
        });
    candidates.clear();
    while !scope.is_empty() {
        if let Some(scope_nodes) = ctx.input.by_qual.get(scope) {
            if !ctx.profile.is_some_and(|p| p.class_scope())
                && !definition_scope
                && ((q.owner.is_some_and(|n| {
                    matches!(
                        n.kind.as_str(),
                        k if k == NodeKind::Function.as_str() || k == NodeKind::Method.as_str()
                    )
                }) && scope_nodes.iter().any(|n| {
                    n.path == *ctx.path
                        && matches!(
                            n.kind.as_str(),
                            k if k == NodeKind::Class.as_str() || k == NodeKind::Impl.as_str()
                        )
                })) || (q.r.kind == EdgeKind::Calls.as_str()
                    && q.tail.is_empty()
                    && scope_nodes.iter().any(|n| {
                        n.path == *ctx.path
                            && matches!(
                                n.kind.as_str(),
                                k if k == NodeKind::Struct.as_str() || k == NodeKind::Enum.as_str()
                            )
                    })))
            {
                scope = scope.rsplit_once('.').map_or("", |(p, _)| p);
                continue;
            }
            if ctx.profile.is_none_or(|p| p.value_binding_applies(q.r)) {
                let mut binding_owners = scope_nodes.iter().filter(|node| {
                    let node_flow =
                        ctx.input.interner.lookup_symbol(&node.id).and_then(|id| {
                            ctx.input.flows.and_then(|flows| flows.get(&id)).copied()
                        });
                    node.path == *ctx.path
                        && has_node_binding(
                            ctx.input.interner,
                            ctx.input.node_bindings,
                            node.id.as_str(),
                            q.first,
                        )
                        && ctx
                            .language_linker
                            .lexical_binding_active(q.r, node, node_flow, q.first)
                        && !q.owner.is_some_and(|owner| {
                            ctx.input.semantic_context.admits_commonjs_binding(
                                node,
                                q.first,
                                owner,
                                q.r.position(),
                            )
                        })
                });
                if let Some(binding_owner) = binding_owners.next() {
                    shadowed = true;
                    if !q.tail.is_empty() && binding_owners.next().is_none() {
                        typed_receiver = typed_parameters
                            .get(binding_owner.id.as_str())
                            .and_then(|parameters| parameters.get(q.first));
                    }
                    break;
                }
            }
        }
        if ctx
            .aliases
            .get(scope)
            .is_some_and(|a| a.contains_key(q.first))
        {
            break;
        }
        if q.tail.is_empty() {
            candidates.extend(
                ctx.lookup(qualified(lookup_key, scope, q.expression))
                    .iter()
                    .copied()
                    .filter(|n| {
                        n.path == *ctx.path && ctx.language_linker.admits_lexical_candidate(n)
                    }),
            );
            lexical_head_declared |= !candidates.is_empty();
            if !candidates.is_empty() {
                break;
            }
        } else if builtin_member_candidate {
            lexical_head_declared |= ctx
                .lookup(qualified(lookup_key, scope, q.first))
                .iter()
                .any(|n| n.path == *ctx.path && ctx.language_linker.admits_lexical_candidate(n));
        }
        scope = scope.rsplit_once('.').map_or("", |(p, _)| p);
    }
    let known_receiver = !q.tail.is_empty()
        && q.owner.is_some_and(|n| {
            ctx.profile
                .is_some_and(|p| ctx.language_linker.resolve_receiver(p, q.first, n))
        })
        && q.owner.is_some_and(|n| {
            if has_node_binding(
                ctx.input.interner,
                ctx.input.node_rebindings,
                n.id.as_str(),
                q.first,
            ) {
                return false;
            }
            n.qualname.rsplit_once('.').is_some_and(|(scope, _)| {
                ctx.input.by_qual.get(scope).is_some_and(|nodes| {
                    nodes.iter().any(|n| {
                        n.path == *ctx.path
                            && matches!(
                                n.kind.as_str(),
                                k if k == NodeKind::Class.as_str()
                                    || k == NodeKind::Impl.as_str()
                            )
                    })
                })
            })
        });
    ReferenceState {
        shadowed,
        typed_receiver: typed_receiver.copied(),
        lexical_head_declared,
        known_receiver,
        imported,
        definition_scope,
        receiver_member: None,
        explicit_address: false,
        scope,
    }
}

fn check_implicit_receiver_external<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    implicit_receiver: Option<(&str, String)>,
    state: &ReferenceState<'res>,
    candidates: &[&'res Node],
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let Some((implicit_receiver, evidence)) = implicit_receiver.filter(|_| {
        !state.shadowed
            && state.imported.is_none()
            && !state.lexical_head_declared
            && candidates.is_empty()
    }) else {
        return false;
    };
    let locally_owned_member = q
        .active_profile
        .is_some_and(|profile| profile.linker().is_implicit_self(q.first))
        && q.owner.is_some_and(|owner| {
            let receiver = state
                .known_receiver
                .then(|| ctx.local_receiver_class(owner))
                .flatten()
                .map(value_flow::TypeTarget::Local);
            (state.known_receiver && receiver.is_none())
                || receiver
                    .as_ref()
                    .or_else(|| ctx.input.value_flow.lookup(owner, q.first, q.r.position()))
                    .is_some_and(|receiver| {
                        matches!(
                            receiver,
                            value_flow::TypeTarget::Unknown
                                | value_flow::TypeTarget::Ambiguous
                                | value_flow::TypeTarget::Candidates { .. }
                        ) || !matches!(
                            ctx.input.semantic_context.lookup_member_mode(
                                receiver,
                                implicit_receiver
                                    .split('.')
                                    .next()
                                    .unwrap_or(implicit_receiver),
                                owner,
                                false
                            ),
                            semantic_context::Member::Unknown
                        )
                    })
        });
    if !locally_owned_member {
        graph.reference_coverage(
            ctx.path,
            q.r,
            CoverageStatus::External.as_str(),
            evidence.into(),
        );
        return true;
    }
    false
}
