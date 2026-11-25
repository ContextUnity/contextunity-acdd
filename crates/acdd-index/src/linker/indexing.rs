use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::profile::ProfileLinker;
use crate::models::{tags::NodeKind, Facts, Node};
use crate::{
    languages::{
        self, dependency_registry::DependencyRegistry, ImportPath, LanguageFamily, LanguageProfile,
    },
    linker::traits::{ImportNormalizationContext, ModulesByNamespace, PackageExports},
};
use hashbrown::HashMap;
use rayon::prelude::*;

const PARALLEL_LINKER_PREP_MIN_FILES: usize = 32;
use std::path::Path;

pub(super) struct NamespaceIndexes<'a> {
    pub(super) profiles: HashMap<&'a str, &'static dyn LanguageProfile>,
    pub(super) normalized_imports: HashMap<(&'a str, &'a str), Option<ImportPath>>,
    pub(super) modules_by_namespace: ModulesByNamespace<'a>,
}

impl<'a> NamespaceIndexes<'a> {
    fn collect_modules_by_namespace(
        nodes: &[&'a Node],
        profiles: &HashMap<&str, &'static dyn LanguageProfile>,
        dependency_registry: &'a DependencyRegistry,
    ) -> ModulesByNamespace<'a> {
        let mut modules_by_namespace: ModulesByNamespace<'a> = HashMap::new();
        for node in nodes.iter().filter(|node| node.kind == NodeKind::Module) {
            let Some(profile) = profiles.get(node.path.as_str()) else {
                continue;
            };
            let (workspace, local_path) = languages::workspace_path(&node.path);
            let local_module = profile.module_name(local_path);
            let mut namespaces = vec![node.qualname.clone()];
            if !workspace.is_empty() && node.qualname.starts_with(&format!("{workspace}.")) {
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
        modules_by_namespace
    }

    pub(super) fn build(
        all: &'a (dyn crate::FactsLookup + Sync),
        nodes: &[&'a Node],
        root: Option<&Path>,
        dependency_registry: &'a DependencyRegistry,
    ) -> Self {
        let profiles: HashMap<&str, &'static dyn LanguageProfile> = all
            .iter_facts()
            .filter_map(|(path, facts)| {
                facts
                    .nodes
                    .iter()
                    .find(|node| node.kind == NodeKind::Module)
                    .and_then(|node| languages::by_id(node.language.as_str()))
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
        let files: Vec<_> = all.iter_facts().collect();
        let norm_ctx = ImportNormalizationContext {
            root,
            dependencies: dependency_registry,
            profiles: &profiles,
            family_module_stems: &family_module_stems,
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
        let build_modules =
            || Self::collect_modules_by_namespace(nodes, &profiles, dependency_registry);
        let (normalized_imports, modules_by_namespace) =
            if files.len() >= PARALLEL_LINKER_PREP_MIN_FILES {
                rayon::join(build_imports, build_modules)
            } else {
                (build_imports(), build_modules())
            };

        Self {
            profiles,
            normalized_imports,
            modules_by_namespace,
        }
    }
}

pub(super) fn package_exports<'index, 'a>(
    profiles: &'index HashMap<&'a str, &'static dyn LanguageProfile>,
    modules_by_namespace: &'index ModulesByNamespace<'a>,
    by_module: &'index HashMap<&'a str, Vec<&'a Node>>,
    node_rebindings: &'index super::NodeBindingIndex,
    all: &'a dyn crate::FactsLookup,
    root: Option<&Path>,
    interner: &mut LinkerInterner<'a>,
) -> HashMap<&'static str, PackageExports<'a>> {
    let language_profiles: HashMap<&'static str, &'static dyn LanguageProfile> = profiles
        .values()
        .map(|profile| (profile.id(), *profile))
        .collect();
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
    package_exports
}

pub(super) struct InheritanceIndexes<'a> {
    pub(super) python_receivers: super::python::receivers::PythonReceivers<'a>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_members: super::rust::profile_linker::RustMembers<'a>,
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
        #[cfg(feature = "lang-rust")]
        rust_forwarding: &'index super::rust::forwarding::RustForwarding<'a>,
    ) -> Self {
        let build_python = || {
            #[cfg(feature = "lang-python")]
            {
                super::python::profile_linker::build_python_receivers(
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
                    package_exports,
                    dependency_registry,
                )
            }
            #[cfg(not(feature = "lang-python"))]
            {
                let _ = (
                    interner,
                    by_id,
                    by_qual,
                    node_bindings,
                    node_rebindings,
                    by_module,
                    namespaces,
                    package_exports,
                    dependency_registry,
                );
                super::python::receivers::PythonReceivers::build(
                    all,
                    |_, _| super::python::receivers::Base::Unknown,
                    |_, _| None,
                )
            }
        };
        #[cfg(feature = "lang-rust")]
        let (python_receivers, rust_members) = rayon::join(build_python, || {
            super::rust::profile_linker::RustMembers::build(
                all,
                interner,
                flows,
                rust_forwarding,
                dependency_registry,
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
