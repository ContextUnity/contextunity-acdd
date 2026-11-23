use super::context::FileResolver;
use crate::models::tags::{EdgeKind, ReferenceKind};

mod candidates;
mod context;
mod dispatch;
mod inferred;
mod members;
mod parameters;
mod relation_edges;

pub(super) fn phase_value_flow<'a, 'file>(resolver: &mut FileResolver<'a, 'file>)
where
    'a: 'file,
{
    let mut lookup_key = String::new();
    let (ctx, graph) = context::PhaseContext::from_resolver(resolver);
    let typed_parameters = parameters::collect_typed_parameters(&ctx, &mut lookup_key);
    let mut candidates = Vec::new();
    for r in ctx.references.iter().filter(|r| {
        matches!(
            r.kind.as_str(),
            k if k == EdgeKind::Calls.as_str()
                || k == EdgeKind::Inherits.as_str()
                || k == EdgeKind::Implements.as_str()
                || k == EdgeKind::Bases.as_str()
                || k == EdgeKind::Decorates.as_str()
                || k == EdgeKind::Mutates.as_str()
                || k == EdgeKind::Handles.as_str()
                || k == EdgeKind::References.as_str()
                || k == ReferenceKind::Injects.as_str()
        )
    }) {
        dispatch::resolve_reference(
            &ctx,
            graph,
            &typed_parameters,
            r,
            &mut candidates,
            &mut lookup_key,
        );
    }
}
