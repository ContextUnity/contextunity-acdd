use super::context::PhaseContext;
use super::inferred::{RefQuery, ReferenceState};
use super::relation_edges;
use super::*;
use crate::models::tags::{CoverageStatus, EdgeConfidence, EdgeKind, LanguageId, NodeKind};

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
        && !matches!(state.receiver_member, Some(receivers::Member::Local(_)))
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
                            m.kind == NodeKind::Module.as_str() && m.qualname == ctx.file_module
                        }))
                || (!state.shadowed
                    && q.r.kind == EdgeKind::References.as_str()
                    && n.qualname == q.expression)
                || state.rust_explicit_address
        });
    }
    #[cfg(feature = "lang-html")]
    if q.r.kind == EdgeKind::Calls.as_str()
        && q.tail.is_empty()
        && q.owner.is_some_and(|owner| {
            owner.kind == NodeKind::TemplateScope.as_str() && owner.details["html_handler"] == true
        })
        && state.imported.is_none()
        && state.typed_receiver.is_none()
    {
        candidates.extend(
            ctx.linker
                .html_scripts
                .providers(ctx.path, q.first, q.r.line, q.r.column),
        );
    }
    if q.r.kind == EdgeKind::Calls.as_str()
        && q.tail.is_empty()
        && state.imported.is_none()
        && !state.shadowed
        && candidates.is_empty()
    {
        if let Some(owner) = q.owner {
            candidates.extend(ctx.input.semantic_context.implicit_callables(owner, q.first));
        }
    }
    candidates.retain(|n| {
        #[cfg(feature = "lang-rust")]
        let rust_macro_reference =
            rust::resolution::macro_reference_candidate(q.active_profile, q.owner, n);
        #[cfg(not(feature = "lang-rust"))]
        let rust_macro_reference = false;
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
            k if k == EdgeKind::Mutates.as_str() => n.kind == NodeKind::Field.as_str(),
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
                ) || (n.kind == NodeKind::Function.as_str()
                    && q.tail.is_empty()
                    && state.imported.is_some()
                    && !ctx.is_type_only(q.first, q.owner, state.definition_scope)
                    && ctx
                        .input
                        .interner
                        .lookup_symbol(&q.r.source)
                        .is_some_and(|source| {
                            ctx.value_queries.contains(&(
                                source,
                                q.r.expression.as_str(),
                                q.r.line,
                                q.r.column,
                            ))
                        }))
                    || rust_macro_reference
                    || PhaseContext::is_default_export_vue_module(n)
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
            typescript::resolution::admits_static_receiver_candidate(candidate, q.expression)
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
    #[cfg(feature = "lang-rust")]
    if candidates.len() > 1
        && ctx
            .profile
            .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
        && matches!(
            r.kind.as_str(),
            k if k == EdgeKind::Calls.as_str() || k == EdgeKind::Handles.as_str()
        )
        && candidates
            .iter()
            .any(|n| ctx.input.semantic_context.is_rust_inherent_member(n))
        && candidates
            .iter()
            .any(|n| !ctx.input.semantic_context.is_rust_inherent_member(n))
    {
        candidates.retain(|n| ctx.input.semantic_context.is_rust_inherent_member(n));
    }
    #[cfg(feature = "lang-rust")]
    super::super::rust::resolution::macro_analysis_gap(
        ctx.profile,
        r,
        candidates,
        ctx.path,
        graph,
    );
    #[cfg(not(feature = "lang-rust"))]
    let _ = (ctx, graph, r);
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
    #[cfg(feature = "lang-rust")]
    if r.kind == EdgeKind::Implements.as_str() {
        if let Some(implementation) = q.owner.filter(|node| node.kind == NodeKind::Impl.as_str()) {
            (source, source_heuristic) = rust::resolution::impl_source(
                implementation,
                &implementation.qualname,
                ctx.file_module,
                ctx.input.by_qual,
                ctx.input.by_name,
                ctx.aliases,
                lookup_key,
            );
        }
    }
    #[cfg(not(feature = "lang-rust"))]
    let _ = lookup_key;
    let is_builtin = candidates.is_empty()
        && !state.shadowed
        && state.imported.is_none()
        && !state.lexical_head_declared
        && ctx.builtin_in_scope(q.expression, r);
    let rust_stdlib_builtin = is_builtin
        && ctx
            .profile
            .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
        && q.first != "Self";
    let python_stdlib_builtin = is_builtin
        && ctx
            .profile
            .is_some_and(|profile| profile.id() == LanguageId::Python.as_str());
    let html_template_builtin = is_builtin
        && q.expression.starts_with("template.")
        && q.active_profile
            .is_some_and(|profile| profile.id() == LanguageId::Html.as_str());
    let js_builtin_origin = is_builtin
        .then(|| {
            javascript::resolution::builtin_origin(
                q.first,
                q.active_profile,
                ctx.input
                    .semantic_context
                    .autoimport_evidence(ctx.path, q.first),
            )
        })
        .flatten();
    #[cfg(feature = "lang-html")]
    let html_builtin_evidence =
        html_template_builtin.then(|| html::templates::builtin_evidence(q.expression, ctx.template_rules));
    #[cfg(feature = "lang-html")]
    let html_builtin_origin = html_builtin_evidence
        .as_deref()
        .unwrap_or("builtin:shared_template");
    #[cfg(not(feature = "lang-html"))]
    let html_builtin_origin = "builtin:shared_template";
    let component_builtin_origin = is_builtin
        .then(|| ctx.language_context.component_macro(q.expression))
        .flatten()
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
            (is_builtin && ctx.configured_autoimport(q.expression)).then(|| {
                ctx.input
                    .semantic_context
                    .autoimport_evidence(ctx.path, q.expression)
                    .unwrap_or("configured runtime")
                    .to_owned()
            })
        });
    let component_builtin_origin = component_builtin_origin.as_deref();
    let status = if matches!(state.typed_receiver, Some(ReceiverType::Ambiguous))
        || matches!(state.receiver_member, Some(receivers::Member::Ambiguous))
    {
        CoverageStatus::Ambiguous.as_str()
    } else if (candidates.len() == 1 || is_builtin) && source.is_some() && !source_heuristic {
        CoverageStatus::Resolved.as_str()
    } else if candidates.is_empty() || source.is_none() || source_heuristic {
        CoverageStatus::Unresolved.as_str()
    } else {
        CoverageStatus::Ambiguous.as_str()
    };
    #[cfg(feature = "lang-rust")]
    let rust_external_import = super::super::rust::semantic_context::external_annotation_type(
        ctx.profile,
        q.first,
        q.first,
        ctx.path,
        ctx.input.dependency_registry,
    )
    .is_some();
    #[cfg(not(feature = "lang-rust"))]
    let rust_external_import = false;
    let external_origin =
        if let Some(receivers::Member::External(module, import_line, _, _)) = state.receiver_member
        {
            Some((module, import_line))
        } else if let Some(ReceiverType::External(module, import_line)) = state.typed_receiver {
            (import_line <= r.line).then_some((module, import_line))
        } else if rust_stdlib_builtin {
            Some(("Rust standard library", 0))
        } else if python_stdlib_builtin {
            Some(("Python standard library", 0))
        } else if html_template_builtin {
            Some((html_builtin_origin, 0))
        } else if let Some(origin) = component_builtin_origin {
            Some((origin, 0))
        } else if let Some(origin) = js_builtin_origin {
            Some((origin, 0))
        } else if status == CoverageStatus::Unresolved.as_str()
            && !state.shadowed
            && candidates.is_empty()
            && (state.imported.is_some_and(|targets| targets.is_empty())
                || rust_external_import
                || ctx.external_call_for(q.first, q.owner, r.position()).is_some())
        {
            if let Some(target) = ctx
                .external_call_for(q.first, q.owner, r.position())
                .filter(|(_, import_line)| *import_line <= r.line)
            {
                Some((target.0.as_str(), target.1))
            } else if rust_external_import {
                Some((q.first, 0))
            } else {
                None
            }
        } else {
            None
        };
    let framework_provider = match state.receiver_member {
        Some(receivers::Member::External(_, _, provider, _)) => provider,
        _ => None,
    };
    let status = if external_origin.is_some() {
        CoverageStatus::External.as_str()
    } else {
        status
    };
    let stub_target = match candidates {
        [candidate] if ctx.language_linker.is_declaration_only(&candidate.path) => {
            Some(candidate.path.as_str())
        }
        _ => None,
    };
    let evidence: Cow<'static, str> = if let Some(stub_path) = stub_target {
        format!("type-stub declaration at {stub_path}; runtime implementation not statically indexed").into()
    } else if rust_stdlib_builtin {
        format!("Rust standard library prelude: {}", q.expression).into()
    } else if python_stdlib_builtin {
        format!("Python standard library built-in: {}", q.expression).into()
    } else if html_template_builtin {
        format!("{html_builtin_origin} {}", q.expression).into()
    } else if let Some(origin) = component_builtin_origin {
        format!("{origin} built-in: {}", q.expression).into()
    } else if let Some(origin) = js_builtin_origin {
        format!("{origin} built-in: {}", q.expression).into()
    } else if let Some((module, import_line)) = external_origin {
        if let Some(provider) = framework_provider {
            let diagnostic_marker = if ctx.profile.is_some_and(|profile| {
                profile.id() == LanguageId::Python.as_str() && !profile.is_stdlib(module)
            }) {
                "; diagnostic_reason=framework_magic"
            } else {
                ""
            };
            format!("finite Python framework member {provider}.{} via external import {module} at line {import_line}{diagnostic_marker}", q.tail).into()
        } else if ctx.profile.is_some_and(|profile| {
            matches!(
                profile.id(),
                p if p == LanguageId::Javascript.as_str() || p == LanguageId::Typescript.as_str()
            ) && profile.is_stdlib(module)
        }) {
            format!("builtin:node import {module}: {}", q.expression).into()
        } else {
            match (
                r.kind == EdgeKind::Calls.as_str(),
                ctx.profile
                    .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
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
            source.is_some()
        )
        .into()
    };
    graph.reference_coverage(ctx.path, r, status, evidence);
    if let (Some(source), [candidate]) = (source, candidates) {
        let exact_same_class_member_reference = r.kind == EdgeKind::References.as_str()
            && matches!(
                &state.receiver_member,
                Some(receivers::Member::Local(member))
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
        let kind_str = if r.kind == EdgeKind::Bases.as_str()
            || r.kind == EdgeKind::Inherits.as_str()
        {
            let is_interface = candidate.kind == NodeKind::Interface.as_str()
                || python::resolution::has_protocol_or_abc_base(candidate);
            if is_interface && q.owner.is_some_and(|n| n.kind != NodeKind::Interface.as_str()) {
                EdgeKind::Implements.as_str()
            } else {
                EdgeKind::Inherits.as_str()
            }
        } else {
            r.kind.as_str()
        };
        graph.push_edge(
            ctx.path,
            src,
            dst,
            kind_str,
            r.line,
            r.expression.as_str(),
            if source_heuristic {
                EdgeConfidence::Heuristic.as_str()
            } else if state.typed_receiver.is_some()
                || (state.receiver_member.is_some() && !exact_same_class_member_reference)
            {
                EdgeConfidence::Inferred.as_str()
            } else {
                EdgeConfidence::Exact.as_str()
            },
        );
        if (kind_str == EdgeKind::Implements.as_str() || kind_str == EdgeKind::Inherits.as_str())
            && r.kind != EdgeKind::Decorates.as_str()
        {
            let source_container_id =
                if q.owner.is_some_and(|n| n.kind == NodeKind::Impl.as_str()) {
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
