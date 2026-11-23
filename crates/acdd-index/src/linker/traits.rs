//! Language-owned import and receiver resolution used by the shared linker.

pub use crate::languages::profile::{
    FileRelationContext, ImportContext, ImportNormalizationContext, ImportResolution,
    LanguageLinker,
};
pub(crate) use crate::languages::profile::{resolve_default_import, GENERIC_LINKER};
pub(crate) use super::types::{
    disambiguate_rust_cfg_siblings, filter_stubs_and_overloads, import_provider_modules,
    insert_package_export, package_export, BuiltinPropertyLookup,
};
pub use super::types::{ModulesByNamespace, PackageExports};

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
