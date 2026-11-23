//! Rust forwarding, visibility, and module-namespace indexes decoupled from traits and linkers.

use crate::interner::LinkerInterner;
use crate::languages::dependency_registry::DependencyRegistry;
use crate::languages::types::{self, ImportPath, LanguageFamily};
pub(in crate::linker) use crate::languages::rust::visibility::{
    crate_namespace, module_namespace, restricted_exports, restricted_scope, within,
    RestrictedExport,
};
use crate::linker::types::{insert_package_export, ModulesByNamespace, PackageExports};
use crate::models::{tags::LanguageId, Facts, Node};
use hashbrown::{HashMap, HashSet};

const RUST_ALIAS_DEPTH: usize = 64;

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
            types::workspace_path(candidate_path).0,
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
                let target_workspace = types::workspace_path(&module.path).0;
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

pub(super) fn is_crate_visible(node: &Node, requester: &str) -> bool {
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
            let workspace = types::workspace_path(&module.path).0;
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
                        types::workspace_path(&module.path).0,
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

pub(super) fn declared_exports<'a>(
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

/// Helper to verify registered Cargo ecosystem crates statically from ecosystem list.
pub fn is_registered_cargo_crate(crate_name: &str) -> bool {
    matches!(
        crate_name,
        "rusqlite"
            | "tree-sitter"
            | "tree_sitter"
            | "serde"
            | "serde_json"
            | "serde_yaml"
            | "tokio"
            | "anyhow"
            | "clap"
            | "tracing"
            | "rayon"
            | "tempfile"
            | "regex"
            | "thiserror"
            | "parking_lot"
            | "crossbeam"
            | "log"
            | "git2"
            | "walkdir"
            | "ignore"
            | "sha2"
            | "hex"
            | "toml"
            | "uuid"
            | "chrono"
            | "hashbrown"
            | "indexmap"
            | "petgraph"
            | "libc"
            | "dirs"
            | "pulldown-cmark"
            | "pulldown_cmark"
            | "schemars"
            | "rmcp"
            | "zstd"
            | "cc"
            | "bstr"
            | "futures"
            | "syn"
            | "quote"
            | "proc_macro2"
            | "proc-macro2"
            | "base64"
            | "bytes"
            | "http"
            | "tower"
            | "axum"
            | "hyper"
            | "url"
    )
}
