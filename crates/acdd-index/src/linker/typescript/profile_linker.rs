use crate::interner::LinkerInterner;
use crate::languages::{
    dependency_registry,
    manifests,
    profile::LanguageProfile,
    types::{self as languages, ImportPath, LanguageFamily},
    typescript::{
        commonjs::{normalize_ts_import, ts_module_name},
        dom::typed_dom_receiver,
    },
};
use crate::linker::traits::{
    insert_package_export, package_export, resolve_default_import, BuiltinPropertyLookup,
    ImportContext, ImportNormalizationContext, ImportResolution, LanguageLinker,
    ModulesByNamespace, PackageExports,
};
use crate::linker::types::NodeBindingIndex;
use crate::linker::value_flow::types::TypeTarget;
use crate::models::{tags::EdgeKind, Facts, Node};
use hashbrown::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) struct TypeScriptLinker;
pub(crate) static TYPESCRIPT_LINKER: TypeScriptLinker = TypeScriptLinker;

fn javascript(file: &Facts) -> bool {
    file.nodes
        .iter()
        .any(|node| matches!(node.language.as_str(), "typescript" | "javascript" | "vue"))
}

fn provider<'a>(
    path: &str,
    module: &str,
    modules: &ModulesByNamespace<'a>,
    root: Option<&Path>,
) -> Option<&'a Node> {
    let _ = root;
    let namespace = normalize_ts_import(path, module)?;
    let candidates = modules
        .get(&LanguageFamily("javascript"))?
        .get(&namespace.namespace)?;
    let owner = languages::workspace_path(path).0;
    let local: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|node| languages::workspace_path(&node.path).0 == owner)
        .collect();
    let candidates = if local.is_empty() && !namespace.relative {
        candidates.as_slice()
    } else {
        &local
    };
    match candidates {
        [node] => Some(*node),
        _ => None,
    }
}

fn target<'a>(
    exports: &PackageExports<'a>,
    interner: &LinkerInterner<'_>,
    module: &Node,
    name: &str,
) -> Option<&'a Node> {
    package_export(exports, interner, module.path.as_str(), name).flatten()
}

fn component_module_paths(name: &str, root: &str, extension: &str) -> Vec<String> {
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Vec::new();
    }
    let mut boundaries = vec![0];
    boundaries.extend(name.char_indices().filter_map(|(index, character)| {
        (index > 0 && character.is_ascii_uppercase()).then_some(index)
    }));
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .into_iter()
        .map(|boundary| {
            let mut path = String::with_capacity(root.len() + name.len() + 8);
            path.push_str(root);
            path.push('/');
            for (index, character) in name[..boundary].char_indices() {
                if index > 0 && character.is_ascii_uppercase() {
                    path.push('/');
                }
                path.push(character.to_ascii_lowercase());
            }
            if boundary > 0 {
                path.push('/');
            }
            path.push_str(&name[boundary..]);
            path.push_str(extension);
            path
        })
        .collect()
}

fn configured_component_modules<'a>(
    context: &ImportContext<'_, 'a, '_>,
    name: &str,
) -> Vec<&'a Node> {
    let rules = context.dependencies.language_context_for_path(context.path);
    let Some(scope) = context
        .dependencies
        .nearest_manifest_scope_for_path(LanguageFamily("javascript"), context.path)
    else {
        return Vec::new();
    };
    let mut modules = Vec::new();
    for rule in &rules.syntax_rules {
        let manifests::syntax::SyntaxOperation::ComponentEnvironment {
            package,
            directory: root,
            extension,
            virtual_module,
            ..
        } = &rule.operation
        else {
            continue;
        };
        if context.reference.module.as_deref() != Some(virtual_module)
            || !rule.activation_family().is_some_and(|family| {
                context
                    .dependencies
                    .declares_for_path(family, context.path, package)
            })
        {
            continue;
        }
        let root = if scope.is_empty() {
            root.to_owned()
        } else {
            format!("{scope}/{root}")
        };
        for relative in component_module_paths(name, &root, extension) {
            modules.extend(
                context
                    .by_module
                    .get(relative.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| node.kind == "module" && node.language == "vue"),
            );
        }
    }
    modules.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    modules.dedup_by(|left, right| left.id == right.id);
    modules
}

impl LanguageLinker for TypeScriptLinker {
    fn construct_value<'a>(
        &self,
        owner: &'a Node,
        callee: &str,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::javascript::value_flow::construct(owner, callee)
    }

    fn evaluate_call_fallback<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        callee: &str,
        site: Option<&crate::semantic::CallSiteEvidence>,
    ) -> Option<TypeTarget<'a>> {
        let target = crate::linker::javascript::value_flow::evaluate_call(
            eval.index,
            eval.owner,
            callee,
            eval.at,
            eval.resolver,
            eval.trace,
        )?;
        Some(
            eval.resolver
                .resolve_call_result(eval.owner, callee, site, eval.at, Some(target.clone()))
                .unwrap_or(target),
        )
    }

    fn evaluate_member_call_fallback<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        receiver: &TypeTarget<'a>,
        callee: &str,
    ) -> Option<TypeTarget<'a>> {
        crate::linker::javascript::value_flow::platform_member_return(
            eval.owner, receiver, callee,
        )
    }

    fn needs_family_module_stems(&self) -> bool {
        true
    }

    fn normalize_file_imports<'a>(
        &self,
        profile: &dyn LanguageProfile,
        path: &'a str,
        facts: &'a Facts,
        ctx: &ImportNormalizationContext<'_, 'a>,
        out: &mut HashMap<(&'a str, &'a str), Option<ImportPath>>,
    ) {
        let empty = std::collections::HashSet::new();
        let indexed_modules = ctx
            .family_module_stems
            .get(&profile.family())
            .unwrap_or(&empty);
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            let module = reference.module.as_deref().unwrap_or(&reference.expression);
            out.entry((path, module)).or_insert_with(|| {
                ctx.dependencies
                    .normalize_javascript_import(profile.id(), path, module, indexed_modules)
                    .or_else(|| profile.normalize_import_with_root(ctx.root, path, module))
            });
        }
    }

    fn resolve_builtin_property<'a>(
        &self,
        path: &str,
        receiver_name: &str,
        field: &str,
        semantic_context: &dyn BuiltinPropertyLookup,
    ) -> Option<TypeTarget<'a>> {
        semantic_context
            .property_type_for_path(path, receiver_name, field)
            .map(TypeTarget::Builtin)
    }

    fn builtin_member_origin(
        &self,
        receiver: &TypeTarget<'_>,
        default_origin: Option<&'static str>,
        _computed_call: bool,
    ) -> Option<&'static str> {
        match receiver {
            TypeTarget::Builtin(name)
                if name == "Web.Response"
                    || typed_dom_receiver(name).is_some() =>
            {
                Some("builtin:web_api")
            }
            _ => default_origin,
        }
    }

    fn tracks_external_aliases(&self) -> bool {
        true
    }
    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        let mut result = resolve_default_import(context);
        {
            let imported_name = context
                .reference
                .alias
                .as_deref()
                .unwrap_or(context.reference.expression.as_str());
            let providers = configured_component_modules(context, imported_name);
            if !providers.is_empty() {
                context.modules.clear();
                context.candidates.clear();
                context.modules.extend(providers.iter().copied());
                context.candidates.extend(providers);
                return result;
            }
        }
        let typescript_source = context.path.rsplit_once('.').is_some_and(|(_, extension)| {
            matches!(extension, "ts" | "tsx" | "mts" | "cts" | "vue")
        });
        if !typescript_source {
            context
                .candidates
                .retain(|node| !matches!(node.kind.as_str(), "type" | "interface"));
        }
        if let (Some([module]), Some(name)) = (
            Some(context.modules.as_slice()),
            context.tail().map(str::to_owned),
        ) {
            if !name.is_empty() {
                let exported = target(context.package_exports, context.interner, module, &name);
                let direct_type_export = typescript_source
                    && exported.is_none()
                    && matches!(context.candidates.as_slice(), [node] if matches!(node.kind.as_str(), "type" | "interface")
                    && module.details["exports"].as_array().is_some_and(|bindings| bindings.iter().any(|binding| {
                        binding["type_only"] == true
                            && binding["name"].as_str() == Some(name.as_str())
                            && binding["local"].as_str() == Some(node.name.as_str())
                            && binding["module"].is_null()
                    })));
                if (name != "default" || exported.is_some()) && !direct_type_export {
                    context.candidates.clear();
                }
                if let Some(node) = exported {
                    result.reexport_symbol = node.path != module.path || node.name != name;
                    context.candidates.push(node);
                }
            }
        }
        result
    }
    fn resolve_imported_member<'a>(
        &self,
        interner: &LinkerInterner<'a>,
        module: &'a Node,
        member: &str,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        exports: &PackageExports<'a>,
        _lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        target(exports, interner, module, member)
            .into_iter()
            .collect()
    }
    fn package_exports<'a>(
        &self,
        interner: &mut LinkerInterner<'a>,
        _node_rebindings: &NodeBindingIndex,
        all: &'a dyn crate::models::FactsLookup,
        modules: &ModulesByNamespace<'a>,
        by_module: &HashMap<&'a str, Vec<&'a Node>>,
        root: Option<&Path>,
    ) -> PackageExports<'a> {
        package_exports(interner, all.values_facts(), modules, by_module, root)
    }
    fn needs_reference_identity(&self, _path: &str, facts: &Facts) -> bool {
        javascript(facts)
    }
    fn reference_identity_changed(&self, _path: &str, old: &Facts, new: &Facts) -> bool {
        let imports = |facts: &Facts| {
            facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
                .map(|reference| {
                    (
                        reference.expression.clone(),
                        reference.alias.clone(),
                        reference.module.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        imports(old) != imports(new)
    }
    fn required_full_facts(
        &self,
        facts: &dyn crate::models::FactsLookup,
        _affected: &BTreeSet<String>,
        catalog: &[(&str, &str)],
    ) -> BTreeSet<String> {
        required_full_facts(facts, _affected, catalog)
    }
}

fn type_position_kind(kind: &str) -> bool {
    matches!(kind, "type" | "interface" | "class" | "enum")
}

fn propagate_package_export_iterations<'a>(
    interner: &mut LinkerInterner<'a>,
    exports: &mut PackageExports<'a>,
    seen: &HashSet<(&'a str, &'a str)>,
    stars: &[(&'a str, &'a Node)],
    pending: &[(&'a str, &'a str, &'a Node, &'a str, bool)],
) {
    loop {
        let mut additions = HashMap::<(&str, &str), Option<&Node>>::new();
        for (owner, provider) in stars {
            let Some(provider_path) = interner.lookup_path(&provider.path) else {
                continue;
            };
            for (&name_id, &node) in exports.get(&provider_path).into_iter().flatten() {
                let name = interner.resolve_symbol(name_id);
                if name == "default" || seen.contains(&(*owner, name)) {
                    continue;
                }
                let entry = additions.entry((*owner, name)).or_insert(node);
                if entry.map(|node| node.id.as_str()) != node.map(|node| node.id.as_str()) {
                    *entry = None;
                }
            }
        }
        let mut changed = false;
        for ((owner, name), node) in additions {
            let (Some(owner_id), Some(name_id)) =
                (interner.lookup_path(owner), interner.lookup_symbol(name))
            else {
                continue;
            };
            let existing = exports.entry(owner_id).or_default();
            if !existing.contains_key(&name_id)
                || existing
                    .get(&name_id)
                    .copied()
                    .flatten()
                    .map(|node| node.id.as_str())
                    != node.map(|node| node.id.as_str())
            {
                existing.insert(name_id, node);
                changed = true;
            }
        }
        for (owner, name, provider, member, type_only) in pending {
            let mut node = target(exports, interner, provider, member);
            if *type_only && node.is_some_and(|n| !type_position_kind(n.kind.as_str())) {
                node = None;
            }
            let (Some(owner_id), Some(name_id)) =
                (interner.lookup_path(owner), interner.lookup_symbol(name))
            else {
                continue;
            };
            let existing = exports.get_mut(&owner_id).expect("export owner exists");
            if existing
                .get(&name_id)
                .copied()
                .flatten()
                .map(|node| node.id.as_str())
                != node.map(|node| node.id.as_str())
            {
                existing.insert(name_id, node);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

struct PackageExportBuildState<'a> {
    interner: LinkerInterner<'a>,
    exports: PackageExports<'a>,
    pending: Vec<(&'a str, &'a str, &'a Node, &'a str, bool)>,
    seen: HashSet<(&'a str, &'a str)>,
    stars: Vec<(&'a str, &'a Node)>,
}

fn process_export_binding<'a>(
    state: &mut PackageExportBuildState<'a>,
    file: &'a Facts,
    module: &'a Node,
    binding: &'a serde_json::Value,
    by_module: &HashMap<&'a str, Vec<&'a Node>>,
    resolve_provider: &mut impl FnMut(&'a str, &'a str) -> Option<&'a Node>,
) {
    let path = module.path.as_str();
    if binding["star"] == true {
        if let Some(provider) = binding["module"]
            .as_str()
            .and_then(|source| resolve_provider(path, source))
        {
            state.stars.push((path, provider));
        }
        return;
    }
    let Some(name) = binding["name"].as_str() else {
        return;
    };
    let local = binding["local"].as_str();
    if local.is_some_and(|local| module.rebinding_names().any(|candidate| candidate == local)) {
        return;
    }
    if !state.seen.insert((path, name)) {
        insert_package_export(&mut state.exports, &mut state.interner, path, name, None);
        state
            .pending
            .retain(|(owner, alias, _, _, _)| *owner != path || *alias != name);
        return;
    }
    let type_only = binding["type_only"] == true;
    let mut source = binding["module"].as_str();
    let mut member = local;
    if source.is_none() {
        if let Some(reference) = file.references.iter().find(|reference| {
            reference.kind == "imports"
                && reference.source == module.id
                && reference.alias.as_deref() == local
        }) {
            source = reference.module.as_deref();
            member = if reference.expression == "*" || source == Some(reference.expression.as_str())
            {
                None
            } else {
                Some(reference.expression.as_str())
            };
        }
    }
    let owner_path = state.interner.intern_path(path);
    let name_id = state.interner.intern_symbol(name);
    let owner = state.exports.entry(owner_path).or_default();
    owner.insert(name_id, None);
    if let Some(source) = source {
        if let Some(provider) = resolve_provider(path, source) {
            if let Some(member) = member {
                state.pending.push((path, name, provider, member, type_only));
            } else if !type_only || type_position_kind(provider.kind.as_str()) {
                owner.insert(name_id, Some(provider));
            }
        }
    } else if let Some(local) = local {
        let qualname = format!("{}.{}", module.qualname, local);
        let mut candidates: Vec<&Node> = by_module
            .get(path)
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| node.qualname == qualname)
            .collect();
        if type_only {
            candidates.retain(|node| type_position_kind(node.kind.as_str()));
        } else {
            candidates.retain(|node| !matches!(node.kind.as_str(), "type" | "interface"));
        }
        if let [node] = candidates.as_slice() {
            owner.insert(name_id, Some(node));
        }
    }
}

fn package_exports<'a>(
    interner: &mut LinkerInterner<'a>,
    all: impl IntoIterator<Item = &'a Facts>,
    modules: &ModulesByNamespace<'a>,
    by_module: &HashMap<&'a str, Vec<&'a Node>>,
    root: Option<&Path>,
) -> PackageExports<'a> {
    let mut state = PackageExportBuildState {
        interner: std::mem::take(interner),
        exports: PackageExports::new(),
        pending: Vec::new(),
        seen: HashSet::new(),
        stars: Vec::new(),
    };
    let mut providers = HashMap::new();
    let mut resolve_provider = |path: &'a str, source: &'a str| {
        *providers
            .entry((path, source))
            .or_insert_with(|| provider(path, source, modules, root))
    };
    for file in all.into_iter().filter(|file| javascript(file)) {
        let Some(module) = file.nodes.iter().find(|node| node.kind == "module") else {
            continue;
        };
        let Some(bindings) = module.details["exports"].as_array() else {
            continue;
        };
        for binding in bindings {
            process_export_binding(
                &mut state,
                file,
                module,
                binding,
                by_module,
                &mut resolve_provider,
            );
        }
    }
    propagate_package_export_iterations(
        &mut state.interner,
        &mut state.exports,
        &state.seen,
        &state.stars,
        &state.pending,
    );
    *interner = state.interner;
    state.exports
}

fn collect_component_export_paths<'a>(
    catalog: &[(&str, &str)],
    scope: &str,
    exports: impl Iterator<Item = (&'a str, &'a str)>,
    dependencies: &dependency_registry::DependencyRegistry,
    required: &mut BTreeSet<String>,
) {
    for (directory, extension) in exports {
        for (candidate, language) in catalog {
            if *language == "typescript"
                && dependency_registry::component_export_matches_provider(
                    candidate,
                    scope,
                    directory,
                    extension,
                    dependencies,
                )
            {
                required.insert((*candidate).to_owned());
            }
        }
    }
}

fn collect_rule_configuration_exports(
    rule: &manifests::syntax::SyntaxRule,
    (path, scope, config_path): (&str, &str, &str),
    config: &crate::typed_facts::TypedFacts,
    catalog: &[(&str, &str)],
    dependencies: &dependency_registry::DependencyRegistry,
    selected: &mut BTreeSet<(String, String, String, String)>,
    required: &mut BTreeSet<String>,
) {
    let manifests::syntax::SyntaxOperation::ComponentEnvironment {
        ref modules,
        ref callable_imports,
        ..
    } = rule.operation
    else {
        return;
    };
    let configurations = config
        .facts
        .nodes
        .iter()
        .find(|node| node.kind == "module")
        .and_then(|node| config.flows.get(node))
        .map(|flow| flow.component_configurations());
    for configuration in configurations
        .into_iter()
        .flat_map(|items| manifests::semantic::matching_component_configurations(items, rule))
    {
        if let Some(descriptor) = callable_imports
            .as_ref()
            .filter(|_| configuration.callable_imports.is_some())
        {
            if selected.insert((
                config_path.to_owned(),
                rule.pack.clone(),
                rule.id.clone(),
                String::new(),
            )) {
                collect_component_export_paths(
                    catalog,
                    scope,
                    descriptor
                        .exports
                        .iter()
                        .map(|e| (e.directory.as_str(), e.extension.as_str())),
                    dependencies,
                    required,
                );
            }
        }
        for (name, addition) in dependency_registry::active_component_module_additions(
            configuration,
            modules,
            rule,
            path,
            dependencies,
        ) {
            if selected.insert((
                config_path.to_owned(),
                rule.pack.clone(),
                rule.id.clone(),
                name.to_owned(),
            )) {
                collect_component_export_paths(
                    catalog,
                    scope,
                    addition
                        .exports
                        .iter()
                        .map(|e| (e.directory.as_str(), e.extension.as_str())),
                    dependencies,
                    required,
                );
            }
        }
    }
}

pub(crate) fn required_configuration_facts(
    facts: &BTreeMap<String, crate::typed_facts::TypedFacts>,
    catalog: &[(&str, &str)],
    dependencies: &dependency_registry::DependencyRegistry,
) -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let mut selected = BTreeSet::new();
    for (path, _) in facts.iter().filter(|(_, file)| javascript(&file.facts)) {
        let Some(scope) =
            dependencies.nearest_manifest_scope_for_path(LanguageFamily("javascript"), path)
        else {
            continue;
        };
        for rule in &dependencies.language_context_for_path(path).syntax_rules {
            let manifests::syntax::SyntaxOperation::ComponentEnvironment {
                ref package,
                ref config_file,
                ..
            } = rule.operation
            else {
                continue;
            };
            let Some(config_path) =
                dependency_registry::component_environment_config_path(
                    rule,
                    path,
                    scope,
                    package,
                    config_file,
                    dependencies,
                ) else {
                continue;
            };
            if catalog
                .iter()
                .any(|(candidate, _)| *candidate == config_path)
            {
                required.insert(config_path.clone());
            }
            if let Some(config) = facts.get(&config_path) {
                collect_rule_configuration_exports(
                    rule,
                    (path, scope, &config_path),
                    config,
                    catalog,
                    dependencies,
                    &mut selected,
                    &mut required,
                );
            }
        }
    }
    required
}

fn required_full_facts(
    facts: &dyn crate::models::FactsLookup,
    _affected: &BTreeSet<String>,
    catalog: &[(&str, &str)],
) -> BTreeSet<String> {
    let mut requested = HashSet::new();
    for (path, file) in facts.iter_facts().filter(|(_, file)| javascript(file)) {
        let imports = file
            .references
            .iter()
            .filter(|reference| reference.kind == "imports")
            .filter_map(|reference| reference.module.as_deref());
        let exports = file
            .nodes
            .iter()
            .filter(|node| node.kind == "module")
            .flat_map(|module| module.details["exports"].as_array().into_iter().flatten())
            .filter_map(|binding| binding["module"].as_str());
        for import in imports.chain(exports) {
            if let Some(import) = normalize_ts_import(path, import) {
                requested.insert((
                    languages::workspace_path(path).0,
                    import.namespace,
                    import.relative,
                ));
            }
        }
    }
    catalog
        .iter()
        .filter_map(|(path, language)| {
            if !matches!(*language, "javascript" | "typescript" | "vue") {
                return None;
            }
            let (workspace, local) = languages::workspace_path(path);
            let names = [ts_module_name(path), ts_module_name(local)];
            requested
                .iter()
                .any(|(owner, name, relative)| {
                    (!relative || *owner == workspace) && names.contains(name)
                })
                .then(|| (*path).to_owned())
        })
        .collect()
}
