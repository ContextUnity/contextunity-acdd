//! Canonical symbol admission for the shared value-flow reducer.
//!
//! Import providers are selected once using the same language linker and
//! workspace namespaces as ordinary graph linking. No disk or SQL is consulted
//! while reducing values or resolving calls.

pub(super) use crate::interner::{PathId, SymbolId};
pub(super) use crate::linker::types::{
    import_provider_modules, ModulesByNamespace, PackageExports,
};
pub(super) use crate::linker::value_flow::{SemanticResolver, Symbol, SymbolRole, TypeTarget};
pub(super) use crate::languages::{
    self,
    context::LanguageContext,
    dependency_registry::DependencyRegistry,
    types::ImportPath,
};
pub(super) use crate::{
    models::{Facts, Node, ReceiverHint, Reference},
    semantic::SourcePosition,
};
pub(super) use hashbrown::{HashMap, HashSet};

pub(super) type ComponentScope = (PathId, SymbolId, SymbolId);
pub(super) type ArgumentFactoryKey = (SymbolId, usize, SymbolId);
pub(super) type ArgumentFactoryCandidates =
    Vec<std::sync::Arc<crate::linker::value_flow::ConfiguredReceiverAuthority>>;
pub(super) type ArgumentFactories = HashMap<ArgumentFactoryKey, ArgumentFactoryCandidates>;
pub(super) type HierarchyAuthorities =
    HashMap<(SymbolId, SymbolId), std::sync::Arc<crate::linker::value_flow::ConfiguredReceiverAuthority>>;
pub(super) type ImplicitBuiltinMap = HashMap<SymbolId, String>;

pub(super) struct ReceiverTypes {
    pub(super) rules: std::sync::Arc<LanguageContext>,
    pub(super) indices: Vec<usize>,
}

impl ReceiverTypes {
    pub(super) fn iter(&self) -> impl Iterator<Item = &languages::manifests::syntax::ReceiverTypeRule> {
        self.iter_with_meta().map(|(_, _, receiver)| receiver)
    }

    pub(super) fn iter_with_meta(
        &self,
    ) -> impl Iterator<
        Item = (
            &str,
            &str,
            &languages::manifests::syntax::ReceiverTypeRule,
        ),
    > {
        self.indices
            .iter()
            .filter_map(|&index| {
                let rule = &self.rules.syntax_rules[index];
                match &rule.operation {
                    languages::manifests::syntax::SyntaxOperation::ReceiverType { receiver } => {
                        Some((rule.pack.as_str(), rule.id.as_str(), receiver))
                    }
                    _ => None,
                }
            })
    }
}

pub(super) struct ResultScope<'a> {
    pub(super) config: &'a Node,
    pub(super) module: SymbolId,
    pub(super) exports: HashMap<SymbolId, Vec<(&'a Node, SymbolId)>>,
    pub(super) calls: HashMap<SymbolId, std::sync::Arc<crate::linker::value_flow::ConfiguredCallResult>>,
}

#[derive(Default)]
pub(super) struct CallableCandidates<'a> {
    pub(super) nodes: Vec<&'a Node>,
    pub(super) incomplete: bool,
}

pub(super) struct CallableModule<'a> {
    pub(super) owner: &'a Node,
    pub(super) exports: HashMap<SymbolId, CallableCandidates<'a>>,
}

pub(super) struct CallableScope<'a> {
    pub(super) implicit: bool,
    pub(super) aliases: Vec<SymbolId>,
    pub(super) names: HashMap<SymbolId, CallableCandidates<'a>>,
    pub(super) modules: HashMap<PathId, std::sync::Arc<CallableModule<'a>>>,
    pub(super) incomplete: bool,
}

pub(in crate::linker) enum Imported<'a> {
    Nodes(Vec<&'a Node>, SourcePosition, bool),
    External(String, SourcePosition, &'a Reference, bool),
    Unknown,
}

pub(in crate::linker) enum Member<'a> {
    Local(&'a Node),
    External(&'a str, usize, Option<&'a str>),
    ConfiguredExternal {
        authority: &'a crate::linker::value_flow::ConfiguredReceiverAuthority,
        origin: &'a TypeTarget<'a>,
        evidence: &'a str,
    },
    Builtin,
    Ambiguous,
    Unknown,
}

pub(super) fn name_rebound(owner: &Node, name: &str) -> bool {
    owner.rebinding_names().any(|candidate| candidate == name)
}

pub(super) type FactoryImport<'a> = Option<(&'a Node, SymbolId, SourcePosition)>;

#[derive(Clone, Copy)]
pub(super) enum FactoryBinding<'a> {
    Declaration(&'a crate::semantic::BindingFact),
    Initializer(&'a crate::semantic::BindingFact),
    Invalid,
}

pub(super) enum FactoryDeclarations<'a> {
    Indexed {
        bindings: Option<&'a [SymbolId]>,
        rebindings: Option<&'a [SymbolId]>,
    },
    Raw(&'a Node),
}

pub(crate) struct Context<'a> {
    pub(super) interner: &'a crate::interner::LinkerInterner<'a>,
    pub(super) dependencies: &'a DependencyRegistry,
    pub(super) named: crate::linker::named_bindings::NamedBindings<'a>,
    pub(super) flows: Option<&'a HashMap<SymbolId, &'a crate::typed_facts::FlowState>>,
    pub(super) by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
    pub(super) by_id: &'a HashMap<SymbolId, &'a Node>,
    pub(super) node_bindings: &'a crate::linker::types::NodeBindingIndex,
    pub(super) node_rebindings: &'a crate::linker::types::NodeBindingIndex,
    pub(super) exports: &'a HashMap<&'a str, PackageExports<'a>>,
    pub(in crate::linker) imports: HashMap<SymbolId, HashMap<SymbolId, Imported<'a>>>,
    pub(super) factory_imports: HashMap<(SymbolId, SymbolId), FactoryImport<'a>>,
    pub(super) factory_bindings: HashMap<(SymbolId, SymbolId), FactoryBinding<'a>>,
    pub(super) factory_declarations: HashMap<SymbolId, FactoryDeclarations<'a>>,
    pub(super) factory_callees: HashSet<SymbolId>,
    pub(super) receiver_types: HashMap<PathId, std::sync::Arc<ReceiverTypes>>,
    pub(super) argument_factories: HashMap<PathId, std::sync::Arc<ArgumentFactories>>,
    pub(super) hierarchy_results: HashMap<PathId, std::sync::Arc<HierarchyAuthorities>>,
    pub(super) hierarchy_return_specs: HashMap<
        PathId,
        std::sync::Arc<HashMap<String, languages::manifests::syntax::ReturnTypeSpec>>,
    >,
    pub(super) component_builtins: HashMap<PathId, HashMap<SymbolId, String>>,
    pub(super) component_candidate_ids: HashMap<ComponentScope, usize>,
    pub(super) component_candidates: Vec<HashMap<SymbolId, Vec<&'a Node>>>,
    pub(super) component_consumers: HashMap<PathId, Vec<usize>>,
    pub(super) implicit_builtins: HashMap<PathId, std::sync::Arc<ImplicitBuiltinMap>>,
    pub(super) autoimports: HashMap<PathId, HashMap<SymbolId, String>>,
    pub(super) result_scopes: HashMap<PathId, Vec<std::sync::Arc<ResultScope<'a>>>>,
    pub(super) callable_scopes: HashMap<PathId, Vec<std::sync::Arc<CallableScope<'a>>>>,
    pub(super) commonjs: crate::linker::javascript::commonjs_bindings::ResolvedCommonJsBindings<'a>,
    pub(super) members: HashMap<(SymbolId, SymbolId, bool), Vec<&'a Node>>,
    pub(super) python: Option<&'a crate::linker::python::receivers::PythonReceivers<'a>>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust: Option<&'a crate::linker::rust::profile_linker::RustMembers<'a>>,
    #[cfg(feature = "lang-rust")]
    pub(super) rust_forwarding: &'a crate::linker::rust::forwarding::RustForwarding<'a>,
    #[cfg(feature = "lang-typescript")]
    pub(super) typescript: Option<&'a crate::linker::typescript::members::TypeScriptMembers<'a>>,
}

/// Borrowed read-only access to the language-owned member indexes.
///
/// The shared linker depends on this lookup seam instead of carrying one
/// language-specific index field per profile. Each index remains built and
/// owned by its language resolver; lookups only borrow that precomputed state.
pub(in crate::linker) struct LanguageMemberLookup<'ctx, 'a> {
    pub(super) context: &'ctx Context<'a>,
}

impl<'ctx, 'a> LanguageMemberLookup<'ctx, 'a> {
    pub(in crate::linker) fn new(context: &'ctx Context<'a>) -> Self {
        Self { context }
    }

    pub(in crate::linker) fn python(
        &self,
        class: &Node,
        member: &str,
        associated: bool,
    ) -> crate::linker::python::receivers::PythonReceiverMember<'a> {
        self.context
            .python
            .map_or(crate::linker::python::receivers::PythonReceiverMember::Unknown, |index| {
                index.lookup(class, member, associated)
            })
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn rust_receiver(&self, owner: &Node) -> Option<&'a Node> {
        self.context
            .rust
            .and_then(|index| index.receiver_for(owner))
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn rust_deref_target(&self, receiver: &Node) -> Option<&'a Node> {
        self.context
            .rust
            .and_then(|index| index.deref_target(receiver))
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn rust<'lookup>(
        &'lookup self,
        receiver: &Node,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> &'lookup [&'a Node] {
        self.context.rust.map_or(&[], |index| {
            index.lookup(receiver, member, associated, caller)
        })
    }

    #[cfg(feature = "lang-typescript")]
    pub(in crate::linker) fn typescript<'lookup>(
        &'lookup self,
        class: &Node,
        member: &str,
        associated: bool,
    ) -> &'lookup [&'a Node] {
        self.context
            .typescript
            .map_or(&[], |index| index.lookup(class, member, associated))
    }
}

impl<'a> Context<'a> {
    #[cfg(feature = "lang-rust")]
    pub(super) fn rayon_extension_base(
        &self,
        receiver: &TypeTarget<'_>,
        member: &str,
        caller: &Node,
    ) -> Option<&'static str> {
        let receiver =
            crate::linker::rust::value_flow::reference_target(receiver).unwrap_or(receiver);
        let rayon_container_base = |name: &str| -> Option<&'static str> {
            match name {
                "HashMap" => Some("HashMap"),
                "BTreeMap" => Some("BTreeMap"),
                "HashSet" => Some("HashSet"),
                "BTreeSet" => Some("BTreeSet"),
                _ => None,
            }
        };
        let base = match receiver {
            TypeTarget::RustCollection { .. } => "Vec",
            TypeTarget::RustSlice { .. } => "Slice",
            TypeTarget::RustTypedContainer { base, .. } => rayon_container_base(base)?,
            TypeTarget::RustDeref { wrapper, .. } => rayon_container_base(wrapper)?,
            TypeTarget::Builtin(name) if name == "Vec" => "Vec",
            TypeTarget::Builtin(name) => rayon_container_base(name.as_str())?,
            _ => return None,
        };
        if !self.dependencies.declares_for_path(
            languages::LanguageFamily("rust"),
            &caller.path,
            "rayon",
        ) {
            return None;
        }
        let mut scope = caller.qualname.as_str();
        let mut imported = false;
        loop {
            let module_boundary = self
                .nodes_in_path(scope, &caller.path)
                .any(|owner| matches!(owner.kind.as_str(), "module" | "module_declaration"));
            imported |= self.nodes_in_path(scope, &caller.path).any(|owner| {
                self.symbol_id(&owner.id).and_then(|id| self.imports.get(&id)).is_some_and(|aliases| {
                    aliases.values().any(|import| matches!(import, Imported::External(origin, _, reference, _)
                        if reference.alias.as_deref() == Some("*") && origin.trim_end_matches("::*") == "rayon::prelude"))
                })
            });
            if imported || module_boundary {
                break;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        if !imported {
            return None;
        }
        let lookup_base = if matches!(base, "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet") {
            "Vec"
        } else {
            base
        };
        let (_, rule) = self.dependencies.class_hierarchy_authority_for_path(
            &caller.path,
            lookup_base,
            Some("rust-cargo"),
        )?;
        (rule.provider == "rayon" && rule.members.iter().any(|allowed| allowed == member))
            .then_some(base)
    }

    pub(in crate::linker) fn symbol_name(&self, symbol: SymbolId) -> &str {
        self.interner.resolve_symbol(symbol)
    }

    pub(super) fn path_id(&self, path: &str) -> Option<PathId> {
        self.interner.lookup_path(path)
    }

    pub(in crate::linker) fn symbol_id(&self, symbol: &str) -> Option<SymbolId> {
        self.interner.lookup_symbol(symbol)
    }

    pub(super) fn factory_declares(&self, owner: &Node, name: &str) -> bool {
        let name_id = self.symbol_id(name);
        match self
            .symbol_id(&owner.id)
            .and_then(|owner| self.factory_declarations.get(&owner))
        {
            Some(FactoryDeclarations::Raw(declaration)) => declaration
                .binding_names()
                .chain(declaration.rebinding_names())
                .any(|binding| binding == name),
            Some(FactoryDeclarations::Indexed {
                bindings,
                rebindings,
            }) => name_id.is_some_and(|name_id| {
                bindings.is_some_and(|names| names.binary_search(&name_id).is_ok())
                    || rebindings.is_some_and(|names| names.binary_search(&name_id).is_ok())
            }),
            None => false,
        }
    }
    pub(super) fn configured_unshadowed(&self, owner: &'a Node, name: &str) -> bool {
        let mut scope = owner.qualname.as_str();
        let mut key = String::new();
        for _ in 0..crate::linker::value_flow::SemanticLimits::default().alias_depth {
            if self
                .by_qual
                .get(scope)
                .into_iter()
                .flatten()
                .any(|lexical| {
                    lexical.path == owner.path
                        && (self.factory_declares(lexical, name)
                            || self
                                .symbol_id(&lexical.id)
                                .zip(self.symbol_id(name))
                                .is_some_and(|key| self.factory_bindings.contains_key(&key))
                            || self
                                .symbol_id(&lexical.id)
                                .and_then(|owner| self.imports.get(&owner))
                                .is_some_and(|imports| {
                                    self.symbol_id(name)
                                        .is_some_and(|name| imports.contains_key(&name))
                                }))
                })
                || self
                    .by_qual
                    .get(crate::linker::types::qualified(&mut key, scope, name))
                    .into_iter()
                    .flatten()
                    .any(|node| node.path == owner.path)
            {
                return false;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                return true;
            };
            scope = parent;
        }
        false
    }

    pub(super) fn implicit_factory_result(&self, owner: &'a Node, callee: &str) -> Option<TypeTarget<'a>> {
        let callee_id = self.symbol_id(callee)?;
        let mut providers = self
            .result_scopes
            .get(&self.path_id(owner.path.as_str())?)
            .into_iter()
            .flatten()
            .filter_map(|scope| scope.exports.get(&callee_id))
            .flatten();
        let (provider, binding) = providers.next()?;
        if !self.configured_unshadowed(owner, callee) {
            return None;
        }
        if providers.next().is_some() {
            return Some(TypeTarget::Ambiguous);
        }
        Some(
            self.factory_value(provider, *binding, owner)
                .unwrap_or(TypeTarget::Unknown),
        )
    }

    pub(super) fn factory_value(
        &self,
        provider: &'a Node,
        binding: SymbolId,
        consumer: &Node,
    ) -> Option<TypeTarget<'a>> {
        let definition = self
            .named
            .factories
            .get(&(self.interner.lookup_symbol(&provider.id)?, binding))?;
        let [fact] = definition.facts.as_slice() else {
            return Some(TypeTarget::Ambiguous);
        };
        if let Some(required) = &fact.configured_module {
            let required = self.symbol_id(required)?;
            let provider_path = self.path_id(provider.path.as_str())?;
            let consumer_path = self.path_id(consumer.path.as_str())?;
            let consumer_scopes = self.result_scopes.get(&consumer_path);
            let admitted = self
                .result_scopes
                .get(&provider_path)
                .into_iter()
                .flatten()
                .any(|scope| {
                    scope.module == required
                        && consumer_scopes.into_iter().flatten().any(|other| {
                            other.module == required && other.config.id == scope.config.id
                        })
                });
            if !admitted {
                return Some(TypeTarget::Unknown);
            }
        }
        Some(definition.value.clone())
    }

    pub(crate) fn property_type_for_path(
        &self,
        path: &str,
        receiver: &str,
        member: &str,
    ) -> Option<String> {
        #[cfg(feature = "lang-typescript")]
        let lookup_name =
            crate::languages::typescript::typed_dom_receiver(receiver).unwrap_or(receiver);
        #[cfg(not(feature = "lang-typescript"))]
        let lookup_name = receiver;
        if let Some(receiver) = self.receiver_type(path, lookup_name) {
            return receiver.properties.get(member).cloned();
        }
        #[cfg(feature = "lang-typescript")]
        {
            crate::languages::typescript::typed_dom_property(receiver, member).map(str::to_owned)
        }
        #[cfg(not(feature = "lang-typescript"))]
        None
    }
}

impl crate::linker::types::BuiltinPropertyLookup for Context<'_> {
    fn property_type_for_path(&self, path: &str, receiver: &str, member: &str) -> Option<String> {
        Context::property_type_for_path(self, path, receiver, member)
    }
}

impl<'a> Context<'a> {
    pub(in crate::linker) fn receiver_types_for_path(
        &self,
        path: &str,
    ) -> Option<impl Iterator<Item = &languages::manifests::syntax::ReceiverTypeRule>> {
        let path = self.path_id(path)?;
        self.receiver_types.get(&path).map(|rules| rules.iter())
    }

    pub(super) fn receiver_types_with_meta_for_path(
        &self,
        path: &str,
    ) -> Option<
        impl Iterator<
            Item = (
                &str,
                &str,
                &languages::manifests::syntax::ReceiverTypeRule,
            ),
        >,
    > {
        let path = self.path_id(path)?;
        self.receiver_types
            .get(&path)
            .map(|rules| rules.iter_with_meta())
    }

    pub(super) fn receiver_rule_with_meta(
        &self,
        path: &str,
        name: &str,
    ) -> Option<(&str, &str, &languages::manifests::syntax::ReceiverTypeRule)> {
        let mut candidates = self
            .receiver_types_with_meta_for_path(path)?
            .filter(|(_, _, receiver)| receiver.name == name);
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    }

    pub(in crate::linker) fn polymorphic_implementors(
        &self,
        abstract_type: &'a Node,
        member: &str,
        contract_decl: &'a Node,
    ) -> Option<Vec<&'a Node>> {
        #[cfg(feature = "lang-typescript")]
        if matches!(
            abstract_type.language.as_str(),
            "typescript" | "javascript" | "vue"
        ) && (abstract_type.kind == "interface"
            || abstract_type.details["is_abstract"] == true)
        {
            let index = self.typescript?;
            let impls = index.implementors(abstract_type.id.as_str());
            if impls.is_empty() {
                return None;
            }
            let methods: Vec<_> = impls
                .iter()
                .filter_map(|impl_class| {
                    match index.lookup(impl_class, member, false) {
                        [node] if node.id != contract_decl.id => Some(*node),
                        _ => None,
                    }
                })
                .collect();
            return (!methods.is_empty()).then_some(methods);
        }
        if abstract_type.language == "python"
            && crate::linker::python::resolution::has_protocol_or_abc_base(abstract_type)
        {
            let index = self.python?;
            let impls = index.implementors(abstract_type.id.as_str());
            if impls.is_empty() {
                return None;
            }
            let methods: Vec<_> = impls
                .iter()
                .filter_map(|impl_class| match index.lookup(impl_class, member, false) {
                    crate::linker::python::receivers::PythonReceiverMember::Local(node) if node.id != contract_decl.id => {
                        Some(node)
                    }
                    _ => None,
                })
                .collect();
            return (!methods.is_empty()).then_some(methods);
        }
        None
    }
    pub(in crate::linker) fn hierarchy_authority_for_base(
        &self,
        path: &str,
        base: &str,
    ) -> Option<(
        String,
        std::sync::Arc<crate::linker::value_flow::ConfiguredReceiverAuthority>,
    )> {
        let (pack, rule) = self
            .dependencies
            .class_hierarchy_authority_for_path(path, base, None)?;
        if let Some(authority) = self
            .path_id(path)
            .zip(self.symbol_id(pack))
            .zip(self.symbol_id(base))
            .and_then(|((path_id, pack_id), base_id)| {
                self.hierarchy_results
                    .get(&path_id)
                    .and_then(|map| map.get(&(pack_id, base_id)))
            })
        {
            return Some((pack.to_owned(), authority.clone()));
        }
        Some((
            pack.to_owned(),
            std::sync::Arc::new(
                crate::linker::value_flow::ConfiguredReceiverAuthority::Hierarchy {
                    pack: pack.to_owned(),
                    rule: std::sync::Arc::new(rule.clone()),
                },
            ),
        ))
    }
    pub(in crate::linker) fn dependencies_digest(&self) -> String {
        self.dependencies.digest().to_string()
    }
    pub(super) fn python_super_member(&self, owner: &'a Node, member: &str) -> Symbol<'a> {
        if owner.language != "python"
            || !owner
                .details
                .get("receiver_name")
                .and_then(|value| value.as_str())
                .is_some_and(|name| matches!(name, "self" | "cls"))
        {
            return Symbol::Unknown;
        }
        let Some((class_qual, _)) = owner.qualname.rsplit_once('.') else {
            return Symbol::Unknown;
        };
        let Some(class) = self
            .nodes_in_path(class_qual, &owner.path)
            .find(|node| node.kind == "class")
        else {
            return Symbol::Unknown;
        };
        let mut scope = owner.qualname.as_str();
        while !scope.is_empty() {
            if self.nodes_in_path(scope, &owner.path).any(|node| {
                node.binding_names().any(|name| name == "super")
                    || self.node_has_rebinding(node, "super")
                    || self.imported_alias(node, "super").is_some()
            }) {
                return Symbol::Unknown;
            }
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        match self.python.map(|index| index.lookup(class, member, true)) {
            Some(crate::linker::python::receivers::PythonReceiverMember::Local(node)) => Symbol::Callable(node),
            _ => Symbol::Unknown,
        }
    }
    pub(super) fn receiver_type(
        &self,
        path: &str,
        name: &str,
    ) -> Option<&languages::manifests::syntax::ReceiverTypeRule> {
        let path = self.path_id(path)?;
        let mut candidates = self
            .receiver_types
            .get(&path)?
            .iter()
            .filter(|receiver| receiver.name == name);
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    }

    pub(in crate::linker) fn exact_external_import(
        &self,
        owner: &Node,
        alias: &str,
        module: &str,
        symbol: &str,
        at: SourcePosition,
    ) -> bool {
        self.symbol_id(&owner.id)
            .zip(self.symbol_id(alias))
            .is_some_and(|(owner_id, alias_id)| {
                matches!(
                    self.imports
                        .get(&owner_id)
                        .and_then(|imports| imports.get(&alias_id)),
                    Some(Imported::External(origin, position, reference, _))
                        if origin == module
                            && *position < at
                            && reference.source == owner.id
                            && reference.module.as_deref() == Some(module)
                            && reference.expression == symbol
                            && reference.alias.as_deref() == Some(alias)
                )
            })
    }

    pub(in crate::linker) fn component_builtin(&self, path: &str, tag: &str) -> Option<&str> {
        let tag = self.symbol_id(tag)?;
        self.component_builtins
            .get(&self.path_id(path)?)?
            .get(&tag)
            .map(String::as_str)
    }

    pub(in crate::linker) fn component_candidates(&self, path: &str, tag: &str) -> Vec<&'a Node> {
        let (Some(path), Some(tag)) = (self.path_id(path), self.symbol_id(tag)) else {
            return Vec::new();
        };
        let mut candidates: Vec<_> = self
            .component_consumers
            .get(&path)
            .into_iter()
            .flatten()
            .filter_map(|&index| self.component_candidates.get(index)?.get(&tag))
            .flatten()
            .copied()
            .collect();
        candidates.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        candidates.dedup_by(|left, right| left.id == right.id);
        candidates
    }

    pub(in crate::linker) fn autoimport(&self, path: &str, name: &str) -> bool {
        let Some(path) = self.path_id(path) else {
            return false;
        };
        let Some(name) = self.symbol_id(name) else {
            return false;
        };
        self.autoimports
            .get(&path)
            .is_some_and(|names| names.contains_key(&name))
            || self
                .implicit_builtins
                .get(&path)
                .is_some_and(|names| names.contains_key(&name))
    }

    /// Only configured, complete exported callables supplement ordinary lexical calls.
    pub(in crate::linker) fn implicit_callables(&self, owner: &'a Node, name: &str) -> Vec<&'a Node> {
        let mut candidates = Vec::new();
        let Some(name_id) = self.symbol_id(name) else {
            return candidates;
        };
        let Some(path) = self.path_id(&owner.path) else {
            return candidates;
        };
        for scope in self
            .callable_scopes
            .get(&path)
            .into_iter()
            .flatten()
            .filter(|scope| scope.implicit)
        {
            if scope.incomplete {
                return Vec::new();
            }
            if let Some(found) = scope.names.get(&name_id) {
                if found.incomplete {
                    return Vec::new();
                }
                for node in &found.nodes {
                    if !candidates
                        .iter()
                        .any(|previous: &&Node| previous.id == node.id)
                    {
                        if candidates.len() == 8 {
                            return Vec::new();
                        }
                        candidates.push(*node);
                    }
                }
            }
        }
        if !candidates.is_empty() && !self.configured_unshadowed(owner, name) {
            candidates.clear();
        }
        candidates
    }

    /// Confined aliases normalize into the existing canonical import path.
    pub(in crate::linker) fn callable_import_path(
        &self,
        path: &str,
        reference: &Reference,
    ) -> Option<ImportPath> {
        let (module, _) = self.callable_import_module(path, reference)?;
        Some(ImportPath {
            namespace: module.qualname.clone(),
            relative: true,
            symbol_path: false,
        })
    }

    pub(super) fn callable_import_module(
        &self,
        path: &str,
        reference: &Reference,
    ) -> Option<(&'a Node, &CallableCandidates<'a>)> {
        let source = reference.module.as_deref()?;
        let path = self.path_id(path)?;
        let mut selected: Option<(&Node, &CallableCandidates<'a>)> = None;
        for scope in self.callable_scopes.get(&path).into_iter().flatten() {
            if scope.incomplete {
                return None;
            }
            for alias in &scope.aliases {
                let alias = self.interner.resolve_symbol(*alias);
                let Some(tail) = source.strip_prefix(alias) else {
                    continue;
                };
                if tail
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
                    || tail.contains('\\')
                {
                    return None;
                }
                let module = scope.modules.get(&self.path_id(tail)?)?;
                let target = module
                    .exports
                    .get(&self.symbol_id(reference.expression.as_str())?)?;
                if target.incomplete || target.nodes.is_empty() {
                    return None;
                }
                if selected.is_some_and(|(previous, _)| previous.id != module.owner.id) {
                    return None;
                }
                selected = Some((module.owner, target));
            }
        }
        let selected = selected?;
        let alias = reference.alias.as_deref()?;
        let owner = reference.source.as_str();
        let owner_id = self.symbol_id(owner)?;
        let alias_id = self.symbol_id(alias)?;
        let rebound = match self.factory_declarations.get(&owner_id) {
            Some(FactoryDeclarations::Indexed { rebindings, .. }) => {
                rebindings.is_some_and(|names| names.binary_search(&alias_id).is_ok())
            }
            Some(FactoryDeclarations::Raw(node)) => {
                node.rebinding_names().any(|name| name == alias)
            }
            None => false,
        };
        if rebound || self.factory_bindings.contains_key(&(owner_id, alias_id)) {
            return None;
        }
        Some(selected)
    }

    pub(in crate::linker) fn autoimport_evidence(&self, path: &str, name: &str) -> Option<&str> {
        let path = self.path_id(path)?;
        let name = self.symbol_id(name)?;
        self.autoimports
            .get(&path)
            .and_then(|names| names.get(&name))
            .or_else(|| {
                self.implicit_builtins
                    .get(&path)
                    .and_then(|names| names.get(&name))
            })
            .map(String::as_str)
    }

    pub(in crate::linker) fn unshadowed_global(
        &self,
        owner: &'a Node,
        name: &str,
        at: SourcePosition,
    ) -> bool {
        let mut scope = owner.qualname.as_str();
        let mut lookup_key = String::with_capacity(128);
        loop {
            if self.by_qual.get(scope).into_iter().flatten().any(|node| {
                node.path == owner.path
                    && (node.binding_names().any(|candidate| candidate == name)
                        || node.rebinding_names().any(|candidate| candidate == name)
                        || self.symbol_id(&node.id).is_some_and(|node_id| {
                            self.imports.get(&node_id).is_some_and(|imports| {
                                self.symbol_id(name)
                                    .is_some_and(|name| imports.contains_key(&name))
                            })
                        })
                        || self.value_flow(node).is_some_and(|flow| {
                            flow.bindings
                                .iter()
                                .any(|binding| binding.name == name && binding.position <= at)
                        }))
            }) || self
                .by_qual
                .get(crate::linker::types::qualified(&mut lookup_key, scope, name))
                .into_iter()
                .flatten()
                .any(|node| node.path == owner.path)
            {
                return false;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        true
    }

    pub(in crate::linker) fn tracks_commonjs_binding(&self, scope: &Node, alias: &str) -> bool {
        self.commonjs.tracks(scope, alias)
    }

    pub(in crate::linker) fn node_has_rebinding(&self, owner: &Node, alias: &str) -> bool {
        self.symbol_id(&owner.id)
            .zip(self.symbol_id(alias))
            .is_some_and(|(owner, alias)| {
                self.node_rebindings
                    .get(&owner)
                    .is_some_and(|bindings| bindings.binary_search(&alias).is_ok())
            })
    }

    pub(in crate::linker) fn admits_commonjs_binding(
        &self,
        scope: &Node,
        alias: &str,
        owner: &Node,
        at: SourcePosition,
    ) -> bool {
        self.commonjs.admits(scope, alias, owner, at)
    }
    pub(super) fn visible(owner: &Node, candidate: &Node, at: SourcePosition, role: SymbolRole) -> bool {
        if let Some(visible) = crate::linker::rust::value_flow::visible(owner, candidate) {
            return visible;
        }
        if matches!(role, SymbolRole::Type) {
            return true;
        }
        if candidate
            .qualname
            .rsplit_once('.')
            .is_none_or(|(parent, _)| parent != owner.qualname)
        {
            return true;
        }
        if crate::linker::typescript::semantic_context::visible(owner, candidate) {
            return true;
        }
        SourcePosition {
            line: candidate.line,
            column: candidate.details["column"].as_u64().unwrap_or(0) as usize,
        } < at
    }

    pub(in crate::linker) fn named_target(
        &self,
        owner: &'a Node,
        domain: &str,
        key: &str,
        origin: Option<&crate::semantic::ExtractionRuleOrigin>,
    ) -> Symbol<'a> {
        use crate::semantic::{NamedBindingRole, NamedTargetKind, SymbolOperand};
        let scope = crate::linker::named_bindings::project(&owner.path, self.dependencies);
        let Some(scope_id) = self.interner.lookup_path(scope) else {
            return Symbol::Unknown;
        };
        let origin_scopes = origin.and_then(|origin| {
            self.symbol_id(&origin.pack)
                .zip(self.symbol_id(&origin.rule))
                .and_then(|(pack, rule)| self.named.scopes(scope_id, pack, rule))
        });
        let mut scoped_ids = Vec::new();
        if let Some(scopes) = origin_scopes {
            scoped_ids.extend_from_slice(scopes);
        } else {
            scoped_ids.push(scope_id);
        }
        let (Some(domain), Some(key)) = (self.symbol_id(domain), self.symbol_id(key)) else {
            return Symbol::Unknown;
        };
        let mut selected = None;
        let mut count = 0usize;
        let mut uncertain = false;
        for scope in scoped_ids {
            let (matching, unknown) = self.named.definitions(scope, domain, key);
            count += matching.len();
            selected = selected.or_else(|| matching.first());
            uncertain |= unknown;
        }
        if uncertain || count == 0 {
            return Symbol::Unknown;
        }
        if count != 1 {
            return Symbol::Ambiguous;
        }
        let Some(definition) = selected else {
            return Symbol::Unknown;
        };
        let NamedBindingRole::Definition {
            target,
            target_kind,
        } = &definition.fact.role
        else {
            return Symbol::Unknown;
        };
        match target {
            SymbolOperand::Node { id } => {
                let Some(target) = self.interner.lookup_node(self.by_id, id) else {
                    return Symbol::Unknown;
                };
                if target.path != definition.owner.path {
                    return Symbol::Unknown;
                }
                match target_kind {
                    NamedTargetKind::Object if target.kind == "object" => {
                        Symbol::Type(TypeTarget::Local(target))
                    }
                    NamedTargetKind::ListenerScope if target.kind == "template_scope" => {
                        Symbol::Type(TypeTarget::Local(target))
                    }
                    NamedTargetKind::Callable
                        if matches!(target.kind.as_str(), "function" | "method") =>
                    {
                        Symbol::Callable(target)
                    }
                    _ => Symbol::Unknown,
                }
            }
            SymbolOperand::Name { expression } if *target_kind == NamedTargetKind::Callable => {
                match self.resolve_symbol(
                    definition.owner,
                    expression,
                    definition.fact.position,
                    SymbolRole::Callable,
                ) {
                    symbol @ (Symbol::Callable(_) | Symbol::Ambiguous) => symbol,
                    _ => Symbol::Unknown,
                }
            }
            _ => Symbol::Unknown,
        }
    }

    /// None preserves ordinary template fallback; observed uncertainty suppresses it.
    pub(in crate::linker) fn template_named_target(
        &self,
        owner: &'a Node,
        domain: &str,
        key: &str,
        kind: &str,
    ) -> Option<Symbol<'a>> {
        use crate::semantic::StaticString;
        if kind != "filter" {
            return None;
        }
        let scope = crate::linker::named_bindings::project(&owner.path, self.dependencies);
        let Some(scope_id) = self.interner.lookup_path(scope) else {
            return Some(Symbol::Unknown);
        };
        let (Some(domain), Some(key)) = (self.symbol_id(domain), self.symbol_id(key)) else {
            return Some(Symbol::Unknown);
        };
        let owner_path = self.path_id(&owner.path)?;
        if self
            .named
            .registry_uncertain(scope_id, domain, key, owner_path, self.interner)
        {
            return Some(Symbol::Unknown);
        }
        let mut result = None;
        for registry in self.named.registry_definitions(scope_id, domain) {
            if !registry.root.contains_path(owner_path, self.interner) {
                continue;
            }
            let definition = &registry.definition;
            match &definition.fact.key {
                StaticString::Literal { value } if self.symbol_id(value.as_str()) != Some(key) => {
                    continue;
                }
                StaticString::Unknown => return Some(Symbol::Unknown),
                _ => {}
            }
            let symbol = match registry.target {
                Ok(Some(node)) => Symbol::Callable(node),
                Ok(None) => Symbol::Unknown,
                Err(()) => Symbol::Ambiguous,
            };
            result = Some(match (result, symbol) {
                (_, Symbol::Unknown | Symbol::Type(_)) => return Some(Symbol::Unknown),
                (Some(Symbol::Unknown), _) => return Some(Symbol::Unknown),
                (Some(Symbol::Ambiguous), _) | (_, Symbol::Ambiguous) => Symbol::Ambiguous,
                (Some(Symbol::Callable(previous)), Symbol::Callable(next))
                    if previous.id != next.id =>
                {
                    Symbol::Ambiguous
                }
                (_, symbol) => symbol,
            });
        }
        result
    }

    pub(super) fn prepare_registry_bindings(&mut self) {
        use crate::semantic::{NamedBindingRole, SymbolOperand, TypeExpr};
        let prepared: Vec<_> = self
            .named
            .registries
            .iter()
            .map(|(key, registries)| {
                let targets: Vec<_> = registries
                    .iter()
                    .map(|registry| {
                        let definition = &registry.definition;
                        let NamedBindingRole::RegistryDefinition {
                            receiver:
                                SymbolOperand::Name {
                                    expression: receiver,
                                },
                            target,
                        } = &definition.fact.role
                        else {
                            return Ok(None);
                        };
                        let Some(annotation) = definition.owner.parameter_type(receiver) else {
                            return Ok(None);
                        };
                        let receiver = crate::linker::value_flow::resolve_type(
                            definition.owner,
                            &TypeExpr::Named {
                                name: annotation.to_owned(),
                            },
                            definition.fact.position,
                            self,
                            crate::linker::value_flow::SemanticLimits::default(),
                        );
                        let TypeTarget::External {
                            module,
                            import_line,
                        } = receiver
                        else {
                            return Ok(None);
                        };
                        let expected = format!(
                            "{}.{}",
                            registry.receiver_type.module, registry.receiver_type.export
                        );
                        if (module != registry.receiver_type.module && module != expected)
                            || !self.registry_receiver_import(
                                definition.owner,
                                annotation,
                                &registry.receiver_type,
                                import_line,
                                definition.fact.position,
                            )
                        {
                            return Ok(None);
                        }
                        let symbol = match target {
                            SymbolOperand::Name { expression } => self.resolve_symbol(
                                definition.owner,
                                expression,
                                definition.fact.position,
                                SymbolRole::Callable,
                            ),
                            SymbolOperand::Node { id } => self
                                .interner
                                .lookup_node(self.by_id, id)
                                .filter(|node| {
                                    node.path == definition.owner.path
                                        && matches!(node.kind.as_str(), "function" | "method")
                                })
                                .map_or(Symbol::Unknown, Symbol::Callable),
                            SymbolOperand::Unknown => Symbol::Unknown,
                        };
                        match symbol {
                            Symbol::Callable(node) => Ok(Some(node)),
                            Symbol::Ambiguous => Err(()),
                            _ => Ok(None),
                        }
                    })
                    .collect();
                (*key, targets)
            })
            .collect();
        for (key, targets) in prepared {
            if let Some(registries) = self.named.registries.get_mut(&key) {
                for (registry, target) in registries.iter_mut().zip(targets) {
                    registry.target = target;
                }
            }
        }
    }

    fn registry_receiver_import(
        &self,
        owner: &'a Node,
        annotation: &str,
        expected: &languages::manifests::projections::ImportedReceiverType,
        import_line: usize,
        at: SourcePosition,
    ) -> bool {
        let (alias, member) = annotation.split_once('.').unwrap_or((annotation, ""));
        let mut scope = owner.qualname.as_str();
        loop {
            for node in self.nodes_in_path(scope, &owner.path) {
                let import = self
                    .symbol_id(&node.id)
                    .and_then(|node| self.imports.get(&node))
                    .and_then(|imports| {
                        self.symbol_id(alias).and_then(|alias| imports.get(&alias))
                    });
                if let Some(import) = import {
                    return matches!(import, Imported::External(module, position, reference, _)
                        if module == &expected.module
                            && position.line == import_line && *position < at
                            && reference.source == node.id
                            && reference.module.as_deref() == Some(expected.module.as_str())
                            && reference.alias.as_deref() == Some(alias)
                            && ((member.is_empty() && reference.expression == expected.export)
                                || (member == expected.export && reference.expression == expected.module)));
                }
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                return false;
            };
            scope = parent;
        }
    }

    pub(in crate::linker) fn named_member(
        &self,
        owner: &'a Node,
        name: &str,
        at: SourcePosition,
    ) -> Option<Symbol<'a>> {
        use crate::semantic::{NamedBindingRole, StaticString, ValueExpr};
        use languages::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        let (root, _) = name.split_once('.')?;
        let rules = self.dependencies.language_context_for_path(&owner.path);
        for &index in rules.named_member_rules(root) {
            let rule = &rules.syntax_rules[index];
            let SyntaxOperation::NamedBinding {
                domain,
                binding: NamedBindingForm::MemberReference { root },
            } = &rule.operation
            else {
                continue;
            };
            let Some(tail) = name
                .strip_prefix(root.as_str())
                .and_then(|tail| tail.strip_prefix('.'))
            else {
                continue;
            };
            let (key, member) = tail.split_once('.').unwrap_or((tail, ""));
            let proven = self.value_flow(owner).is_some_and(|flow| {
                flow.named_bindings().any(|fact| {
                    fact.origin.pack == rule.pack
                        && fact.origin.rule == rule.id
                        && fact.position.line == at.line
                        && matches!(fact.role, NamedBindingRole::Reference)
                        && matches!(&fact.key, StaticString::Literal { value } if value == key)
                })
            });
            if !proven {
                return Some(Symbol::Unknown);
            }
            let target = self.named_target(owner, domain.as_str(), key, None);
            if member.is_empty() {
                return Some(target);
            }
            let Symbol::Type(TypeTarget::Local(object)) = target else {
                return Some(target);
            };
            let Some(ValueExpr::Object { members, .. }) = self
                .value_flow(object)
                .and_then(|flow| flow.return_value.as_ref())
            else {
                return Some(Symbol::Unknown);
            };
            let matching: Vec<_> = members.iter().filter(|item| item.name == member).collect();
            let [item] = matching.as_slice() else {
                return Some(Symbol::Unknown);
            };
            let nodes = self
                .interner
                .lookup_path(&object.path)
                .and_then(|path| self.named.positions.get(&(path, item.position)))
                .map_or(&[][..], Vec::as_slice);
            let matching: Vec<_> = nodes
                .iter()
                .copied()
                .filter(|node| node.name == member)
                .collect();
            return Some(match matching.as_slice() {
                [node] => Symbol::Callable(node),
                [] => Symbol::Unknown,
                _ => Symbol::Ambiguous,
            });
        }
        None
    }

    pub(in crate::linker) fn with_python(
        mut self,
        members: &'a crate::linker::python::receivers::PythonReceivers<'a>,
    ) -> Self {
        self.python = Some(members);
        self
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn with_rust(
        mut self,
        members: &'a crate::linker::rust::profile_linker::RustMembers<'a>,
    ) -> Self {
        self.rust = Some(members);
        self
    }

    #[cfg(feature = "lang-typescript")]
    pub(in crate::linker) fn with_typescript(
        mut self,
        members: &'a crate::linker::typescript::members::TypeScriptMembers<'a>,
    ) -> Self {
        self.typescript = Some(members);
        self
    }

    pub(super) fn classify(&self, nodes: impl Iterator<Item = &'a Node>, role: SymbolRole) -> Symbol<'a> {
        let mut candidates: Vec<&'a Node> = nodes
            .filter(|node| {
                node.language != "javascript"
                    || node.details["prototype"] != true
                    || node.details["prototype_valid"] == true
            })
            .filter(|node| match role {
                SymbolRole::Type => matches!(
                    node.kind.as_str(),
                    "class" | "struct" | "enum" | "type" | "type_alias" | "interface" | "trait"
                ),
                SymbolRole::Constructor => {
                    matches!(node.kind.as_str(), "class" | "struct" | "enum")
                        || (node.language == "javascript"
                            && node.kind == "function"
                            && node.details["prototype_constructor"] == true)
                }
                SymbolRole::Callable => {
                    matches!(
                        node.kind.as_str(),
                        "function" | "method" | "class" | "struct" | "enum"
                    ) || (node.language == "rust" && node.kind == "constant")
                }
            })
            .collect();
        crate::linker::types::filter_stubs_and_overloads(&mut candidates);
        if candidates.is_empty() {
            return Symbol::Unknown;
        }
        if candidates.len() > 1 {
            return Symbol::Ambiguous;
        }
        let node = candidates[0];
        if (matches!(node.kind.as_str(), "function" | "method")
            || (node.language == "rust" && node.kind == "constant"))
            && !matches!(role, SymbolRole::Constructor)
        {
            Symbol::Callable(node)
        } else {
            Symbol::Type(TypeTarget::Local(node))
        }
    }

    pub(in crate::linker) fn nodes_in_path<'s>(
        &'s self,
        qualified: &str,
        path: &'s str,
    ) -> impl Iterator<Item = &'a Node> + 's {
        self.by_qual
            .get(qualified)
            .into_iter()
            .flatten()
            .copied()
            .filter(move |node| node.path == path)
    }

    pub(in crate::linker) fn imported_alias(
        &self,
        node: &Node,
        alias: &str,
    ) -> Option<&Imported<'a>> {
        self.symbol_id(&node.id)
            .and_then(|owner| self.imports.get(&owner))
            .and_then(|aliases| self.symbol_id(alias).and_then(|alias| aliases.get(&alias)))
    }

    pub(super) fn member(
        &self,
        receiver: &'a Node,
        member: &str,
        associated: bool,
        caller: &'a Node,
    ) -> Symbol<'a> {
        #[cfg(feature = "lang-typescript")]
        if matches!(
            receiver.language.as_str(),
            "typescript" | "javascript" | "vue"
        ) {
            if let Some(index) = self.typescript {
                return self.classify(
                    index.lookup(receiver, member, associated).iter().copied(),
                    SymbolRole::Callable,
                );
            }
        }
        if receiver.language == "python" && receiver.kind == "class" {
            return match self
                .python
                .map(|index| index.lookup(receiver, member, false))
            {
                Some(crate::linker::python::receivers::PythonReceiverMember::Local(node)) => {
                    self.classify(std::iter::once(node), SymbolRole::Callable)
                }
                Some(crate::linker::python::receivers::PythonReceiverMember::Ambiguous) => Symbol::Ambiguous,
                _ => Symbol::Unknown,
            };
        }
        #[cfg(feature = "lang-rust")]
        if receiver.language == "rust" {
            return self.rust.map_or(Symbol::Unknown, |index| {
                self.classify(
                    index
                        .lookup(receiver, member, associated, caller)
                        .iter()
                        .copied(),
                    SymbolRole::Callable,
                )
            });
        }
        let _ = (associated, caller);
        let mut lookup_key = String::with_capacity(128);
        self.classify(
            self.by_qual
                .get(crate::linker::types::qualified(
                    &mut lookup_key,
                    &receiver.qualname,
                    member,
                ))
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| node.path == receiver.path),
            SymbolRole::Callable,
        )
    }

    pub(in crate::linker) fn lookup_member<'s>(
        &'s self,
        receiver: &'s TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
    ) -> Member<'s> {
        self.lookup_member_mode(receiver, member, caller, false)
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn dynamic_dispatch<'s>(
        &'s self,
        receiver: &'s Node,
        member: &'s str,
    ) -> Option<(&'a Node, &'s [&'a Node])> {
        self.rust?.dynamic_dispatch(receiver, member)
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn is_rust_inherent_member(&self, method: &Node) -> bool {
        self.rust
            .is_some_and(|rust| rust.is_inherent_member(method))
    }

    #[cfg(not(feature = "lang-rust"))]
    pub(in crate::linker) fn is_rust_inherent_member(&self, _method: &Node) -> bool {
        false
    }

    #[cfg(feature = "lang-typescript")]
    pub(in crate::linker) fn lookup_component_template_field<'s>(
        &'s self,
        receiver: &'s TypeTarget<'a>,
        member: &str,
    ) -> Member<'s> {
        let TypeTarget::Local(receiver) = receiver else {
            return Member::Unknown;
        };
        if !matches!(
            receiver.language.as_str(),
            "typescript" | "javascript" | "vue"
        ) || !matches!(receiver.kind.as_str(), "class" | "interface")
        {
            return Member::Unknown;
        }
        match self
            .typescript
            .map(|index| index.lookup_field(receiver, member))
        {
            Some([field]) => Member::Local(field),
            Some(fields) if fields.len() > 1 => Member::Ambiguous,
            _ => Member::Unknown,
        }
    }

    #[cfg(not(feature = "lang-typescript"))]
    pub(in crate::linker) fn lookup_component_template_field<'s>(
        &'s self,
        _receiver: &'s TypeTarget<'a>,
        _member: &str,
    ) -> Member<'s> {
        Member::Unknown
    }
}
