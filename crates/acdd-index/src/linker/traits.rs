//! Language-owned import and receiver resolution used by the shared linker.

pub(crate) use super::types::{
    disambiguate_rust_cfg_siblings, filter_stubs_and_overloads, import_provider_modules,
    insert_package_export, package_export, BuiltinPropertyLookup,
};
pub use super::types::{ModulesByNamespace, PackageExports};
pub(crate) use crate::languages::profile::{resolve_default_import, GENERIC_LINKER};
pub use crate::languages::profile::{
    FileRelationContext, ImportContext, ImportNormalizationContext, ImportResolution,
    LanguageLinker,
};

/// A workspace namespace selected by an explicit module path.
pub struct ExplicitModuleTarget {
    pub(in crate::linker) namespace: String,
    pub(in crate::linker) workspace: String,
}

/// Borrowed state used by language-owned reference dispatch.
pub struct ReferencePhase<'a, 'b> {
    pub(in crate::linker) input: &'b super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) reference: &'b crate::models::Reference,
    pub(in crate::linker) profile: Option<&'a dyn crate::languages::LanguageProfile>,
    pub(in crate::linker) owner: Option<&'a crate::models::Node>,
    pub(in crate::linker) path: &'b str,
    pub(in crate::linker) file_module: &'b str,
    pub(in crate::linker) ordinary_receivers: &'b hashbrown::HashSet<&'b str>,
    pub(in crate::linker) graph: &'b mut crate::models::compact_graph::CompactGraph,
    #[cfg(feature = "lang-html")]
    pub(in crate::linker) templates: &'b super::html::templates::TemplateRegistry<'a>,
    #[cfg(feature = "lang-html")]
    pub(in crate::linker) loads: &'b [&'b crate::models::Reference],
    #[cfg(feature = "lang-html")]
    pub(in crate::linker) registry_rules:
        &'b [&'b crate::languages::manifests::syntax::SyntaxRule],
}

#[cfg(test)]
mod tests {
    use crate::linker::types::filter_stubs_and_overloads;
    use crate::models::Node;

    fn node(id: &str, is_stub: bool, is_overload: bool) -> Node {
        Node {
            native_semantic: None,
            id: id.to_owned(),
            kind: "function".to_owned(),
            name: id.to_owned(),
            qualname: id.to_owned(),
            path: "module.ts".to_owned(),
            line: 1,
            end_line: 1,
            is_test: false,
            language: "typescript".to_owned(),
            generated: false,
            details: serde_json::json!({
                "is_stub": is_stub,
                "is_overload": is_overload,
            }),
        }
    }

    #[test]
    fn filter_stubs_and_overloads_keeps_concrete_candidates_when_present() {
        let stub = node("stub", true, false);
        let implementation = node("implementation", false, false);
        let overload = node("overload", false, true);
        let mut candidates = vec![&stub, &implementation, &overload];

        filter_stubs_and_overloads(&mut candidates);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["implementation"]
        );
    }
}
