use super::context::PhaseContext;
use super::members;
use super::*;
use crate::models::tags::{CoverageStatus, EdgeKind, LanguageId, NodeKind};

pub(super) struct RefQuery<'a, 'q> {
    pub(super) r: &'q Reference,
    pub(super) owner: Option<&'a Node>,
    pub(super) active_profile: Option<&'a dyn LanguageProfile>,
    pub(super) builtin_origin: Option<&'static str>,
    pub(super) scope: &'a str,
    pub(super) expression: &'q str,
    pub(super) first: &'q str,
    pub(super) tail: &'q str,
    pub(super) rust_keyword_explicit: bool,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_module_target: Option<&'q rust::resolution::RustExplicitTarget>,
}

pub(super) struct ReferenceState<'res> {
    pub(super) shadowed: bool,
    pub(super) typed_receiver: Option<ReceiverType<'res>>,
    pub(super) lexical_head_declared: bool,
    pub(super) known_receiver: bool,
    pub(super) imported: Option<&'res Vec<&'res Node>>,
    pub(super) definition_scope: bool,
    pub(super) receiver_member: Option<receivers::Member<'res>>,
    pub(super) rust_explicit_address: bool,
    pub(super) scope: &'res str,
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
    let component_template_member =
        vue::resolution::is_component_template_member(q.r, q.owner, q.tail);
    let typed_member_read = q.r.kind == EdgeKind::References.as_str()
        && !q.tail.is_empty()
        && (!state.known_receiver
            || ctx
                .profile
                .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str()))
        && ctx.profile.is_some_and(|profile| {
            matches!(profile.family().0, "typescript" | "javascript")
                || profile.id() == LanguageId::Rust.as_str()
        });
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
    #[cfg(feature = "lang-typescript")]
    if component_template_member && !q.tail.contains('.') {
        if let Some(result) = vue::resolution::component_slot_member(
            ctx.input,
            ctx.path,
            owner,
            q.first,
            q.tail,
            |n, o, d| ctx.alias_for(n, o, d),
        ) {
            emit_inferred(graph, ctx.path, q.r, result, q.builtin_origin);
            return true;
        }
    }
    let implicit_receiver = if state.known_receiver {
        #[cfg(feature = "lang-rust")]
        let rust_receiver = ctx.linker.member_lookup.rust_receiver(owner);
        #[cfg(not(feature = "lang-rust"))]
        let rust_receiver: Option<&Node> = None;
        rust_receiver
            .or_else(|| ctx.local_receiver_class(owner))
            .map(value_flow::TypeTarget::Local)
    } else {
        None
    };
    let inferred = implicit_receiver.as_ref().or_else(|| {
        if !ctx.profile.is_none_or(|p| p.value_binding_applies(q.r)) {
            return None;
        }
        let receiver = ctx.input.value_flow.lookup(owner, q.first, at)?;
        let incomplete_reference = matches!(
            receiver,
            value_flow::TypeTarget::RustReference { target, .. }
                if matches!(target.as_ref(), value_flow::TypeTarget::Unknown)
        );
        if incomplete_reference && matches!(state.typed_receiver, Some(ReceiverType::External(..))) {
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
        if owner.language == LanguageId::Rust.as_str()
            && q.r.kind == EdgeKind::Calls.as_str()
            && matches!(receiver, value_flow::TypeTarget::Builtin(name) if name == "__rust_closure")
        {
            graph.reference_coverage(
                ctx.path,
                q.r,
                CoverageStatus::Resolved.as_str(),
                "invocation of a lexically proven Rust closure".into(),
            );
            return true;
        }
        if let value_flow::TypeTarget::Callable(callable) = receiver {
            emit_inferred(
                graph,
                ctx.path,
                q.r,
                semantic_context::Member::Local(callable),
                q.builtin_origin,
            );
            return true;
        }
        return false;
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
        return false;
    };
    let resolved_receiver = nested_receiver.as_ref();
    let associated = implicit_receiver.is_some()
        && fields.is_empty()
        && receiver_language_requires_static(owner)
        && owner.details["is_static"] == true;
    let result = if owner.language == LanguageId::Rust.as_str()
        && q.r.kind == EdgeKind::References.as_str()
    {
        ctx.input
            .semantic_context
            .lookup_property(resolved_receiver, member, owner)
    } else {
        ctx.input
            .semantic_context
            .lookup_member_mode(resolved_receiver, member, owner, associated)
    };
    let result = if q.r.kind == EdgeKind::References.as_str()
        && q.active_profile.is_some_and(|profile| {
            matches!(
                profile.id(),
                p if p == LanguageId::Javascript.as_str() || p == LanguageId::Typescript.as_str()
            )
        })
        && matches!(result, semantic_context::Member::Unknown)
    {
        ctx.input
            .semantic_context
            .lookup_property(resolved_receiver, member, owner)
    } else {
        result
    };
    let result = match result {
        semantic_context::Member::Local(node)
            if !super::super::typescript::semantic_context::prototype_visible_at(
                node, ctx.path, q.r.line, q.r.column,
            ) =>
        {
            semantic_context::Member::Unknown
        }
        other => other,
    };
    let result = match (q.r.kind.as_str(), &result) {
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Calls.as_str() && node.kind == NodeKind::Field.as_str() =>
        {
            semantic_context::Member::Unknown
        }
        (k, semantic_context::Member::Local(node))
            if k == EdgeKind::Mutates.as_str() && node.kind == NodeKind::Method.as_str() =>
        {
            semantic_context::Member::Unknown
        }
        _ => result,
    };
    let origin = ctx
        .language_linker
        .builtin_member_origin(resolved_receiver, q.builtin_origin, false);
    emit_inferred(graph, ctx.path, q.r, result, origin);
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
    if let Some(receivers::Member::Local(method)) = state.receiver_member {
        candidates.clear();
        candidates.push(method);
    }
    if matches!(
        state.receiver_member,
        Some(
            receivers::Member::Unknown
                | receivers::Member::Ambiguous
                | receivers::Member::External(..)
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
    #[cfg(feature = "lang-rust")]
    {
        state.rust_explicit_address = resolve_rust_explicit_paths(
            ctx,
            q.rust_keyword_explicit,
            q.rust_module_target,
            state,
            candidates,
            lookup_key,
        );
    }
    #[cfg(not(feature = "lang-rust"))]
    let _ = q.rust_keyword_explicit;
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
        let commonjs_default = super::super::javascript::resolution::commonjs_default_callable(
            q.r,
            q.active_profile,
            q.first,
            q.owner,
            ctx.facts,
            imported,
            ctx.input.semantic_context,
            ctx.input.interner,
            ctx.language_linker,
            ctx.input.by_module,
            ctx.input.by_qual,
            ctx.language_package_exports(),
            lookup_key,
        );
        if let Some(callable) = commonjs_default {
            candidates.push(callable);
        } else {
            candidates.extend_from_slice(imported);
        }
        return false;
    }
    if let (Some(owner), [receiver]) = (q.owner, imported.as_slice()) {
        if matches!(
            receiver.language.as_str(),
            l if l == LanguageId::Rust.as_str()
                || l == LanguageId::Typescript.as_str()
                || l == LanguageId::Javascript.as_str()
                || l == LanguageId::Vue.as_str()
        ) && matches!(
            receiver.kind.as_str(),
            k if k == NodeKind::Class.as_str()
                || k == NodeKind::Struct.as_str()
                || k == NodeKind::Enum.as_str()
                || k == NodeKind::Interface.as_str()
        ) {
            let receiver_target = value_flow::TypeTarget::Local(receiver);
            let member =
                ctx.input
                    .semantic_context
                    .lookup_member_mode(&receiver_target, q.tail, owner, true);
            let member = match (q.r.kind.as_str(), &member) {
                (k, semantic_context::Member::Local(node))
                    if k == EdgeKind::Calls.as_str() && node.kind == NodeKind::Field.as_str() =>
                {
                    semantic_context::Member::Unknown
                }
                (k, semantic_context::Member::Local(node))
                    if k == EdgeKind::Mutates.as_str() && node.kind == NodeKind::Method.as_str() =>
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
        }
    }
    if matches!(
        q.r.kind.as_str(),
        k if k == EdgeKind::Calls.as_str() || k == EdgeKind::Handles.as_str()
    ) && ctx
        .alias_import_positions
        .get(&(state.scope, q.first))
        .is_some_and(|position| *position <= q.r.position())
    {
        if let [module] = imported.as_slice() {
            if let Some(receivers::Member::External(origin, line, provider, _)) =
                super::super::python::resolution::imported_module_member(
                    ctx.profile,
                    module,
                    q.tail,
                    ctx.input.node_rebindings,
                    ctx.input.interner,
                    ctx.input.by_qual,
                    lookup_key,
                    &ctx.linker.member_lookup,
                )
            {
                emit_inferred(
                    graph,
                    ctx.path,
                    q.r,
                    semantic_context::Member::External(origin, line, provider),
                    q.builtin_origin,
                );
                return true;
            }
        }
    }
    for n in imported {
        if n.kind == NodeKind::Module.as_str()
            && matches!(
                n.language.as_str(),
                l if l == LanguageId::Typescript.as_str()
                    || l == LanguageId::Javascript.as_str()
                    || l == LanguageId::Vue.as_str()
            )
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
        #[cfg(feature = "lang-rust")]
        if n.language == LanguageId::Rust.as_str()
            && matches!(
                n.kind.as_str(),
                k if k == NodeKind::Struct.as_str()
                    || k == NodeKind::Enum.as_str()
                    || k == NodeKind::Trait.as_str()
            )
            && !q.tail.contains('.')
        {
            if let Some(owner) = q.owner {
                let targets = ctx.linker.member_lookup.rust(n, q.tail, true, owner);
                if !targets.is_empty() {
                    candidates.extend_from_slice(targets);
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
        #[cfg(feature = "lang-rust")]
        if candidates.len() == before_len
            && n.language == LanguageId::Rust.as_str()
            && n.kind == NodeKind::Module.as_str()
        {
            if let Some((ty_name, member_name)) = q.tail.split_once('.') {
                if !member_name.contains('.') {
                    if let Some(owner) = q.owner {
                        candidates.extend_from_slice(lookup_rust_associated_member(
                            ctx.lookup(qualified(lookup_key, &n.qualname, ty_name)),
                            n.path.as_str(),
                            member_name,
                            owner,
                            &ctx.linker.member_lookup,
                        ));
                    }
                }
            }
        }
    }
    if candidates.is_empty() {
        if let [module] = imported.as_slice() {
            if module.kind == NodeKind::Module.as_str()
                && !matches!(
                    module.language.as_str(),
                    l if l == LanguageId::Typescript.as_str()
                        || l == LanguageId::Javascript.as_str()
                        || l == LanguageId::Vue.as_str()
                )
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
        candidates.extend_from_slice(ctx.lookup(qualified(lookup_key, ctx.file_module, q.expression)));
    } else if state.known_receiver {
        if let Some(owner) = q.owner {
            #[cfg(feature = "lang-rust")]
            let receiver_scope = (owner.language == LanguageId::Rust.as_str())
                .then(|| ctx.linker.member_lookup.rust_receiver(owner))
                .flatten()
                .map(|receiver| receiver.qualname.as_str());
            #[cfg(not(feature = "lang-rust"))]
            let receiver_scope: Option<&str> = None;
            let scope = receiver_scope.unwrap_or_else(|| {
                let scope = owner.qualname.rsplit_once('.').map_or("", |(p, _)| p);
                if owner.language == LanguageId::Rust.as_str() {
                    scope.split_once('<').map_or(scope, |(base, _)| base)
                } else {
                    scope
                }
            });
            candidates.extend_from_slice(ctx.lookup(qualified(lookup_key, scope, q.tail)));
        }
    } else if !state.shadowed {
        candidates.extend_from_slice(ctx.lookup(qualified(lookup_key, ctx.file_module, q.expression)));
        #[cfg(feature = "lang-rust")]
        if candidates.is_empty()
            && ctx
                .profile
                .is_some_and(|p| p.id() == LanguageId::Rust.as_str())
            && !q.tail.contains('.')
        {
            if let Some(owner) = q.owner {
                candidates.extend_from_slice(lookup_rust_associated_member(
                    ctx.lookup(qualified(lookup_key, ctx.file_module, q.first)),
                    ctx.path,
                    q.tail,
                    owner,
                    &ctx.linker.member_lookup,
                ));
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
fn resolve_rust_explicit_paths<'a, 'file, 'res>(
    ctx: &'res PhaseContext<'a, 'file, 'res>,
    rust_keyword_explicit: bool,
    rust_module_target: Option<&rust::resolution::RustExplicitTarget>,
    state: &ReferenceState<'res>,
    candidates: &mut Vec<&'res Node>,
    lookup_key: &mut String,
) -> bool
where
    'a: 'file,
    'file: 'res,
{
    let mut rust_explicit_address = rust_keyword_explicit;
    if rust_keyword_explicit {
        candidates.clear();
        if let Some(target) = rust_module_target {
            candidates.extend_from_slice(ctx.lookup(target.namespace.as_str()));
            if candidates.is_empty() {
                candidates.extend(super::super::rust::resolution::explicit_reexport_targets(
                    ctx.input,
                    ctx.file_module,
                    target,
                    lookup_key,
                ));
            }
        }
    } else if !state.shadowed
        && state.imported.is_none()
        && state.typed_receiver.is_none()
        && state.receiver_member.is_none()
        && !state.lexical_head_declared
        && candidates.is_empty()
    {
        if let Some(target) = rust_module_target {
            candidates.extend(
                ctx.lookup(target.namespace.as_str())
                    .iter()
                    .copied()
                    .filter(|n| {
                        ctx.input
                            .rust_forwarding
                            .module_path_visible(&n.qualname, ctx.file_module)
                    }),
            );
            if candidates.is_empty() {
                candidates.extend(super::super::rust::resolution::explicit_reexport_targets(
                    ctx.input,
                    ctx.file_module,
                    target,
                    lookup_key,
                ));
            }
            rust_explicit_address = !candidates.is_empty();
        }
    }
    rust_explicit_address
}

#[cfg(feature = "lang-rust")]
fn lookup_rust_associated_member<'lookup, 'a>(
    candidates: &[&'a Node],
    expected_path: &str,
    member_name: &str,
    owner: &Node,
    member_lookup: &'lookup semantic_context::LanguageMemberLookup<'_, 'a>,
) -> &'lookup [&'a Node] {
    let mut matching = candidates.iter().copied().filter(|target| {
        target.path == expected_path
            && matches!(
                target.kind.as_str(),
                k if k == NodeKind::Struct.as_str()
                    || k == NodeKind::Enum.as_str()
                    || k == NodeKind::Trait.as_str()
            )
    });
    let Some(ty_node) = matching.next() else {
        return &[];
    };
    if matching.next().is_some() {
        return &[];
    }
    member_lookup.rust(ty_node, member_name, true, owner)
}
