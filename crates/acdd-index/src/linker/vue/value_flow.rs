use crate::models::Node;

pub(in crate::linker) fn setup_scope_complete(owner: &Node, parent_is_module: bool) -> bool {
    owner.kind == crate::models::tags::NodeKind::TemplateScope
        && owner.language == crate::models::tags::LanguageId::Vue
        && parent_is_module
}

pub(in crate::linker) fn owns_module_fields(owner: &Node) -> bool {
    owner.kind == crate::models::tags::NodeKind::Module
        && owner.language == crate::models::tags::LanguageId::Vue
}
