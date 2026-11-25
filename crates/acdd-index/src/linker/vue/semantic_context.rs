use crate::models::Node;

pub(in crate::linker) fn template_import_visible(owner: &Node) -> bool {
    owner.kind == crate::models::tags::NodeKind::TemplateScope
        && owner.language == crate::models::tags::LanguageId::Vue
}
