use crate::interner::{LinkerInterner, SymbolId};
use crate::models::{
    tags::{LanguageId, NodeKind},
    Facts, Node,
};
use crate::{
    languages::{
        self, dependency_registry::DependencyRegistry, ImportPath, LanguageFamily, LanguageProfile,
    },
    linker::traits::{ImportNormalizationContext, ModulesByNamespace, PackageExports},
};
use hashbrown::HashMap;
use rayon::prelude::*;

pub const PARALLEL_LINKER_PREP_MIN_FILES: usize = 32;
use std::path::Path;

pub(super) struct NamespaceIndexes<'a> {
    pub(super) profiles: HashMap<&'a str, &'static dyn LanguageProfile>,
    pub(super) normalized_imports: HashMap<(&'a str, &'a str), Option<ImportPath>>,
    pub(super) modules_by_namespace: ModulesByNamespace<'a>,
    pub(super) rust_library_modules: HashMap<&'a str, (&'a str, &'a Node)>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_forwarding: languages::rust::linker::RustForwarding<'a>,
}

impl<'a> NamespaceIndexes<'a> {
    pub(super) fn build(
        all: &'a (dyn crate::FactsLookup + Sync),
        nodes: &[&'a Node],
        root: Option<&Path>,
        dependency_registry: &'a DependencyRegistry,
        interner: &mut LinkerInterner<'a>,
    ) -> Self {
        let profiles: HashMap<&str, &'static dyn LanguageProfile> = all
            .iter_facts()
            .filter_map(|(path, facts)| {
                facts
                    .nodes
                    .iter()
                    .find(|node| node.kind == NodeKind::Module.as_str())
                    .and_then(|node| languages::by_id(&node.language))
                    .map(|profile| (path, profile))
            })
            .collect();
        let mut family_module_stems: HashMap<LanguageFamily, std::collections::HashSet<&str>> =
            HashMap::new();
        for (path, profile) in &profiles {
            if profile.linker().needs_family_module_stems() {
                family_module_stems
                    .entry(profile.family())
                    .or_default()
                    .insert(languages::module_stem(path).trim_end_matches("/index"));
            }
        }
        let rust_library_modules: HashMap<&str, (&str, &Node)> = all
            .iter_facts()
            .filter_map(|(path, facts)| {
                let (workspace, local_path) = languages::workspace_path(path);
                if local_path != "src/lib.rs" {
                    return None;
                }
                let module = facts.nodes.iter().find(|node| {
                    node.kind == NodeKind::Module.as_str()
                        && node.language == LanguageId::Rust.as_str()
                })?;
                let name = dependency_registry.rust_library_for_path(path)?;
                Some((workspace, (name, module)))
            })
            .collect();
        let files: Vec<_> = all.iter_facts().collect();
        let norm_ctx = ImportNormalizationContext {
            root,
            dependencies: dependency_registry,
            profiles: &profiles,
            family_module_stems: &family_module_stems,
            #[cfg(feature = "lang-rust")]
            rust_library_modules: &rust_library_modules,
        };
        let normalize_file = |file: &(&'a str, &'a Facts)| {
            let (path, facts) = *file;
            let mut normalized_imports = HashMap::new();
            if let Some(profile) = profiles.get(path).copied() {
                profile.linker().normalize_file_imports(
                    profile,
                    path,
                    facts,
                    &norm_ctx,
                    &mut normalized_imports,
                );
            }
            normalized_imports
        };
        let build_imports = || {
            if files.len() >= PARALLEL_LINKER_PREP_MIN_FILES {
                files.par_iter().flat_map_iter(normalize_file).collect()
            } else {
                files.iter().flat_map(normalize_file).collect()
            }
        };
        let build_modules = || {
            let mut modules_by_namespace: ModulesByNamespace<'a> = HashMap::new();
            for node in nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Module.as_str())
            {
                if let Some(profile) = profiles.get(node.path.as_str()) {
                    let (workspace, local_path) = languages::workspace_path(&node.path);
                    let local_module = profile.module_name(local_path);
                    let mut namespaces = vec![node.qualname.clone()];
                    if !workspace.is_empty() && node.qualname.starts_with(&format!("{workspace}."))
                    {
                        namespaces.push(node.qualname[workspace.len() + 1..].to_string());
                    }
                    if !namespaces.contains(&local_module) {
                        namespaces.push(local_module);
                    }
                    profile.linker().module_namespaces(
                        *profile,
                        node,
                        dependency_registry,
                        &mut namespaces,
                    );
                    if let Some((_, source_relative)) = local_path.split_once("/src/") {
                        let source_module = profile.module_name(source_relative);
                        if !namespaces.contains(&source_module) {
                            namespaces.push(source_module);
                        }
                    } else if let Some(source_relative) = local_path.strip_prefix("src/") {
                        let source_module = profile.module_name(source_relative);
                        if !namespaces.contains(&source_module) {
                            namespaces.push(source_module);
                        }
                    }
                    for namespace in namespaces {
                        let modules = modules_by_namespace
                            .entry(profile.family())
                            .or_default()
                            .entry(namespace)
                            .or_default();
                        if !modules.iter().any(|existing| existing.id == node.id) {
                            modules.push(node);
                        }
                    }
                }
            }
            modules_by_namespace
        };
        let (normalized_imports, modules_by_namespace) =
            if files.len() >= PARALLEL_LINKER_PREP_MIN_FILES {
                rayon::join(build_imports, build_modules)
            } else {
                (build_imports(), build_modules())
            };

        #[cfg(feature = "lang-rust")]
        let rust_forwarding = languages::rust::linker::RustForwarding::build(
            all,
            &modules_by_namespace,
            &normalized_imports,
            interner,
        );

        Self {
            profiles,
            normalized_imports,
            modules_by_namespace,
            rust_library_modules,
            #[cfg(feature = "lang-rust")]
            rust_forwarding,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn package_exports<'index, 'a>(
    profiles: &'index HashMap<&'a str, &'static dyn LanguageProfile>,
    modules_by_namespace: &'index ModulesByNamespace<'a>,
    by_module: &'index HashMap<&'a str, Vec<&'a Node>>,
    node_rebindings: &'index super::NodeBindingIndex,
    all: &'a dyn crate::FactsLookup,
    root: Option<&Path>,
    interner: &mut LinkerInterner<'a>,
    #[cfg(feature = "lang-rust")] rust_forwarding: &'index languages::rust::linker::RustForwarding<
        'a,
    >,
) -> HashMap<&'static str, PackageExports<'a>> {
    let mut language_profiles: HashMap<&'static str, &'static dyn LanguageProfile> = profiles
        .values()
        .map(|profile| (profile.id(), *profile))
        .collect();
    #[cfg(feature = "lang-rust")]
    language_profiles.remove(LanguageId::Rust.as_str());
    let mut package_exports: HashMap<&'static str, PackageExports<'a>> = language_profiles
        .into_iter()
        .map(|(id, profile)| {
            (
                id,
                profile.linker().package_exports(
                    interner,
                    node_rebindings,
                    all,
                    modules_by_namespace,
                    by_module,
                    root,
                ),
            )
        })
        .collect();
    package_exports.insert("", HashMap::new());
    #[cfg(feature = "lang-rust")]
    package_exports.insert(
        LanguageId::Rust.as_str(),
        rust_forwarding.package_exports().clone(),
    );
    package_exports
}

pub(super) struct InheritanceIndexes<'a> {
    pub(super) python_receivers: super::python::receivers::PythonReceivers<'a>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_members: languages::rust::linker::RustMembers<'a>,
}

impl<'a> InheritanceIndexes<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build<'index>(
        all: &'a (dyn crate::FactsLookup + Sync),
        interner: &LinkerInterner<'a>,
        #[cfg(feature = "lang-rust")] flows: Option<
            &'index HashMap<SymbolId, &'a crate::typed_facts::FlowState>,
        >,
        by_id: &'index HashMap<SymbolId, &'a Node>,
        by_qual: &'index HashMap<&'a str, Vec<&'a Node>>,
        node_bindings: &'index super::NodeBindingIndex,
        node_rebindings: &'index super::NodeBindingIndex,
        by_module: &'index HashMap<&'a str, Vec<&'a Node>>,
        namespaces: &'index NamespaceIndexes<'a>,
        package_exports: &'index HashMap<&'static str, PackageExports<'a>>,
        dependency_registry: &'a DependencyRegistry,
    ) -> Self {
        let build_python = || {
            #[cfg(feature = "lang-python")]
            {
                languages::python::linker::build_python_receivers(
                    all,
                    interner,
                    by_id,
                    by_qual,
                    node_bindings,
                    node_rebindings,
                    by_module,
                    &namespaces.profiles,
                    &namespaces.normalized_imports,
                    &namespaces.modules_by_namespace,
                    #[cfg(feature = "lang-rust")]
                    &namespaces.rust_forwarding,
                    package_exports,
                    dependency_registry,
                )
            }
            #[cfg(not(feature = "lang-python"))]
            {
                let _ = (interner, by_id, by_qual, node_bindings, node_rebindings, by_module, namespaces, package_exports, dependency_registry);
                super::python::receivers::PythonReceivers::build(
                    all,
                    |_, _| super::python::receivers::Base::Unknown,
                    |_, _| None,
                )
            }
        };
        #[cfg(feature = "lang-rust")]
        let (python_receivers, rust_members) = rayon::join(build_python, || {
            languages::rust::linker::RustMembers::build(
                all,
                interner,
                flows,
                &namespaces.rust_forwarding,
            )
        });
        #[cfg(not(feature = "lang-rust"))]
        let python_receivers = build_python();
        Self {
            python_receivers,
            #[cfg(feature = "lang-rust")]
            rust_members,
        }
    }
}
