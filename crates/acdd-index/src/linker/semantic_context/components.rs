use super::context::*;

type ComponentRuleKey = (
    crate::interner::SymbolId,
    crate::interner::SymbolId,
    crate::interner::SymbolId,
);
type ResultScopeKey = (ComponentRuleKey, crate::interner::SymbolId);

fn component_rule_key(
    interner: &crate::interner::LinkerInterner<'_>,
    config: &Node,
    rule: &languages::manifests::syntax::SyntaxRule,
) -> ComponentRuleKey {
    (
        interner
            .lookup_symbol(&config.id)
            .expect("catalog configuration"),
        interner
            .lookup_symbol(&rule.pack)
            .expect("component rule pack is interned"),
        interner
            .lookup_symbol(&rule.id)
            .expect("component rule id is interned"),
    )
}

fn scoped_matching_provider_module<'a, 'e>(
    provider_path: &str,
    facts: &'a Facts,
    scope: &str,
    exports: &'e [languages::manifests::syntax::ComponentExport],
    dependencies: &languages::dependency_registry::DependencyRegistry,
) -> Option<(&'a Node, &'e languages::manifests::syntax::ComponentExport)> {
    let selector = exports.iter().find(|selector| {
        languages::dependency_registry::component_export_matches_provider(
            provider_path,
            scope,
            &selector.directory,
            &selector.extension,
            dependencies,
        )
    })?;
    let module = facts.nodes.iter().find(|node| node.kind == "module")?;
    Some((module, selector))
}

pub(super) fn component_names(
    path: &str,
    project: &str,
    root: &str,
    extension: &str,
) -> Option<(String, String)> {
    let relative =
        languages::manifests::semantic::relative_under_directory(path, project, root)?;
    let stem = relative.strip_suffix(extension)?;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
    {
        return None;
    }
    let mut pascal = String::new();
    for segment in stem.split('/') {
        if segment.is_empty() {
            return None;
        }
        for word in segment.split(['-', '_']) {
            let mut chars = word.chars();
            let first = chars.next()?;
            pascal.push(first.to_ascii_uppercase());
            pascal.extend(chars);
        }
    }
    let mut kebab = String::with_capacity(pascal.len() + 4);
    for (index, character) in pascal.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            kebab.push('-');
        }
        kebab.push(character.to_ascii_lowercase());
    }
    Some((pascal, kebab))
}

impl<'a> super::Context<'a> {
    fn collect_implicit_builtins(
        &self,
        rules: &languages::LanguageContext,
        module_language: &str,
    ) -> (std::sync::Arc<ImplicitBuiltinMap>, bool) {
        use languages::manifests::syntax::SyntaxOperation;
        let mut names_map = HashMap::new();
        let mut has_component = false;
        for rule in &rules.syntax_rules {
            if matches!(rule.operation, SyntaxOperation::ComponentEnvironment { .. }) {
                has_component = true;
            }
            if rule.language != module_language {
                continue;
            }
            if let SyntaxOperation::ImplicitBuiltins { names } = &rule.operation {
                names_map.extend(names.iter().filter_map(|name| {
                    self.interner.lookup_symbol(name).map(|name| {
                        (
                            name,
                            format!(
                                "{}; rule:{}:{}",
                                rule.display
                                    .builtin
                                    .as_deref()
                                    .unwrap_or("builtin:configured"),
                                rule.pack,
                                rule.id
                            ),
                        )
                    })
                }));
            }
        }
        (std::sync::Arc::new(names_map), has_component)
    }

    fn build_callable_scope(
        &self,
        all: &'a dyn crate::FactsLookup,
        scope: &str,
        descriptor: &languages::manifests::syntax::CallableImports,
        state: &crate::semantic::CallableImportConfiguration,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> CallableScope<'a> {
        let mut catalog = CallableScope {
            implicit: state.enabled && state.scan,
            aliases: descriptor
                .aliases
                .iter()
                .map(|alias| {
                    self.interner
                        .lookup_symbol(alias)
                        .expect("callable import alias is interned")
                })
                .collect(),
            names: HashMap::new(),
            modules: HashMap::new(),
            incomplete: false,
        };
        for (provider_path, file) in all.iter_facts() {
            let Some((provider, selector)) = scoped_matching_provider_module(
                provider_path,
                file,
                scope,
                &descriptor.exports,
                dependencies,
            ) else {
                continue;
            };
            let exported = self.collect_callable_module_exports(
                provider_path,
                provider,
                &mut catalog.incomplete,
            );
            merge_callable_candidates(&mut catalog.names, &exported);
            let relative = if scope.is_empty() {
                Some(provider_path)
            } else {
                languages::manifests::semantic::relative_under_directory(provider_path, "", scope)
            };
            let Some(stem) = relative.and_then(|rel| rel.strip_suffix(&selector.extension)) else {
                continue;
            };
            let rel_path = relative.expect("stem implies relative");
            let module = std::sync::Arc::new(CallableModule {
                owner: provider,
                exports: exported,
            });
            catalog.modules.insert(
                self.interner
                    .lookup_path(stem)
                    .expect("callable module stem is interned"),
                module.clone(),
            );
            catalog.modules.insert(
                self.interner
                    .lookup_path(rel_path)
                    .expect("callable module path is interned"),
                module,
            );
        }
        catalog
    }

    fn collect_callable_module_exports(
        &self,
        provider_path: &str,
        provider: &'a Node,
        catalog_incomplete: &mut bool,
    ) -> HashMap<SymbolId, CallableCandidates<'a>> {
        let mut exported = HashMap::<SymbolId, CallableCandidates<'a>>::new();
        for binding in provider.details["exports"].as_array().into_iter().flatten() {
            if binding["type_only"] == true || binding["name"] == "default" {
                continue;
            }
            let Some(public) = binding["name"].as_str() else {
                *catalog_incomplete = true;
                continue;
            };
            let public_id = self
                .interner
                .lookup_symbol(public)
                .expect("callable export name is interned");
            let candidates = exported.entry(public_id).or_default();
            let target = binding["local"]
                .as_str()
                .filter(|_| binding["module"].is_null())
                .and_then(|local| {
                    self.exports
                        .get(provider.language.as_str())?
                        .get(&self.interner.lookup_path(provider_path)?)?
                        .get(&public_id)
                        .copied()
                        .flatten()
                        .filter(|node| {
                            node.path == *provider_path
                                && node.name == local
                                && node.kind == "function"
                        })
                });
            if let Some(target) = target {
                if !candidates.nodes.iter().any(|node| node.id == target.id) {
                    candidates.nodes.push(target);
                }
            } else {
                candidates.incomplete = true;
            }
        }
        exported
    }

    #[allow(clippy::too_many_arguments)]
    fn build_result_scope(
        &self,
        all: &'a dyn crate::FactsLookup,
        scope: &str,
        config_node: &'a Node,
        module_id: SymbolId,
        name: &str,
        addition: &languages::manifests::syntax::ComponentModule,
        rule: &languages::manifests::syntax::SyntaxRule,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> ResultScope<'a> {
        let mut exports = HashMap::<SymbolId, Vec<(&Node, SymbolId)>>::new();
        for (provider_path, provider_facts) in all.iter_facts() {
            let Some((provider, _)) = scoped_matching_provider_module(
                provider_path,
                provider_facts,
                scope,
                &addition.exports,
                dependencies,
            ) else {
                continue;
            };
            for binding in provider.details["exports"].as_array().into_iter().flatten() {
                if binding["type_only"] == true || binding["module"].as_str().is_some() {
                    continue;
                }
                let (Some(public), Some(local)) =
                    (binding["name"].as_str(), binding["local"].as_str())
                else {
                    continue;
                };
                if public == "default" {
                    continue;
                }
                exports
                    .entry(
                        self.interner
                            .lookup_symbol(public)
                            .expect("result export name is interned"),
                    )
                    .or_default()
                    .push((
                        provider,
                        self.interner
                            .lookup_symbol(local)
                            .expect("result export binding is interned"),
                    ));
            }
        }
        let calls = addition
            .call_results
            .iter()
            .map(|(callee, members)| {
                let callee = self
                    .interner
                    .lookup_symbol(callee)
                    .expect("result callee is interned");
                let members = members
                    .iter()
                    .map(|member| {
                        self.interner
                            .lookup_symbol(member)
                            .expect("result member is interned")
                    })
                    .collect();
                (
                    callee,
                    std::sync::Arc::new(super::super::value_flow::ConfiguredCallResult {
                        evidence: format!(
                            "configured result; config:{}; module:{}; package:{}; rule:{}:{}",
                            config_node.path, name, addition.package, rule.pack, rule.id
                        ),
                        callee,
                        members,
                    }),
                )
            })
            .collect();
        ResultScope {
            config: config_node,
            module: module_id,
            exports,
            calls,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn populate_component_candidates(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        scope: &str,
        path_id: PathId,
        directory: &str,
        extension: &str,
        rule: &languages::manifests::syntax::SyntaxRule,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) {
        let key = (
            self.interner
                .lookup_path(scope)
                .expect("component manifest scope is interned"),
            self.interner
                .lookup_symbol(&rule.pack)
                .expect("component rule pack is interned"),
            self.interner
                .lookup_symbol(&rule.id)
                .expect("component rule id is interned"),
        );
        let (index, inserted) = if let Some(&index) = self.component_candidate_ids.get(&key) {
            (index, false)
        } else {
            let index = self.component_candidates.len();
            self.component_candidate_ids.insert(key, index);
            self.component_candidates.push(HashMap::new());
            (index, true)
        };
        self.component_consumers
            .entry(path_id)
            .or_default()
            .push(index);
        if !inserted {
            return;
        }
        let candidates = &mut self.component_candidates[index];
        for (provider_path, provider) in all.iter_facts() {
            let Some((pascal, kebab)) =
                component_names(provider_path, scope, directory, extension)
            else {
                continue;
            };
            let Some(provider_module) = (dependencies
                .nearest_manifest_scope_for_path(languages::LanguageFamily("javascript"), provider_path)
                == Some(scope))
            .then(|| provider.nodes.iter().find(|node| node.kind == "module"))
            .flatten() else {
                continue;
            };
            for name in [pascal, kebab] {
                let name = self
                    .interner
                    .lookup_symbol(&name)
                    .expect("component candidate name is interned");
                let providers = candidates.entry(name).or_default();
                if !providers
                    .iter()
                    .any(|existing| existing.id == provider_module.id)
                {
                    providers.push(provider_module);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_component_environment_rule(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        path: &str,
        path_id: PathId,
        module_language: &str,
        scope: &str,
        rule: &languages::manifests::syntax::SyntaxRule,
        dependencies: &languages::dependency_registry::DependencyRegistry,
        result_catalog: &mut HashMap<ResultScopeKey, std::sync::Arc<ResultScope<'a>>>,
        callable_catalog: &mut HashMap<ComponentRuleKey, std::sync::Arc<CallableScope<'a>>>,
    ) {
        let languages::manifests::syntax::SyntaxOperation::ComponentEnvironment {
            package,
            config_file,
            directory,
            extension,
            autoimports,
            builtins,
            modules,
            callable_imports,
            ..
        } = &rule.operation
        else {
            return;
        };
        let Some(config_path) = languages::dependency_registry::component_environment_config_path(
            rule,
            path,
            scope,
            package,
            config_file,
            dependencies,
        ) else {
            return;
        };
        let config = all
            .get_facts(&config_path)
            .and_then(|facts| facts.nodes.iter().find(|node| node.kind == "module"));
        if rule.language == module_language && config.is_some() {
            self.autoimports.entry(path_id).or_default().extend(
                autoimports.iter().filter_map(|name| {
                    self.interner.lookup_symbol(name).map(|name| {
                        (
                            name,
                            format!(
                                "{}; rule:{}:{}",
                                rule.display
                                    .autoimport
                                    .as_deref()
                                    .unwrap_or("builtin:configured_autoimport"),
                                rule.pack,
                                rule.id
                            ),
                        )
                    })
                }),
            );
        }
        let configuration = config
            .and_then(|node| {
                self.interner
                    .lookup_symbol(node.id.as_str())
                    .and_then(|id| self.flows?.get(&id))
            })
            .and_then(|state| match state {
                crate::typed_facts::FlowState::Valid { facts, .. } => {
                    Some(facts.component_configurations())
                }
                _ => None,
            })
            .into_iter()
            .flat_map(|items| {
                languages::manifests::semantic::matching_component_configurations(items, rule)
            })
            .collect::<Vec<_>>();
        if let ([configuration], Some(config_node)) = (configuration.as_slice(), config) {
            if let (Some(descriptor), Some(state)) =
                (callable_imports, &configuration.callable_imports)
            {
                let key = component_rule_key(self.interner, config_node, rule);
                if !callable_catalog.contains_key(&key) {
                    let catalog =
                        self.build_callable_scope(all, scope, descriptor, state, dependencies);
                    callable_catalog.insert(key, std::sync::Arc::new(catalog));
                }
                self.callable_scopes
                    .entry(path_id)
                    .or_default()
                    .push(callable_catalog[&key].clone());
            }
            for (name, addition) in
                languages::dependency_registry::active_component_module_additions(
                    configuration,
                    modules,
                    rule,
                    path,
                    dependencies,
                )
            {
                let module_id = self
                    .interner
                    .lookup_symbol(name)
                    .expect("configured module is interned");
                let key: ResultScopeKey =
                    (component_rule_key(self.interner, config_node, rule), module_id);
                if let hashbrown::hash_map::Entry::Vacant(entry) = result_catalog.entry(key) {
                    let scope_val = self.build_result_scope(
                        all,
                        scope,
                        config_node,
                        module_id,
                        name,
                        addition,
                        rule,
                        dependencies,
                    );
                    entry.insert(std::sync::Arc::new(scope_val));
                }
                self.result_scopes
                    .entry(path_id)
                    .or_default()
                    .push(result_catalog[&key].clone());
            }
        }
        if !path.ends_with(extension) {
            return;
        }
        let builtin = self.component_builtins.entry(path_id).or_default();
        for name in builtins {
            builtin.insert(
                self.interner
                    .lookup_symbol(name)
                    .expect("component builtin is interned"),
                format!(
                    "{}; rule:{}:{}",
                    rule.display
                        .component
                        .as_deref()
                        .unwrap_or("builtin:configured_component"),
                    rule.pack,
                    rule.id
                ),
            );
        }
        let [config] = configuration.as_slice() else {
            return;
        };
        for (name, addition) in languages::dependency_registry::active_component_module_additions(
            config,
            modules,
            rule,
            path,
            dependencies,
        ) {
            for component in &addition.components {
                builtin.insert(
                    self.interner
                        .lookup_symbol(component)
                        .expect("configured component is interned"),
                    format!(
                        "{}:{name}; rule:{}:{}",
                        rule.display
                            .component
                            .as_deref()
                            .unwrap_or("builtin:configured_component"),
                        rule.pack,
                        rule.id
                    ),
                );
            }
        }
        if config.enabled {
            self.populate_component_candidates(
                all,
                scope,
                path_id,
                directory,
                extension,
                rule,
                dependencies,
            );
        }
    }

    pub(super) fn prepare_component_environments(
        &mut self,
        all: &'a dyn crate::FactsLookup,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) {
        let mut implicit_builtins_cache: HashMap<
            (usize, SymbolId),
            (std::sync::Arc<ImplicitBuiltinMap>, bool),
        > = HashMap::new();
        let mut pinned_rules: Vec<std::sync::Arc<languages::LanguageContext>> = Vec::new();
        let mut result_catalog: HashMap<ResultScopeKey, std::sync::Arc<ResultScope<'a>>> =
            HashMap::new();
        let mut callable_catalog: HashMap<ComponentRuleKey, std::sync::Arc<CallableScope<'a>>> =
            HashMap::new();
        for (path, source) in all.iter_facts() {
            let Some(module) = source.nodes.iter().find(|node| node.kind == "module") else {
                continue;
            };
            let path_id = self
                .interner
                .lookup_path(path)
                .expect("component catalog path is interned");
            let language = self
                .interner
                .lookup_symbol(&module.language)
                .expect("component language is interned");
            let rules = dependencies.language_context_for_path(path);
            let rules_key = (std::sync::Arc::as_ptr(&rules) as usize, language);
            let (implicit, has_component_env) =
                implicit_builtins_cache.entry(rules_key).or_insert_with(|| {
                    pinned_rules.push(std::sync::Arc::clone(&rules));
                    self.collect_implicit_builtins(&rules, &module.language)
                });
            if !implicit.is_empty() {
                self.implicit_builtins
                    .insert(path_id, std::sync::Arc::clone(implicit));
            }
            if !*has_component_env {
                continue;
            }
            let Some(scope) = dependencies
                .nearest_manifest_scope_for_path(languages::LanguageFamily("javascript"), path)
            else {
                continue;
            };
            for rule in &rules.syntax_rules {
                self.process_component_environment_rule(
                    all,
                    path,
                    path_id,
                    &module.language,
                    scope,
                    rule,
                    dependencies,
                    &mut result_catalog,
                    &mut callable_catalog,
                );
            }
        }
    }
}

fn merge_callable_candidates<'a>(
    names: &mut HashMap<SymbolId, CallableCandidates<'a>>,
    exported: &HashMap<SymbolId, CallableCandidates<'a>>,
) {
    for (name, candidates) in exported {
        let combined = names.entry(*name).or_default();
        combined.incomplete |= candidates.incomplete;
        for node in &candidates.nodes {
            if !combined.nodes.iter().any(|previous| previous.id == node.id) {
                if combined.nodes.len() < 8 {
                    combined.nodes.push(node);
                } else {
                    combined.incomplete = true;
                }
            }
        }
    }
}
