use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::{self, dependency_registry::DependencyRegistry, ImportPath, LanguageFamily, LanguageProfile};
use crate::linker::traits::{
    insert_package_export, package_export, resolve_default_import, ImportContext,
    ImportNormalizationContext, ImportResolution, LanguageLinker, ModulesByNamespace, PackageExports,
};
use crate::models::{
    tags::{EdgeKind, LanguageId},
    Facts, Node,
};
use hashbrown::{HashMap, HashSet};

const RUST_ALIAS_DEPTH: usize = 64;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct RestrictedExport {
    pub name: String,
    pub module: Option<String>,
    pub scope: String,
}

fn module_namespace(name: &str) -> &str {
    name.strip_suffix(".lib")
        .or_else(|| name.strip_suffix(".main"))
        .unwrap_or(name)
}

fn crate_namespace(path: &str) -> String {
    let (workspace, local) = languages::workspace_path(path);
    let namespace = if let Some((prefix, _)) = local.split_once("/src/") {
        format!("{}.src", prefix.replace('/', "."))
    } else {
        local.split('/').next().unwrap_or(local).to_owned()
    };
    if workspace.is_empty() {
        namespace
    } else {
        format!("{workspace}.{namespace}")
    }
}

fn within(requester: &str, scope: &str) -> bool {
    let requester = module_namespace(requester);
    requester == scope
        || requester
            .strip_prefix(scope)
            .is_some_and(|tail| tail.starts_with('.'))
}

pub(super) fn restricted_scope(signature: &str, module: &str, path: &str) -> Option<String> {
    let restriction = signature.strip_prefix("pub(")?.split_once(')')?.0.trim();
    let module = module_namespace(module);
    let crate_root = crate_namespace(path);
    let scope = match restriction {
        "crate" => crate_root,
        "self" => module.to_owned(),
        "super" => module.rsplit_once('.')?.0.to_owned(),
        restriction => {
            let mut cur = module;
            let mut rest = restriction.strip_prefix("in ")?.trim();
            loop {
                if rest == "crate" {
                    break crate_root;
                }
                if rest == "self" {
                    break cur.to_owned();
                }
                if let Some(tail) = rest.strip_prefix("crate::") {
                    break format!("{crate_root}.{}", tail.replace("::", "."));
                }
                if rest == "super" {
                    break cur.rsplit_once('.')?.0.to_owned();
                }
                if let Some(tail) = rest.strip_prefix("super::") {
                    cur = cur.rsplit_once('.')?.0;
                    rest = tail;
                    continue;
                }
                if rest.starts_with("self::") {
                    rest = rest.strip_prefix("self::")?;
                }
                break format!("{cur}.{}", rest.replace("::", "."));
            }
        }
    };
    (within(module, &scope) && within(&scope, &crate_namespace(path))).then_some(scope)
}

fn restricted_exports(module: &Node) -> Vec<RestrictedExport> {
    module
        .details
        .get("rust_restricted_exports")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

pub(crate) fn resolve_workspace_crate_namespace<'a>(
    profile_paths: impl Iterator<Item = &'a str>,
    dependencies: &DependencyRegistry,
    requester_path: &str,
    crate_name: &str,
    tail: &str,
) -> Option<(String, String)> {
    let declared = dependencies.declares_for_path(
        LanguageFamily(LanguageId::Rust.as_str()),
        requester_path,
        crate_name,
    );
    let sibling_scope = (!declared
        && dependencies.sibling_rust_library_for_path(requester_path) == Some(crate_name))
    .then(|| {
        dependencies
            .rust_package_for_path(requester_path)
            .map(|(scope, _)| scope)
    })
    .flatten();
    if !declared && sibling_scope.is_none() {
        return None;
    }
    let mut matches = profile_paths.filter_map(|candidate_path| {
        if !candidate_path.ends_with("/src/lib.rs") && candidate_path != "src/lib.rs" {
            return None;
        }
        let (pkg_scope, lib_name) = dependencies.rust_package_for_path(candidate_path)?;
        (lib_name == crate_name && (declared || sibling_scope == Some(pkg_scope))).then_some((
            pkg_scope,
            super::workspace_path(candidate_path).0,
        ))
    });
    let (pkg_scope, workspace) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let base_namespace = if pkg_scope.is_empty() {
        "src".to_owned()
    } else {
        format!("{}.src", pkg_scope.replace('/', "."))
    };
    let namespace = if tail.is_empty() || tail == "*" {
        base_namespace
    } else {
        format!("{base_namespace}.{}", tail.replace("::", "."))
    };
    Some((namespace, workspace.to_owned()))
}

fn canonical_namespace<'n>(name: &'n str, crate_root: &str) -> std::borrow::Cow<'n, str> {
    if let Some(tail) = name.strip_prefix(crate_root) {
        for entry in [".lib", ".main"] {
            if let Some(tail) = tail.strip_prefix(entry) {
                if tail.is_empty() || tail.starts_with('.') {
                    return format!("{crate_root}{tail}").into();
                }
            }
        }
    }
    name.into()
}

struct ModuleDomain {
    scope: String,
    crate_root: String,
}

#[derive(Default)]
struct ModuleVisibility {
    domains: HashMap<String, ModuleDomain>,
}

impl ModuleVisibility {
    fn build(all: &dyn crate::FactsLookup) -> Self {
        let mut domains = HashMap::new();
        for declaration in all
            .values_facts()
            .flat_map(|facts| &facts.nodes)
            .filter(|node| {
                node.language == "rust"
                    && node.kind == "module_declaration"
                    && node.details["is_public"] != true
            })
        {
            let Some((owner, _)) = declaration.qualname.rsplit_once('.') else {
                continue;
            };
            let crate_root = crate_namespace(&declaration.path);
            let owner = canonical_namespace(owner, &crate_root);
            let scope = declaration.details["signature"]
                .as_str()
                .and_then(|signature| restricted_scope(signature, &owner, &declaration.path))
                .unwrap_or_else(|| owner.into_owned());
            for namespace in [
                declaration.qualname.as_str().into(),
                canonical_namespace(&declaration.qualname, &crate_root),
            ] {
                domains.insert(
                    namespace.into_owned(),
                    ModuleDomain {
                        scope: scope.clone(),
                        crate_root: crate_root.clone(),
                    },
                );
            }
        }
        Self { domains }
    }

    fn domains<'s>(&'s self, namespace: &'s str) -> impl Iterator<Item = &'s ModuleDomain> {
        std::iter::successors(Some(namespace), |namespace| {
            namespace.rsplit_once('.').map(|(parent, _)| parent)
        })
        .filter_map(|namespace| self.domains.get(namespace))
    }

    fn visible(&self, namespace: &str, requester: &str) -> bool {
        self.domains(namespace).all(|domain| {
            within(
                &canonical_namespace(requester, &domain.crate_root),
                &domain.scope,
            )
        })
    }
}

#[derive(Clone)]
struct AliasTargets<'a> {
    nodes: Vec<&'a Node>,
    scope: Option<String>,
}

impl<'a> AliasTargets<'a> {
    fn restrict(&mut self, scope: Option<&str>) {
        let Some(scope) = scope else {
            return;
        };
        match self.scope.as_deref() {
            None => self.scope = Some(scope.to_owned()),
            Some(existing) if within(scope, existing) => self.scope = Some(scope.to_owned()),
            Some(existing) if within(existing, scope) => {}
            Some(_) => self.nodes.clear(),
        }
    }
}

fn direct_members<'a>(
    all: &'a dyn crate::FactsLookup,
    module: &'a Node,
    name: &str,
) -> Vec<&'a Node> {
    let qualified = format!("{}.{}", module.qualname, name);
    all.get_facts(module.path.as_str())
        .into_iter()
        .flat_map(|facts| facts.nodes.iter())
        .filter(|node| {
            node.qualname == qualified && !matches!(node.kind.as_str(), "module" | "impl")
        })
        .collect()
}

fn is_public_export(module: &Node, name: &str) -> bool {
    module.details["exports"].as_array().is_some_and(|exports| {
        exports
            .iter()
            .any(|export| export["name"].as_str() == Some(name))
    })
}

fn is_public_glob(module: &Node, target: &str) -> bool {
    module.details["exports"].as_array().is_some_and(|exports| {
        exports
            .iter()
            .any(|export| export["star"] == true && export["module"].as_str() == Some(target))
    })
}

pub(crate) fn modules_at_import<'a>(
    modules_by_namespace: &ModulesByNamespace<'a>,
    family: LanguageFamily,
    normalized: &ImportPath,
    workspace: &str,
    visible: impl FnOnce(&str) -> bool,
) -> Vec<(&'a Node, String)> {
    if !visible(&normalized.namespace) {
        return Vec::new();
    }
    let mut namespace = normalized.namespace.as_str();
    loop {
        let modules: Vec<_> = modules_by_namespace
            .get(&family)
            .and_then(|namespaces| namespaces.get(namespace))
            .into_iter()
            .flatten()
            .copied()
            .filter(|module| {
                let target_workspace = languages::workspace_path(&module.path).0;
                if target_workspace == workspace {
                    return true;
                }
                if target_workspace.is_empty() {
                    return false;
                }
                let target_root = format!("{}.src", target_workspace.replace('/', "."));
                normalized.namespace == target_root
                    || normalized.namespace.strip_prefix(&target_root)
                        .is_some_and(|tail| tail.starts_with('.'))
            })
            .collect();
        if !modules.is_empty() {
            let tail = normalized
                .namespace
                .strip_prefix(namespace)
                .unwrap_or_default()
                .trim_start_matches('.')
                .to_owned();
            return modules
                .into_iter()
                .map(|module| (module, tail.clone()))
                .collect();
        }
        if !normalized.symbol_path {
            return Vec::new();
        }
        let Some((parent, _)) = namespace.rsplit_once('.') else {
            return Vec::new();
        };
        namespace = parent;
    }
}

type AliasKey<'a> = (&'a str, String, bool);

fn is_descendant_module(requester: &str, owner: &Node) -> bool {
    let namespace = owner
        .qualname
        .strip_suffix(".lib")
        .or_else(|| owner.qualname.strip_suffix(".main"))
        .or_else(|| owner.qualname.strip_suffix(".mod"))
        .unwrap_or(&owner.qualname);
    requester
        .strip_prefix(namespace)
        .is_some_and(|suffix| suffix.starts_with('.'))
}

pub(crate) fn field_visible(field: &Node, receiver: &Node, requester: &str) -> bool {
    let module = receiver.qualname.rsplit_once('.').map_or("", |(module, _)| module);
    within(requester, module)
        || field.details["is_public"] == true
        || field.details["signature"].as_str()
            .and_then(|signature| restricted_scope(signature, module, &field.path))
            .is_some_and(|scope| within(requester, &scope))
}

fn is_crate_visible(node: &Node, requester: &str) -> bool {
    node.details["is_public"] == true
        || node.details["signature"]
            .as_str()
            .and_then(|signature| {
                restricted_scope(
                    signature,
                    node.qualname
                        .rsplit_once('.')
                        .map_or("", |(module, _)| module),
                    &node.path,
                )
            })
            .is_some_and(|scope| within(requester, &scope))
}

struct AliasResolver<'a, 'lookup> {
    all: &'a dyn crate::FactsLookup,
    modules_by_namespace: &'lookup ModulesByNamespace<'a>,
    normalized_imports: &'lookup HashMap<(&'a str, &'a str), Option<ImportPath>>,
    memo: HashMap<(AliasKey<'a>, usize), AliasTargets<'a>>,
    visiting: HashSet<AliasKey<'a>>,
    module_visibility: &'lookup ModuleVisibility,
}

fn merge_restricted_targets<'a>(
    mut targets: AliasTargets<'a>,
    module_qualname: &str,
    extra_scope: Option<&str>,
    scope: &mut Option<String>,
    candidates: &mut Vec<&'a Node>,
) {
    if !targets
        .scope
        .as_deref()
        .is_none_or(|s| within(module_qualname, s))
    {
        return;
    }
    targets.restrict(extra_scope);
    let mut combined = AliasTargets {
        nodes: targets.nodes,
        scope: scope.clone(),
    };
    combined.restrict(targets.scope.as_deref());
    *scope = combined.scope;
    candidates.extend(combined.nodes);
}

struct ResolvedImportStep<'a, 'b> {
    module: &'a Node,
    name: &'b str,
    public_only: bool,
    named_scope: Option<&'b str>,
    depth: usize,
    restricted: &'b [RestrictedExport],
}

impl<'a, 'lookup> AliasResolver<'a, 'lookup> {
    fn resolve_direct_alias(
        &self,
        module: &'a Node,
        name: &str,
        public_only: bool,
    ) -> Option<(Option<AliasTargets<'a>>, bool)> {
        let mut direct = direct_members(self.all, module, name);
        let module_declaration = direct.iter().any(|node| node.kind == "module_declaration");
        direct.retain(|node| node.kind != "module_declaration");
        if direct.is_empty() {
            return Some((None, module_declaration));
        }
        if public_only {
            direct.retain(|node| is_crate_visible(node, &module.qualname));
        }
        let mut result = AliasTargets {
            nodes: direct,
            scope: None,
        };
        for node in result.nodes.clone() {
            if node.details["is_public"] != true {
                let scope = node.details["signature"]
                    .as_str()
                    .and_then(|signature| {
                        restricted_scope(signature, &module.qualname, &node.path)
                    })
                    .unwrap_or_else(|| module_namespace(&module.qualname).to_owned());
                result.restrict(Some(&scope));
            }
        }
        Some((Some(result), module_declaration))
    }

    fn resolve_target_module_alias(
        &mut self,
        step: &ResolvedImportStep<'a, '_>,
        reference: &crate::models::Reference,
        target_module: &'a Node,
        tail: &str,
        scope: &mut Option<String>,
        candidates: &mut Vec<&'a Node>,
    ) -> bool {
        let glob = reference.alias.as_deref() == Some("*");
        let raw = reference.module.as_deref().unwrap_or(&reference.expression);
        let require_public = step.public_only
            || (step.module.qualname != target_module.qualname
                && !is_descendant_module(&step.module.qualname, target_module));
        if glob {
            if !matches!(tail, "" | "*") {
                return false;
            }
            if !step.public_only
                && is_descendant_module(&step.module.qualname, target_module)
                && self.visiting.iter().any(|(path, alias, _)| {
                    *path == target_module.path && alias == step.name
                })
            {
                return true;
            }
            match self.resolve(target_module, step.name, require_public, step.depth + 1) {
                Some(targets) => {
                    let export_scope = step
                        .restricted
                        .iter()
                        .find(|export| export.module.as_deref() == Some(raw))
                        .map(|export| export.scope.as_str());
                    merge_restricted_targets(
                        targets,
                        &step.module.qualname,
                        export_scope,
                        scope,
                        candidates,
                    );
                    true
                }
                None => false,
            }
        } else if tail.is_empty() {
            candidates.push(target_module);
            let mut targets = AliasTargets {
                nodes: std::mem::take(candidates),
                scope: scope.take(),
            };
            for domain in self.module_visibility.domains(&target_module.qualname) {
                targets.restrict(Some(&domain.scope));
            }
            *candidates = targets.nodes;
            *scope = targets.scope;
            true
        } else if tail.contains('.') {
            let mut direct = direct_members(self.all, target_module, tail);
            if require_public {
                direct.retain(|node| is_crate_visible(node, &step.module.qualname));
            }
            let ok = !direct.is_empty();
            candidates.extend(direct);
            ok
        } else {
            match self.resolve(target_module, tail, require_public, step.depth + 1) {
                Some(targets) if !targets.nodes.is_empty() => {
                    merge_restricted_targets(
                        targets,
                        &step.module.qualname,
                        step.named_scope,
                        scope,
                        candidates,
                    );
                    true
                }
                _ => false,
            }
        }
    }

    fn resolve(
        &mut self,
        module: &'a Node,
        name: &str,
        public_only: bool,
        depth: usize,
    ) -> Option<AliasTargets<'a>> {
        if depth >= RUST_ALIAS_DEPTH {
            return None;
        }
        let (direct_res, module_declaration) = self.resolve_direct_alias(module, name, public_only)?;
        if let Some(result) = direct_res {
            return Some(result);
        }
        let facts = self.all.get_facts(module.path.as_str())?;
        let named = facts.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.source == module.id
                && reference.alias.as_deref() == Some(name)
        });
        if module_declaration && !named {
            return None;
        }
        let restricted = restricted_exports(module);
        let named_scope = restricted
            .iter()
            .find(|export| export.name == name && export.module.is_none())
            .map(|export| export.scope.as_str());
        if public_only && named && !is_public_export(module, name) && named_scope.is_none() {
            return Some(AliasTargets {
                nodes: Vec::new(),
                scope: None,
            });
        }
        let memo_key = ((module.path.as_str(), name.to_owned(), public_only), depth);
        if let Some(candidates) = self.memo.get(&memo_key) {
            return Some(candidates.clone());
        }
        let (key, _) = memo_key;
        if !self.visiting.insert(key.clone()) {
            return None;
        }
        let mut candidates = Vec::new();
        let mut scope: Option<String> = None;
        let mut complete = true;
        let step = ResolvedImportStep {
            module,
            name,
            public_only,
            named_scope,
            depth,
            restricted: &restricted,
        };
        for reference in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && reference.source == module.id
                && reference.alias.as_deref() == Some(if named { name } else { "*" })
                && (!public_only
                    || named
                    || is_public_glob(
                        module,
                        reference.module.as_deref().unwrap_or(&reference.expression),
                    )
                    || restricted
                        .iter()
                        .any(|export| export.module.as_deref() == reference.module.as_deref()))
        }) {
            let glob = reference.alias.as_deref() == Some("*");
            let raw = reference.module.as_deref().unwrap_or(&reference.expression);
            let Some(normalized) = self
                .normalized_imports
                .get(&(module.path.as_str(), raw))
                .and_then(Option::as_ref)
            else {
                if !glob {
                    complete = false;
                }
                continue;
            };
            let workspace = languages::workspace_path(&module.path).0;
            let modules = modules_at_import(
                self.modules_by_namespace,
                LanguageFamily("rust"),
                normalized,
                workspace,
                |namespace| self.module_visibility.visible(namespace, &module.qualname),
            );
            if glob && modules.is_empty() && !normalized.relative {
                continue;
            }
            if modules.is_empty() || (glob && modules.len() != 1) {
                complete = false;
                continue;
            }
            for (target_module, tail) in modules {
                complete &= self.resolve_target_module_alias(
                    &step,
                    reference,
                    target_module,
                    &tail,
                    &mut scope,
                    &mut candidates,
                );
            }
        }
        self.visiting.remove(&key);
        if !complete {
            return None;
        }
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        candidates.dedup_by_key(|node| node.id.as_str());
        if public_only && !candidates.is_empty() {
            match self.resolve(module, name, false, depth) {
                Some(local)
                    if local.nodes.len() == candidates.len()
                        && candidates.iter().all(|candidate| {
                            local.nodes.iter().any(|node| node.id == candidate.id)
                        }) => {}
                _ => return None,
            }
        }
        let mut result = AliasTargets {
            nodes: candidates,
            scope,
        };
        if named && !is_public_export(module, name) {
            result.restrict(Some(
                named_scope.unwrap_or_else(|| module_namespace(&module.qualname)),
            ));
        }
        self.memo.insert((key, depth), result.clone());
        Some(result)
    }
}

/// Canonical named-import targets prepared once from persisted Rust facts.
/// Private aliases are admitted only to descendant modules; public aliases use
/// only the finite export projection recorded by the Rust profile.
#[derive(Default)]
pub(crate) struct RustForwarding<'a> {
    private: HashMap<&'a str, HashMap<&'a str, Vec<&'a Node>>>,
    public: HashMap<&'a str, HashMap<&'a str, Vec<&'a Node>>>,
    restricted: HashMap<&'a str, HashMap<&'a str, AliasTargets<'a>>>,
    exports: PackageExports<'a>,
    module_visibility: ModuleVisibility,
    lib_modules: Vec<&'a Node>,
}

impl<'a> RustForwarding<'a> {
    pub(crate) fn build(
        all: &'a dyn crate::FactsLookup,
        modules_by_namespace: &ModulesByNamespace<'a>,
        normalized_imports: &HashMap<(&'a str, &'a str), Option<ImportPath>>,
        interner: &mut LinkerInterner<'a>,
    ) -> Self {
        interner.intern_symbol("crate");
        interner.intern_symbol("self");
        interner.intern_symbol("super");
        let module_visibility = ModuleVisibility::build(all);
        let mut resolver = AliasResolver {
            all,
            modules_by_namespace,
            normalized_imports,
            memo: HashMap::new(),
            visiting: HashSet::new(),
            module_visibility: &module_visibility,
        };
        let mut private: HashMap<&'a str, HashMap<&'a str, Vec<&'a Node>>> = HashMap::new();
        let mut public: HashMap<&'a str, HashMap<&'a str, Vec<&'a Node>>> = HashMap::new();
        let mut exports = PackageExports::new();
        let mut restricted_bindings: HashMap<&str, HashMap<&str, AliasTargets<'a>>> =
            HashMap::new();
        let modules: Vec<_> = all
            .values_facts()
            .flat_map(|facts| {
                facts.nodes.iter().filter(|node| {
                    matches!(node.kind.as_str(), "module" | "module_declaration")
                        && node.language == "rust"
                })
            })
            .collect();
        let mut names_by_module = HashMap::<&str, HashSet<&str>>::new();
        for module in &modules {
            let Some(facts) = all.get_facts(module.path.as_str()) else {
                continue;
            };
            let names = names_by_module.entry(module.path.as_str()).or_default();
            names.extend(["std", "core"]);
            names.extend(
                facts
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.qualname
                            .rsplit_once('.')
                            .is_some_and(|(owner, _)| owner == module.qualname)
                    })
                    .map(|node| node.name.as_str()),
            );
            names.extend(
                facts
                    .references
                    .iter()
                    .filter(|reference| {
                        reference.kind == "imports" && reference.source == module.id
                    })
                    .filter_map(|reference| reference.alias.as_deref())
                    .filter(|name| *name != "*"),
            );
        }
        for _ in 0..RUST_ALIAS_DEPTH {
            let mut additions = Vec::new();
            for module in &modules {
                let Some(facts) = all.get_facts(module.path.as_str()) else {
                    continue;
                };
                for reference in
                    facts
                        .references
                        .iter()
                        .filter(|reference| {
                            reference.kind == "imports"
                                && reference.source == module.id
                                && reference.alias.as_deref() == Some("*")
                        })
                {
                    let raw = reference.module.as_deref().unwrap_or(&reference.expression);
                    let Some(normalized) = normalized_imports
                        .get(&(module.path.as_str(), raw))
                        .and_then(Option::as_ref)
                    else {
                        continue;
                    };
                    let targets = modules_at_import(
                        modules_by_namespace,
                        LanguageFamily("rust"),
                        normalized,
                        languages::workspace_path(&module.path).0,
                        |namespace| module_visibility.visible(namespace, &module.qualname),
                    );
                    if let [(target, tail)] = targets.as_slice() {
                        if matches!(tail.as_str(), "" | "*") {
                            additions.extend(
                                names_by_module
                                    .get(target.path.as_str())
                                    .into_iter()
                                    .flatten()
                                    .map(|name| (module.path.as_str(), *name)),
                            );
                        }
                    }
                }
            }
            let mut changed = false;
            for (path, name) in additions {
                changed |= names_by_module.entry(path).or_default().insert(name);
            }
            if !changed {
                break;
            }
        }

        for facts in all.values_facts() {
            let Some(module) = facts
                .nodes
                .iter()
                .find(|node| node.kind == "module" && node.language == "rust")
            else {
                continue;
            };
            for name in names_by_module
                .get(module.path.as_str())
                .into_iter()
                .flatten()
                .copied()
            {
                let private_result = resolver.resolve(module, name, false, 0);
                if !matches!(&private_result, Some(targets) if targets.nodes.is_empty()) {
                    private.entry(module.path.as_str()).or_default().insert(
                        name,
                        private_result.map_or_else(Vec::new, |targets| targets.nodes),
                    );
                }
                let public_result = resolver.resolve(module, name, true, 0);
                if !matches!(&public_result, Some(targets) if targets.nodes.is_empty()) {
                    if let Some(targets) = public_result
                        .as_ref()
                        .filter(|targets| targets.scope.is_some())
                    {
                        restricted_bindings
                            .entry(module.path.as_str())
                            .or_default()
                            .insert(name, targets.clone());
                        continue;
                    }
                    let public_targets =
                        public_result.map_or_else(Vec::new, |targets| targets.nodes);
                    insert_package_export(
                        &mut exports,
                        interner,
                        module.path.as_str(),
                        name,
                        (public_targets.len() == 1).then(|| public_targets[0]),
                    );
                    public
                        .entry(module.path.as_str())
                        .or_default()
                        .insert(name, public_targets);
                }
            }
        }

        let lib_modules = modules
            .iter()
            .copied()
            .filter(|node| {
                node.kind == "module"
                    && (node.path == "src/lib.rs" || node.path.ends_with("/src/lib.rs"))
            })
            .collect();

        Self {
            private,
            public,
            restricted: restricted_bindings,
            exports,
            module_visibility,
            lib_modules,
        }
    }

    pub(crate) fn workspace_crate_root(
        &self,
        dependencies: &DependencyRegistry,
        requester_path: &str,
        crate_name: &str,
    ) -> Option<&'a Node> {
        if crate_name == "crate" {
            let ns = crate_namespace(requester_path);
            let lib_qual = format!("{ns}.lib");
            return self
                .lib_modules
                .iter()
                .copied()
                .find(|node| node.qualname == lib_qual);
        }
        let (ns, _) = resolve_workspace_crate_namespace(
            self.lib_modules.iter().map(|node| node.path.as_str()),
            dependencies,
            requester_path,
            crate_name,
            "",
        )?;
        let lib_qual = format!("{ns}.lib");
        self.lib_modules
            .iter()
            .copied()
            .find(|node| node.qualname == lib_qual)
    }

    pub(crate) fn package_exports(&self) -> &PackageExports<'a> {
        &self.exports
    }

    pub(crate) fn module_path_visible(&self, namespace: &str, requester: &str) -> bool {
        self.module_visibility.visible(namespace, requester)
    }

    pub(crate) fn targets<'forwarding>(
        &'forwarding self,
        module: &Node,
        name: &str,
        requester: &str,
    ) -> &'forwarding [&'a Node] {
        if !self.module_path_visible(&module.qualname, requester) {
            return &[];
        }
        if let Some(targets) = self
            .restricted
            .get(module.path.as_str())
            .and_then(|bindings| bindings.get(name))
        {
            return if targets
                .scope
                .as_deref()
                .is_some_and(|scope| within(requester, scope))
            {
                &targets.nodes
            } else {
                &[]
            };
        }
        if let Some(targets) = self
            .public
            .get(module.path.as_str())
            .and_then(|aliases| aliases.get(name))
        {
            return targets;
        }
        if is_descendant_module(requester, module) {
            return self
                .private
                .get(module.path.as_str())
                .and_then(|aliases| aliases.get(name))
                .map(Vec::as_slice)
                .unwrap_or_default();
        }
        &[]
    }

    pub(crate) fn visible_bindings<'s>(
        &'s self,
        module: &Node,
        requester: &str,
    ) -> impl Iterator<Item = (&'a str, &'s [&'a Node])> + 's {
        let visible = self.module_path_visible(&module.qualname, requester);
        let bindings = if requester == module.qualname || is_descendant_module(requester, module) {
            &self.private
        } else {
            &self.public
        };
        let restricted = self.restricted.get(module.path.as_str());
        let ordinary = bindings
            .get(module.path.as_str())
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(move |(name, _)| {
                !restricted.is_some_and(|bindings| bindings.contains_key(**name))
            })
            .map(|(name, targets)| (*name, targets.as_slice()));
        let requester = requester.to_owned();
        ordinary
            .chain(
                restricted
                    .into_iter()
                    .flat_map(|bindings| bindings.iter())
                    .filter(move |(_, targets)| {
                        targets
                            .scope
                            .as_deref()
                            .is_some_and(|scope| within(&requester, scope))
                    })
                    .map(|(name, targets)| (*name, targets.nodes.as_slice())),
            )
            .filter(move |_| visible)
    }
}

fn declared_exports<'a>(
    all: impl IntoIterator<Item = &'a Facts>,
    interner: &mut LinkerInterner<'a>,
) -> PackageExports<'a> {
    let mut exports = PackageExports::new();
    for facts in all {
        let Some(module) = facts
            .nodes
            .iter()
            .find(|node| node.kind == "module" && node.language == "rust")
        else {
            continue;
        };
        let Some(bindings) = module.details["exports"].as_array() else {
            continue;
        };
        for name in bindings
            .iter()
            .filter_map(|binding| binding["name"].as_str())
        {
            insert_package_export(&mut exports, interner, module.path.as_str(), name, None);
        }
    }
    exports
}

fn relative_import(namespace: String) -> Option<ImportPath> {
    Some(ImportPath {
        namespace,
        relative: true,
        symbol_path: true,
    })
}

fn format_namespace(base: &str, tail: &str) -> String {
    if tail.is_empty() || tail == "*" {
        base.to_owned()
    } else {
        format!("{base}.{}", tail.replace("::", "."))
    }
}

fn normalize_declared_child_module(
    path: &str,
    module: &str,
    facts: &Facts,
    ctx: &ImportNormalizationContext<'_, '_>,
) -> Option<ImportPath> {
    let local_import = module.strip_prefix("self::").unwrap_or(module);
    let (first, tail) = local_import.split_once("::").unwrap_or((local_import, ""));
    let declared = facts.references.iter().any(|reference| {
        reference.kind == EdgeKind::Imports.as_str()
            && reference.alias.as_deref() == Some(first)
            && reference.module.as_deref() == Some(format!("self::{first}").as_str())
    });
    if !declared {
        return None;
    }
    let (workspace, local) = languages::workspace_path(path);
    let source = std::path::Path::new(local);
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    let integration_root = parent.file_name().is_some_and(|name| name == "tests");
    let directory = if integration_root
        || matches!(
            source.file_name().and_then(|name| name.to_str()),
            Some("lib.rs" | "main.rs" | "mod.rs")
        ) {
        parent.to_path_buf()
    } else {
        source.with_extension("")
    };
    for candidate in [
        directory.join(format!("{first}.rs")),
        directory.join(first).join("mod.rs"),
    ] {
        let local = candidate.to_string_lossy().replace('\\', "/");
        let indexed = if workspace.is_empty() {
            local.clone()
        } else {
            format!("{workspace}/{local}")
        };
        if ctx
            .profiles
            .get(indexed.as_str())
            .is_some_and(|profile| profile.id() == LanguageId::Rust.as_str())
        {
            let mut namespace = languages::rust::RUST.module_name(&local);
            if !tail.is_empty() && tail != "*" {
                namespace.push('.');
                namespace.push_str(&tail.replace("::", "."));
            }
            return relative_import(namespace);
        }
    }
    None
}

fn normalize_single_rust_import(
    profile: &dyn LanguageProfile,
    path: &str,
    module: &str,
    facts: &Facts,
    ctx: &ImportNormalizationContext<'_, '_>,
    root_rust_crate: Option<&str>,
) -> Option<ImportPath> {
    if let Some(child_import) = normalize_declared_child_module(path, module, facts, ctx) {
        return Some(child_import);
    }
    let (first, tail) = module.split_once("::").unwrap_or((module, ""));
    if let Some((pkg_scope, own_lib)) = ctx.dependencies.sibling_rust_package_library_for_path(path)
    {
        if own_lib == first {
            let lib_path = if pkg_scope.is_empty() {
                std::borrow::Cow::Borrowed("src/lib.rs")
            } else {
                std::borrow::Cow::Owned(format!("{pkg_scope}/src/lib.rs"))
            };
            if ctx.profiles.contains_key(lib_path.as_ref()) {
                let base_namespace = if pkg_scope.is_empty() {
                    "src".to_owned()
                } else {
                    format!("{}.src", pkg_scope.replace('/', "."))
                };
                return relative_import(format_namespace(&base_namespace, tail));
            }
        }
    }
    let mut targets = ctx
        .rust_library_modules
        .iter()
        .filter(|(_, (library, _))| *library == first);
    if let Some((target_workspace, (library, _))) = targets.next() {
        if targets.next().is_some() {
            return None;
        }
        if !ctx
            .dependencies
            .declares_for_path(LanguageFamily(LanguageId::Rust.as_str()), path, first)
            && (ctx.dependencies.sibling_rust_library_for_path(path) != Some(*library)
                || *target_workspace != super::workspace_path(path).0)
        {
            return None;
        }
        let base_namespace = if target_workspace.is_empty() {
            "src".to_owned()
        } else {
            format!("{}.src", target_workspace.replace('/', "."))
        };
        return relative_import(format_namespace(&base_namespace, tail));
    }
    if let Some((namespace, _)) = resolve_workspace_crate_namespace(
        ctx.profiles.keys().copied(),
        ctx.dependencies,
        path,
        first,
        tail,
    ) {
        return relative_import(namespace);
    }
    if ctx
        .dependencies
        .rust_package_for_path(path)
        .is_some_and(|(_, own_lib)| own_lib == first)
        && !ctx.dependencies.is_rust_sibling_target_path(path)
        && !ctx.dependencies.declares_for_path(
            LanguageFamily(LanguageId::Rust.as_str()),
            path,
            first,
        )
    {
        return None;
    }
    if let Some(normalized) =
        root_rust_crate.and_then(|name| languages::rust::RUST.normalize_root_crate_import(name, module))
    {
        return Some(normalized);
    }
    profile.normalize_import_with_root(ctx.root, path, module)
}

pub(crate) struct RustLinker;
pub(crate) static RUST_LINKER: RustLinker = RustLinker;
impl LanguageLinker for RustLinker {
    fn normalize_file_imports<'a>(
        &self,
        profile: &dyn LanguageProfile,
        path: &'a str,
        facts: &'a Facts,
        ctx: &ImportNormalizationContext<'_, 'a>,
        out: &mut HashMap<(&'a str, &'a str), Option<ImportPath>>,
    ) {
        let root_rust_crate = ctx
            .dependencies
            .root_rust_crate_for_path(path)
            .filter(|_| {
                let workspace = languages::workspace_path(path).0;
                if workspace.is_empty() {
                    ctx.profiles.contains_key("src/lib.rs")
                } else {
                    ctx.profiles
                        .contains_key(format!("{workspace}/src/lib.rs").as_str())
                }
            });
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports.as_str())
        {
            let module = reference.module.as_deref().unwrap_or(&reference.expression);
            out.entry((path, module)).or_insert_with(|| {
                normalize_single_rust_import(profile, path, module, facts, ctx, root_rust_crate)
            });
        }
    }

    fn module_namespaces(
        &self,
        _profile: &dyn LanguageProfile,
        node: &Node,
        dependency_registry: &DependencyRegistry,
        out: &mut Vec<String>,
    ) {
        let (workspace, local_path) = languages::workspace_path(&node.path);
        if local_path == "src/lib.rs" {
            let is_workspace_lib = dependency_registry
                .rust_library_for_path(&node.path)
                .is_some();
            if is_workspace_lib || !out.iter().any(|existing| existing == "src") {
                out.push("src".to_owned());
            }
            let _ = workspace;
        }
        if let Some(prefix) = node.path.strip_suffix("/src/lib.rs") {
            out.push(format!("{}.src", prefix.replace('/', ".")));
        }
    }

    fn needs_reference_identity(&self, _path: &str, facts: &Facts) -> bool {
        facts
            .references
            .iter()
            .any(|reference| reference.kind == EdgeKind::Imports.as_str())
    }

    fn reference_identity_changed(&self, _path: &str, old: &Facts, new: &Facts) -> bool {
        fn imports(facts: &Facts) -> HashSet<(&str, Option<&str>, Option<&str>)> {
            facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
                .map(|reference| {
                    (
                        reference.expression.as_str(),
                        reference.alias.as_deref(),
                        reference.module.as_deref(),
                    )
                })
                .collect()
        }

        imports(old) != imports(new)
    }

    fn resolve_import<'ctx, 'a, 'input>(
        &self,
        context: &mut ImportContext<'ctx, 'a, 'input>,
    ) -> ImportResolution<'a> {
        let requester = context
            .by_module
            .get(context.path)
            .into_iter()
            .flatten()
            .find(|node| node.id == context.reference.source)
            .or_else(|| {
                context
                    .by_module
                    .get(context.path)
                    .into_iter()
                    .flatten()
                    .find(|node| node.kind == "module")
            })
            .map_or("", |node| node.qualname.as_str());
        if context.rust_forwarding.is_some_and(|forwarding| {
            context.normalized.is_some_and(|normalized| {
                !forwarding.module_path_visible(&normalized.namespace, requester)
            })
        }) {
            context.modules.clear();
            context.candidates.clear();
            return ImportResolution::default();
        }
        let resolution = resolve_default_import(context);
        context.candidates.retain(|node| {
            node.kind != "impl"
                && (node.kind == "module"
                    && context.rust_forwarding.is_none_or(|forwarding| {
                        forwarding.module_path_visible(&node.qualname, requester)
                    })
                    || is_crate_visible(node, requester)
                    || node
                        .qualname
                        .rsplit_once('.')
                        .is_some_and(|(scope, _)| within(requester, module_namespace(scope))))
        });
        if !context.candidates.is_empty() {
            return resolution;
        }
        if let Some(normalized) = context.normalized {
            if let Some(nodes) = context.by_qual.get(normalized.namespace.as_str()) {
                context
                    .candidates
                    .extend(nodes.iter().copied().filter(|n| n.path == context.path));
                if !context.candidates.is_empty() {
                    return resolution;
                }
            }
            if let Some(owner) = context
                .by_module
                .get(context.path)
                .into_iter()
                .flatten()
                .find(|n| n.id == context.reference.source)
            {
                let owner_qual = format!(
                    "{}.{}",
                    owner.qualname,
                    context.reference.expression.replace("::", ".")
                );
                if let Some(nodes) = context.by_qual.get(owner_qual.as_str()) {
                    context
                        .candidates
                        .extend(nodes.iter().copied().filter(|n| n.path == context.path));
                    if !context.candidates.is_empty() {
                        if context.modules.is_empty() {
                            context.modules.push(owner);
                        }
                        return resolution;
                    }
                }
            }
        }
        let Some(name) = context.tail().filter(|name| !name.is_empty()) else {
            return resolution;
        };
        if context.modules.len() != 1 {
            return resolution;
        }
        let module = context.modules[0];
        if module.language != "rust" {
            return resolution;
        }
        let Some(forwarding) = context.rust_forwarding else {
            return resolution;
        };
        let requester = context
            .by_module
            .get(context.path)
            .into_iter()
            .flatten()
            .find(|node| node.kind == "module")
            .map_or(module.qualname.as_str(), |owner| owner.qualname.as_str());
        context
            .candidates
            .extend(forwarding.targets(module, name, requester).iter().copied());
        resolution
    }

    fn resolve_imported_member<'a>(
        &self,
        interner: &LinkerInterner<'a>,
        module: &'a Node,
        member: &str,
        by_module: &HashMap<&'a str, Vec<&'a Node>>,
        by_qual: &HashMap<&'a str, Vec<&'a Node>>,
        exports: &PackageExports<'a>,
        lookup_key: &mut String,
    ) -> Vec<&'a Node> {
        if let Some(Some(target)) = package_export(exports, interner, module.path.as_str(), member)
        {
            return vec![target];
        }
        lookup_key.clear();
        lookup_key.push_str(&module.qualname);
        lookup_key.push('.');
        lookup_key.push_str(member);
        let direct: Vec<&'a Node> = by_module
            .get(module.path.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| node.qualname == lookup_key.as_str() && node.kind != "module")
            .collect();
        if !direct.is_empty() {
            return direct;
        }
        if member.contains('.') {
            let is_visible_imported = |node: &Node| {
                node.language == "rust"
                    && !matches!(node.kind.as_str(), "module" | "impl")
                    && (node.details["is_public"] == true
                        || (crate_namespace(&module.path) == crate_namespace(&node.path)
                            && node.details["signature"]
                                .as_str()
                                .is_some_and(|sig| sig.starts_with("pub("))))
            };
            let mut targets: Vec<&'a Node> = by_qual
                .get(lookup_key.as_str())
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| is_visible_imported(node))
                .collect();
            if targets.is_empty() {
                let ns = module_namespace(&module.qualname);
                if ns != module.qualname {
                    lookup_key.clear();
                    lookup_key.push_str(ns);
                    lookup_key.push('.');
                    lookup_key.push_str(member);
                    targets.extend(
                        by_qual
                            .get(lookup_key.as_str())
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|node| is_visible_imported(node)),
                    );
                }
            }
            if !targets.is_empty() {
                return targets;
            }
        }
        let prefix = format!("{}.", module.qualname);
        let items: Vec<&'a Node> = by_module
            .get(module.path.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                node.qualname.starts_with(&prefix)
                    && matches!(node.kind.as_str(), "macro" | "function")
            })
            .collect();
        if let [single] = items.as_slice() {
            return vec![single];
        }
        items
            .into_iter()
            .filter(|node| node.name == member)
            .collect()
    }

    fn package_exports<'a>(
        &self,
        interner: &mut LinkerInterner<'a>,
        _node_rebindings: &crate::linker::NodeBindingIndex,
        all: &'a dyn crate::FactsLookup,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&std::path::Path>,
    ) -> PackageExports<'a> {
        declared_exports(all.values_facts(), interner)
    }


    fn required_full_facts(
        &self,
        facts: &dyn crate::FactsLookup,
        affected: &std::collections::BTreeSet<String>,
        catalog: &[(&str, &str)],
    ) -> std::collections::BTreeSet<String> {
        required_full_facts(facts, affected, catalog)
    }
}

#[derive(Hash)]
struct MemberLookup<'a>(&'a str, &'a str, bool);
impl hashbrown::Equivalent<(&str, &str, bool)> for MemberLookup<'_> {
    fn equivalent(&self, key: &(&str, &str, bool)) -> bool {
        self.0 == key.0 && self.1 == key.1 && self.2 == key.2
    }
}
#[derive(Hash)]
struct TraitLookup<'a>(&'a str, &'a str, bool, &'a str);
impl hashbrown::Equivalent<(&str, &str, bool, &str)> for TraitLookup<'_> {
    fn equivalent(&self, key: &(&str, &str, bool, &str)) -> bool {
        self.0 == key.0 && self.1 == key.1 && self.2 == key.2 && self.3 == key.3
    }
}
#[derive(Hash)]
struct DynamicLookup<'a>(&'a str, &'a str);
impl hashbrown::Equivalent<(&str, &str)> for DynamicLookup<'_> {
    fn equivalent(&self, key: &(&str, &str)) -> bool {
        self.0 == key.0 && self.1 == key.1
    }
}

type TraitImplMap<'a> = HashMap<(&'a str, &'a str, bool), Vec<(&'a Node, &'a Node)>>;

type BuiltinTraitLookup<'a> = (&'static str, &'a str, bool, &'a str);
type BuiltinTraitImplMap<'a> = HashMap<(&'static str, &'a str, bool), Vec<(&'a Node, &'a Node)>>;

pub(crate) struct RustMembers<'a> {
    deref_targets: HashMap<&'a str, &'a Node>,
    inherent: HashMap<(&'a str, &'a str, bool), Vec<&'a Node>>,
    trait_members: HashMap<(&'a str, &'a str, bool, &'a str), Vec<&'a Node>>,
    builtin_trait_members: HashMap<BuiltinTraitLookup<'a>, Vec<&'a Node>>,
    receivers_by_method: HashMap<&'a str, &'a Node>,
    dynamic_declarations: HashMap<(&'a str, &'a str), &'a Node>,
    dynamic_implementors: HashMap<(&'a str, &'a str), Vec<&'a Node>>,
    display_impls: HashSet<&'a str>,
}

fn builtin_receiver_key(raw: &str) -> Option<&'static str> {
    let mut name = raw.trim();
    while let Some(rest) = name.strip_prefix('&') {
        name = rest.trim_start();
        if name.starts_with('\'') {
            name = name.split_once(char::is_whitespace)?.1.trim_start();
        }
        name = name.strip_prefix("mut ").unwrap_or(name).trim_start();
    }
    if name.starts_with('[') && name.ends_with(']') {
        return Some("Slice");
    }
    let base = name
        .split_once('<')
        .map_or(name, |(b, _)| b)
        .trim()
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .trim();
    match base {
        "str" => Some("str"),
        "bool" => Some("bool"),
        "char" => Some("char"),
        "u8" => Some("u8"),
        "u16" => Some("u16"),
        "u32" => Some("u32"),
        "u64" => Some("u64"),
        "u128" => Some("u128"),
        "usize" => Some("usize"),
        "i8" => Some("i8"),
        "i16" => Some("i16"),
        "i32" => Some("i32"),
        "i64" => Some("i64"),
        "i128" => Some("i128"),
        "isize" => Some("isize"),
        "f32" => Some("f32"),
        "f64" => Some("f64"),
        "Vec" => Some("Vec"),
        "String" => Some("String"),
        "Option" => Some("Option"),
        "Result" => Some("Result"),
        "Box" => Some("Box"),
        "Arc" => Some("Arc"),
        "Rc" => Some("Rc"),
        "Cow" => Some("Cow"),
        "HashMap" => Some("HashMap"),
        "HashSet" => Some("HashSet"),
        "BTreeMap" => Some("BTreeMap"),
        "BTreeSet" => Some("BTreeSet"),
        "VecDeque" => Some("VecDeque"),
        "Path" => Some("Path"),
        "PathBuf" => Some("PathBuf"),
        _ => None,
    }
}

fn finalize_member_list(methods: &mut Vec<&Node>) {
    methods.sort_by_key(|method| &method.id);
    methods.dedup_by_key(|method| method.id.as_str());
    crate::linker::traits::disambiguate_rust_cfg_siblings(methods);
}

fn namespace(path: &str, module: &str, name: &str) -> String {
    let (_, path) = super::workspace_path(path);
    if let Some(tail) = name.strip_prefix("crate::") {
        let root = path
            .split_once("/src/")
            .map(|(prefix, _)| format!("{}.src", prefix.replace('/', ".")))
            .unwrap_or_else(|| "src".to_owned());
        format!("{root}.{}", tail.replace("::", "."))
    } else if let Some(tail) = name.strip_prefix("self::") {
        format!("{module}.{}", tail.replace("::", "."))
    } else if let Some(mut tail) = name.strip_prefix("super::") {
        let mut module = module.rsplit_once('.').map_or("", |(parent, _)| parent);
        while let Some(rest) = tail.strip_prefix("super::") {
            module = module.rsplit_once('.').map_or("", |(parent, _)| parent);
            tail = rest;
        }
        format!("{module}.{}", tail.replace("::", "."))
    } else {
        format!("{module}.{}", name.replace("::", "."))
    }
}

fn resolve_qualified_path(
    scope_path: &str,
    module_qual: &str,
    target: &str,
    dependencies: &DependencyRegistry,
    profile_paths: &[&str],
) -> String {
    if !target.starts_with("crate::")
        && !target.starts_with("self::")
        && !target.starts_with("super::")
    {
        let clean = target.trim_end_matches("::");
        let (head, tail) = clean.split_once("::").unwrap_or((clean, ""));
        if let Some((ns, _)) = resolve_workspace_crate_namespace(
            profile_paths.iter().copied(),
            dependencies,
            scope_path,
            head,
            tail,
        ) {
            return ns;
        }
    }
    let mut resolved = namespace(scope_path, module_qual, target);
    while resolved.ends_with('.') {
        resolved.pop();
    }
    resolved
}

#[allow(clippy::too_many_arguments)]
fn resolve_visible_macro<'a>(
    facts: &'a Facts,
    owner_mod: &'a Node,
    macro_raw: &str,
    by_qual: &HashMap<&str, Vec<&'a Node>>,
    modules_by_ns: &HashMap<&str, Vec<&'a Node>>,
    forwarding: &RustForwarding<'a>,
    crate_exported_macros: &HashMap<(&str, &str), Vec<&'a Node>>,
    dependencies: &DependencyRegistry,
    profile_paths: &[&str],
) -> Option<&'a Node> {
    let normalized_macro = macro_raw
        .strip_prefix("$crate::")
        .map(|tail| format!("crate::{tail}"))
        .unwrap_or_else(|| macro_raw.to_owned());
    let mut candidates: Vec<&'a Node> = Vec::new();
    let owner_crate_root = owner_mod
        .path
        .split_once("/src/")
        .map_or("", |(prefix, _)| prefix);
    let collect_forwarded = |mod_qual: &str, mac_name: &str, out: &mut Vec<&'a Node>| {
        for provider in modules_by_ns.get(mod_qual).into_iter().flatten() {
            for (name, targets) in forwarding.visible_bindings(provider, &owner_mod.qualname) {
                if name == mac_name {
                    out.extend(targets.iter().filter(|n| n.kind == "macro"));
                }
            }
        }
    };
    let collect_path = |scope_qual: &str, target_expr: &str, out: &mut Vec<&'a Node>| {
        let qual = resolve_qualified_path(
            &owner_mod.path,
            scope_qual,
            target_expr,
            dependencies,
            profile_paths,
        );
        for cand in by_qual
            .get(qual.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|n| n.kind == "macro")
        {
            if cand.path == owner_mod.path || is_crate_visible(cand, &owner_mod.qualname) {
                out.push(cand);
            }
        }
        if let Some((mod_expr, mac_name)) = target_expr.rsplit_once("::") {
            let mod_qual = resolve_qualified_path(
                &owner_mod.path,
                scope_qual,
                mod_expr,
                dependencies,
                profile_paths,
            );
            collect_forwarded(mod_qual.as_str(), mac_name, out);
            if mod_expr == "crate" {
                out.extend(
                    crate_exported_macros
                        .get(&(owner_crate_root, mac_name))
                        .into_iter()
                        .flatten()
                        .copied(),
                );
            }
        }
    };
    if normalized_macro.contains("::") {
        collect_path(&owner_mod.qualname, &normalized_macro, &mut candidates);
    } else {
        let mut scope = owner_mod.qualname.as_str();
        while !scope.is_empty() {
            let qual = format!("{scope}.{normalized_macro}");
            candidates.extend(
                by_qual
                    .get(qual.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|n| n.kind == "macro" && n.path == owner_mod.path),
            );
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        for import in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && (reference.source == owner_mod.id
                    || owner_mod
                        .qualname
                        .starts_with(reference.source.as_str()))
        }) {
            let import_scope = facts
                .nodes
                .iter()
                .find(|n| n.id == import.source)
                .unwrap_or(owner_mod);
            if import.alias.as_deref() == Some(normalized_macro.as_str()) {
                let target_expr = import.module.as_deref().unwrap_or(&import.expression);
                collect_path(&import_scope.qualname, target_expr, &mut candidates);
            } else if import.alias.as_deref() == Some("*") {
                let target_expr = import.module.as_deref().unwrap_or(&import.expression);
                let target_ns = resolve_qualified_path(
                    &owner_mod.path,
                    &import_scope.qualname,
                    target_expr.strip_suffix('*').unwrap_or(target_expr),
                    dependencies,
                    profile_paths,
                );
                collect_forwarded(target_ns.as_str(), &normalized_macro, &mut candidates);
            }
        }
    }
    candidates.sort_by_key(|node| node.id.as_str());
    candidates.dedup_by_key(|node| node.id.as_str());
    (candidates.len() == 1).then(|| candidates[0])
}

fn resolve_type<'a>(
    facts: &'a Facts,
    scope: &Node,
    name: &str,
    by_qual: &HashMap<&str, Vec<&'a Node>>,
    dependencies: &DependencyRegistry,
    profile_paths: &[&str],
) -> Option<&'a Node> {
    if name.is_empty() || name.contains(['<', '>', '&', '[', '(', '!']) {
        return None;
    }
    let module = facts
        .nodes
        .iter()
        .filter(|node| {
            node.path == scope.path
                && node.language == "rust"
                && matches!(node.kind.as_str(), "module" | "module_declaration")
                && (scope.qualname == node.qualname
                    || scope
                        .qualname
                        .strip_prefix(node.qualname.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.')))
        })
        .max_by_key(|node| node.qualname.len())
        .or_else(|| {
            facts
                .nodes
                .iter()
                .find(|node| node.kind == "module" && node.language == "rust")
        })?;
    let (head, tail) = name.split_once("::").unwrap_or((name, ""));
    let imports: Vec<_> = facts
        .references
        .iter()
        .filter(|reference| {
            reference.kind == "imports"
                && reference.alias.as_deref() == Some(head)
                && (reference.source == module.id || reference.source == scope.id)
        })
        .collect();
    let qualified = match imports.as_slice() {
        [] => resolve_qualified_path(
            &scope.path,
            &module.qualname,
            name,
            dependencies,
            profile_paths,
        ),
        [import] => {
            let target = import.module.as_deref()?;
            let target = if tail.is_empty() {
                target.to_owned()
            } else {
                format!("{target}::{tail}")
            };
            resolve_qualified_path(
                &scope.path,
                &module.qualname,
                &target,
                dependencies,
                profile_paths,
            )
        }
        _ => return None,
    };
    let workspace = super::workspace_path(&scope.path).0;
    let pick_unique = |qualified: &str| -> Option<&'a Node> {
        let candidates = by_qual.get(qualified)?;
        let mut candidates = candidates.iter().copied().filter(|node| {
            super::workspace_path(&node.path).0 == workspace
                && matches!(node.kind.as_str(), "struct" | "enum" | "trait" | "type")
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    };
    if let Some(found) = pick_unique(qualified.as_str()) {
        return Some(found);
    }
    if imports.is_empty() && !name.contains("::") {
        let mut ancestor = scope.qualname.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('.') {
            if parent.len() <= module.qualname.len() {
                break;
            }
            if let Some(found) = pick_unique(&format!("{parent}.{name}")) {
                return Some(found);
            }
            ancestor = parent;
        }
    }
    if imports.is_empty() && !name.contains("::") {
        let mut wildcard_matches = Vec::new();
        for import in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && reference.alias.as_deref() == Some("*")
                && (reference.source == module.id || reference.source == scope.id)
        }) {
            let Some(target) = import
                .module
                .as_deref()
                .or(Some(import.expression.as_str()))
                .map(|m| m.trim_end_matches("::*"))
                .filter(|m| !m.is_empty())
            else {
                continue;
            };
            let wc_qual = resolve_qualified_path(
                &scope.path,
                &module.qualname,
                &format!("{target}::{name}"),
                dependencies,
                profile_paths,
            );
            if let Some(found) = pick_unique(&wc_qual) {
                if !wildcard_matches
                    .iter()
                    .any(|existing: &&Node| existing.id == found.id)
                {
                    wildcard_matches.push(found);
                }
            }
        }
        if let [unique] = wildcard_matches.as_slice() {
            return Some(*unique);
        }
    }
    None
}

struct RustMemberBuildTables<'a> {
    by_qual: HashMap<&'a str, Vec<&'a Node>>,
    by_id: HashMap<&'a str, &'a Node>,
    modules_by_ns: HashMap<&'a str, Vec<&'a Node>>,
    traits_by_parent_qual: HashMap<&'a str, Vec<&'a Node>>,
    profile_paths: Vec<&'a str>,
}

fn collect_rust_member_tables<'a>(all: &'a dyn crate::FactsLookup) -> RustMemberBuildTables<'a> {
    let profile_paths: Vec<&str> = all.iter_facts().map(|(path, _)| path).collect();
    let mut by_qual = HashMap::<&str, Vec<&Node>>::new();
    let mut by_id = HashMap::<&str, &Node>::new();
    let mut modules_by_ns = HashMap::<&str, Vec<&Node>>::new();
    let mut traits_by_parent_qual = HashMap::<&str, Vec<&Node>>::new();
    for node in all
        .values_facts()
        .flat_map(|facts| &facts.nodes)
        .filter(|node| node.language == "rust")
    {
        by_qual.entry(&node.qualname).or_default().push(node);
        by_id.insert(node.id.as_str(), node);
        if node.kind == "module" {
            modules_by_ns.entry(&node.qualname).or_default().push(node);
            let stripped = node
                .qualname
                .strip_suffix(".lib")
                .or_else(|| node.qualname.strip_suffix(".main"))
                .or_else(|| node.qualname.strip_suffix(".mod"));
            if let Some(stripped) = stripped {
                modules_by_ns.entry(stripped).or_default().push(node);
            }
        } else if node.kind == "trait" {
            if let Some((parent_qual, _)) = node.qualname.rsplit_once('.') {
                traits_by_parent_qual
                    .entry(parent_qual)
                    .or_default()
                    .push(node);
            }
        }
    }
    RustMemberBuildTables {
        by_qual,
        by_id,
        modules_by_ns,
        traits_by_parent_qual,
        profile_paths,
    }
}

type MacroTraitTables<'a> = (
    HashMap<&'a str, Vec<&'a Node>>,
    HashMap<(&'a str, &'a str), Vec<&'a Node>>,
);

type TraitDeclTables<'a> = (
    HashMap<&'a str, Vec<&'a Node>>,
    HashMap<&'a str, Vec<&'a Node>>,
);

type ImplReceiverMap<'a> = HashMap<&'a str, (&'a Node, bool, Option<&'a Node>)>;
type BuiltinImplReceiverMap<'a> = HashMap<&'a str, (&'static str, &'a Node)>;

struct RustMemberBuildEnv<'a, 'env> {
    tables: &'env RustMemberBuildTables<'a>,
    interner: &'env LinkerInterner<'a>,
    flows: Option<&'env HashMap<SymbolId, &'env crate::typed_facts::FlowState>>,
    forwarding: &'env RustForwarding<'a>,
    dependencies: &'env DependencyRegistry,
}

fn collect_macro_trait_maps<'a>(
    all: &'a dyn crate::FactsLookup,
    tables: &RustMemberBuildTables<'a>,
    dependencies: &DependencyRegistry,
) -> MacroTraitTables<'a> {
    let mut macro_traits = HashMap::<&str, Vec<&Node>>::new();
    let mut crate_exported_macros = HashMap::<(&str, &str), Vec<&Node>>::new();
    for node in all
        .values_facts()
        .flat_map(|facts| &facts.nodes)
        .filter(|node| node.language == "rust" && node.kind == "macro")
    {
        if node.details["is_public"] == true {
            let crate_root = node
                .path
                .split_once("/src/")
                .map_or("", |(prefix, _)| prefix);
            crate_exported_macros
                .entry((crate_root, node.name.as_str()))
                .or_default()
                .push(node);
        }
        let Some(impl_traits) = node.details["rust_macro_impl_traits"].as_array() else {
            continue;
        };
        let mod_qual = node.qualname.rsplit_once('.').map_or("", |(q, _)| q);
        for trait_val in impl_traits {
            let Some(clean) = trait_val.as_str() else {
                continue;
            };
            let normalized_expr = clean
                .strip_prefix("$crate::")
                .map(|tail| format!("crate::{tail}"))
                .unwrap_or_else(|| clean.to_owned());
            let qual = resolve_qualified_path(
                &node.path,
                mod_qual,
                &normalized_expr,
                dependencies,
                &tables.profile_paths,
            );
            if let Some(candidates) = tables.by_qual.get(qual.as_str()) {
                for cand in candidates.iter().copied().filter(|n| n.kind == "trait") {
                    macro_traits.entry(node.id.as_str()).or_default().push(cand);
                }
            }
        }
    }
    (macro_traits, crate_exported_macros)
}

fn collect_trait_decl_tables<'a>(
    all: &'a dyn crate::FactsLookup,
    tables: &RustMemberBuildTables<'a>,
    index: &mut RustMembers<'a>,
) -> TraitDeclTables<'a> {
    let mut trait_defaults = HashMap::<&str, Vec<&Node>>::new();
    let mut trait_methods = HashMap::<&str, Vec<&Node>>::new();
    for facts in all.values_facts() {
        for edge in facts
            .edges
            .iter()
            .filter(|edge| edge.kind == "contains")
        {
            let (Some(owner), Some(method)) = (
                tables.by_id.get(edge.src.as_str()),
                tables.by_id.get(edge.dst.as_str()),
            ) else {
                continue;
            };
            if owner.kind == "trait"
                && method.kind == "method"
                && method.details["unproven_bounds"] != true
            {
                trait_methods
                    .entry(owner.id.as_str())
                    .or_default()
                    .push(method);
                if method.details["trait_default"] == true {
                    trait_defaults
                        .entry(owner.id.as_str())
                        .or_default()
                        .push(method);
                }
                if method.details["is_static"] != true {
                    index
                        .dynamic_declarations
                        .insert((owner.id.as_str(), method.name.as_str()), method);
                }
            }
        }
    }
    (trait_defaults, trait_methods)
}

fn resolve_deref_impl_target<'a>(
    facts: &'a Facts,
    module: &'a Node,
    owner: &'a Node,
    env: &RustMemberBuildEnv<'a, '_>,
) -> Option<(&'a Node, Option<&'a Node>)> {
    let RustMemberBuildEnv {
        tables,
        interner,
        flows,
        forwarding,
        dependencies,
    } = *env;
    let trait_name = owner.details["impl_trait"].as_str()?;
    let mut absolute_trait = trait_name.starts_with("::");
    let bare_trait_name = !trait_name.contains("::");
    let trait_name = trait_name.trim_start_matches("::");
    let trait_name = if matches!(trait_name, "std::ops::Deref" | "core::ops::Deref") {
        trait_name
    } else if bare_trait_name {
        let imports: Vec<_> = facts
            .references
            .iter()
            .filter(|reference| {
                reference.kind == "imports"
                    && reference.alias.as_deref() == Some(trait_name)
                    && (reference.source == module.id || reference.source == owner.id)
            })
            .collect();
        let [import] = imports.as_slice() else {
            return None;
        };
        if facts
            .nodes
            .iter()
            .any(|node| node.qualname == format!("{}.{}", module.qualname, trait_name))
        {
            return None;
        }
        let target = import.module.as_deref().unwrap_or(&import.expression);
        absolute_trait = target.starts_with("::");
        let target = target.trim_start_matches("::");
        if !matches!(target, "std::ops::Deref" | "core::ops::Deref") {
            return None;
        }
        target
    } else {
        return None;
    };
    let receiver = owner.details["receiver_type"]
        .as_str()
        .and_then(|name| {
            resolve_type(
                facts,
                owner,
                name,
                &tables.by_qual,
                dependencies,
                &tables.profile_paths,
            )
        })
        .filter(|node| node.kind == "struct" && node.details["deref_unproven"] != true)?;
    let head = trait_name.split("::").next().unwrap_or_default();
    let shadowed = forwarding
        .visible_bindings(module, &module.qualname)
        .any(|(name, _)| name == head)
        || facts
            .nodes
            .iter()
            .any(|node| node.qualname == format!("{}.{}", module.qualname, head))
        || facts.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.alias.as_deref() == Some(head)
                && (reference.source == module.id || reference.source == owner.id)
        });
    if !((!shadowed || absolute_trait)
        && owner.details["generic_impl"] != true
        && owner.details["deref_unproven"] != true)
    {
        return Some((receiver, None));
    }
    let targets: Vec<_> = facts
        .edges
        .iter()
        .filter(|edge| edge.kind == "contains" && edge.src == owner.id)
        .filter_map(|edge| tables.by_id.get(edge.dst.as_str()).copied())
        .filter(|node| node.kind == "type" && node.name == "Target")
        .collect();
    let [target] = targets.as_slice() else {
        return Some((receiver, None));
    };
    let alias = if target.details["deref_unproven"] == true {
        None
    } else if let Some(flows) = flows {
        let state = interner
            .lookup_symbol(&target.id)
            .and_then(|target_id| flows.get(&target_id).copied());
        match state {
            Some(crate::typed_facts::FlowState::Valid { facts, .. }) => facts.alias_type.clone(),
            _ => None,
        }
    } else {
        crate::semantic::ValueFlowFacts::deserialize_for_scope(&target.details["value_flow"], false)
            .ok()
            .and_then(|facts| facts.alias_type.clone())
    };
    let resolved = match alias {
        Some(crate::semantic::TypeExpr::Named { name }) => resolve_type(
            facts,
            target,
            &name,
            &tables.by_qual,
            dependencies,
            &tables.profile_paths,
        )
        .filter(|node| node.kind == "struct" && node.details["deref_unproven"] != true),
        _ => None,
    };
    Some((receiver, resolved))
}

fn populate_deref_targets<'a>(
    all: &'a dyn crate::FactsLookup,
    env: &RustMemberBuildEnv<'a, '_>,
    index: &mut RustMembers<'a>,
) {
    let mut deref_candidates = HashMap::<&str, Option<&Node>>::new();
    for facts in all.values_facts() {
        let Some(module) = facts
            .nodes
            .iter()
            .find(|node| node.kind == "module" && node.language == "rust")
        else {
            continue;
        };
        for owner in facts.nodes.iter().filter(|node| node.kind == "impl") {
            if let Some((receiver, target)) = resolve_deref_impl_target(facts, module, owner, env) {
                deref_candidates
                    .entry(receiver.id.as_str())
                    .and_modify(|candidate| *candidate = None)
                    .or_insert(target);
            }
        }
    }
    index.deref_targets = deref_candidates
        .into_iter()
        .filter_map(|(receiver, target)| target.map(|target| (receiver, target)))
        .collect();
    let invalid: Vec<_> = index
        .deref_targets
        .keys()
        .copied()
        .filter(|receiver| {
            let mut seen = HashSet::new();
            let mut current = *receiver;
            for _ in 0..RUST_ALIAS_DEPTH {
                if !seen.insert(current) {
                    return true;
                }
                let Some(target) = index.deref_targets.get(current) else {
                    return false;
                };
                current = target.id.as_str();
            }
            true
        })
        .collect();
    for receiver in invalid {
        index.deref_targets.remove(receiver);
    }
}

fn collect_glob_visible_traits<'a>(
    module: &'a Node,
    import_scope: &'a Node,
    target_expr: &str,
    tables: &RustMemberBuildTables<'a>,
    forwarding: &RustForwarding<'a>,
    dependencies: &DependencyRegistry,
    visible_traits: &mut HashMap<&'a str, HashSet<&'a str>>,
) {
    let target_ns = resolve_qualified_path(
        &module.path,
        &import_scope.qualname,
        target_expr.strip_suffix('*').unwrap_or(target_expr),
        dependencies,
        &tables.profile_paths,
    );
    let entry = visible_traits.entry(&module.path).or_default();
    if let Some(providers) = tables.modules_by_ns.get(target_ns.as_str()) {
        for provider in providers {
            for (_, targets) in forwarding.visible_bindings(provider, &module.qualname) {
                entry.extend(
                    targets
                        .iter()
                        .filter(|target| target.kind == "trait")
                        .map(|target| target.id.as_str()),
                );
            }
        }
        return;
    }
    let Some(traits_in_ns) = tables.traits_by_parent_qual.get(target_ns.as_str()) else {
        return;
    };
    entry.extend(
        traits_in_ns
            .iter()
            .filter(|target| target.path == module.path || target.details["is_public"] == true)
            .map(|target| target.id.as_str()),
    );
}

fn collect_file_visible_traits<'a>(
    facts: &'a Facts,
    module: &'a Node,
    tables: &RustMemberBuildTables<'a>,
    forwarding: &RustForwarding<'a>,
    dependencies: &DependencyRegistry,
    visible_traits: &mut HashMap<&'a str, HashSet<&'a str>>,
) {
    for import in facts
        .references
        .iter()
        .filter(|reference| reference.kind == "imports")
    {
        let import_scope = tables
            .by_id
            .get(import.source.as_str())
            .copied()
            .unwrap_or(module);
        if import.alias.as_deref() == Some("*") {
            let target_expr = import.module.as_deref().unwrap_or(&import.expression);
            collect_glob_visible_traits(
                module,
                import_scope,
                target_expr,
                tables,
                forwarding,
                dependencies,
                visible_traits,
            );
            continue;
        }
        let Some(name) = import.alias.as_deref() else {
            continue;
        };
        if let Some(target) = resolve_type(
            facts,
            import_scope,
            name,
            &tables.by_qual,
            dependencies,
            &tables.profile_paths,
        )
        .filter(|node| node.kind == "trait")
        {
            visible_traits
                .entry(&module.path)
                .or_default()
                .insert(&target.id);
        }
    }
    for node in &facts.nodes {
        if node.kind == "trait"
            && node
                .qualname
                .rsplit_once('.')
                .is_some_and(|(scope, _)| scope == module.qualname)
        {
            visible_traits
                .entry(&node.path)
                .or_default()
                .insert(&node.id);
        }
    }
}

fn collect_file_impl_receivers<'a>(
    facts: &'a Facts,
    tables: &RustMemberBuildTables<'a>,
    dependencies: &DependencyRegistry,
    index: &mut RustMembers<'a>,
) -> (ImplReceiverMap<'a>, BuiltinImplReceiverMap<'a>) {
    let mut impl_receivers = HashMap::new();
    let mut builtin_impl_receivers = HashMap::new();
    for owner in facts.nodes.iter().filter(|node| {
        node.kind == "impl"
            && (node.details["generic_impl"] != true || node.details["uniform_generic_impl"] == true)
    }) {
        let raw_receiver = owner.details["receiver_type"].as_str();
        let local_receiver = raw_receiver
            .and_then(|name| {
                let nominal = if owner.details["uniform_generic_impl"] == true {
                    name.split_once('<').map_or(name, |(base, _)| base)
                } else {
                    name
                };
                resolve_type(
                    facts,
                    owner,
                    nominal,
                    &tables.by_qual,
                    dependencies,
                    &tables.profile_paths,
                )
            })
            .filter(|node| matches!(node.kind.as_str(), "struct" | "enum"));
        let impl_trait = owner.details["impl_trait"].as_str();
        let trait_node = impl_trait.and_then(|name| {
            let clean_trait = name.split('<').next().unwrap_or(name).trim();
            resolve_type(
                facts,
                owner,
                clean_trait,
                &tables.by_qual,
                dependencies,
                &tables.profile_paths,
            )
            .filter(|node| node.kind == "trait")
        });
        let Some(receiver) = local_receiver else {
            if let (Some(builtin_key), Some(trait_node)) =
                (raw_receiver.and_then(builtin_receiver_key), trait_node)
            {
                builtin_impl_receivers.insert(owner.id.as_str(), (builtin_key, trait_node));
            }
            continue;
        };
        if impl_trait.is_some_and(|name| {
            let base = name.split('<').next().unwrap_or(name).trim();
            base == "Display"
                || base.ends_with("::Display")
                || base == "ToString"
                || base.ends_with("::ToString")
        }) {
            index.display_impls.insert(receiver.id.as_str());
        }
        let prelude_default = impl_trait.is_some_and(|name| {
            matches!(
                name.split('<').next().unwrap_or(name).trim(),
                "Default" | "std::default::Default" | "core::default::Default"
            )
        });
        let has_impl_trait = impl_trait.is_some() && !prelude_default;
        impl_receivers.insert(owner.id.as_str(), (receiver, has_impl_trait, trait_node));
    }
    (impl_receivers, builtin_impl_receivers)
}

struct MemberAccumulator<'a, 'acc> {
    index: &'acc mut RustMembers<'a>,
    traits: &'acc mut TraitImplMap<'a>,
    builtin_traits: &'acc mut BuiltinTraitImplMap<'a>,
    unproven_deref_members: &'acc mut Vec<(&'a Node, &'a Node, &'a Node, bool)>,
}

fn collect_file_impl_members<'a>(
    facts: &'a Facts,
    tables: &RustMemberBuildTables<'a>,
    impl_receivers: &ImplReceiverMap<'a>,
    builtin_impl_receivers: &BuiltinImplReceiverMap<'a>,
    trait_defaults: &HashMap<&'a str, Vec<&'a Node>>,
    acc: &mut MemberAccumulator<'a, '_>,
) {
    let MemberAccumulator {
        index,
        traits,
        builtin_traits,
        unproven_deref_members,
    } = acc;
    let mut implemented = HashMap::<&str, HashSet<(&str, bool)>>::new();
    for edge in facts.edges.iter().filter(|edge| edge.kind == "contains") {
        let (Some(owner), Some(method)) = (
            tables.by_id.get(edge.src.as_str()),
            tables.by_id.get(edge.dst.as_str()),
        ) else {
            continue;
        };
        let is_member_owner = (owner.kind == "impl"
            && matches!(method.kind.as_str(), "method" | "constant" | "type"))
            || (matches!(owner.kind.as_str(), "struct" | "enum")
                && matches!(method.kind.as_str(), "method" | "constant"))
            || (owner.kind == "trait" && method.kind == "method");
        if !is_member_owner {
            continue;
        }
        if owner.kind == "trait" {
            index.receivers_by_method.insert(&method.id, *owner);
            continue;
        }
        if owner.kind == "impl" {
            if let Some(&(builtin_key, trait_node)) = builtin_impl_receivers.get(owner.id.as_str()) {
                let associated = matches!(method.kind.as_str(), "constant" | "type")
                    || method.details["is_static"] == true;
                builtin_traits
                    .entry((builtin_key, method.name.as_str(), associated))
                    .or_default()
                    .push((method, trait_node));
                continue;
            }
        }
        let (receiver, has_impl_trait, trait_node) = if owner.kind == "impl" {
            let Some(&(receiver, has_impl_trait, trait_node)) =
                impl_receivers.get(owner.id.as_str())
            else {
                continue;
            };
            (receiver, has_impl_trait, trait_node)
        } else {
            (*owner, false, None)
        };
        index.receivers_by_method.insert(&method.id, receiver);
        let associated =
            matches!(method.kind.as_str(), "constant" | "type") || method.details["is_static"] == true;
        if method.details["unproven_bounds"] == true {
            let deref_wrapper = index.deref_targets.contains_key(receiver.id.as_str());
            if deref_wrapper {
                if let Some(trait_node) = trait_node {
                    unproven_deref_members.push((receiver, method, trait_node, associated));
                } else if !has_impl_trait {
                    index
                        .inherent
                        .entry((receiver.id.as_str(), method.name.as_str(), associated))
                        .or_default();
                }
            }
            continue;
        }
        if has_impl_trait && trait_node.is_none() {
            continue;
        }
        implemented
            .entry(owner.id.as_str())
            .or_default()
            .insert((method.name.as_str(), associated));
        let key = (receiver.id.as_str(), method.name.as_str(), associated);
        if let Some(trait_node) = trait_node {
            traits.entry(key).or_default().push((method, trait_node));
        } else {
            index.inherent.entry(key).or_default().push(method);
        }
    }
    for (owner_id, (receiver, _, trait_node)) in impl_receivers {
        let Some(trait_node) = trait_node else {
            continue;
        };
        for method in trait_defaults
            .get(trait_node.id.as_str())
            .into_iter()
            .flatten()
        {
            let associated = method.details["is_static"] == true;
            if implemented
                .get(owner_id)
                .is_some_and(|methods| methods.contains(&(method.name.as_str(), associated)))
            {
                continue;
            }
            traits
                .entry((receiver.id.as_str(), method.name.as_str(), associated))
                .or_default()
                .push((method, *trait_node));
        }
    }
}

fn apply_macro_type_invocations<'a>(
    all: &'a dyn crate::FactsLookup,
    env: &RustMemberBuildEnv<'a, '_>,
    macro_tables: &MacroTraitTables<'a>,
    trait_methods: &HashMap<&'a str, Vec<&'a Node>>,
    traits: &mut TraitImplMap<'a>,
) {
    let RustMemberBuildEnv {
        tables,
        forwarding,
        dependencies,
        ..
    } = *env;
    let (macro_traits, crate_exported_macros) = macro_tables;
    for facts in all.values_facts() {
        for owner_mod in facts
            .nodes
            .iter()
            .filter(|node| matches!(node.kind.as_str(), "module" | "module_declaration"))
        {
            let Some(invocations) = owner_mod.details["rust_macro_type_invocations"].as_array()
            else {
                continue;
            };
            for pair in invocations {
                let Some([macro_raw, target_name]) = pair
                    .as_array()
                    .and_then(|arr| Some([arr.first()?.as_str()?, arr.get(1)?.as_str()?]))
                else {
                    continue;
                };
                let Some(macro_node) = resolve_visible_macro(
                    facts,
                    owner_mod,
                    macro_raw,
                    &tables.by_qual,
                    &tables.modules_by_ns,
                    forwarding,
                    crate_exported_macros,
                    dependencies,
                    &tables.profile_paths,
                ) else {
                    continue;
                };
                let Some(trait_nodes) = macro_traits.get(macro_node.id.as_str()) else {
                    continue;
                };
                let Some(receiver) = resolve_type(
                    facts,
                    owner_mod,
                    target_name,
                    &tables.by_qual,
                    dependencies,
                    &tables.profile_paths,
                )
                .filter(|node| matches!(node.kind.as_str(), "struct" | "enum")) else {
                    continue;
                };
                for &trait_node in trait_nodes {
                    for &method in trait_methods
                        .get(trait_node.id.as_str())
                        .into_iter()
                        .flatten()
                    {
                        let associated = method.details["is_static"] == true;
                        traits
                            .entry((receiver.id.as_str(), method.name.as_str(), associated))
                            .or_default()
                            .push((method, trait_node));
                    }
                }
            }
        }
    }
}

fn finalize_rust_members<'a>(
    visible_traits: &HashMap<&'a str, HashSet<&'a str>>,
    traits: &mut TraitImplMap<'a>,
    builtin_traits: &mut BuiltinTraitImplMap<'a>,
    unproven_deref_members: Vec<(&'a Node, &'a Node, &'a Node, bool)>,
    index: &mut RustMembers<'a>,
) {
    for methods in index.inherent.values_mut() {
        methods.sort_by_key(|method| &method.id);
        methods.dedup_by_key(|method| method.id.as_str());
        if methods.iter().any(|method| method.details["synthesized"] != true) {
            methods.retain(|method| method.details["synthesized"] != true);
        }
        crate::linker::traits::disambiguate_rust_cfg_siblings(methods);
    }
    for methods in traits.values_mut() {
        methods.sort_by_key(|(method, _)| &method.id);
        methods.dedup_by_key(|(method, _)| method.id.as_str());
    }
    for methods in builtin_traits.values_mut() {
        methods.sort_by_key(|(method, _)| &method.id);
        methods.dedup_by_key(|(method, _)| method.id.as_str());
    }
    let mut trait_consumers = HashMap::<&str, Vec<&str>>::new();
    for (path, trait_ids) in visible_traits {
        for trait_id in trait_ids {
            trait_consumers.entry(trait_id).or_default().push(path);
        }
    }
    for ((receiver, member, associated), methods) in traits.iter() {
        for (method, trait_node) in methods {
            for path in trait_consumers.get(trait_node.id.as_str()).into_iter().flatten() {
                index
                    .trait_members
                    .entry((receiver, member, *associated, path))
                    .or_default()
                    .push(method);
            }
        }
    }
    for ((receiver_key, member, associated), methods) in builtin_traits.iter() {
        for (method, trait_node) in methods {
            for path in trait_consumers.get(trait_node.id.as_str()).into_iter().flatten() {
                index
                    .builtin_trait_members
                    .entry((*receiver_key, *member, *associated, path))
                    .or_default()
                    .push(method);
            }
        }
    }
    for (receiver, method, trait_node, associated) in unproven_deref_members {
        for path in trait_consumers.get(trait_node.id.as_str()).into_iter().flatten() {
            index
                .trait_members
                .entry((receiver.id.as_str(), method.name.as_str(), associated, path))
                .or_default();
        }
    }
    for methods in index.trait_members.values_mut() {
        finalize_member_list(methods);
    }
    for methods in index.builtin_trait_members.values_mut() {
        finalize_member_list(methods);
    }
    for ((_, member, associated), methods) in traits.iter() {
        if *associated {
            continue;
        }
        for (method, trait_node) in methods {
            let key = (trait_node.id.as_str(), *member);
            if index
                .dynamic_declarations
                .get(&key)
                .is_some_and(|declaration| declaration.id != method.id)
            {
                index
                    .dynamic_implementors
                    .entry(key)
                    .or_default()
                    .push(method);
            }
        }
    }
    for methods in index.dynamic_implementors.values_mut() {
        methods.sort_by_key(|method| &method.id);
        methods.dedup_by_key(|method| method.id.as_str());
        if methods.len() > crate::linker::value_flow::MAX_CANDIDATE_BRANCH_JOINS {
            methods.clear();
        }
    }
}

impl<'a> RustMembers<'a> {
    pub(crate) fn build(
        all: &'a dyn crate::FactsLookup,
        interner: &LinkerInterner<'a>,
        flows: Option<&HashMap<SymbolId, &crate::typed_facts::FlowState>>,
        forwarding: &RustForwarding<'a>,
        dependencies: &DependencyRegistry,
    ) -> Self {
        let tables = collect_rust_member_tables(all);
        let mut index = Self {
            deref_targets: HashMap::new(),
            inherent: HashMap::new(),
            trait_members: HashMap::new(),
            builtin_trait_members: HashMap::new(),
            receivers_by_method: HashMap::new(),
            dynamic_declarations: HashMap::new(),
            dynamic_implementors: HashMap::new(),
            display_impls: HashSet::new(),
        };
        let mut traits: TraitImplMap<'a> = HashMap::new();
        let mut builtin_traits: BuiltinTraitImplMap<'a> = HashMap::new();
        let mut visible_traits: HashMap<&'a str, HashSet<&'a str>> = HashMap::new();
        let mut unproven_deref_members = Vec::new();
        let macro_tables = collect_macro_trait_maps(all, &tables, dependencies);
        let (trait_defaults, trait_methods) = collect_trait_decl_tables(all, &tables, &mut index);
        let env = RustMemberBuildEnv {
            tables: &tables,
            interner,
            flows,
            forwarding,
            dependencies,
        };
        populate_deref_targets(all, &env, &mut index);
        for facts in all.values_facts() {
            let Some(module) = facts
                .nodes
                .iter()
                .find(|node| node.kind == "module" && node.language == "rust")
            else {
                continue;
            };
            collect_file_visible_traits(
                facts,
                module,
                &tables,
                forwarding,
                dependencies,
                &mut visible_traits,
            );
            let (impl_receivers, builtin_impl_receivers) =
                collect_file_impl_receivers(facts, &tables, dependencies, &mut index);
            let mut acc = MemberAccumulator {
                index: &mut index,
                traits: &mut traits,
                builtin_traits: &mut builtin_traits,
                unproven_deref_members: &mut unproven_deref_members,
            };
            collect_file_impl_members(
                facts,
                &tables,
                &impl_receivers,
                &builtin_impl_receivers,
                &trait_defaults,
                &mut acc,
            );
        }
        apply_macro_type_invocations(all, &env, &macro_tables, &trait_methods, &mut traits);
        finalize_rust_members(
            &visible_traits,
            &mut traits,
            &mut builtin_traits,
            unproven_deref_members,
            &mut index,
        );
        index
    }

    pub(crate) fn lookup(
        &self,
        receiver: &Node,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> &[&'a Node] {
        if receiver.kind == "trait" && !associated {
            return self
                .dynamic_declarations
                .get(&DynamicLookup(receiver.id.as_str(), member))
                .map_or(&[], std::slice::from_ref);
        }
        let mut receiver_id = receiver.id.as_str();
        for _ in 0..RUST_ALIAS_DEPTH {
            if let Some(methods) = self
                .inherent
                .get(&MemberLookup(receiver_id, member, associated))
            {
                return methods;
            }
            if let Some(methods) = self.trait_members.get(&TraitLookup(
                receiver_id,
                member,
                associated,
                caller.path.as_str(),
            )) {
                return methods;
            }
            if associated {
                break;
            }
            let Some(target) = self.deref_targets.get(receiver_id) else {
                break;
            };
            receiver_id = target.id.as_str();
        }
        &[]
    }

    pub(crate) fn lookup_builtin(
        &self,
        receiver_key: &str,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> &[&'a Node] {
        self.builtin_trait_members
            .get(&TraitLookup(
                receiver_key,
                member,
                associated,
                caller.path.as_str(),
            ))
            .map_or(&[], Vec::as_slice)
    }

    pub(crate) fn deref_target(&self, receiver: &Node) -> Option<&'a Node> {
        self.deref_targets.get(receiver.id.as_str()).copied()
    }

    pub(crate) fn receiver_for(&self, method: &Node) -> Option<&'a Node> {
        self.receivers_by_method.get(method.id.as_str()).copied()
    }

    pub(crate) fn is_inherent_member(&self, method: &Node) -> bool {
        self.inherent
            .values()
            .any(|methods| methods.iter().any(|m| m.id == method.id))
    }

    pub(crate) fn has_display(&self, receiver: &Node) -> bool {
        self.display_impls.contains(receiver.id.as_str())
    }
    pub(crate) fn dynamic_dispatch<'s>(
        &'s self,
        receiver: &'s Node,
        member: &'s str,
    ) -> Option<(&'a Node, &'s [&'a Node])> {
        let key = (receiver.id.as_str(), member);
        let declaration = self.dynamic_declarations.get(&key)?;
        Some((
            *declaration,
            self.dynamic_implementors
                .get(&key)
                .map_or(&[], Vec::as_slice),
        ))
    }
}

fn required_full_facts(
    facts: &dyn crate::FactsLookup,
    affected: &std::collections::BTreeSet<String>,
    catalog: &[(&str, &str)],
) -> std::collections::BTreeSet<String> {
    let workspaces: HashSet<_> = facts
        .iter_facts()
        .filter(|(path, facts)| {
            affected.contains(*path)
                && facts.nodes.iter().any(|node| node.language == "rust")
                && facts.references.iter().any(|reference| {
                    reference.kind == "calls"
                        && (reference.expression.contains('.')
                            || reference.expression.contains("::"))
                })
        })
        .map(|(path, _)| super::workspace_path(path).0)
        .collect();
    catalog
        .iter()
        .filter(|(path, language)| {
            *language == "rust" && workspaces.contains(super::workspace_path(path).0)
        })
        .map(|(path, _)| (*path).to_owned())
        .collect()
}
