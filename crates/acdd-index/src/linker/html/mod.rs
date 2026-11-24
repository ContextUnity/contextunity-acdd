#[cfg(feature = "lang-html")]
pub(super) mod relations;
#[cfg(feature = "lang-html")]
pub(super) mod scripts;
#[cfg(feature = "lang-html")]
pub(crate) mod templates;

#[cfg(feature = "lang-html")]
pub(crate) fn special_reference(phase: &mut super::traits::ReferencePhase<'_, '_>) -> bool {
    crate::linker::html::templates::resolve_reference(
        phase.input,
        phase.templates,
        phase.path,
        phase.loads,
        phase.registry_rules,
        phase.reference,
        phase.graph,
    )
}
