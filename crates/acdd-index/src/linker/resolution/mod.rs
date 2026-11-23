use crate::languages::module_name;
use crate::linker::compact::{sort_graph, GraphPart};
use crate::linker::{html, semantic_context};
use crate::models::tags::NodeKind;
use rayon::prelude::*;
use std::borrow::Cow;

pub(super) mod context;
mod documentation;
#[cfg(feature = "lang-html")]
mod framework_projections;
mod imports;
pub(in crate::linker) mod input;
mod value_flow_phase;

use context::{FileResolver, LinkerContext};
pub(super) use input::ResolutionInput;

pub(super) fn resolve<'a>(input: ResolutionInput<'a>) -> Vec<GraphPart> {
    let ResolutionInput {
        all,
        owners,
        dependency_registry,
        flows,
        sort_per_file,
        occurrences,
        by_id,
        by_module,
        profiles,
        normalized_imports,
        modules_by_namespace,
        semantic_context,
        prepared_semantic_facts,
        ..
    } = input;
    let member_lookup = semantic_context::LanguageMemberLookup::new(semantic_context);

    #[cfg(feature = "lang-html")]
    let html_templates =
        html::templates::TemplateRegistry::build(all, flows, input.interner, dependency_registry);
    #[cfg(feature = "lang-html")]
    let template_roots = html::templates::TemplateRootRegistry::build(
        all,
        dependency_registry,
        flows,
        input.interner,
    );
    #[cfg(feature = "lang-html")]
    let html_scripts = html::scripts::ScriptRegistry::build(
        all,
        normalized_imports,
        modules_by_namespace,
        dependency_registry,
    );
    let linker_context = LinkerContext {
        member_lookup,
        #[cfg(feature = "lang-html")]
        html_templates,
        #[cfg(feature = "lang-html")]
        template_roots,
        #[cfg(feature = "lang-html")]
        html_scripts,
    };

    let prepared_files: Vec<_> = all
        .iter_facts()
        .filter(|(path, _)| owners.is_none_or(|owner_paths| owner_paths.contains(*path)))
        .map(|(path, file)| {
            (
                path,
                file,
                dependency_registry.language_context_for_path(path),
            )
        })
        .collect();

    let parts: Vec<GraphPart> = prepared_files
        .par_iter()
        .map(|(path, facts, file_language_context)| {
            let path = *path;
            let facts = *facts;
            let restored_references = occurrences.and_then(|files| files.get(path)).map(|bytes| {
                crate::typed_facts::decode_catalog_references(bytes, &facts.references)
            });
            let references = restored_references.as_deref().unwrap_or(&facts.references);
            let profile = profiles.get(path).copied();
            let file_module: Cow<'_, str> = facts
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Module.as_str())
                .map(|node| Cow::Borrowed(node.qualname.as_str()))
                .unwrap_or_else(|| {
                    Cow::Owned(
                        profile
                            .map_or_else(|| module_name(path), |profile| profile.module_name(path)),
                    )
                });
            let mut file_resolver = FileResolver::new(
                &input,
                path,
                facts,
                references,
                file_module.as_ref(),
                file_language_context.as_ref(),
                &linker_context,
            );
            imports::phase_imports(&mut file_resolver);
            #[cfg(feature = "lang-html")]
            framework_projections::phase_framework_projections(&mut file_resolver);
            value_flow_phase::phase_value_flow(&mut file_resolver);
            documentation::phase_documentation(&mut file_resolver);
            if let Some(profile) = profile {
                profile.linker().compose_file_relations(
                    &mut crate::linker::traits::FileRelationContext {
                        path,
                        facts,
                        references,
                        dependencies: dependency_registry,
                        semantic_context,
                        by_module,
                        by_id,
                        prepared_semantic_facts,
                        graph: &mut file_resolver.graph,
                    },
                );
            }
            if sort_per_file {
                sort_graph(&mut file_resolver.graph);
            }
            debug_assert_eq!(file_resolver.path_arc.as_ref(), path);
            std::mem::take(&mut file_resolver.graph)
        })
        .map(|graph| GraphPart::new(graph, occurrences.is_some()))
        .collect();
    parts
}
