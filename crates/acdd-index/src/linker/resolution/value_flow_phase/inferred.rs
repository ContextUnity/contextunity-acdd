use crate::languages::profile::{LanguageMemberResult, ProfileLinker};
use super::super::context::*;
use super::context::PhaseContext;
use super::members;
use crate::languages::LanguageProfile;
use crate::linker::{
    semantic_context,
    traits::receiver_language_requires_static,
    types::{qualified, ReceiverType},
    value_flow,
};
use crate::models::{
    compact_graph::CompactGraph as Graph,
    tags::{CoverageStatus, EdgeKind, NodeKind},
    Node, Reference,
};

pub(super) struct RefQuery<'a, 'q> {
    pub(super) r: &'q Reference,
    pub(super) owner: Option<&'a Node>,
    pub(super) active_profile: Option<&'a dyn LanguageProfile>,
    pub(super) builtin_origin: Option<&'static str>,
    pub(super) scope: &'a str,
    pub(super) expression: &'q str,
    pub(super) first: &'q str,
    pub(super) tail: &'q str,
    pub(super) keyword_explicit: bool,
    pub(super) module_target: Option<&'q crate::linker::traits::ExplicitModuleTarget>,
}

pub(super) struct ReferenceState<'res> {
    pub(super) shadowed: bool,
    pub(super) typed_receiver: Option<ReceiverType<'res>>,
    pub(super) lexical_head_declared: bool,
    pub(super) known_receiver: bool,
    pub(super) imported: Option<&'res Vec<&'res Node>>,
    pub(super) definition_scope: bool,
    pub(super) receiver_member: Option<LanguageMemberResult<'res>>,
    pub(super) explicit_address: bool,
    pub(super) scope: &'res str,
}

pub(super) fn emit_resolved_with_polymorphism(
    graph: &mut Graph,
    path: &str,
    reference: &Reference,
    receiver: (&value_flow::TypeTarget<'_>, &str),
    result: semantic_context::Member<'_>,
    origin: Option<&str>,
    semantic_context: &semantic_context::Context<'_>,
) {
    let (resolved_receiver, member) = receiver;
    if matches!(reference.kind.as_str(), "calls" | "references") {
        if let (
            value_flow::TypeTarget::Local(abstract_type),
            semantic_context::Member::Local(contract_decl),
        ) = (resolved_receiver, &result)
        {
            if let Some(implementors) =
                semantic_context.polymorphic_implementors(abstract_type, member, contract_decl)
            {
                if implementors.len() > value_flow::MAX_CANDIDATE_BRANCH_JOINS {
                    graph.reference_coverage(
                        path,
                        reference,
                        "unresolved",
                        format!(
                            "polymorphic dispatch exceeded bound (K={}): {} has {} concrete implementors; Unknown/Incomplete; no truncated possible_calls emitted",
                            value_flow::MAX_CANDIDATE_BRANCH_JOINS,
                            contract_decl.qualname,
                            implementors.len()
                        )
                        .into(),
                    );
                    graph.reference_edge(
                        path,
                        reference,
                        contract_decl.id.clone(),
                        crate::models::tags::EdgeKind::parse(&reference.kind),
                        crate::models::tags::EdgeConfidence::Inferred,
                    );
                    return;
                }
                emit_inferred(graph, path, reference, result, origin);
                for target in implementors {
                    graph.reference_edge(
                        path,
                        reference,
                        target.id.clone(),
                        crate::models::tags::EdgeKind::PossibleCalls,
                        crate::models::tags::EdgeConfidence::Possible,
                    );
                }
                return;
            }
        }
    }
    emit_inferred(graph, path, reference, result, origin);
}

fn emit_resolved_member<'a, 'file, 'res>(
    graph: &mut Graph,
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    q: &RefQuery<'a, '_>,
    result: semantic_context::Member<'a>,
) -> bool {
    if matches!(result, semantic_context::Member::Unknown) {
        return false;
    }
    emit_inferred(graph, ctx.path, q.r, result, q.builtin_origin);
    true
}

fn resolve_empty_tail_inferred_callable<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    owner: &'a Node,
    receiver: &'res value_flow::TypeTarget<'a>,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    if q.r.kind == EdgeKind::Calls.as_str() {
        if let Some(evidence) = ctx.language_linker.callable_marker_evidence(receiver) {
            graph.reference_coverage(
                ctx.path,
                q.r,
                CoverageStatus::Resolved.as_str(),
                evidence.into(),
            );
            return true;
        }
    }
    if let value_flow::TypeTarget::Callable(callable) = receiver {
        if callable.kind == NodeKind::Constant {
            return false;
        }
        emit_inferred(
            graph,
            ctx.path,
            q.r,
            semantic_context::Member::Local(callable),
            q.builtin_origin,
        );
        return true;
    }
    if q.r.kind == EdgeKind::Calls.as_str()
        && matches!(receiver, value_flow::TypeTarget::CallableReturn(_))
    {
        let evidence = crate::languages::by_id(owner.language.as_str())
            .map_or("invocation of a typed Callable binding", |profile| profile.linker().typed_callable_evidence());
        graph.reference_coverage(
            ctx.path,
            q.r,
            CoverageStatus::Resolved.as_str(),
            evidence.into(),
        );
        return true;
    }
    if let Some((constructor_member, call_member)) = (q.r.kind == EdgeKind::Calls.as_str())
        .then(|| crate::languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().callable_members())).flatten()
    {
        match receiver {
            value_flow::TypeTarget::ClassType(inner) => {
                let result = match inner.as_ref() {
                    value_flow::TypeTarget::Local(class) => {
                        match ctx.input.semantic_context.lookup_member_mode(
                            inner.as_ref(),
                            constructor_member,
                            owner,
                            false,
                        ) {
                            semantic_context::Member::Unknown => {
                                semantic_context::Member::Local(class)
                            }
                            init => init,
                        }
                    }
                    value_flow::TypeTarget::External {
                        module,
                        import_line,
                    } => semantic_context::Member::External(module, *import_line, None),
                    value_flow::TypeTarget::Builtin(_) => semantic_context::Member::Builtin,
                    _ => semantic_context::Member::Unknown,
                };
                if emit_resolved_member(graph, ctx, q, result) {
                    return true;
                }
            }
            value_flow::TypeTarget::Local(class) if class.kind == NodeKind::Class => {
                let result = ctx
                    .input
                    .semantic_context
                    .lookup_member_mode(receiver, call_member, owner, false);
                if emit_resolved_member(graph, ctx, q, result) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

pub(super) fn resolve_inferred_receiver_member<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let component_template_member = q.owner
        .and_then(|owner| crate::languages::by_id(owner.language.as_str()))
        .is_some_and(|profile| profile.linker().is_component_template_member(q.r, q.owner, q.tail));
    let typed_member_read = q.r.kind == EdgeKind::References.as_str()
        && !q.tail.is_empty()
        && ctx.profile.is_some_and(|profile| profile.linker().allows_typed_member_read(state.known_receiver));
    if !(matches!(
        q.r.kind.as_str(),
        k if k == EdgeKind::Calls.as_str() || k == EdgeKind::Handles.as_str()
    ) || component_template_member
        || typed_member_read)
    {
        return false;
    }
    let Some(owner) = q.owner else {
        return false;
    };
    let at = q.r.position();
    if component_template_member && !q.tail.contains('.')
        && ctx.language_linker.component_slot_reference(&mut crate::linker::traits::ComponentSlotReference {
            phase: ctx.reference_phase(graph, q.r, Some(owner), q.active_profile), first: q.first, tail: q.tail,
            builtin_origin: q.builtin_origin, alias_for: &|n, o, d| ctx.alias_for(n, o, d),
        }) {
        return true;
    }
    let implicit_receiver = if state.known_receiver {
        ctx.linker.member_lookup.receiver_for(owner.language.as_str(), owner)
            .or_else(|| ctx.local_receiver_class(owner))
            .map(value_flow::TypeTarget::Local)
    } else {
        None
    };
    let constant_receiver = (implicit_receiver.is_none()
        && crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().supports_constant_receiver())
        && !q.tail.is_empty()
        && ctx.profile.is_none_or(|p| p.value_binding_applies(q.r))
        && ctx.input.value_flow.lookup(owner, q.first, at).is_none())
    .then(|| {
        use crate::linker::value_flow::{SemanticResolver, Symbol, SymbolRole};
        if let Symbol::Callable(node) =
            ctx.input
                .semantic_context
                .resolve_symbol(owner, q.first, at, SymbolRole::Callable)
        {
            ctx.input.value_flow.constant_type(node)
        } else {
            None
        }
    })
    .flatten();
    let inferred = implicit_receiver
        .as_ref()
        .or(constant_receiver)
        .or_else(|| {
            if !ctx.profile.is_none_or(|p| p.value_binding_applies(q.r)) {
                return None;
            }
            let receiver = ctx.input.value_flow.lookup(owner, q.first, at)?;
            let incomplete_reference = ctx.language_linker.incomplete_receiver(receiver);
            if incomplete_reference
                && matches!(state.typed_receiver, Some(ReceiverType::External(..)))
            {
                None
            } else {
                Some(receiver)
            }
        });
    if members::resolve_candidate_join(
        ctx.path,
        q.r,
        q.tail,
        owner,
        inferred,
        ctx.input.semantic_context,
        graph,
    ) {
        return true;
    }
    let Some(receiver) =
        inferred.filter(|receiver| !matches!(receiver, value_flow::TypeTarget::Unknown))
    else {
        return false;
    };
    if q.tail.is_empty() {
        return resolve_empty_tail_inferred_callable(ctx, graph, q, owner, receiver);
    }
    let (fields, member) = q.tail.rsplit_once('.').unwrap_or(("", q.tail));
    let Some(nested_receiver) = chained_member_receiver(
        receiver,
        fields,
        owner,
        ctx.path,
        q.r.position(),
        ctx.input,
        ctx.profile,
    ) else {
        if !fields.is_empty() && crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().chained_member_fallback()) {
            let result = ctx
                .input
                .semantic_context
                .lookup_member_mode(receiver, q.tail, owner, false);
            if emit_resolved_member(graph, ctx, q, result) {
                return true;
            }
        }
        return false;
    };
    let resolved_receiver = nested_receiver.as_ref();
    let associated = implicit_receiver.is_some()
        && fields.is_empty()
        && receiver_language_requires_static(owner)
        && owner.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC) == true;
    let result = if q.r.kind == EdgeKind::References.as_str()
        && crate::languages::by_id(owner.language.as_str())
            .is_some_and(|profile| profile.linker().property_before_member())
    {
        ctx.input
            .semantic_context
            .lookup_property(resolved_receiver, member, owner)
    } else {
        ctx.input
            .semantic_context
            .lookup_member_mode(resolved_receiver, member, owner, associated)
    };
    let result =
        if q.r.kind == EdgeKind::References.as_str() && q.active_profile.is_some_and(|profile| {
            profile.linker().property_after_unknown(profile)
        }) && matches!(result, semantic_context::Member::Unknown)
        {
            ctx.input
                .semantic_context
                .lookup_property(resolved_receiver, member, owner)
        } else {
            result
        };
    let result = match result {
        semantic_context::Member::Local(node)
            if crate::languages::by_id(node.language.as_str()).is_some_and(|profile| {
                !profile
                    .linker()
                    .member_visible_at(node, ctx.path, q.r.line, q.r.column)
            }) =>
        {
            semantic_context::Member::Unknown
        }
        other => other,
    };
    let result = match (q.r.kind.as_str(), &result) {
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Calls.as_str() && node.kind == NodeKind::Field =>
        {
            semantic_context::Member::Unknown
        }
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Mutates.as_str() && node.kind == NodeKind::Method =>
        {
            semantic_context::Member::Unknown
        }
        _ => result,
    };
    let origin =
        ctx.language_linker
            .builtin_member_origin(resolved_receiver, q.builtin_origin, false);
    emit_resolved_with_polymorphism(
        graph,
        ctx.path,
        q.r,
        (resolved_receiver, member),
        result,
        origin,
        ctx.input.semantic_context,
    );
    true
}

pub(super) fn resolve_lexical_or_imported_candidates<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    state: &mut ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    if let Some(ReceiverType::Local(receiver)) = state.typed_receiver {
        candidates.extend(
            ctx.lookup(qualified(lookup_key, &receiver.qualname, q.tail))
                .iter()
                .copied()
                .filter(|candidate| candidate.path == receiver.path),
        );
    }
    if let Some(LanguageMemberResult::Local(method)) = &state.receiver_member {
        candidates.clear();
        candidates.push(*method);
    }
    if matches!(
        &state.receiver_member,
        Some(
            LanguageMemberResult::Unknown
                | LanguageMemberResult::Ambiguous
                | LanguageMemberResult::External { .. }
        )
    ) {
        candidates.clear();
    }
    if candidates.is_empty() && state.typed_receiver.is_none() && state.receiver_member.is_none() {
        if resolve_from_imported(ctx, graph, q, state, candidates, lookup_key) {
            return true;
        }
        if candidates.is_empty()
            && state.imported.is_none()
            && state.typed_receiver.is_none()
            && state.receiver_member.is_none()
        {
            resolve_lexical_fallback(ctx, q, state, candidates, lookup_key);
        }
    }
    state.explicit_address = resolve_explicit_paths(
            ctx,
            q.owner,
            q.keyword_explicit,
            q.module_target,
            state,
            candidates,
            lookup_key,
        );
    false
}

fn resolve_imported_static_type_member<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    imported: &[&'res Node],
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let (Some(owner), [receiver]) = (q.owner, imported) else {
        return false;
    };
    if !crate::languages::by_id(receiver.language.as_str()).is_some_and(|profile| profile.linker().supports_static_import_receiver())
        || !matches!(receiver.kind, NodeKind::Class | NodeKind::Struct | NodeKind::Enum | NodeKind::Interface) {
        return false;
    }
    let receiver_target = value_flow::TypeTarget::Local(receiver);
    let member =
        ctx.input
            .semantic_context
            .lookup_member_mode(&receiver_target, q.tail, owner, true);
    let member = match (q.r.kind.as_str(), &member) {
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Calls.as_str() && node.kind == NodeKind::Field =>
        {
            semantic_context::Member::Unknown
        }
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Mutates.as_str() && node.kind == NodeKind::Method =>
        {
            semantic_context::Member::Unknown
        }
        _ => member,
    };
    if !matches!(member, semantic_context::Member::Unknown)
        || receiver_language_requires_static(imported[0])
    {
        emit_inferred(graph, ctx.path, q.r, member, q.builtin_origin);
        return true;
    }
    false
}

fn resolve_from_imported<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    graph: &mut Graph,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let Some(imported) = state.imported.filter(|_| !state.shadowed) else {
        return false;
    };
    if q.tail.is_empty() {
        let phase = ctx.reference_phase(graph, q.r, q.owner, q.active_profile);
        let commonjs_default = ctx.language_linker.imported_default_callable(
            &phase, q.first, ctx.facts, imported, ctx.language_package_exports(), lookup_key,
        );
        if let Some(callable) = commonjs_default {
            candidates.push(callable);
        } else {
            candidates.extend_from_slice(imported);
        }
        return false;
    }
    if resolve_imported_static_type_member(ctx, graph, q, imported) {
        return true;
    }
    fn resolve_imported_module_call<'a, 'file, 'res>(
        ctx: &'res PhaseContext<'a, 'file, 'res>,
        graph: &mut Graph,
        q: &RefQuery<'a, '_>,
        state: &ReferenceState<'res>,
        imported: &[&'res Node],
        lookup_key: &mut String,
    ) -> bool
    where
        'a: 'file,
        'file: 'res,
    {
        if !(matches!(
            q.r.kind.as_str(),
            k if k == EdgeKind::Calls.as_str() || k == EdgeKind::Handles.as_str()
        ) && ctx
            .alias_import_positions
            .get(&(state.scope, q.first))
            .is_some_and(|position| *position <= q.r.position()))
        {
            return false;
        }
        let [module] = imported else {
            return false;
        };
        let phase = ctx.reference_phase(graph, q.r, q.owner, ctx.profile);
        let Some((origin, line, provider)) = ctx.language_linker
            .imported_external_member(&phase, module, q.tail, lookup_key)
        else {
            return false;
        };
        emit_inferred(
            graph,
            ctx.path,
            q.r,
            semantic_context::Member::External(origin, line, provider),
            q.builtin_origin,
        );
        true
    }

    if resolve_imported_module_call(ctx, graph, q, state, imported, lookup_key) {
        return true;
    }
    for n in imported {
        if n.kind == NodeKind::Module
            && crate::languages::by_id(n.language.as_str()).is_some_and(|profile| profile.linker().imported_module_first())
        {
            candidates.extend(ctx.language_linker.resolve_imported_member(
                ctx.input.interner,
                n,
                q.tail,
                ctx.input.by_module,
                ctx.input.by_qual,
                ctx.language_package_exports(),
                lookup_key,
            ));
            continue;
        }
        if crate::languages::by_id(n.language.as_str())
            .is_some_and(|profile| profile.linker().associated_member_owner(n))
            && !q.tail.contains('.')
        {
            if let Some(owner) = q.owner {
                let targets = ctx
                    .linker
                    .member_lookup
                    .lookup_member(n.language.as_str(), n, q.tail, true, owner)
                    .into_nodes();
                if !targets.is_empty() {
                    candidates.extend(targets);
                    continue;
                }
            }
        }
        let before_len = candidates.len();
        candidates.extend(
            ctx.lookup(qualified(lookup_key, &n.qualname, q.tail))
                .iter()
                .copied()
                .filter(|target| target.path == n.path),
        );
        if candidates.len() == before_len
            && crate::languages::by_id(n.language.as_str()).is_some_and(|profile| profile.linker().associated_module_owner(n))
        {
            if let Some((ty_name, member_name)) = q.tail.split_once('.') {
                if !member_name.contains('.') {
                    if let Some(owner) = q.owner {
                        candidates.extend(lookup_associated_member(
                            ctx.lookup(qualified(lookup_key, &n.qualname, ty_name)),
                            n.path.as_str(),
                            member_name,
                            owner,
                            &ctx.linker.member_lookup,
                            ctx.language_linker,
                        ));
                    }
                }
            }
        }
    }
    if needs_explicit_reexport(candidates) {
        let phase = ctx.reference_phase(graph, q.r, q.owner, ctx.profile);
        let callable = ctx.language_linker.imported_callable(&phase, imported, q.tail);
        if !callable.is_empty() {
            candidates.retain(|n| !matches!(n.kind, NodeKind::Module | NodeKind::ModuleDeclaration));
            candidates.extend(callable);
        }
        if needs_explicit_reexport(candidates) {
            if let [module] = imported.as_slice() {
                if module.kind == NodeKind::Module
                    && !crate::languages::by_id(module.language.as_str())
                        .is_some_and(|profile| profile.linker().imported_module_first())
                {
                    candidates.extend(ctx.language_linker.resolve_imported_member(
                        ctx.input.interner,
                        module,
                        q.tail,
                        ctx.input.by_module,
                        ctx.input.by_qual,
                        ctx.language_package_exports(),
                        lookup_key,
                    ));
                }
            }
        }
    }
    false
}

fn resolve_lexical_fallback<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    q: &RefQuery<'a, '_>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    if !q.tail.is_empty() && !state.shadowed {
        candidates.extend_from_slice(ctx.lookup(q.expression));
    }
    if !candidates.is_empty() {
        return;
    }
    if q.tail.is_empty() {
        candidates.extend_from_slice(ctx.lookup(qualified(
            lookup_key,
            ctx.file_module,
            q.expression,
        )));
    } else if state.known_receiver {
        if let Some(owner) = q.owner {
            let receiver_scope = ctx.linker.member_lookup.receiver_for(owner.language.as_str(), owner)
                .map(|receiver| receiver.qualname.as_str());
            let scope = receiver_scope.unwrap_or_else(|| {
                let scope = owner.qualname.rsplit_once('.').map_or("", |(p, _)| p);
                ctx.language_linker.member_owner_scope(scope)
            });
            candidates.extend_from_slice(ctx.lookup(qualified(lookup_key, scope, q.tail)));
        }
    } else if !state.shadowed {
        candidates.extend_from_slice(ctx.lookup(qualified(
            lookup_key,
            ctx.file_module,
            q.expression,
        )));
        if candidates.is_empty() && !q.tail.contains('.') {
            if let Some(owner) = q.owner {
                candidates.extend(lookup_associated_member(
                    ctx.lookup(qualified(lookup_key, ctx.file_module, q.first)),
                    ctx.path,
                    q.tail,
                    owner,
                    &ctx.linker.member_lookup,
                    ctx.language_linker,
                ));
            }
        }
    }
}

fn push_explicit_constant_member<'a, 'res>(
    ctx: &'res PhaseContext<'a, '_, 'res>,
    owner: Option<&'res Node>,
    namespace: &str,
    candidates: &mut Vec<&'res Node>,
) {
    let (Some(owner), Some((const_qual, member))) = (owner, namespace.rsplit_once('.')) else {
        return;
    };
    candidates.extend(
        ctx.lookup(const_qual)
            .iter()
            .copied()
            .filter(|node| node.kind == crate::models::tags::NodeKind::Constant)
            .filter_map(|node| {
                ctx.language_linker.explicit_constant_member(node, member, owner, ctx.input.semantic_context)
            }),
    );
}

fn needs_explicit_reexport(candidates: &[&Node]) -> bool {
    candidates.is_empty()
        || candidates
            .iter()
            .all(|n| matches!(n.kind, NodeKind::Module | NodeKind::ModuleDeclaration))
}

fn extend_explicit_reexports<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    target: &crate::linker::traits::ExplicitModuleTarget,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) where
    'a: 'file,
    'file: 'res,
{
    let reexported = ctx.language_linker.explicit_reexport_targets(
        crate::linker::traits::ExplicitReexport {
            input: ctx.input,
            file_module: ctx.file_module,
            target,
            lookup_key,
        },
    );
    if !reexported.is_empty() {
        candidates.retain(|n| !matches!(n.kind, NodeKind::Module | NodeKind::ModuleDeclaration));
        candidates.extend(reexported);
    }
}

fn resolve_explicit_paths<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    owner: Option<&'res Node>,
    keyword_explicit: bool,
    module_target: Option<&crate::linker::traits::ExplicitModuleTarget>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let mut explicit_address = keyword_explicit;
    if keyword_explicit {
        candidates.clear();
        if let Some(target) = module_target {
            candidates.extend_from_slice(ctx.lookup(target.namespace.as_str()));
            if needs_explicit_reexport(candidates) {
                extend_explicit_reexports(ctx, target, candidates, lookup_key);
            }
            if candidates.is_empty() {
                push_explicit_constant_member(ctx, owner, &target.namespace, candidates);
            }
        }
    } else if !state.shadowed
        && state.imported.is_none()
        && state.typed_receiver.is_none()
        && state.receiver_member.is_none()
        && (!state.lexical_head_declared || module_target.is_some())
        && needs_explicit_reexport(candidates)
    {
        if let Some(target) = module_target {
            candidates.extend(
                ctx.lookup(target.namespace.as_str())
                    .iter()
                    .copied()
                    .filter(|n| {
                        ctx.language_linker.explicit_module_visible(&crate::linker::traits::ExplicitModuleVisibility {
                            input: ctx.input, node: n, file_module: ctx.file_module,
                        })
                    }),
            );
            if needs_explicit_reexport(candidates) {
                extend_explicit_reexports(ctx, target, candidates, lookup_key);
            }
            explicit_address = !candidates.is_empty();
        }
    }
    explicit_address
}

fn lookup_associated_member<'a>(
    candidates: &[&'a Node],
    expected_path: &str,
    member_name: &str,
    owner: &Node,
    member_lookup: &semantic_context::LanguageMemberLookup<'_, 'a>,
    linker: &dyn crate::languages::profile::LanguageLinker,
) -> Vec<&'a Node> {
    let mut matching = candidates.iter().copied().filter(|target| {
        target.path == expected_path && linker.associated_member_owner(target)
    });
    let Some(ty_node) = matching.next() else {
        return Vec::new();
    };
    if matching.next().is_some() {
        return Vec::new();
    }
    member_lookup
        .lookup_member(owner.language.as_str(), ty_node, member_name, true, owner)
        .into_nodes()
}
