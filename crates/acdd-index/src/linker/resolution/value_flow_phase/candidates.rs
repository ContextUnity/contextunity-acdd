use crate::languages::profile::{LanguageMemberResult, ProfileLinker};
use super::context::PhaseContext;
use super::inferred::{RefQuery, ReferenceState};
use super::relation_edges;
use crate::linker::types::ReceiverType;
use crate::models::{
    compact_graph::CompactGraph as Graph,
    tags::{CoverageStatus, EdgeConfidence, EdgeKind, NodeKind},
    Node, Reference,
};
use std::borrow::Cow;

pub(super) fn finalize_and_emit<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    filter_candidates(ctx, q, state, candidates);
    disambiguate_candidates(ctx, graph, q.r, candidates);
    emit_resolution_outcome(ctx, graph, q, state, candidates, lookup_key);
}

fn filter_candidates<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
) where
    'a: 'file,
    'file: 'res,
{
    if state.imported.is_none()
        && state.typed_receiver.is_none()
        && !matches!(
            &state.receiver_member,
            Some(LanguageMemberResult::Local(_))
        )
    {
        candidates.retain(|n| {
            n.path == *ctx.path
                || (ctx
                    .profile
                    .is_some_and(|p| p.sibling_accessible(ctx.path, &n.path))
                    && ctx
                        .input
                        .by_module
                        .get(n.path.as_str())
                        .into_iter()
                        .flatten()
                        .any(|m| {
                            m.kind == NodeKind::Module && m.qualname == ctx.file_module
                        }))
                || (!state.shadowed
                    && q.r.kind == EdgeKind::References.as_str()
                    && n.qualname == q.expression)
                || state.explicit_address
        });
    }
    if q.r.kind == EdgeKind::Calls.as_str()
        && q.tail.is_empty()
        && state.imported.is_none()
        && state.typed_receiver.is_none()
    {
        if let Some(extension) = ctx.reference_extension() {
            candidates.extend(extension.candidates(ctx.path, q.owner, q.r, q.first));
        }
    }
    if q.r.kind == EdgeKind::Calls.as_str()
        && q.tail.is_empty()
        && state.imported.is_none()
        && !state.shadowed
        && candidates.is_empty()
    {
        if let Some(owner) = q.owner {
            candidates.extend(
                ctx.input
                    .semantic_context
                    .implicit_callables(owner, q.first),
            );
        }
    }
    candidates.retain(|n| {
        let macro_reference = q.active_profile
            .is_some_and(|profile| profile.linker().macro_reference_candidate(q.owner, n));
        match q.r.kind.as_str() {
            k if k == EdgeKind::Inherits.as_str() || k == EdgeKind::Bases.as_str() => {
                matches!(
                    n.kind.as_str(),
                    nk if nk == NodeKind::Class.as_str()
                        || nk == NodeKind::Interface.as_str()
                        || nk == NodeKind::Struct.as_str()
                )
            }
            k if k == EdgeKind::Implements.as_str() => matches!(
                n.kind.as_str(),
                nk if nk == NodeKind::Trait.as_str() || nk == NodeKind::Interface.as_str()
            ),
            k if k == EdgeKind::Mutates.as_str() => n.kind == NodeKind::Field,
            k if k == EdgeKind::References.as_str() => {
                matches!(
                    n.kind.as_str(),
                    nk if nk == NodeKind::Class.as_str()
                        || nk == NodeKind::Enum.as_str()
                        || nk == NodeKind::Type.as_str()
                        || nk == NodeKind::Struct.as_str()
                        || nk == NodeKind::Trait.as_str()
                        || nk == NodeKind::Interface.as_str()
                        || nk == NodeKind::Field.as_str()
                        || nk == NodeKind::Constant.as_str()
                ) || (n.kind == NodeKind::Function
                    && (!q.tail.is_empty()
                        || (state.imported.is_some()
                            && !ctx.is_type_only(q.first, q.owner, state.definition_scope)
                            && ctx.input.interner.lookup_symbol(&q.r.source).is_some_and(
                                |source| {
                                    ctx.value_queries.contains(&(
                                        source,
                                        q.r.expression.as_str(),
                                        q.r.line,
                                        q.r.column,
                                    ))
                                },
                            ))))
                    || macro_reference
                    || crate::languages::by_id(n.language.as_str())
                        .is_some_and(|profile| profile.linker().component_default_export(n))
            }
            _ => matches!(
                n.kind.as_str(),
                nk if nk == NodeKind::Function.as_str()
                    || nk == NodeKind::Method.as_str()
                    || nk == NodeKind::Class.as_str()
                    || nk == NodeKind::Struct.as_str()
                    || nk == NodeKind::Enum.as_str()
                    || nk == NodeKind::Macro.as_str()
            ),
        }
    });
    if matches!(
        q.r.kind.as_str(),
        k if k == EdgeKind::Calls.as_str() || k == EdgeKind::Handles.as_str()
    ) && !q.tail.is_empty()
    {
        candidates.retain(|candidate| {
            crate::languages::by_id(candidate.language.as_str())
                .is_none_or(|profile| profile.linker().admits_static_receiver_candidate(candidate, q.expression))
        });
    }
}

fn disambiguate_candidates<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    r: &Reference,
    candidates: &mut Vec<&'res Node>,
) where
    'a: 'file,
    'file: 'res,
{
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    candidates.dedup_by_key(|n| &n.id);
    crate::linker::traits::filter_stubs_and_overloads(candidates);
    if candidates.len() > 1 {
        if let Some(profile) = ctx.profile {
            profile.linker().filter_call_candidates(
                ctx.input.semantic_context.member_state(profile.id()),
                r.kind.as_str(),
                candidates,
            );
        }
    }
    if let Some(profile) = ctx.profile {
        profile.linker().record_candidate_gaps(profile, r, candidates, ctx.path, graph);
    }
}

fn builtin_origin_and_evidence<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    q: &RefQuery<'a, '_>,
    is_builtin: bool,
) -> Option<(Cow<'res, str>, Cow<'static, str>)> {
    if !is_builtin {
        return None;
    }
    if let Some((origin, evidence)) = ctx
        .profile
        .and_then(|profile| profile.linker().builtin_evidence(q.first, q.expression))
    {
        return Some((Cow::Borrowed(origin), evidence.into()));
    }
    if let Some(origin) = q.active_profile.zip(ctx.reference_extension()).and_then(|(profile, extension)| {
        profile.linker().template_builtin(q.expression, extension.syntax_rules())
    }) {
        let evidence = format!("{origin} {}", q.expression);
        return Some((Cow::Owned(origin), evidence.into()));
    }
    if let Some(origin) = ctx
        .language_context
        .component_macro(q.expression)
        .map(|(rule, _)| {
            format!(
                "{}; rule:{}:{}",
                rule.display
                    .builtin
                    .as_deref()
                    .unwrap_or("configured macro"),
                rule.pack,
                rule.id
            )
        })
        .or_else(|| {
            ctx.configured_autoimport(q.expression).then(|| {
                ctx.input
                    .semantic_context
                    .autoimport_evidence(ctx.path, q.expression)
                    .unwrap_or("configured runtime")
                    .to_owned()
            })
        })
    {
        let evidence = format!("{origin} built-in: {}", q.expression);
        return Some((Cow::Owned(origin), evidence.into()));
    }
    if let Some(origin) = q.active_profile.and_then(|profile| {
        profile.linker().platform_builtin_origin(
            profile,
        q.first,
        ctx.input
            .semantic_context
            .autoimport_evidence(ctx.path, q.first),
        )
    }) {
        return Some((
            Cow::Borrowed(origin),
            format!("{origin} built-in: {}", q.expression).into(),
        ));
    }
    None
}

fn determine_external_origin_and_evidence<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &[&'res Node],
    is_builtin: bool,
    source_present: bool,
    source_heuristic: bool,
) -> (&'static str, Cow<'static, str>) {
    let r = q.r;
    let base_status = if matches!(state.typed_receiver, Some(ReceiverType::Ambiguous))
        || matches!(
            &state.receiver_member,
            Some(LanguageMemberResult::Ambiguous)
        ) {
        CoverageStatus::Ambiguous.as_str()
    } else if (candidates.len() == 1 || is_builtin) && source_present && !source_heuristic {
        CoverageStatus::Resolved.as_str()
    } else if candidates.is_empty() || !source_present || source_heuristic {
        CoverageStatus::Unresolved.as_str()
    } else {
        CoverageStatus::Ambiguous.as_str()
    };
    let builtin_classified = builtin_origin_and_evidence(ctx, q, is_builtin);
    let external_import = ctx.profile.is_some_and(|profile| {
        profile.linker().external_annotation(
            profile,
        q.first,
        ctx.path,
        ctx.input.dependency_registry,
    )
    });
    let external_origin =
        if let Some(LanguageMemberResult::External { module, import_line, .. }) =
            &state.receiver_member
    {
        Some((Cow::Borrowed(*module), *import_line))
    } else if let Some(ReceiverType::External(module, import_line)) = state.typed_receiver {
        (import_line <= r.line).then_some((Cow::Borrowed(module), import_line))
    } else if let Some((origin, _)) = &builtin_classified {
        Some((origin.clone(), 0))
    } else if base_status == CoverageStatus::Unresolved.as_str()
        && !state.shadowed
        && candidates.is_empty()
        && (state.imported.is_some_and(|targets| targets.is_empty())
            || external_import
                || ctx
                    .external_call_for(q.first, q.owner, r.position())
                    .is_some())
    {
        if let Some(target) = ctx
            .external_call_for(q.first, q.owner, r.position())
            .filter(|(_, import_line)| *import_line <= r.line)
        {
            Some((Cow::Borrowed(target.0.as_str()), target.1))
        } else if external_import {
            Some((Cow::Borrowed(q.first), 0))
        } else {
            None
        }
    } else {
        None
    };
    let framework_provider = match &state.receiver_member {
        Some(LanguageMemberResult::External { provider, .. }) => *provider,
        _ => None,
    };
    let status = if external_origin.is_some() {
        CoverageStatus::External.as_str()
    } else {
        base_status
    };
    let stub_target = match candidates {
        [candidate] if ctx.language_linker.is_declaration_only(&candidate.path) => {
            Some(candidate.path.as_str())
        }
        _ => None,
    };
    let evidence: Cow<'static, str> = if let Some(stub_path) = stub_target {
        format!(
            "type-stub declaration at {stub_path}; runtime implementation not statically indexed"
        )
        .into()
    } else if let Some((_, builtin_evidence)) = builtin_classified {
        builtin_evidence
    } else if let Some((module, import_line)) = external_origin {
        let module = module.as_ref();
        if let Some(provider) = framework_provider {
            let diagnostic_marker = if ctx.profile.is_some_and(|profile| {
                profile.linker().framework_magic_diagnostic(profile, module)
            }) {
                "; diagnostic_reason=framework_magic"
            } else {
                ""
            };
            format!("{} {provider}.{} via external import {module} at line {import_line}{diagnostic_marker}", ctx.language_linker.framework_member_label(), q.tail).into()
        } else if let Some(evidence) = ctx.profile.and_then(|profile| profile.linker().external_builtin_evidence(profile, module, q.expression)) {
            evidence.into()
        } else {
            match (
                r.kind == EdgeKind::Calls.as_str(),
                ctx.profile
                    .is_some_and(|profile| profile.linker().preserves_external_import_line())
                    && import_line > 0,
            ) {
                (true, true) => format!("call through external import {module} at line {import_line}; callable target unverified").into(),
                (true, false) => format!("call through external import {module}; callable target unverified").into(),
                (false, true) => format!("reference through external import {module} at line {import_line}; target unverified").into(),
                (false, false) => format!("reference through external import {module}; target unverified").into(),
            }
        }
    } else if let Some(ReceiverType::Local(receiver)) = state.typed_receiver {
        format!(
            "parameter receiver type {}; {} statically inferred candidates",
            receiver.qualname,
            candidates.len()
        )
        .into()
    } else if matches!(state.typed_receiver, Some(ReceiverType::Ambiguous)) {
        "parameter receiver type has ambiguous declarations".into()
    } else if is_builtin {
        format!("standard library or built-in callee: {}", q.expression).into()
    } else {
        format!(
            "{} lexically justified candidates; implementor resolved={}, heuristic={source_heuristic}",
            candidates.len(),
            source_present
        )
        .into()
    };
    (status, evidence)
}

fn emit_resolution_outcome<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &[&'res Node],
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    let r = q.r;
    let mut source = Some(r.source.clone());
    let mut source_heuristic = false;
    if r.kind == EdgeKind::Implements.as_str() {
        if let Some(implementation) = q.owner.filter(|node| node.kind == NodeKind::Impl) {
            if let Some(result) = ctx.language_linker.implementation_source(crate::linker::traits::ImplementationSource {
                implementation, file_module: ctx.file_module, by_qual: ctx.input.by_qual,
                by_name: ctx.input.by_name, aliases: ctx.aliases, lookup_key,
            }) {
                (source, source_heuristic) = result;
            }
        }
    }
    let is_builtin = candidates.is_empty()
        && !state.shadowed
        && state.imported.is_none()
        && !state.lexical_head_declared
        && ctx.builtin_in_scope(q.expression, r);
    let (status, evidence) = determine_external_origin_and_evidence(
        ctx,
        q,
        state,
        candidates,
        is_builtin,
        source.is_some(),
        source_heuristic,
    );
    graph.reference_coverage(ctx.path, r, status, evidence);
    if let (Some(source), [candidate]) = (source, candidates) {
        let exact_same_class_member_reference = r.kind == EdgeKind::References.as_str()
            && matches!(
                &state.receiver_member,
                Some(LanguageMemberResult::Local(member))
                    if matches!(
                        member.kind.as_str(),
                        k if k == NodeKind::Field.as_str() || k == NodeKind::Class.as_str()
                    )
            );
        let target = candidate.id.as_str();
        let (src, dst) = if r.kind == EdgeKind::Decorates.as_str() {
            (target, source.as_str())
        } else {
            (source.as_str(), target)
        };
        let kind =
            if r.kind == EdgeKind::Bases.as_str() || r.kind == EdgeKind::Inherits.as_str() {
            let is_interface = candidate.kind == NodeKind::Interface
                || crate::languages::by_id(candidate.language.as_str())
                    .is_some_and(|profile| profile.linker().has_protocol_or_abc_base(candidate));
                if is_interface
                    && q.owner
                        .is_some_and(|n| n.kind != NodeKind::Interface)
                {
                EdgeKind::Implements
            } else {
                EdgeKind::Inherits
            }
        } else {
            let Some(kind) = EdgeKind::parse(&r.kind) else {
                return;
            };
            kind
        };
        graph.push_edge(
            ctx.path,
            src,
            dst,
            kind,
            r.line,
            r.expression.as_str(),
            if source_heuristic {
                EdgeConfidence::Heuristic
            } else if state.typed_receiver.is_some()
                || (state.receiver_member.is_some() && !exact_same_class_member_reference)
            {
                EdgeConfidence::Inferred
            } else {
                EdgeConfidence::Exact
            },
        );
        if (kind == EdgeKind::Implements || kind == EdgeKind::Inherits)
            && r.kind != EdgeKind::Decorates.as_str()
        {
            let source_container_id = if q.owner.is_some_and(|n| n.kind == NodeKind::Impl)
            {
                    r.source.as_str()
                } else {
                    src
                };
            relation_edges::emit_inherited_method_edges(
                ctx.input,
                graph,
                ctx.path,
                candidate,
                source_container_id,
            );
        }
    }
}
