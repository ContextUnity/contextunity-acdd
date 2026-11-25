use crate::models::Node;
use crate::semantic::SourcePosition;

use super::super::{semantic_context::Context, value_flow::TypeTarget};

pub(in crate::linker) fn external_factory_result<'a>(
    context: &Context<'a>,
    owner: &'a Node,
    callee: &str,
    at: SourcePosition,
) -> Option<TypeTarget<'a>> {
    let profile = crate::languages::by_id(owner.language.as_str())?;
    (callee == "await fetch"
        && profile.builtin("fetch")
        && profile.builtin_member("Web.Response", "json")
        && context.unshadowed_global(owner, "fetch", at))
    .then(|| TypeTarget::Builtin("Web.Response".to_owned()))
}

pub(in crate::linker) fn receiver_access(owner: &Node, head: &str) -> Option<(bool, bool)> {
    (matches!(owner.language.as_str(), "javascript" | "typescript")
        && head == "this"
        && owner
            .detail_entry(crate::models::details::NodeDetailKey::RECEIVER_NAME)
            .as_str()
            == Some(head))
    .then_some((
        owner.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC) == true,
        true,
    ))
}
