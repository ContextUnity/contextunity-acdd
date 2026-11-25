#[cfg(feature = "lang-html")]
pub(crate) mod reference_phase;
#[cfg(feature = "lang-html")]
pub(crate) mod relations;
#[cfg(feature = "lang-html")]
pub(crate) mod scripts;
#[cfg(feature = "lang-html")]
pub(crate) mod templates;

#[cfg(feature = "lang-html")]
pub(in crate::linker) struct TemplateReferenceState<'a, 'b> {
    pub(in crate::linker) templates: &'b templates::TemplateRegistry<'a>,
    pub(in crate::linker) scripts: &'b scripts::ScriptRegistry<'a>,
    pub(in crate::linker) loads: Vec<&'b crate::models::Reference>,
    pub(in crate::linker) registry_rules: Vec<&'b crate::languages::manifests::syntax::SyntaxRule>,
    pub(in crate::linker) rules: Vec<&'b crate::languages::manifests::syntax::SyntaxRule>,
}

#[cfg(feature = "lang-html")]
impl super::traits::ReferenceExtension for TemplateReferenceState<'_, '_> {
    fn builtin_in_scope(
        &self,
        expression: &str,
        reference: &crate::models::Reference,
        profile: Option<&dyn crate::languages::LanguageProfile>,
    ) -> Option<bool> {
        if !profile.is_some_and(|profile| profile.id() == "html") {
            return None;
        }
        if templates::match_template_symbol(expression, &self.rules, |tag, library| {
            self.loads.iter().any(|load| {
                load.expression == library
                    && load.line < reference.line
                    && load.alias.as_deref().is_none_or(|selected| selected == tag)
            })
        })
        .is_some()
        {
            return Some(true);
        }
        Some(
            expression.starts_with("template.")
                && profile.is_some_and(|profile| profile.builtin(expression)),
        )
    }
    fn syntax_rules(&self) -> &[&crate::languages::manifests::syntax::SyntaxRule] {
        &self.rules
    }
    fn candidates<'a>(
        &'a self,
        path: &str,
        owner: Option<&crate::models::Node>,
        reference: &crate::models::Reference,
        name: &str,
    ) -> Vec<&'a crate::models::Node> {
        use crate::models::{details::NodeDetailKey, tags::NodeKind};
        if owner.is_some_and(|owner| {
            owner.kind == NodeKind::TemplateScope
                && owner.detail_entry(NodeDetailKey::HTML_HANDLER) == true
        }) {
            self.scripts
                .providers(path, name, reference.line, reference.column)
        } else {
            Vec::new()
        }
    }
    fn resolve(&self, phase: &mut super::traits::ReferencePhase<'_, '_>) -> bool {
        crate::linker::html::templates::resolve_reference(
            phase.input,
            self.templates,
            phase.path,
            &self.loads,
            &self.registry_rules,
            phase.reference,
            phase.graph,
        )
    }
}
