use super::context::*;
use crate::linker::traits::ImportContext;

impl<'a> Context<'a> {
    pub(super) fn prepare_callable_aliases(&mut self, all: &'a dyn crate::FactsLookup) {
        let mut updates = Vec::new();
        for (path, file) in all.iter_facts() {
            let Some(path_id) = self.interner.lookup_path(path) else {
                continue;
            };
            if !self.callable_scopes.contains_key(&path_id) {
                continue;
            }
            if self.interner.file_import_mask_for_path(path_id) == 0
                && !file
                    .references
                    .iter()
                    .any(|reference| reference.kind == "imports")
            {
                continue;
            }
            let mut counts = HashMap::new();
            for reference in file
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
            {
                if let Some(alias) = reference.alias.as_deref() {
                    *counts
                        .entry((reference.source.as_str(), alias))
                        .or_insert(0usize) += 1;
                }
            }
            for reference in file
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
            {
                let Some(alias) = reference.alias.as_deref().filter(|alias| *alias != "*") else {
                    continue;
                };
                if counts.get(&(reference.source.as_str(), alias)) != Some(&1) {
                    continue;
                }
                let Some((_, target)) = self.callable_import_module(path, reference) else {
                    continue;
                };
                updates.push((
                    reference.source.as_str(),
                    alias,
                    Imported::Nodes(
                        target.nodes.clone(),
                        reference.position(),
                        matches!(reference.receiver_hint, Some(ReceiverHint::TypeOnlyImport)),
                    ),
                ));
            }
        }
        for (owner, alias, target) in updates {
            let owner = self
                .interner
                .lookup_symbol(owner)
                .expect("callable import owner is interned");
            let alias = self
                .interner
                .lookup_symbol(alias)
                .expect("callable import alias is interned");
            self.imports.entry(owner).or_default().insert(alias, target);
        }
    }

    fn build_rule_caches_for_paths(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
    ) {
        let mut hierarchy_cache: HashMap<usize, Option<std::sync::Arc<HierarchyAuthorities>>> =
            HashMap::new();
        let mut hierarchy_specs_cache: HashMap<
            usize,
            Option<std::sync::Arc<HashMap<String, languages::manifests::syntax::ReturnTypeSpec>>>,
        > = HashMap::new();
        let mut receiver_types_cache: HashMap<
            (usize, &'static str),
            Option<std::sync::Arc<ReceiverTypes>>,
        > = HashMap::new();
        let mut argument_factories_cache: HashMap<
            (usize, &'static str),
            Option<std::sync::Arc<ArgumentFactories>>,
        > = HashMap::new();
        let mut pinned_rules: Vec<std::sync::Arc<languages::LanguageContext>> = Vec::new();
        for (path, facts) in all.iter_facts() {
            let Some(module) = facts.nodes.iter().find(|node| node.kind == "module") else {
                continue;
            };
            let rules = dependencies.language_context_for_path(path);
            let rules_ptr = std::sync::Arc::as_ptr(&rules) as usize;
            if matches!(module.language.as_str(), "python" | "rust") {
                self.cache_hierarchy_rules(
                    path,
                    &rules,
                    rules_ptr,
                    dependencies,
                    &mut pinned_rules,
                    &mut hierarchy_cache,
                    &mut hierarchy_specs_cache,
                );
            }
            self.cache_receiver_rules(
                path,
                &module.language,
                &rules,
                rules_ptr,
                &mut pinned_rules,
                &mut receiver_types_cache,
                &mut argument_factories_cache,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cache_hierarchy_rules(
        &mut self,
        path: &str,
        rules: &std::sync::Arc<languages::LanguageContext>,
        rules_ptr: usize,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
        pinned_rules: &mut Vec<std::sync::Arc<languages::LanguageContext>>,
        hierarchy_cache: &mut HashMap<usize, Option<std::sync::Arc<HierarchyAuthorities>>>,
        hierarchy_specs_cache: &mut HashMap<
            usize,
            Option<std::sync::Arc<HashMap<String, languages::manifests::syntax::ReturnTypeSpec>>>,
        >,
    ) {
        let interner = self.interner;
        let cached = hierarchy_cache.entry(rules_ptr).or_insert_with(|| {
            pinned_rules.push(std::sync::Arc::clone(rules));
            let mut map = HashMap::new();
            let mut specs = HashMap::new();
            for (origin_pack, raw_base) in dependencies.hierarchy_return_targets_for_path(path) {
                if let Ok(spec) = languages::manifests::syntax::ReturnTypeSpec::parse(raw_base) {
                    let mut targets: Vec<String> = Vec::new();
                    spec.compiled().visit_named(&mut |name: &str| {
                        if name != "Item" && !targets.iter().any(|existing| existing == name) {
                            targets.push(name.to_owned());
                        }
                    });
                    specs.insert(raw_base.to_owned(), spec);
                    for target in targets {
                        insert_hierarchy_authority(
                            dependencies,
                            interner,
                            path,
                            origin_pack,
                            &target,
                            &mut map,
                        );
                    }
                } else {
                    insert_hierarchy_authority(
                        dependencies,
                        interner,
                        path,
                        origin_pack,
                        raw_base,
                        &mut map,
                    );
                }
            }
            hierarchy_specs_cache.insert(
                rules_ptr,
                (!specs.is_empty()).then(|| std::sync::Arc::new(specs)),
            );
            (!map.is_empty()).then(|| std::sync::Arc::new(map))
        });
        if let Some(hierarchy) = cached {
            let path_id = self.interner.lookup_path(path).expect("catalog path");
            self.hierarchy_results.insert(path_id, hierarchy.clone());
        }
        if let Some(Some(specs)) = hierarchy_specs_cache.get(&rules_ptr) {
            let path_id = self.interner.lookup_path(path).expect("catalog path");
            self.hierarchy_return_specs.insert(path_id, specs.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cache_receiver_rules(
        &mut self,
        path: &str,
        module_language: &str,
        rules: &std::sync::Arc<languages::LanguageContext>,
        rules_ptr: usize,
        pinned_rules: &mut Vec<std::sync::Arc<languages::LanguageContext>>,
        receiver_types_cache: &mut HashMap<
            (usize, &'static str),
            Option<std::sync::Arc<ReceiverTypes>>,
        >,
        argument_factories_cache: &mut HashMap<
            (usize, &'static str),
            Option<std::sync::Arc<ArgumentFactories>>,
        >,
    ) {
        let language = languages::by_id(module_language)
            .map(|profile| profile.id())
            .unwrap_or("");
        let key = (rules_ptr, language);
        if !receiver_types_cache.contains_key(&key) {
            pinned_rules.push(std::sync::Arc::clone(rules));
            let (indices, factories) =
                collect_receiver_type_and_factory_rules(rules, module_language, self.interner);
            receiver_types_cache.insert(
                key,
                (!indices.is_empty()).then(|| {
                    std::sync::Arc::new(ReceiverTypes {
                        rules: rules.clone(),
                        indices,
                    })
                }),
            );
            argument_factories_cache.insert(
                key,
                (!factories.is_empty()).then(|| std::sync::Arc::new(factories)),
            );
        }
        if let Some(Some(receiver_types)) = receiver_types_cache.get(&key) {
            let path_id = self.interner.lookup_path(path).expect("catalog path");
            self.receiver_types.insert(path_id, receiver_types.clone());
        }
        if let Some(Some(argument_factories)) = argument_factories_cache.get(&key) {
            let path_id = self.interner.lookup_path(path).expect("catalog path");
            self.argument_factories
                .insert(path_id, argument_factories.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn populate_file_imports(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        modules: &'a ModulesByNamespace<'a>,
        by_module: &'a HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
        exports: &'a HashMap<&'a str, PackageExports<'a>>,
        normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
        #[cfg(feature = "lang-rust")] rust_forwarding: &'a super::super::rust::forwarding::RustForwarding<
            'a,
        >,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
    ) {
        for (path, facts) in all.iter_facts() {
            let Some(module) = facts.nodes.iter().find(|node| node.kind == "module") else {
                continue;
            };
            let Some(profile) = languages::by_id(&module.language) else {
                continue;
            };
            let linker = profile.linker();
            let Some(package_exports) = exports.get(profile.id()) else {
                continue;
            };
            let owners: HashMap<&str, &Node> = facts
                .nodes
                .iter()
                .map(|node| (node.id.as_str(), node))
                .collect();
            for reference in facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
            {
                self.record_single_file_import(
                    path,
                    facts,
                    module,
                    profile,
                    linker,
                    package_exports,
                    &owners,
                    reference,
                    modules,
                    by_module,
                    by_qual,
                    normalized_imports,
                    #[cfg(feature = "lang-rust")]
                    rust_forwarding,
                    dependencies,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_single_file_import(
        &mut self,
        path: &'a str,
        facts: &'a Facts,
        module: &'a Node,
        profile: &'static dyn languages::LanguageProfile,
        linker: &'static dyn crate::linker::traits::LanguageLinker,
        package_exports: &'a PackageExports<'a>,
        owners: &HashMap<&str, &'a Node>,
        reference: &'a Reference,
        modules: &'a ModulesByNamespace<'a>,
        by_module: &'a HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
        normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
        #[cfg(feature = "lang-rust")] rust_forwarding: &'a super::super::rust::forwarding::RustForwarding<
            'a,
        >,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
    ) {
        let Some(alias) = reference.alias.as_deref().filter(|alias| *alias != "*") else {
            return;
        };
        let Some(owner) = owners.get(reference.source.as_str()).copied() else {
            return;
        };
        let key = (
            path,
            reference.module.as_deref().unwrap_or(&reference.expression),
        );
        let Some(normalized) = normalized_imports.get(&key).and_then(Option::as_ref) else {
            return;
        };
        let (mut providers, selected) =
            import_provider_modules(path, normalized, profile.family(), modules);
        let mut candidates = Vec::new();
        let mut lookup_key = String::new();
        linker.resolve_import(&mut ImportContext {
            path,
            interner: self.interner,
            dependencies,
            reference,
            #[cfg(feature = "lang-rust")]
            rust_forwarding: Some(rust_forwarding),
            normalized: Some(normalized),
            selected_namespace: selected,
            family: profile.family(),
            modules: &mut providers,
            candidates: &mut candidates,
            modules_by_namespace: modules,
            by_module,
            by_qual,
            package_exports,
            lookup_key: &mut lookup_key,
        });
        let position = reference.position();
        let owner_id = self
            .interner
            .lookup_symbol(&owner.id)
            .expect("catalog owner");
        let alias_id = self
            .interner
            .lookup_symbol(alias)
            .expect("import alias is interned");
        let is_type_only = matches!(
            reference.receiver_hint.as_ref(),
            Some(ReceiverHint::TypeOnlyImport)
        );
        let unproven_guard = is_type_only
            && super::super::python::semantic_context::type_only_import_is_unproven(
                profile,
                path,
                facts,
                modules,
                dependencies,
                self.interner,
                self.node_rebindings,
            );
        if self
            .imports
            .get(&owner_id)
            .is_some_and(|aliases| aliases.contains_key(&alias_id))
        {
            self.factory_imports.insert((owner_id, alias_id), None);
        } else if !is_type_only && providers.len() == 1 {
            let binding = self
                .interner
                .lookup_symbol(&reference.expression)
                .expect("factory import binding is interned");
            self.factory_imports
                .insert((owner_id, alias_id), Some((providers[0], binding, position)));
        }
        let target = if unproven_guard {
            Imported::Unknown
        } else if providers.len() == 1 && !candidates.is_empty() {
            Imported::Nodes(candidates, position, is_type_only)
        } else if providers.is_empty() && !normalized.relative {
            let module_name = reference.module.as_deref().unwrap_or(&reference.expression);
            if super::super::python::semantic_context::dynamic_type_alias(
                &module.language,
                module_name,
                &reference.expression,
            ) {
                Imported::Unknown
            } else {
                let factory_safe =
                    super::super::python::semantic_context::logging_factory_import_is_safe(
                        module, alias, position,
                    );
                Imported::External(module_name.to_owned(), position, reference, factory_safe)
            }
        } else {
            Imported::Unknown
        };
        let aliases = self.imports.entry(owner_id).or_default();
        if aliases.contains_key(&alias_id) {
            aliases.insert(alias_id, Imported::Unknown);
        } else {
            aliases.insert(alias_id, target);
        }
    }

    fn populate_class_members(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
    ) {
        for node in all
            .values_facts()
            .flat_map(|facts| &facts.nodes)
            .filter(|node| node.kind == "method")
        {
            let Some((parent, _)) = node.qualname.rsplit_once('.') else {
                continue;
            };
            let mut classes = by_qual
                .get(parent)
                .into_iter()
                .flatten()
                .copied()
                .filter(|class| {
                    class.path == node.path
                        && matches!(class.kind.as_str(), "class" | "struct" | "interface")
                });
            let Some(class) = classes.next() else {
                continue;
            };
            if classes.next().is_none() {
                self.members
                    .entry((
                        self.interner.lookup_symbol(&class.id).expect("catalog class"),
                        self.interner
                            .lookup_symbol(&node.name)
                            .expect("catalog member"),
                        node.details["is_static"] == true,
                    ))
                    .or_default()
                    .push(node);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::linker) fn build(
        all: &'a dyn crate::FactsLookup,
        interner: &'a crate::interner::LinkerInterner<'a>,
        modules: &'a ModulesByNamespace<'a>,
        by_module: &'a HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &'a HashMap<&'a str, Vec<&'a Node>>,
        by_id: &'a HashMap<SymbolId, &'a Node>,
        node_bindings: &'a super::super::NodeBindingIndex,
        node_rebindings: &'a super::super::NodeBindingIndex,
        exports: &'a HashMap<&'a str, PackageExports<'a>>,
        normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
        #[cfg(feature = "lang-rust")] rust_forwarding: &'a super::super::rust::forwarding::RustForwarding<
            'a,
        >,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
    ) -> Self {
        let mut context = Self {
            interner,
            dependencies,
            flows: None,
            named: Default::default(),
            by_qual,
            by_id,
            node_bindings,
            node_rebindings,
            exports,
            imports: HashMap::new(),
            factory_imports: HashMap::new(),
            factory_bindings: HashMap::new(),
            factory_declarations: HashMap::new(),
            factory_callees: HashSet::new(),
            receiver_types: HashMap::new(),
            argument_factories: HashMap::new(),
            hierarchy_results: HashMap::new(),
            hierarchy_return_specs: HashMap::new(),
            component_builtins: HashMap::new(),
            component_candidate_ids: HashMap::new(),
            component_candidates: Vec::new(),
            component_consumers: HashMap::new(),
            implicit_builtins: HashMap::new(),
            autoimports: HashMap::new(),
            result_scopes: HashMap::new(),
            callable_scopes: HashMap::new(),
            commonjs: super::super::javascript::commonjs_bindings::ResolvedCommonJsBindings::build(all),
            members: HashMap::new(),
            python: None,
            #[cfg(feature = "lang-rust")]
            rust: None,
            #[cfg(feature = "lang-rust")]
            rust_forwarding,
            #[cfg(feature = "lang-typescript")]
            typescript: None,
        };
        context.build_rule_caches_for_paths(all, dependencies);
        context.populate_file_imports(
            all,
            modules,
            by_module,
            by_qual,
            exports,
            normalized_imports,
            #[cfg(feature = "lang-rust")]
            rust_forwarding,
            dependencies,
        );
        #[cfg(feature = "lang-rust")]
        super::super::rust::semantic_context::import_globs(
            &mut context,
            all,
            by_id,
            normalized_imports,
            modules,
            rust_forwarding,
        );
        context.populate_class_members(all, by_qual);
        context
    }
}

fn collect_receiver_type_and_factory_rules(
    rules: &languages::LanguageContext,
    module_language: &str,
    interner: &crate::interner::LinkerInterner<'_>,
) -> (Vec<usize>, ArgumentFactories) {
    let mut indices = Vec::new();
    let mut factories: ArgumentFactories = HashMap::new();
    let Some(profile) = languages::by_id(module_language) else {
        return (indices, factories);
    };
    let family = profile.family();
    for (index, rule) in rules.syntax_rules.iter().enumerate() {
        let languages::manifests::syntax::SyntaxOperation::ReceiverType { receiver } =
            &rule.operation
        else {
            continue;
        };
        if rule.activation_family() != Some(family) {
            continue;
        }
        indices.push(index);
        if !matches!(module_language, "javascript" | "typescript") || rule.language != module_language
        {
            continue;
        }
        for factory in &receiver.argument_factories {
            let languages::manifests::projections::Api::GlobalMember {
                receiver: global,
                member,
            } = &factory.callee
            else {
                continue;
            };
            let Some(callee) = interner.lookup_symbol(&format!("{global}.{member}")) else {
                continue;
            };
            let authority = std::sync::Arc::new(
                super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                    pack: rule.pack.clone(),
                    rule_id: rule.id.clone(),
                    receiver: std::sync::Arc::new(
                        super::super::value_flow::ConfiguredNominalReceiver {
                            rule: receiver.clone(),
                        },
                    ),
                },
            );
            for literal in &factory.literals {
                if let Some(literal) = interner.lookup_symbol(literal) {
                    factories
                        .entry((callee, factory.argument, literal))
                        .or_default()
                        .push(authority.clone());
                }
            }
        }
    }
    (indices, factories)
}

fn insert_hierarchy_authority(
    dependencies: &languages::dependency_registry::DependencyRegistry,
    interner: &crate::interner::LinkerInterner<'_>,
    path: &str,
    origin_pack: &str,
    base: &str,
    map: &mut HierarchyAuthorities,
) {
    let Some((pack, rule)) =
        dependencies.class_hierarchy_authority_for_path(path, base, Some(origin_pack))
    else {
        return;
    };
    let (Some(pack_id), Some(base_id)) = (interner.lookup_symbol(pack), interner.lookup_symbol(base))
    else {
        return;
    };
    map.insert(
        (pack_id, base_id),
        std::sync::Arc::new(super::super::value_flow::ConfiguredReceiverAuthority::Hierarchy {
            pack: pack.to_owned(),
            rule: std::sync::Arc::new(rule.clone()),
        }),
    );
}
