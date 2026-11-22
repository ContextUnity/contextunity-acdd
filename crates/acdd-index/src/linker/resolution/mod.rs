use super::*;
use crate::languages::{dependency_registry::DependencyRegistry, ImportPath, LanguageProfile};
use crate::linker::traits::{
    import_provider_modules, FileRelationContext, ModulesByNamespace, PackageExports,
};
use crate::models::tags::NodeKind;
use crate::typed_facts::FlowState;

type ExternalAliases<'a> = HashMap<&'a str, Option<(String, usize)>>;
mod documentation;
#[cfg(feature = "lang-html")]
mod framework_projections;
mod imports;
mod value_flow_phase;

fn chained_member_receiver<'a, 'idx>(
    receiver: &'idx value_flow::TypeTarget<'a>,
    fields: &str,
    owner: &'a Node,
    path: &str,
    at: crate::semantic::SourcePosition,
    input: &ResolutionInput<'a>,
    profile: Option<&dyn LanguageProfile>,
) -> Option<Cow<'idx, value_flow::TypeTarget<'a>>> {
    let value_flow = input.value_flow;
    let semantic_context = input.semantic_context;
    let mut receiver = Cow::Borrowed(receiver);
    for field in fields.split('.').filter(|field| !field.is_empty()) {
        receiver = match receiver.as_ref() {
            value_flow::TypeTarget::Local(class) => {
                value_flow.field_type(class, field).map(Cow::Borrowed)
            }
            value_flow::TypeTarget::RustDeref { .. }
            | value_flow::TypeTarget::RustReference { .. } => {
                super::rust::value_flow::local_class(receiver.as_ref())
                    .and_then(|class| value_flow.field_type(class, field))
                    .map(Cow::Borrowed)
                    .or_else(|| {
                        match value_flow::SemanticResolver::resolve_member(
                            semantic_context,
                            receiver.as_ref(),
                            field,
                            owner,
                            at,
                        ) {
                            value_flow::Symbol::Type(target) => Some(Cow::Owned(target)),
                            _ => None,
                        }
                    })
            }
            value_flow::TypeTarget::ConfiguredReceiver { .. } => {
                match value_flow::SemanticResolver::resolve_member(
                    semantic_context,
                    receiver.as_ref(),
                    field,
                    owner,
                    at,
                ) {
                    value_flow::Symbol::Type(target) => Some(Cow::Owned(target)),
                    _ => None,
                }
            }
            value_flow::TypeTarget::Builtin(name) => {
                profile.and_then(|profile| {
                    profile.linker().resolve_builtin_property(
                        path,
                        name,
                        field,
                        semantic_context,
                    )
                })
                .map(Cow::Owned)
            }
            _ => None,
        }?;
    }
    Some(receiver)
}

pub(in crate::linker) fn reference_scope<'a>(owner: &'a Node, path: &str) -> &'a str {
    if owner.details["included_from"].is_string()
        && owner.details["source_path"].as_str() == Some(path)
    {
        owner.details["source_qualname"]
            .as_str()
            .filter(|scope| !scope.is_empty())
            .unwrap_or(owner.qualname.as_str())
    } else {
        owner.qualname.as_str()
    }
}

fn document_symbol_candidates<'a>(
    normalized_qualname: &str,
    declared_name: &str,
    source_path: &str,
    target_path: Option<&str>,
    by_qual: &HashMap<&'a str, Vec<&'a Node>>,
    by_name: &HashMap<&'a str, Vec<&'a Node>>,
    by_suffix: &HashMap<&'a str, Vec<&'a Node>>,
) -> Option<Vec<&'a Node>> {
    for tier in [
        by_qual.get(normalized_qualname),
        by_name.get(declared_name),
        by_suffix.get(normalized_qualname),
    ]
    .into_iter()
    .flatten()
    {
        let mut candidates: Option<Vec<&'a Node>> = None;
        for node in tier.iter().copied().filter(|node| {
            !matches!(
                node.kind.as_str(),
                "document" | "invariant" | "component" | "impl"
            ) && target_path.map_or_else(
                    || {
                        acdd_core::config::workspace_path(&node.path).0
                            == acdd_core::config::workspace_path(source_path).0
                    },
                    |path| node.path == path,
                )
        }) {
            candidates.get_or_insert_with(Vec::new).push(node);
        }
        let Some(mut candidates) = candidates else {
            continue;
        };
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        candidates.dedup_by(|left, right| left.id == right.id);
        return Some(candidates);
    }
    None
}

#[derive(Clone, Copy)]
pub(super) struct ResolutionInput<'a> {
    pub(super) all: &'a (dyn crate::FactsLookup + Sync),
    pub(super) interner: &'a crate::interner::LinkerInterner<'a>,
    pub(super) owners: Option<&'a std::collections::BTreeSet<String>>,
    pub(super) dependency_registry: &'a DependencyRegistry,
    pub(super) flows: Option<&'a HashMap<crate::interner::SymbolId, &'a FlowState>>,
    pub(super) sort_per_file: bool,
    pub(super) occurrences: Option<&'a BTreeMap<String, Vec<u8>>>,
    pub(super) by_name: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(super) by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(super) by_suffix: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(super) by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    pub(super) by_module: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(super) node_bindings: &'a super::NodeBindingIndex,
    pub(super) node_rebindings: &'a super::NodeBindingIndex,
    pub(super) children_by_parent: &'a HashMap<crate::interner::SymbolId, Vec<&'a Node>>,
    pub(super) profiles: &'a HashMap<&'a str, &'static dyn LanguageProfile>,
    pub(super) normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    pub(super) modules_by_namespace: &'a ModulesByNamespace<'a>,
    pub(super) rust_library_modules: &'a HashMap<&'a str, (&'a str, &'a Node)>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_forwarding: &'a languages::rust::linker::RustForwarding<'a>,
    pub(super) package_exports: &'a HashMap<&'static str, PackageExports<'a>>,
    pub(super) semantic_context: &'a semantic_context::Context<'a>,
    pub(super) prepared_semantic_facts: &'a framework_relations::PreparedSemanticFacts<'a>,
    pub(super) module_nodes: &'a [&'a Node],
    pub(super) value_flow: &'a value_flow::ValueFlowIndex<'a>,
}

struct LinkerContext<'a> {
    member_lookup: semantic_context::LanguageMemberLookup<'a, 'a>,
    #[cfg(feature = "lang-html")]
    html_templates: html::templates::Registry<'a>,
    #[cfg(feature = "lang-html")]
    template_roots: html::templates::TemplateRootRegistry<'a>,
    #[cfg(feature = "lang-html")]
    html_scripts: html::scripts::Registry<'a>,
}

pub(super) struct FileResolver<'a, 'file> {
    input: &'file ResolutionInput<'a>,
    linker: &'file LinkerContext<'a>,
    path: &'file str,
    path_arc: std::sync::Arc<str>,
    facts: &'a Facts,
    references: &'file [Reference],
    file_module: &'file str,
    language_context: &'file languages::LanguageContext,
    graph: Graph,
    aliases: HashMap<&'file str, HashMap<&'a str, Vec<&'a Node>>>,
    external_aliases: HashMap<&'file str, ExternalAliases<'a>>,
    type_only_aliases: HashMap<&'file str, HashSet<&'a str>>,
    lookup_key: String,
    #[cfg(feature = "lang-html")]
    template_loads: Vec<&'file Reference>,
    #[cfg(feature = "lang-html")]
    template_registry_rules: Vec<&'file languages::manifests::syntax::SyntaxRule>,
    #[cfg(feature = "lang-html")]
    template_rules: Vec<&'file languages::manifests::syntax::SyntaxRule>,
}

impl<'a, 'file> FileResolver<'a, 'file>
where
    'a: 'file,
{
    fn new(
        input: &'file ResolutionInput<'a>,
        path: &'file str,
        facts: &'a Facts,
        references: &'file [Reference],
        file_module: &'file str,
        language_context: &'file languages::LanguageContext,
        linker: &'file LinkerContext<'a>,
    ) -> Self {
        let path_arc = std::sync::Arc::<str>::from(path);
        let mut graph = Graph {
            edges: Vec::with_capacity(facts.edges.len()),
            coverage: Vec::with_capacity(references.len()),
            path_arc: Some(std::sync::Arc::clone(&path_arc)),
            path_pool: HashSet::new(),
            node_id_pool: HashSet::new(),
            evidence_pool: HashSet::new(),
        };
        for edge in &facts.edges {
            graph.push_public_edge(edge);
        }
        Self {
            input,
            linker,
            path,
            path_arc,
            facts,
            references,
            file_module,
            language_context,
            graph,
            aliases: HashMap::new(),
            external_aliases: HashMap::new(),
            type_only_aliases: HashMap::new(),
            lookup_key: String::new(),
            #[cfg(feature = "lang-html")]
            template_loads: Vec::new(),
            #[cfg(feature = "lang-html")]
            template_registry_rules: Vec::new(),
            #[cfg(feature = "lang-html")]
            template_rules: Vec::new(),
        }
    }
}

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
        html::templates::Registry::build(all, flows, input.interner, dependency_registry);
    #[cfg(feature = "lang-html")]
    let template_roots = html::templates::TemplateRootRegistry::build(
        all,
        dependency_registry,
        flows,
        input.interner,
    );
    #[cfg(feature = "lang-html")]
    let html_scripts = html::scripts::Registry::build(
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
                crate::semantic::decode_catalog_references(bytes, &facts.references)
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
                profile.linker().compose_file_relations(&mut FileRelationContext {
                    path,
                    facts,
                    references,
                    dependencies: dependency_registry,
                    semantic_context,
                    by_module,
                    by_id,
                    prepared_semantic_facts,
                    graph: &mut file_resolver.graph,
                });
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
