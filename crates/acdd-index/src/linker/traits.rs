//! Language-owned import and receiver resolution used by the shared linker.

pub(crate) use super::types::{
    import_provider_modules, insert_package_export, package_export, BuiltinPropertyLookup,
};

fn is_stub_or_overload(node: &crate::models::Node) -> bool {
    let flags = crate::models::NodeDetailFlags::from_value(&node.details);
    flags.is_stub || flags.is_overload
}

/// Drop stub and overload candidates only when a concrete implementation is available,
/// and disambiguate mutually exclusive language-owned sibling definitions.
pub(crate) fn filter_stubs_and_overloads(candidates: &mut Vec<&crate::models::Node>) {
    if candidates.len() <= 1 {
        return;
    }

    let has_concrete = candidates.iter().any(|node| !is_stub_or_overload(node));
    let has_stub_or_overload = candidates.iter().any(|node| is_stub_or_overload(node));
    if has_concrete && has_stub_or_overload {
        candidates.retain(|node| !is_stub_or_overload(node));
    }
    if let Some(first) = candidates.first() {
        if candidates.len() > 1
            && candidates
                .iter()
                .all(|node| node.language == first.language)
        {
            if let Some(profile) = crate::languages::by_id(first.language.as_str()) {
                use crate::languages::profile::ProfileLinker;
                profile.linker().disambiguate_candidates(candidates);
            }
        }
    }
}

pub(in crate::linker) fn receiver_language_requires_static(node: &crate::models::Node) -> bool {
    use crate::languages::profile::ProfileLinker;
    crate::languages::by_id(node.language.as_str())
        .is_some_and(|profile| profile.linker().receiver_requires_static())
}
pub use super::types::{ModulesByNamespace, PackageExports};
pub(crate) use crate::languages::profile::{
    resolve_default_import, FileRelationContext, LanguageLinker, GENERIC_LINKER,
};
pub use crate::languages::profile::{ImportContext, ImportNormalizationContext, ImportResolution};

/// A workspace namespace selected by an explicit module path.
pub struct ExplicitModuleTarget {
    pub(in crate::linker) namespace: String,
    pub(in crate::linker) workspace: String,
}

pub(crate) type ExternalMember<'a> = (&'a str, usize, Option<&'a str>);

/// Inputs for resolving the source of an implementation relation.
pub(crate) struct ImplementationSource<'a, 'b> {
    pub(in crate::linker) implementation: &'b crate::models::Node,
    pub(in crate::linker) file_module: &'b str,
    pub(in crate::linker) by_qual: &'b hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>,
    pub(in crate::linker) by_name: &'b hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>,
    pub(in crate::linker) aliases:
        &'b hashbrown::HashMap<&'b str, hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>>,
    pub(in crate::linker) lookup_key: &'b mut String,
}

/// Inputs for looking through explicit module reexports.
pub(crate) struct ExplicitReexport<'a, 'b> {
    pub(in crate::linker) input: &'b super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) file_module: &'b str,
    pub(in crate::linker) target: &'b ExplicitModuleTarget,
    pub(in crate::linker) lookup_key: &'b mut String,
}

pub(crate) struct ExplicitModuleVisibility<'a, 'b> {
    pub(in crate::linker) input: &'b super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) node: &'b crate::models::Node,
    pub(in crate::linker) file_module: &'b str,
}

/// Scope and import positions used to decide whether an alias is visible.
pub(crate) struct ImportVisibility<'a, 'b, 'keys> {
    pub(in crate::linker) input: &'b super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) path: &'b str,
    pub(in crate::linker) aliases: &'b hashbrown::HashMap<
        &'keys str,
        hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>,
    >,
    pub(in crate::linker) alias_import_positions:
        &'b hashbrown::HashMap<(&'keys str, &'keys str), crate::semantic::SourcePosition>,
}

/// Inputs for a language-owned lookup of an implicit receiver.
pub(crate) struct ImplicitReceiver<'a, 'b, 'keys> {
    pub(in crate::linker) phase: ReferencePhase<'a, 'b>,
    pub(in crate::linker) visibility: ImportVisibility<'a, 'b, 'keys>,
    pub(in crate::linker) typed_receiver: Option<&'a crate::models::Node>,
    pub(in crate::linker) known_receiver: bool,
    pub(in crate::linker) shadowed: bool,
    pub(in crate::linker) imported: Option<&'b Vec<&'a crate::models::Node>>,
    pub(in crate::linker) lookup: &'b mut String,
}

pub(crate) trait DynamicAliasResolver<'a> {
    fn alias_for(
        &self,
        name: &str,
        owner: Option<&crate::models::Node>,
        definition_scope: bool,
    ) -> Option<&Vec<&'a crate::models::Node>>;
    fn external_for(
        &self,
        name: &str,
        owner: Option<&crate::models::Node>,
        definition_scope: bool,
    ) -> Option<&(String, usize)>;
}

/// Borrowed inputs for computed receiver lookup.
pub(crate) struct DynamicReceiver<'a, 'b, 'keys> {
    pub(in crate::linker) visibility: ImportVisibility<'a, 'b, 'keys>,
    pub(in crate::linker) file_module: &'b str,
    pub(in crate::linker) reference: &'b crate::models::Reference,
    pub(in crate::linker) constructor_aliases:
        &'b hashbrown::HashMap<(&'keys str, &'keys str), &'keys crate::models::Reference>,
    pub(in crate::linker) candidates: &'b mut Vec<&'a crate::models::Node>,
    pub(in crate::linker) lookup: &'b mut String,
    pub(in crate::linker) resolver: &'b dyn DynamicAliasResolver<'a>,
}

/// Inputs for resolving a member on a locally constructed object.
pub(crate) struct ObjectReference<'a, 'b, 'keys> {
    pub(in crate::linker) phase: ReferencePhase<'a, 'b>,
    pub(in crate::linker) aliases: &'b hashbrown::HashMap<
        &'keys str,
        hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>,
    >,
    pub(in crate::linker) ownership:
        &'b dyn Fn(&crate::models::Node) -> Option<&'a crate::semantic::ObjectOwnershipFact>,
    pub(in crate::linker) calls: &'b hashbrown::HashSet<(&'keys str, usize, usize, &'keys str)>,
    pub(in crate::linker) language_context: &'b crate::languages::LanguageContext,
    pub(in crate::linker) activation_signals:
        &'b crate::languages::manifests::semantic::FileActivationSignals,
}

/// Inputs for resolving a component template member.
type ComponentAliasLookup<'a, 'b> = dyn Fn(&str, Option<&'a crate::models::Node>, bool) -> Option<&'b Vec<&'a crate::models::Node>>
    + 'b;
pub(crate) struct ComponentSlotReference<'a, 'b> {
    pub(in crate::linker) phase: ReferencePhase<'a, 'b>,
    pub(in crate::linker) first: &'b str,
    pub(in crate::linker) tail: &'b str,
    pub(in crate::linker) builtin_origin: Option<&'static str>,
    pub(in crate::linker) alias_for: &'b ComponentAliasLookup<'a, 'b>,
}

/// Shared inputs for preparing workspace reference state.
pub(crate) struct ReferenceWorkspace<'a> {
    pub(crate) all: &'a (dyn crate::FactsLookup + Sync),
    pub(crate) flows: Option<
        &'a hashbrown::HashMap<crate::interner::SymbolId, &'a crate::typed_facts::FlowState>,
    >,
    pub(crate) interner: &'a crate::interner::LinkerInterner<'a>,
    pub(crate) dependencies: &'a crate::languages::dependency_registry::DependencyRegistry,
    pub(crate) imports:
        &'a hashbrown::HashMap<(&'a str, &'a str), Option<crate::languages::ImportPath>>,
    pub(crate) modules: &'a ModulesByNamespace<'a>,
}

/// Borrowed per-file import resolution state passed to language-owned wildcard and re-export hooks.
pub(crate) struct ImportScopeState<'a, 'b, 'file> {
    pub(in crate::linker) input: &'b super::resolution::input::ResolutionInput<'a>,
    pub(in crate::linker) path: &'b str,
    pub(in crate::linker) file_module: &'b str,
    pub(in crate::linker) facts: &'a crate::models::Facts,
    pub(in crate::linker) references: &'b [crate::models::Reference],
    pub(in crate::linker) aliases: &'b mut hashbrown::HashMap<
        &'file str,
        hashbrown::HashMap<&'a str, Vec<&'a crate::models::Node>>,
    >,
    pub(in crate::linker) external_aliases:
        &'b mut hashbrown::HashMap<&'file str, super::resolution::context::ExternalAliases<'a>>,
    pub(in crate::linker) lookup_key: &'b mut String,
}

/// Borrowed workspace inputs passed to language-owned semantic context preparation hooks.
pub(crate) struct SemanticContextPrep<'a> {
    pub(in crate::linker) all: &'a dyn crate::FactsLookup,
    pub(in crate::linker) by_id:
        &'a hashbrown::HashMap<crate::interner::SymbolId, &'a crate::models::Node>,
    pub(in crate::linker) normalized_imports:
        &'a hashbrown::HashMap<(&'a str, &'a str), Option<crate::languages::ImportPath>>,
    pub(in crate::linker) modules: &'a ModulesByNamespace<'a>,
}

/// Workspace state that prepares language-owned per-file reference extensions.
pub(crate) trait ReferenceExtensionFactory<'a>: Send + Sync {
    fn prepare<'file>(
        &'file self,
        input: &'file super::resolution::input::ResolutionInput<'a>,
        path: &'file str,
        references: &'file [crate::models::Reference],
        language_context: &'file crate::languages::LanguageContext,
        graph: &mut crate::models::compact_graph::CompactGraph,
    ) -> Option<Box<dyn ReferenceExtension + 'file>>
    where
        'a: 'file;
}

/// Language-owned per-file reference state.
pub(crate) trait ReferenceExtension {
    fn resolve(&self, phase: &mut ReferencePhase<'_, '_>) -> bool;
    fn candidates<'a>(
        &'a self,
        _path: &str,
        _owner: Option<&crate::models::Node>,
        _reference: &crate::models::Reference,
        _name: &str,
    ) -> Vec<&'a crate::models::Node> {
        Vec::new()
    }
    fn syntax_rules(&self) -> &[&crate::languages::manifests::syntax::SyntaxRule] {
        &[]
    }
    fn builtin_in_scope(
        &self,
        _expression: &str,
        _reference: &crate::models::Reference,
        _profile: Option<&dyn crate::languages::LanguageProfile>,
    ) -> Option<bool> {
        None
    }
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
    pub(in crate::linker) extension: Option<&'b dyn ReferenceExtension>,
}

impl ReferencePhase<'_, '_> {
    pub(crate) fn resolve_extension(&mut self) -> bool {
        self.extension
            .is_some_and(|extension| extension.resolve(self))
    }
    pub(crate) fn resolve_lexical_binding(
        &mut self,
        first: &str,
        component: Option<&crate::models::Node>,
        linker: &dyn LanguageLinker,
        evidence: &str,
    ) -> bool {
        use crate::models::tags::{CoverageStatus, EdgeConfidence, EdgeKind, NodeKind};
        if self.reference.kind != EdgeKind::References.as_str()
            || !self
                .owner
                .is_some_and(|owner| owner.kind == NodeKind::TemplateScope)
        {
            return false;
        }
        let mut scope = self.owner.map_or("", |owner| owner.qualname.as_str());
        let mut binding = None;
        while !scope.is_empty() {
            binding = self.input.by_qual.get(scope).and_then(|nodes| {
                nodes.iter().copied().find(|node| {
                    node.path == *self.path
                        && node.kind == NodeKind::TemplateScope
                        && super::types::has_node_binding(
                            self.input.interner,
                            self.input.node_bindings,
                            &node.id,
                            first,
                        )
                })
            });
            if binding.is_some() {
                break;
            }
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        let binding = binding.or_else(|| linker.template_setup_binding(self.reference, component));
        if let Some(binding) = binding {
            self.graph.reference_edge(
                self.path,
                self.reference,
                binding.id.clone(),
                EdgeKind::References,
                EdgeConfidence::Exact,
            );
            self.graph.reference_coverage(
                self.path,
                self.reference,
                CoverageStatus::Resolved.as_str(),
                format!("{evidence} {}", binding.qualname).into(),
            );
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::filter_stubs_and_overloads;
    use crate::models::Node;

    fn node(id: &str, is_stub: bool, is_overload: bool) -> Node {
        Node {
            native_semantic: None,
            id: id.to_owned(),
            kind: crate::models::tags::NodeKind::Function,
            name: id.to_owned(),
            qualname: id.to_owned(),
            path: "module.ts".to_owned(),
            line: 1,
            end_line: 1,
            is_test: false,
            language: crate::models::tags::LanguageId::Typescript,
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
