use crate::languages::module_name;
use crate::languages::profile::ProfileLinker;
use crate::linker::compact::{sort_graph, GraphPart};
use crate::linker::semantic_context;
use crate::models::tags::NodeKind;
use rayon::prelude::*;
use std::borrow::Cow;

pub(super) mod context;
mod documentation;
mod imports;
pub(crate) mod input;
mod value_flow_phase;

use context::{FileResolver, LinkerContext};
pub(super) use input::ResolutionInput;

pub(super) fn resolve<'a>(input: ResolutionInput<'a>) -> Vec<GraphPart> {
    let ResolutionInput {
        all,
        owners,
        dependency_registry,
        sort_per_file,
        occurrences,
        by_id,
        by_module,
        profiles,
        semantic_context,
        prepared_semantic_facts,
        ..
    } = input;
    let member_lookup = semantic_context::LanguageMemberLookup::new(semantic_context);

    let workspace = crate::linker::traits::ReferenceWorkspace {
        all: input.all,
        flows: input.flows,
        interner: input.interner,
        dependencies: input.dependency_registry,
        imports: input.normalized_imports,
        modules: input.modules_by_namespace,
    };
    let extensions: Vec<_> = crate::languages::profiles()
        .filter_map(|profile| profile.linker().reference_extension_factory(&workspace))
        .collect();
    let linker_context = LinkerContext { member_lookup };

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
                .find(|node| node.kind == NodeKind::Module)
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
            file_resolver.reference_extension = extensions.iter().find_map(|extension| {
                extension.prepare(
                    &input,
                    path,
                    references,
                    file_language_context,
                    &mut file_resolver.graph,
                )
            });
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
