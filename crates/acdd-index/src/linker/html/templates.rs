use crate::languages::{self, LanguageFamily};
use crate::linker::{
    resolution::input::ResolutionInput,
    semantic_context, value_flow,
};
use crate::models::{compact_graph::CompactGraph as Graph, Facts, Node, Reference};
use crate::semantic::{TemplateContextFact, ValueExpr, ValueFlowFacts};
use crate::typed_facts::FlowState;
use hashbrown::{HashMap, HashSet};
use std::collections::BTreeSet;

pub(in crate::linker) struct TemplateRootRegistry<'a> {
    by_root: HashMap<&'a str, Vec<(&'a Node, &'a crate::semantic::ExtractionRuleOrigin)>>,
}

impl<'a> TemplateRootRegistry<'a> {
    pub(in crate::linker) fn build(
        all: &'a dyn crate::FactsLookup,
        dependencies: &languages::dependency_registry::DependencyRegistry,
        flows: Option<&HashMap<crate::interner::SymbolId, &'a FlowState>>,
        interner: &crate::interner::LinkerInterner<'a>,
    ) -> Self {
        let mut by_root: HashMap<&str, Vec<(&Node, &crate::semantic::ExtractionRuleOrigin)>> =
            HashMap::new();
        for (path, source) in all.iter_facts() {
            let mut roots = source
                .nodes
                .iter()
                .filter(|node| node.kind == crate::models::tags::NodeKind::TemplateLoaderRoot && node.language == crate::models::tags::LanguageId::Python)
                .peekable();
            if roots.peek().is_none() {
                continue;
            }
            let rules = dependencies.language_context_for_path(path);
            for node in roots {
                let Some(FlowState::Valid { facts, .. }) = interner
                    .lookup_symbol(&node.id)
                    .and_then(|id| flows.and_then(|flows| flows.get(&id).copied()))
                else {
                    continue;
                };
                let Some(origin) = facts.rule_origins().find(|origin| {
                    rules.syntax_rules.iter().any(|rule| {
                        rule.pack == origin.pack
                            && rule.id == origin.rule
                            && matches!(
                                &rule.operation,
                                languages::manifests::syntax::SyntaxOperation::TemplateLoader {
                                    package,
                                    ..
                                } if rule.activation_family().is_some_and(|family| {
                                    dependencies.declares_for_path(family, path, package)
                                })
                            )
                    })
                }) else {
                    continue;
                };
                if !source_imports_are_external(path, node, all, dependencies) {
                    continue;
                }
                by_root.entry(&node.name).or_default().push((node, origin));
            }
        }
        Self { by_root }
    }

    pub(in crate::linker) fn admits(
        &self,
        path: &str,
        dependencies: &languages::dependency_registry::DependencyRegistry,
        pack: &str,
    ) -> bool {
        let rules = dependencies.language_context_for_path(path);
        if !rules.syntax_rules.iter().any(|rule| {
            matches!(
                &rule.operation,
                languages::manifests::syntax::SyntaxOperation::TemplateLoader { package, .. }
                    if rule.activation_family().is_some_and(|family| {
                        dependencies.declares_for_path(family, path, package)
                    })
            )
        }) {
            return false;
        }
        let Some(scope) =
            dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), path)
        else {
            return false;
        };
        let mut providers = 0;
        let mut prefix = path.rsplit_once('/').map(|(dir, _)| dir);
        while let Some(dir) = prefix {
            if let Some(nodes) = self.by_root.get(dir) {
                providers += nodes
                    .iter()
                    .filter(|(node, origin)| {
                        if origin.pack != pack {
                            return false;
                        }
                        dependencies
                            .nearest_manifest_scope_for_path(LanguageFamily("python"), &node.path)
                            == Some(scope)
                    })
                    .count();
            }
            prefix = dir.rsplit_once('/').map(|(parent, _)| parent);
        }
        providers == 1
    }
}

fn source_imports_are_external(
    owner: &str,
    node: &Node,
    all: &dyn crate::FactsLookup,
    dependencies: &languages::dependency_registry::DependencyRegistry,
) -> bool {
    let Some(profile) = languages::by_id("python") else {
        return false;
    };
    let Some(scope) = dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), owner)
    else {
        return false;
    };
    let Some(imports) = node.detail_entry(crate::models::details::NodeDetailKey::IMPORT_MODULES)
        .as_array()
        .filter(|imports| !imports.is_empty())
    else {
        return false;
    };
    let src_root = if scope.is_empty() {
        "src".to_owned()
    } else {
        format!("{scope}/src")
    };
    for import in imports {
        let Some(module) = import.as_str() else {
            return false;
        };
        let Some(normalized) = profile
            .normalize_import(owner, module)
            .filter(|import| !import.relative)
        else {
            return false;
        };
        let stem = normalized.namespace.replace('.', "/");
        let indexed_module_at = |directory: &str| {
            [".py", ".pyi", "/__init__.py", "/__init__.pyi"]
                .iter()
                .any(|suffix| {
                    let candidate = if directory.is_empty() {
                        format!("{stem}{suffix}")
                    } else {
                        format!("{directory}/{stem}{suffix}")
                    };
                    all.contains_facts(&candidate)
                })
        };
        let mut directory = owner
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        loop {
            if indexed_module_at(directory) || (directory == scope && indexed_module_at(&src_root))
            {
                return false;
            }
            if directory == scope {
                break;
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    true
}

fn template_library_name<'a>(path: &'a str, directory: &str) -> Option<&'a str> {
    let stem = path.strip_suffix(".py")?;
    stem.rmatch_indices('/')
        .find_map(|(index, _)| {
            stem[..index]
                .strip_suffix(directory)
                .filter(|prefix| prefix.ends_with('/'))
                .map(|_| &stem[index + 1..])
        })
        .or_else(|| {
            stem.strip_prefix(directory)
                .and_then(|rest| rest.strip_prefix('/'))
        })
}

pub(crate) fn required_full_facts(
    facts: &dyn crate::FactsLookup,
    catalog: &[(&str, &str)],
    dependencies: &languages::dependency_registry::DependencyRegistry,
) -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let mut python_catalog_by_scope: Option<HashMap<(&str, &str), Vec<&str>>> = None;
    for (owner, facts) in facts.iter_facts() {
        let mut loads = facts
            .references
            .iter()
            .filter(|reference| reference.kind == "template_loads")
            .peekable();
        if loads.peek().is_none() {
            continue;
        }
        let Some(scope) =
            dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), owner)
        else {
            continue;
        };
        let workspace = languages::workspace_path(owner).0;
        let python_by_scope = python_catalog_by_scope.get_or_insert_with(|| {
            let mut by_scope: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
            for &(path, language) in catalog {
                if language != "python" {
                    continue;
                }
                if let Some(path_scope) =
                    dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), path)
                {
                    by_scope
                        .entry((languages::workspace_path(path).0, path_scope))
                        .or_default()
                        .push(path);
                }
            }
            by_scope
        });
        let Some(scoped_paths) = python_by_scope.get(&(workspace, scope)) else {
            continue;
        };
        let loaded_names: HashSet<&str> = loads.map(|load| load.expression.as_str()).collect();
        let rules = dependencies.language_context_for_path(owner);
        for rule in &rules.syntax_rules {
            let languages::manifests::syntax::SyntaxOperation::TemplateLibrary {
                directory, ..
            } = &rule.operation
            else {
                continue;
            };
            for &path in scoped_paths {
                if template_library_name(path, directory)
                    .is_some_and(|name| loaded_names.contains(name))
                {
                    required.insert(path.to_owned());
                }
            }
        }
    }
    required
}

struct Library<'a> {
    origin: crate::semantic::ExtractionRuleOrigin,
    module: &'a Node,
    symbols: HashMap<String, HashMap<&'a str, Vec<&'a Node>>>,
}

pub(in crate::linker) enum Match<'a> {
    Missing,
    Ambiguous,
    Unique {
        module: &'a Node,
        symbol: &'a Node,
        origin: Option<crate::semantic::ExtractionRuleOrigin>,
    },
}

/// Combine configured registries without replacing the existing library/builtin
/// fallback when no scoped registration claims this occurrence.
pub(in crate::linker) fn registry_symbol<'a>(
    owner: &'a Node,
    kind: &str,
    name: &str,
    rules: &[&languages::manifests::syntax::SyntaxRule],
    context: &semantic_context::Context<'a>,
) -> Option<(value_flow::Symbol<'a>, String)> {
    use value_flow::Symbol;
    let mut selected: Option<Symbol<'a>> = None;
    let mut provenance = String::new();
    for rule in rules {
        let languages::manifests::syntax::SyntaxOperation::NamedBinding { domain, .. } =
            &rule.operation
        else {
            continue;
        };
        let Some(symbol) = context.template_named_target(owner, domain, name, kind) else {
            continue;
        };
        provenance.push_str(&format!("; rule:{}:{}", rule.pack, rule.id));
        selected = Some(match (selected, symbol) {
            (Some(Symbol::Ambiguous), _) | (_, Symbol::Ambiguous) => Symbol::Ambiguous,
            (Some(Symbol::Unknown), _) | (_, Symbol::Unknown) => Symbol::Unknown,
            (Some(Symbol::Callable(previous)), Symbol::Callable(next))
                if previous.id != next.id =>
            {
                Symbol::Ambiguous
            }
            (_, symbol) => symbol,
        });
    }
    selected.map(|symbol| (symbol, provenance))
}

fn node_value_flow<'a>(
    node: &'a Node,
    symbol_id: Option<crate::interner::SymbolId>,
    flows: Option<&HashMap<crate::interner::SymbolId, &'a FlowState>>,
) -> Option<std::borrow::Cow<'a, ValueFlowFacts>> {
    if let Some(FlowState::Valid { facts, .. }) =
        symbol_id.and_then(|id| flows.and_then(|flows| flows.get(&id).copied()))
    {
        return Some(std::borrow::Cow::Borrowed(facts.as_ref()));
    }
    ValueFlowFacts::deserialize_for_scope(node.detail_entry(crate::models::details::NodeDetailKey::VALUE_FLOW), false)
        .ok()
        .map(std::borrow::Cow::Owned)
}

pub(in crate::linker) struct TemplateRegistry<'a> {
    by_scope: HashMap<String, HashMap<&'a str, Vec<Library<'a>>>>,
}

impl<'a> TemplateRegistry<'a> {
    pub(in crate::linker) fn build(
        all: &'a dyn crate::FactsLookup,
        flows: Option<&HashMap<crate::interner::SymbolId, &'a FlowState>>,
        interner: &crate::interner::LinkerInterner<'_>,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> Self {
        let mut by_scope: HashMap<String, HashMap<&'a str, Vec<Library<'a>>>> = HashMap::new();
        for (path, facts) in all.iter_facts() {
            let Some(module) = facts
                .nodes
                .iter()
                .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Python)
            else {
                continue;
            };
            let Some(scope) =
                dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), path)
            else {
                continue;
            };
            let rules = dependencies.language_context_for_path(path);
            let mut cached_flow = None;
            let mut cached_functions: Option<Vec<&'a Node>> = None;
            for rule in &rules.syntax_rules {
                let languages::manifests::syntax::SyntaxOperation::TemplateLibrary {
                    directory,
                    module: provider_module,
                    constructor,
                    members,
                    name_keyword,
                } = &rule.operation
                else {
                    continue;
                };
                let Some(library_name) = template_library_name(path, directory).filter(|name| {
                    !name.contains('/') && languages::html::valid_template_name(name)
                }) else {
                    continue;
                };
                let Some(flow) = cached_flow
                    .get_or_insert_with(|| {
                        node_value_flow(module, interner.lookup_symbol(&module.id), flows)
                    })
                    .as_ref()
                else {
                    continue;
                };
                let functions = cached_functions.get_or_insert_with(|| {
                    facts
                        .nodes
                        .iter()
                        .filter(|node| {
                            node.kind == crate::models::tags::NodeKind::Function
                                && node.qualname.rsplit_once('.').is_some_and(|(owner, name)| {
                                    owner == module.qualname && name == node.name
                                })
                                && node.detail_entry(crate::models::details::NodeDetailKey::DECORATORS).is_array()
                        })
                        .collect()
                });
                let mut symbols: HashMap<String, HashMap<&'a str, Vec<&Node>>> = HashMap::new();
                for &function in functions.iter() {
                    let Some(decorators) = function.detail_entry(crate::models::details::NodeDetailKey::DECORATORS).as_array() else {
                        continue;
                    };
                    for decorator in decorators.iter().filter_map(|value| value.as_str()) {
                        let Some((receiver, kind, name)) =
                            registration(decorator, &function.name, members, name_keyword)
                        else {
                            continue;
                        };
                        if verified_library_instance(
                            facts,
                            module,
                            flow,
                            receiver,
                            function.line,
                            provider_module,
                            constructor,
                        ) {
                            symbols
                                .entry(kind.to_owned())
                                .or_default()
                                .entry(name)
                                .or_default()
                                .push(function);
                        }
                    }
                }
                by_scope
                    .entry(scope.to_owned())
                    .or_default()
                    .entry(library_name)
                    .or_default()
                    .push(Library {
                        module,
                        symbols,
                        origin: crate::semantic::ExtractionRuleOrigin {
                            pack: rule.pack.clone(),
                            rule: rule.id.clone(),
                        },
                    });
            }
        }
        Self { by_scope }
    }

    pub(in crate::linker) fn library(
        &self,
        owner: &str,
        name: &str,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> Match<'a> {
        let Some(libraries) = self.libraries(owner, name, dependencies) else {
            return Match::Missing;
        };
        if libraries.len() != 1 {
            return Match::Ambiguous;
        }
        Match::Unique {
            module: libraries[0].module,
            symbol: libraries[0].module,
            origin: Some(libraries[0].origin.clone()),
        }
    }

    pub(in crate::linker) fn symbol(
        &self,
        owner: &str,
        loads: &[&Reference],
        reference: &Reference,
        kind: &str,
        name: &str,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> Match<'a> {
        let mut selected = None;
        for load in loads.iter().copied().filter(|load| {
            load.line < reference.line
                && load
                    .alias
                    .as_deref()
                    .is_none_or(|selected| selected == name)
        }) {
            let Some(libraries) = self.libraries(owner, &load.expression, dependencies) else {
                continue;
            };
            if libraries.len() != 1 {
                return Match::Ambiguous;
            }
            let library = &libraries[0];
            let Some(providers) = library
                .symbols
                .get(kind)
                .and_then(|symbols| symbols.get(name))
            else {
                continue;
            };
            if providers.len() != 1 {
                return Match::Ambiguous;
            }
            let provider = providers[0];
            if selected
                .is_some_and(|(existing, _): (&Node, &Library<'_>)| existing.id != provider.id)
            {
                return Match::Ambiguous;
            }
            selected = Some((provider, library));
        }
        match selected {
            Some((symbol, library)) => Match::Unique {
                module: library.module,
                symbol,
                origin: Some(library.origin.clone()),
            },
            None => Match::Missing,
        }
    }

    fn libraries(
        &self,
        owner: &str,
        name: &str,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> Option<&[Library<'a>]> {
        let scope =
            dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), owner)?;
        self.by_scope.get(scope)?.get(name).map(Vec::as_slice)
    }
}

struct TemplateFile<'a> {
    module: &'a Node,
    variables: HashMap<&'a str, Vec<&'a Node>>,
    macros: HashMap<&'a str, Vec<&'a Node>>,
    imports: Vec<&'a Reference>,
}

pub(in crate::linker) struct ExpressionRegistry<'a> {
    files: HashMap<&'a str, TemplateFile<'a>>,
}

impl<'a> ExpressionRegistry<'a> {
    pub(in crate::linker) fn build(all: &'a dyn crate::FactsLookup) -> Self {
        let mut files = HashMap::new();
        for (path, facts) in all.iter_facts() {
            let Some(module) = facts
                .nodes
                .iter()
                .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Html)
            else {
                continue;
            };
            let mut variables: HashMap<&str, Vec<&Node>> = HashMap::new();
            let mut macros: HashMap<&str, Vec<&Node>> = HashMap::new();
            for node in &facts.nodes {
                match (
                    node.kind.as_str(),
                    node.detail_entry(crate::models::details::NodeDetailKey::TEMPLATE_BINDING).as_str(),
                ) {
                    ("variable", Some("variable")) => {
                        variables.entry(&node.name).or_default().push(node)
                    }
                    ("function", Some("function")) => {
                        macros.entry(&node.name).or_default().push(node)
                    }
                    _ => {}
                }
            }
            let imports = facts
                .references
                .iter()
                .filter(|reference| reference.kind == "template_bindings")
                .collect();
            files.insert(
                path,
                TemplateFile {
                    module,
                    variables,
                    macros,
                    imports,
                },
            );
        }
        Self { files }
    }

    pub(in crate::linker) fn resolve_with_context(
        &self,
        owner: &str,
        reference: &Reference,
        context: &languages::LanguageContext,
    ) -> Match<'a> {
        let Some(file) = self.files.get(owner) else {
            return Match::Missing;
        };
        if let Some(name) = reference.expression.strip_prefix("template.variable.") {
            let Some(bindings) = file.variables.get(name) else {
                return Match::Missing;
            };
            let mut latest: Option<&Node> = None;
            for node in bindings
                .iter()
                .copied()
                .filter(|node| node.line < reference.line)
            {
                if latest.is_some_and(|previous| previous.line == node.line) {
                    return Match::Ambiguous;
                }
                if latest.is_none_or(|previous| previous.line < node.line) {
                    latest = Some(node);
                }
            }
            return latest.map_or(Match::Missing, |symbol| Match::Unique {
                module: file.module,
                symbol,
                origin: None,
            });
        }
        let Some(callee) = reference.expression.strip_prefix("template.macro.") else {
            return Match::Missing;
        };
        let (alias, selected_name) = callee
            .split_once('.')
            .map_or((callee, None), |(alias, name)| (alias, Some(name)));
        let mut selected: Option<(&Node, &Node)> = None;
        if selected_name.is_none() {
            if let Some(macros) = file.macros.get(alias) {
                for symbol in macros
                    .iter()
                    .copied()
                    .filter(|node| node.line < reference.line)
                {
                    if selected.is_some() {
                        return Match::Ambiguous;
                    }
                    selected = Some((file.module, symbol));
                }
            }
        }
        for import in
            file.imports.iter().copied().filter(|import| {
                import.line < reference.line && import.alias.as_deref() == Some(alias)
            })
        {
            let Some(target) = import
                .module
                .as_deref()
                .and_then(|target| languages::html::local_template_target(owner, target, context))
            else {
                continue;
            };
            let Some(provider) = self.files.get(target.as_str()) else {
                continue;
            };
            let macro_name = match selected_name {
                Some(name) if import.expression == "*" => name,
                None if import.expression != "*" => import.expression.as_str(),
                _ => continue,
            };
            let Some(macros) = provider.macros.get(macro_name) else {
                continue;
            };
            if macros.len() != 1 {
                return Match::Ambiguous;
            }
            let symbol = macros[0];
            if selected.is_some_and(|(_, existing)| existing.id != symbol.id) {
                return Match::Ambiguous;
            }
            selected = Some((provider.module, symbol));
        }
        selected.map_or(Match::Missing, |(module, symbol)| Match::Unique {
            module,
            symbol,
            origin: None,
        })
    }
}

pub(in crate::linker) struct RenderProvider<'a> {
    pub(in crate::linker) source: &'a Node,
    pub(in crate::linker) context: TemplateContextFact,
}

pub(in crate::linker) struct RenderContextRegistry<'a> {
    providers: Vec<RenderProvider<'a>>,
    by_template: HashMap<&'a str, Vec<usize>>,
    by_provider: HashMap<&'a str, Vec<(usize, &'a Node)>>,
}

impl<'a> RenderContextRegistry<'a> {
    fn collect_template_includes(
        all: &'a dyn crate::FactsLookup,
        modules: &HashMap<&'a str, &'a Node>,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> HashMap<&'a str, Vec<&'a str>> {
        let mut includes: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
        for (path, source) in all.iter_facts() {
            if !modules.contains_key(path) {
                continue;
            }
            let mut include_refs = source
                .references
                .iter()
                .filter(|reference| reference.kind == "includes")
                .peekable();
            if include_refs.peek().is_none() {
                continue;
            }
            let context = dependencies.language_context_for_path(path);
            for reference in include_refs {
                if let Some(target) =
                    languages::html::local_template_target(path, &reference.expression, &context)
                {
                    if let Some((&target_path, _)) = modules.get_key_value(target.as_str()) {
                        includes.entry(path).or_default().push(target_path);
                    }
                }
            }
        }
        includes
    }

    fn record_transitive_render_targets(
        (path, target_path, index, workspace, scope): (&'a str, &'a str, usize, &str, &str),
        modules: &HashMap<&'a str, &'a Node>,
        includes: &HashMap<&'a str, Vec<&'a str>>,
        dependencies: &languages::dependency_registry::DependencyRegistry,
        by_template: &mut HashMap<&'a str, Vec<usize>>,
        by_provider: &mut HashMap<&'a str, Vec<(usize, &'a Node)>>,
    ) {
        let mut visited = HashSet::new();
        let mut frontier = vec![(target_path, 0usize)];
        while let Some((current, depth)) = frontier.pop() {
            if !visited.insert(current) || depth > 16 {
                continue;
            }
            if languages::workspace_path(current).0 != workspace
                || dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), current)
                    != Some(scope)
            {
                continue;
            }
            let Some(module) = modules.get(current).copied() else {
                continue;
            };
            by_template.entry(current).or_default().push(index);
            by_provider.entry(path).or_default().push((index, module));
            if let Some(children) = includes.get(current) {
                frontier.extend(children.iter().map(|&child| (child, depth + 1)));
            }
        }
    }

    pub(in crate::linker) fn build(
        all: &'a dyn crate::FactsLookup,
        flows: Option<&HashMap<crate::interner::SymbolId, &'a FlowState>>,
        semantic_context: &semantic_context::Context<'a>,
        dependencies: &languages::dependency_registry::DependencyRegistry,
    ) -> Self {
        let modules: HashMap<&'a str, &'a Node> = all
            .iter_facts()
            .filter_map(|(path, source)| {
                source
                    .nodes
                    .iter()
                    .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Html)
                    .map(|node| (path, node))
            })
            .collect();
        let includes = Self::collect_template_includes(all, &modules, dependencies);
        let mut providers = Vec::new();
        let mut by_template: HashMap<&'a str, Vec<usize>> = HashMap::new();
        let mut by_provider: HashMap<&'a str, Vec<(usize, &'a Node)>> = HashMap::new();
        for (path, source) in all.iter_facts() {
            let Some(import_owner) = source
                .nodes
                .iter()
                .find(|node| node.kind == crate::models::tags::NodeKind::Module && node.language == crate::models::tags::LanguageId::Python)
            else {
                continue;
            };
            let Some(scope) =
                dependencies.nearest_manifest_scope_for_path(LanguageFamily("python"), path)
            else {
                continue;
            };
            let rules = dependencies.language_context_for_path(path);
            let workspace = languages::workspace_path(path).0;
            for node in source.nodes.iter().filter(|node| {
                node.language == crate::models::tags::LanguageId::Python
                    && matches!(node.kind.as_str(), "module" | "function" | "method")
            }) {
                let Some(flow) = node_value_flow(node, semantic_context.symbol_id(&node.id), flows)
                else {
                    continue;
                };
                for context in flow.template_contexts() {
                    let Some(origin) = &context.origin else {
                        continue;
                    };
                    let Some((render_module, render_symbol)) =
                        rules.syntax_rules.iter().find_map(|rule| {
                            if rule.pack != origin.pack || rule.id != origin.rule {
                                return None;
                            }
                            let languages::manifests::syntax::SyntaxOperation::RenderContext {
                                module,
                                export,
                                package,
                                ..
                            } = &rule.operation
                            else {
                                return None;
                            };
                            if !rule.activation_family().is_some_and(|family| {
                                dependencies.declares_for_path(family, path, package)
                            }) {
                                return None;
                            }
                            Some((module.as_str(), export.as_str()))
                        })
                    else {
                        continue;
                    };
                    if !semantic_context.exact_external_import(
                        import_owner,
                        &context.import_alias,
                        render_module,
                        render_symbol,
                        context.position,
                    ) {
                        continue;
                    }
                    let Some(target) = context.target_path.as_deref() else {
                        continue;
                    };
                    if languages::workspace_path(target).0 != workspace
                        || dependencies
                            .nearest_manifest_scope_for_path(LanguageFamily("python"), target)
                            != Some(scope)
                    {
                        continue;
                    }
                    let Some((&target_path, _)) = modules.get_key_value(target) else {
                        continue;
                    };
                    let index = providers.len();
                    providers.push(RenderProvider {
                        source: node,
                        context: context.clone(),
                    });
                    Self::record_transitive_render_targets(
                        (path, target_path, index, workspace, scope),
                        &modules,
                        &includes,
                        dependencies,
                        &mut by_template,
                        &mut by_provider,
                    );
                }
            }
        }
        Self {
            providers,
            by_template,
            by_provider,
        }
    }

    pub(in crate::linker) fn providers(
        &self,
        path: &str,
    ) -> impl Iterator<Item = &RenderProvider<'a>> {
        self.by_template
            .get(path)
            .into_iter()
            .flatten()
            .map(|index| &self.providers[*index])
    }

    pub(in crate::linker) fn targets(
        &self,
        path: &str,
    ) -> impl Iterator<Item = (&RenderProvider<'a>, &'a Node)> {
        self.by_provider
            .get(path)
            .into_iter()
            .flatten()
            .map(|(index, module)| (&self.providers[*index], *module))
    }
}

fn registration<'a, 'm>(
    decorator: &'a str,
    default_name: &'a str,
    members: &'m std::collections::BTreeMap<String, String>,
    name_keyword: &str,
) -> Option<(&'a str, &'m str, &'a str)> {
    let declaration = decorator.trim().strip_prefix('@')?;
    let (callee, arguments) = if let Some((callee, arguments)) = declaration.split_once('(') {
        (callee.trim(), Some(arguments.strip_suffix(')')?.trim()))
    } else {
        (declaration.trim(), None)
    };
    let (receiver, method) = callee.rsplit_once('.')?;
    if !languages::html::valid_template_name(receiver) {
        return None;
    }
    let kind = members.get(method)?.as_str();
    let name = match arguments {
        None | Some("") => default_name,
        Some(arguments) => {
            let (key, literal) = arguments.split_once('=')?;
            if key.trim() != name_keyword {
                return None;
            }
            let literal = literal.trim();
            let quote = literal.chars().next()?;
            if !matches!(quote, '\'' | '"') || !literal.ends_with(quote) || literal.len() < 2 {
                return None;
            }
            &literal[1..literal.len() - 1]
        }
    };
    languages::html::valid_template_name(name).then_some((receiver, kind, name))
}

fn verified_library_instance(
    facts: &Facts,
    module: &Node,
    flow: &ValueFlowFacts,
    receiver: &str,
    function_line: usize,
    provider_module: &str,
    constructor: &str,
) -> bool {
    let Some(latest) = flow
        .bindings
        .iter()
        .filter(|binding| binding.name == receiver && binding.position.line < function_line)
        .max_by_key(|binding| binding.position)
    else {
        return false;
    };
    if latest.conditional {
        return false;
    }
    let ValueExpr::Call { callee, .. } = &latest.value else {
        return false;
    };
    let (alias, expected_symbol, expected_module) = match callee.rsplit_once('.') {
        Some((alias, member))
            if member == constructor && languages::html::valid_template_name(alias) =>
        {
            let Some((parent_module, module_export)) = provider_module.rsplit_once('.') else {
                return false;
            };
            (alias, module_export, parent_module)
        }
        None if callee == constructor => (constructor, constructor, provider_module),
        _ => return false,
    };
    let mut imports = facts.references.iter().filter(|reference| {
        reference.kind == "imports"
            && reference.source == module.id
            && reference.alias.as_deref() == Some(alias)
            && reference.line < latest.position.line
    });
    let Some(import) = imports.next() else {
        return false;
    };
    if imports.next().is_some()
        || import.expression != expected_symbol
        || import.module.as_deref() != Some(expected_module)
    {
        return false;
    }
    !flow.bindings.iter().any(|binding| {
        binding.name == alias
            && binding.position.line > import.line
            && binding.position.line < latest.position.line
    })
}

pub(super) fn match_template_symbol<'a>(
    expression: &str,
    template_rules: &[&'a languages::manifests::syntax::SyntaxRule],
    mut match_library: impl FnMut(&str, &str) -> bool,
) -> Option<&'a languages::manifests::syntax::SyntaxRule> {
    let tag_name = expression.strip_prefix("template.tag.");
    let filter_name = expression.strip_prefix("template.filter.");
    for rule in template_rules {
        let languages::manifests::syntax::SyntaxOperation::TemplateSymbols {
            tags,
            filters,
            library_routes,
            ..
        } = &rule.operation
        else {
            continue;
        };
        if let Some((tag, library)) = tag_name.and_then(|tag| {
            library_routes
                .get(tag)
                .map(|library| (tag, library.as_str()))
        }) {
            if match_library(tag, library) {
                return Some(rule);
            }
        }
        if tag_name.is_some_and(|name| tags.iter().any(|tag| tag == name))
            || filter_name.is_some_and(|name| filters.iter().any(|filter| filter == name))
        {
            return Some(rule);
        }
    }
    None
}

pub(crate) fn builtin_evidence(
    expression: &str,
    template_rules: &[&languages::manifests::syntax::SyntaxRule],
) -> String {
    if let Some(rule) = match_template_symbol(expression, template_rules, |_, _| true) {
        let label = rule.display.builtin.as_deref();
        return format!(
            "{}; rule:{}:{}",
            label.unwrap_or("builtin:configured_template"),
            rule.pack,
            rule.id
        );
    }
    "builtin:shared_template".to_owned()
}


pub(in crate::linker) fn resolve_reference(
    input: &ResolutionInput<'_>,
    templates: &TemplateRegistry<'_>,
    path: &str,
    loads: &[&Reference],
    registry_rules: &[&languages::manifests::syntax::SyntaxRule],
    reference: &Reference,
    graph: &mut Graph,
) -> bool {
    if reference.kind != "references"
        || !input
            .profiles
            .get(path)
            .is_some_and(|profile| profile.id() == "html")
    {
        return false;
    }
    if reference.expression.starts_with("template.variable.")
        || reference.expression.starts_with("template.macro.")
    {
        return true;
    }
    let Some((kind, name)) = reference
        .expression
        .strip_prefix("template.tag.")
        .map(|name| ("tag", name))
        .or_else(|| {
            reference
                .expression
                .strip_prefix("template.filter.")
                .map(|name| ("filter", name))
        })
    else {
        return false;
    };
    if let Some(owner) = input
        .interner
        .lookup_node(input.by_id, reference.source.as_str())
    {
        if let Some((symbol, provenance)) =
            registry_symbol(owner, kind, name, registry_rules, input.semantic_context)
        {
            let (status, evidence) = match symbol {
                value_flow::Symbol::Callable(target) => {
                    graph.push_edge(
                        path,
                        reference.source.clone(),
                        target.id.clone(),
                        crate::models::tags::EdgeKind::References,
                        reference.line,
                        format!(
                            "configured template {kind} from {}{provenance}",
                            target.path
                        ),
                        crate::models::tags::EdgeConfidence::Exact,
                    );
                    (
                        "resolved",
                        format!(
                            "configured template {kind} registered in {}{provenance}",
                            target.path
                        ),
                    )
                }
                value_flow::Symbol::Ambiguous => (
                    "ambiguous",
                    format!("configured template {kind} {name} has multiple providers{provenance}"),
                ),
                _ => (
                    "unresolved",
                    format!(
                        "configured template {kind} {name} has uncertain registration{provenance}"
                    ),
                ),
            };
            graph.reference_coverage(path, reference, status, evidence.into());
            return true;
        }
    }
    match templates.symbol(
        path,
        loads,
        reference,
        kind,
        name,
        input.dependency_registry,
    ) {
        Match::Unique {
            module,
            symbol,
            origin,
        } => {
            let provenance = origin
                .map(|origin| format!("; rule:{}:{}", origin.pack, origin.rule))
                .unwrap_or_default();
            graph.push_edge(
                path,
                reference.source.clone(),
                symbol.id.clone(),
                crate::models::tags::EdgeKind::References,
                reference.line,
                format!(
                    "template {kind} from {} via {}{provenance}",
                    symbol.path, module.path
                ),
                crate::models::tags::EdgeConfidence::Exact,
            );
            graph.reference_coverage(
                path,
                reference,
                "resolved",
                format!("template {kind} registered in {}{provenance}", symbol.path).into(),
            );
            true
        }
        Match::Ambiguous => {
            graph.reference_coverage(
                path,
                reference,
                "ambiguous",
                format!("template {kind} {name} has multiple registered providers").into(),
            );
            true
        }
        Match::Missing => false,
    }
}
