use crate::languages::{dependency_registry::DependencyRegistry, ImportPath, LanguageProfile};
use crate::linker::{semantic_context, semantic_facts, traits, types, value_flow};
use crate::models::Node;
use crate::typed_facts::FlowState;
use hashbrown::HashMap;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) struct ResolutionInput<'a> {
    pub(in crate::linker) all: &'a (dyn crate::FactsLookup + Sync),
    pub(in crate::linker) interner: &'a crate::interner::LinkerInterner<'a>,
    pub(in crate::linker) owners: Option<&'a std::collections::BTreeSet<String>>,
    pub(in crate::linker) dependency_registry: &'a DependencyRegistry,
    pub(in crate::linker) flows: Option<&'a HashMap<crate::interner::SymbolId, &'a FlowState>>,
    pub(in crate::linker) sort_per_file: bool,
    pub(in crate::linker) occurrences: Option<&'a BTreeMap<String, Vec<u8>>>,
    pub(in crate::linker) by_name: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(in crate::linker) by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(in crate::linker) by_suffix: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(in crate::linker) by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    pub(in crate::linker) by_module: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(in crate::linker) node_bindings: &'a types::NodeBindingIndex,
    pub(in crate::linker) node_rebindings: &'a types::NodeBindingIndex,
    pub(in crate::linker) children_by_parent: &'a HashMap<crate::interner::SymbolId, Vec<&'a Node>>,
    pub(in crate::linker) profiles: &'a HashMap<&'a str, &'static dyn LanguageProfile>,
    pub(in crate::linker) normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    pub(in crate::linker) modules_by_namespace: &'a traits::ModulesByNamespace<'a>,
    pub(in crate::linker) package_exports: &'a HashMap<&'static str, traits::PackageExports<'a>>,
    pub(in crate::linker) semantic_context: &'a semantic_context::Context<'a>,
    pub(in crate::linker) prepared_semantic_facts: &'a semantic_facts::PreparedSemanticFacts<'a>,
    pub(in crate::linker) module_nodes: &'a [&'a Node],
    pub(in crate::linker) value_flow: &'a value_flow::ValueFlowIndex<'a>,
}
