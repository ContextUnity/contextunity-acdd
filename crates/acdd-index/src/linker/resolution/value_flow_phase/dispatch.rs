use super::candidates;
use super::context::{has_node_binding, PhaseContext};
use super::inferred::{self, RefQuery, ReferenceState};
use super::members;
use super::*;
use crate::models::tags::{CoverageStatus, EdgeConfidence, EdgeKind, LanguageId, NodeKind, ReferenceKind};

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
    let owner = ctx.input.interner.lookup_node(ctx.input.by_id, r.source.as_str());
    let active_profile = ctx.reference_profile(r);
    if let Some((result, emit)) = super::super::typescript::resolution::property_reference(
        ctx.input,
        r,
        &ctx.ordinary_member_receivers,
        active_profile,
    ) {
        if emit {
            emit_inferred(graph, ctx.path, r, result, None);
        }
        return;
    }
    if !configured_hint_is_effective(ctx, graph, r) {
        return;
    }
    #[cfg(feature = "lang-rust")]
    if super::super::rust::resolution::trait_object_call(
        active_profile,
        r,
        owner,
        ctx.path,
        ctx.input.value_flow,
        ctx.input.semantic_context,
        graph,
    ) {
        return;
    }
    let builtin_origin = match active_profile.map(|profile| profile.family().0) {
        Some("rust") => Some("Rust standard library"),
        Some("python") => Some("Python standard library"),
        Some("typescript" | "javascript") => Some("JavaScript/Web platform"),
        _ => None,
    };
    #[cfg(feature = "lang-html")]
    if html::templates::resolve_reference(
        ctx.input,
        &ctx.linker.html_templates,
        ctx.path,
        ctx.template_loads,
        ctx.template_registry_rules,
        r,
        graph,
    ) {
        return;
    }
    if members::resolve_computed_call(ctx.input, ctx.path, r, active_profile, builtin_origin, graph) {
        return;
    }
    if resolve_vue_template_typed_member(ctx, graph, r, owner) {
        return;
    }

    let expression = if r.expression.contains("::") {
        Cow::Owned(r.expression.replace("::", "."))
    } else if ctx.profile.is_some_and(|profile| {
        matches!(
            profile.id(),
            p if p == LanguageId::Javascript.as_str()
                || p == LanguageId::Typescript.as_str()
                || p == LanguageId::Vue.as_str()
                || p == LanguageId::Html.as_str()
        )
    }) && r.expression.contains("?.")
    {
        Cow::Owned(r.expression.replace("?.", "."))
    } else {
        Cow::Borrowed(r.expression.as_str())
    };
    #[cfg(feature = "lang-rust")]
    let rust_keyword_explicit = ctx.profile.is_some_and(|profile| {
        profile.id() == LanguageId::Rust.as_str()
            && ["super::", "crate::", "self::"]
                .iter()
                .any(|prefix| r.expression.starts_with(prefix))
    });
    #[cfg(not(feature = "lang-rust"))]
    let rust_keyword_explicit = false;
    #[cfg(feature = "lang-rust")]
    let rust_module_target = super::super::rust::resolution::rust_explicit_module_target(
        ctx.input,
        ctx.profile,
        ctx.path,
        ctx.file_module,
        &r.expression,
    );
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
    #[cfg(feature = "lang-typescript")]
    let implicit_receiver = if first == "this" {
        tail
    } else if tail.is_empty() {
        first
    } else {
        ""
    };
    #[cfg(feature = "lang-typescript")]
    let static_implicit_reference = r.dynamic
        && active_profile.is_some_and(|profile| {
            matches!(
                profile.id(),
                p if p == LanguageId::Javascript.as_str()
                    || p == LanguageId::Typescript.as_str()
                    || p == LanguageId::Vue.as_str()
                    || p == LanguageId::Html.as_str()
            )
        })
        && !implicit_receiver.is_empty()
        && ctx.language_context.syntax_rules.iter().any(|rule| {
            let languages::manifests::syntax::SyntaxOperation::ImplicitReceivers { names } =
                &rule.operation
            else {
                return false;
            };
            ctx.language_context
                .is_syntax_rule_active(rule, ctx.path, &ctx.facts.activation_signals)
                && names.iter().any(|name| name == implicit_receiver)
        });
    #[cfg(not(feature = "lang-typescript"))]
    let static_implicit_reference = false;
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
        rust_keyword_explicit,
        #[cfg(feature = "lang-rust")]
        rust_module_target: rust_module_target.as_ref(),
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

    #[cfg(feature = "lang-typescript")]
    if check_implicit_receiver_external(ctx, graph, &query, implicit_receiver, &state, candidates) {
        return;
    }

    if inferred::resolve_inferred_receiver_member(ctx, graph, &query, &state) {
        return;
    }

    state.receiver_member = python::resolution::implicit_receiver_member(
        &ctx.python_resolution(),
        r,
        state.typed_receiver,
        state.known_receiver,
        state.shadowed,
        state.imported,
        lookup_key,
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
        #[cfg(feature = "lang-rust")]
        if ctx
            .profile
            .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
            && rust::resolution::dynamic_receiver_member(ctx.input, ctx.path, r, owner, graph)
        {
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
        ctx,
        graph,
        &query,
        &mut state,
        candidates,
        lookup_key,
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

fn resolve_vue_template_typed_member(
    ctx: &PhaseContext<'_, '_, '_>,
    graph: &mut Graph,
    r: &Reference,
    owner: Option<&Node>,
) -> bool {
    if r.kind != EdgeKind::References.as_str()
        || !owner.is_some_and(|owner| owner.kind == NodeKind::TemplateScope.as_str())
    {
        return false;
    }
    if vue::resolution::resolve_component_tag(ctx.input, ctx.path, r, owner, graph) {
        return true;
    }
    if let Some(ReceiverHint::ConfiguredTypedMember { field_id, origin }) = &r.receiver_hint {
        if !ctx.language_context.syntax_rules.iter().any(|rule| {
            rule.pack == origin.pack
                && rule.id == origin.rule
                && ctx.language_context.is_syntax_rule_active(
                    rule,
                    ctx.path,
                    &ctx.facts.activation_signals,
                )
                && matches!(
                    rule.operation,
                    languages::manifests::syntax::SyntaxOperation::ComponentMacros { .. }
                )
        }) {
            return false;
        }
        if let Some(field) = ctx
            .input
            .interner
            .lookup_node(ctx.input.by_id, field_id.as_str())
            .filter(|field| {
                field.path == *ctx.path
                    && field.kind == NodeKind::Field.as_str()
                    && field.name
                        == r.expression
                            .rsplit('.')
                            .next()
                            .unwrap_or(r.expression.as_str())
            })
        {
            graph.reference_edge(
                ctx.path,
                r,
                field.id.clone(),
                EdgeKind::References.as_str(),
                EdgeConfidence::Exact.as_str(),
            );
            graph.reference_coverage(
                ctx.path,
                r,
                CoverageStatus::Resolved.as_str(),
                format!(
                    "script setup typed macro field {}; rule:{}:{}",
                    field.qualname, origin.pack, origin.rule
                )
                .into(),
            );
            return true;
        }
    }
    false
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
    #[cfg(feature = "lang-rust")]
    if ctx
        .profile
        .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
        && rust::resolution::dynamic_receiver_member(ctx.input, ctx.path, r, _owner, graph)
    {
        return;
    }
    let mut computed = receivers::Member::Unknown;
    let mut literal_builtin = false;
    if ctx
        .profile
        .is_some_and(|profile| profile.id() == LanguageId::Python.as_str())
    {
        (computed, literal_builtin) = python::resolution::dynamic_receiver_member(
            &ctx.python_resolution(),
            r,
            &ctx.path_constructor_aliases,
            candidates,
            lookup_key,
            |name, o, d| ctx.alias_for(name, o, d),
            |name, o, d| ctx.external_for(name, o, d),
        );
    }
    let (status, evidence): (&str, Cow<'static, str>) = match computed {
        receivers::Member::Local(method)
            if matches!(
                method.kind.as_str(),
                k if k == NodeKind::Method.as_str() || k == NodeKind::Function.as_str()
            ) =>
        {
            graph.reference_edge(
                ctx.path,
                r,
                method.id.clone(),
                r.kind.as_str(),
                EdgeConfidence::Inferred.as_str(),
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
        receivers::Member::External(module, line, Some(provider), _) => {
            let diagnostic_marker = if ctx.profile.is_some_and(|profile| {
                profile.id() == LanguageId::Python.as_str() && !profile.is_stdlib(module)
            }) {
                "; diagnostic_reason=framework_magic"
            } else {
                ""
            };
            (
                CoverageStatus::External.as_str(),
                format!("finite Python framework member of {provider}: {} via external import {module} at line {line}{diagnostic_marker}", r.expression).into(),
            )
        }
        receivers::Member::External(module, _, None, _) => (
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

fn resolve_early_language_specials<'a>(
    ctx: &PhaseContext<'a, '_, '_>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
) -> bool {
    #[cfg(feature = "lang-typescript")]
    if super::super::javascript::resolution::has_returned_object_builtin_this_member(
        ctx.input,
        ctx.path,
        q.r,
        q.expression,
        q.active_profile,
        &ctx.returned_object_field_writes,
    ) {
        emit_inferred(
            graph,
            ctx.path,
            q.r,
            semantic_context::Member::Builtin,
            q.builtin_origin,
        );
        return true;
    }
    #[cfg(feature = "lang-typescript")]
    if super::super::javascript::resolution::object_factory_this_member(
        ctx.input,
        ctx.path,
        q.r,
        ctx.aliases,
        |node| ctx.object_ownership(node),
        &ctx.lexical_this_calls,
        graph,
    ) {
        return true;
    }
    #[cfg(feature = "lang-typescript")]
    if super::super::typescript::resolution::lexical_this_member(
        ctx.input,
        ctx.path,
        q.r,
        q.expression,
        &ctx.lexical_this_calls,
        &mut String::new(),
        graph,
    ) {
        return true;
    }
    if q.r.kind == EdgeKind::Calls.as_str() && q.tail.is_empty() {
        if let Some(origin) =
            vue::resolution::component_macro_callable_origin(q.r, ctx.language_context)
        {
            graph.reference_coverage(
                ctx.path,
                q.r,
                CoverageStatus::External.as_str(),
                format!("{origin} callable from this file's Vue script setup binding").into(),
            );
            return true;
        }
    }
    if q.r.kind == EdgeKind::References.as_str()
        && q.tail.is_empty()
        && q.owner
            .is_some_and(|owner| owner.kind == NodeKind::TemplateScope.as_str())
    {
        if let Some(binding) = vue::resolution::template_lexical_binding(
            ctx.input,
            ctx.path,
            q.r,
            q.owner,
            q.first,
            ctx.component_module,
        ) {
            graph.reference_edge(
                ctx.path,
                q.r,
                binding.id.clone(),
                EdgeKind::References.as_str(),
                EdgeConfidence::Exact.as_str(),
            );
            graph.reference_coverage(
                ctx.path,
                q.r,
                CoverageStatus::Resolved.as_str(),
                format!("Vue template lexical binding in {}", binding.qualname).into(),
            );
            return true;
        }
    }
    #[cfg(feature = "lang-rust")]
    if rust::resolution::resolve_self_reference(
        ctx.profile,
        q.r,
        q.owner,
        ctx.path,
        &ctx.linker.member_lookup,
        graph,
    ) {
        return true;
    }
    if q.r.kind != EdgeKind::Imports.as_str()
        && matches!(q.r.receiver_hint.as_ref(), Some(ReceiverHint::TypeOnlyImport))
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
            python::resolution::import_visible_at(&ctx.python_resolution(), q.r, definition_scope)
        });
    candidates.clear();
    while !scope.is_empty() {
        if let Some(scope_nodes) = ctx.input.by_qual.get(scope) {
            if !ctx.profile.is_some_and(|p| p.class_scope())
                && !definition_scope
                && q.owner.is_some_and(|n| {
                    matches!(
                        n.kind.as_str(),
                        k if k == NodeKind::Function.as_str() || k == NodeKind::Method.as_str()
                    )
                })
                && scope_nodes.iter().any(|n| {
                    n.path == *ctx.path
                        && matches!(
                            n.kind.as_str(),
                            k if k == NodeKind::Class.as_str() || k == NodeKind::Impl.as_str()
                        )
                })
            {
                scope = scope.rsplit_once('.').map_or("", |(p, _)| p);
                continue;
            }
            if ctx.profile.is_none_or(|p| p.value_binding_applies(q.r)) {
                let mut binding_owners = scope_nodes.iter().filter(|node| {
                    node.path == *ctx.path
                        && has_node_binding(
                            ctx.input.interner,
                            ctx.input.node_bindings,
                            node.id.as_str(),
                            q.first,
                        )
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
                    .filter(|n| n.path == *ctx.path),
            );
            lexical_head_declared |= !candidates.is_empty();
            if !candidates.is_empty() {
                break;
            }
        } else if builtin_member_candidate {
            lexical_head_declared |= ctx
                .lookup(qualified(lookup_key, scope, q.first))
                .iter()
                .any(|n| n.path == *ctx.path);
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
        rust_explicit_address: false,
        scope,
    }
}

#[cfg(feature = "lang-typescript")]
fn check_implicit_receiver_external<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    implicit_receiver: &str,
    state: &ReferenceState<'res>,
    candidates: &[&'res Node],
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let Some(evidence) = q
        .active_profile
        .filter(|profile| {
            !state.shadowed
                && state.imported.is_none()
                && !state.lexical_head_declared
                && candidates.is_empty()
                && !implicit_receiver.is_empty()
                && matches!(
                    profile.id(),
                    p if p == LanguageId::Javascript.as_str()
                        || p == LanguageId::Typescript.as_str()
                        || p == LanguageId::Vue.as_str()
                        || p == LanguageId::Html.as_str()
                )
        })
        .and_then(|_| {
            ctx.language_context.syntax_rules.iter().find_map(|rule| {
                let languages::manifests::syntax::SyntaxOperation::ImplicitReceivers { names } =
                    &rule.operation
                else {
                    return None;
                };
                if !ctx.language_context.is_syntax_rule_active(
                    rule,
                    ctx.path,
                    &ctx.facts.activation_signals,
                ) {
                    return None;
                }
                names.iter().any(|name| name == implicit_receiver).then(|| {
                    format!(
                        "{}; rule:{}:{}",
                        rule.display
                            .builtin
                            .as_deref()
                            .unwrap_or("framework builtin: configured implicit receiver"),
                        rule.pack,
                        rule.id
                    )
                })
            })
        })
    else {
        return false;
    };
    let locally_owned_member = q.first == "this"
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
