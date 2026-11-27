use super::super::value_flow::types::{Symbol, SymbolRole, TypeTarget};
use crate::languages::LanguageProfile;
use crate::models::Node;
use crate::semantic::TypeExpr;

pub(in crate::linker) fn is_builtin_promise_type(
    owner: &Node,
    profile: &dyn LanguageProfile,
    name: &str,
    role: SymbolRole,
) -> bool {
    matches!(owner.language.as_str(), "typescript" | "javascript" | "vue")
        && matches!(role, SymbolRole::Type)
        && name == "Promise"
        && profile.builtin("Promise")
}

pub(in crate::linker) fn visible(owner: &Node, candidate: &Node) -> bool {
    matches!(owner.language.as_str(), "javascript" | "typescript" | "vue")
        && candidate.kind == crate::models::tags::NodeKind::Function
}

pub(in crate::linker) fn admits_payload(
    owner: &Node,
    wrapper: &TypeTarget<'_>,
    argument_count: usize,
) -> bool {
    matches!(owner.language.as_str(), "typescript" | "javascript" | "vue")
        && argument_count == 1
        && matches!(wrapper, TypeTarget::Builtin(name) if name == "Promise")
}

#[cfg(feature = "lang-typescript")]
pub(in crate::linker) fn prototype_visible_at(
    node: &Node,
    path: &str,
    line: usize,
    column: usize,
) -> bool {
    if node.language != crate::models::tags::LanguageId::Javascript
        || *node.detail_entry(crate::models::details::NodeDetailKey::PROTOTYPE) != true
        || node.path != path
    {
        return true;
    }
    let assigned_line = node
        .detail_entry(crate::models::details::NodeDetailKey::PROTOTYPE_ASSIGNMENT_LINE)
        .as_u64()
        .unwrap_or(node.line as u64) as usize;
    let assigned_column = node
        .detail_entry(crate::models::details::NodeDetailKey::PROTOTYPE_ASSIGNMENT_COLUMN)
        .as_u64()
        .or_else(|| {
            node.detail_entry(crate::models::details::NodeDetailKey::COLUMN)
                .as_u64()
        })
        .unwrap_or(0) as usize;
    (assigned_line, assigned_column) <= (line, column)
}

#[cfg(not(feature = "lang-typescript"))]
pub(in crate::linker) fn prototype_visible_at(
    _node: &Node,
    _path: &str,
    _line: usize,
    _column: usize,
) -> bool {
    true
}

#[cfg(feature = "lang-typescript")]
pub(in crate::linker) use crate::languages::typescript::typed_dom_receiver;

#[cfg(not(feature = "lang-typescript"))]
pub(in crate::linker) fn typed_dom_receiver(_name: &str) -> Option<&'static str> {
    None
}

pub(in crate::linker) fn resolve_applied<'a>(
    owner: &'a Node,
    args: &[TypeExpr],
    nominal: &Symbol<'a>,
) -> Option<TypeTarget<'a>> {
    if matches!(owner.language.as_str(), "typescript" | "javascript" | "vue")
        && args.len() == 1
        && matches!(nominal, Symbol::Type(TypeTarget::Builtin(name)) if name == "Promise")
    {
        Some(TypeTarget::Builtin("Promise".to_owned()))
    } else {
        None
    }
}
