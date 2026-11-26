//! Rust forwarding, visibility, and module-namespace indexes decoupled from traits and linkers.

use crate::interner::LinkerInterner;
use crate::languages::dependency_registry::DependencyRegistry;
pub(in crate::linker) use crate::languages::rust::visibility::{
    crate_namespace, module_namespace, restricted_exports, restricted_scope, within,
    RestrictedExport,
};
use crate::languages::types::{self, ImportPath, LanguageFamily};
use crate::linker::types::{insert_package_export, ModulesByNamespace, PackageExports};
use crate::models::{tags::LanguageId, Facts, Node};
use hashbrown::{HashMap, HashSet};

const RUST_ALIAS_DEPTH: usize = 64;

pub(in crate::linker::rust) fn is_inline_module_declaration(node: &Node) -> bool {
    node.kind == crate::models::tags::NodeKind::ModuleDeclaration
        && node.detail_entry(crate::models::details::NodeDetailKey::INLINE_MODULE) == true
}

pub(in crate::linker::rust) fn is_module_scope(node: &Node) -> bool {
    node.kind == crate::models::tags::NodeKind::Module || is_inline_module_declaration(node)
}

enum ModuleImport<'a> {
    Borrowed(&'a ImportPath),
    Owned(ImportPath),
}

impl std::ops::Deref for ModuleImport<'_> {
    type Target = ImportPath;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(path) => path,
            Self::Owned(path) => path,
        }
    }
}

fn normalize_module_import<'a, 'b>(
    module: &'a Node,
    raw: &'a str,
    normalized_imports: &'b HashMap<(&'a str, &'a str), Option<ImportPath>>,
) -> Option<ModuleImport<'b>> {
    if is_inline_module_declaration(module)
        && (raw == "self"
            || raw.starts_with("self::")
            || raw == "super"
            || raw.starts_with("super::"))
    {
        let crate_root = crate_namespace(&module.path);
        let canon = canonical_namespace(&module.qualname, &crate_root);
        let expr = if raw == "self" {
            "self::"
        } else if raw == "super" {
            "super::"
        } else {
            raw
        };
        let namespace = resolve_relative_module_prefix(&module.path, &canon, expr)?;
        return Some(ModuleImport::Owned(ImportPath {
            namespace,
            relative: true,
            symbol_path: true,
        }));
    }
    normalized_imports
        .get(&(module.path.as_str(), raw))
        .and_then(Option::as_ref)
        .map(ModuleImport::Borrowed)
}

pub(crate) enum WorkspaceCrateMatch {
    Resolved(String, String),
    Isolated,
    Missing,
}

fn lookup_workspace_crate<'a>(
    profile_paths: impl Iterator<Item = &'a str>,
    dependencies: &DependencyRegistry,
    requester_path: &str,
    crate_name: &str,
    tail: &str,
) -> WorkspaceCrateMatch {
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
    let mut matches = profile_paths.filter_map(|candidate_path| {
        if !candidate_path.ends_with("/src/lib.rs") && candidate_path != "src/lib.rs" {
            return None;
        }
        let (pkg_scope, lib_name) = dependencies.rust_package_for_path(candidate_path)?;
        let ws = types::workspace_path(candidate_path).0;
        let workspace_lib = dependencies.rust_library_for_path(candidate_path).is_some();
        (lib_name == crate_name).then_some((pkg_scope, ws, workspace_lib))
    });
    let Some((pkg_scope, workspace, workspace_lib)) = matches.next() else {
        return WorkspaceCrateMatch::Missing;
    };
    if matches.next().is_some() {
        return WorkspaceCrateMatch::Isolated;
    }
    if !declared && sibling_scope != Some(pkg_scope) {
        return if workspace_lib {
            WorkspaceCrateMatch::Isolated
        } else {
            WorkspaceCrateMatch::Missing
        };
    }
    WorkspaceCrateMatch::Resolved(
        format_crate_namespace(pkg_scope, tail),
        workspace.to_owned(),
    )
}

pub(crate) fn resolve_workspace_crate_namespace<'a>(
    profile_paths: impl Iterator<Item = &'a str>,
    dependencies: &DependencyRegistry,
    requester_path: &str,
    crate_name: &str,
    tail: &str,
) -> Option<(String, String)> {
    match lookup_workspace_crate(
        profile_paths,
        dependencies,
        requester_path,
        crate_name,
        tail,
    ) {
        WorkspaceCrateMatch::Resolved(ns, workspace) => Some((ns, workspace)),
        WorkspaceCrateMatch::Isolated | WorkspaceCrateMatch::Missing => None,
    }
}

pub(in crate::linker::rust) fn resolve_relative_module_prefix(
    path: &str,
    module_qual: &str,
    expression: &str,
) -> Option<String> {
    let mut resolved = if let Some(tail) = expression.strip_prefix("crate::") {
        let root_ns = crate_namespace(path);
        let root_ns = root_ns.strip_suffix(".rs").unwrap_or(&root_ns);
        if tail.is_empty() || tail == "*" {
            root_ns.to_owned()
        } else {
            format!("{root_ns}.{}", tail.replace("::", "."))
        }
    } else if let Some(tail) = expression.strip_prefix("self::") {
        if tail.is_empty() || tail == "*" {
            module_qual.to_owned()
        } else {
            format!("{module_qual}.{}", tail.replace("::", "."))
        }
    } else {
        let mut tail = expression.strip_prefix("super::")?;
        let mut module = module_qual.rsplit_once('.')?.0;
        while let Some(rest) = tail.strip_prefix("super::") {
            module = module.rsplit_once('.')?.0;
            tail = rest;
        }
        if tail.is_empty() || tail == "*" {
            module.to_owned()
        } else {
            format!("{module}.{}", tail.replace("::", "."))
        }
    };
    while resolved.ends_with('.') {
        resolved.pop();
    }
    Some(resolved)
}

pub(in crate::linker::rust) fn resolve_qualified_path(
    scope_path: &str,
    module_qual: &str,
    target: &str,
    dependencies: &DependencyRegistry,
    profile_paths: &[&str],
) -> String {
    if target.starts_with("crate::")
        || target.starts_with("self::")
        || target.starts_with("super::")
    {
        return resolve_relative_module_prefix(scope_path, module_qual, target).unwrap_or_default();
    }
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
    let mut resolved = format!("{module_qual}.{}", target.replace("::", "."));
    while resolved.ends_with('.') {
        resolved.pop();
    }
    resolved
}

fn format_crate_namespace(scope: &str, tail: &str) -> String {
    let base = if scope.is_empty() {
        "src".to_owned()
    } else {
        format!("{}.src", scope.replace('/', "."))
    };
    if tail.is_empty() || tail == "*" {
        base
    } else {
        format!("{base}.{}", tail.replace("::", "."))
    }
}

fn scope_lib_namespace(
    profiles_contains: impl Fn(&str) -> bool,
    scope: &str,
    workspace: Option<&str>,
    tail: &str,
) -> Option<(String, String)> {
    let lib_path = if scope.is_empty() {
        std::borrow::Cow::Borrowed("src/lib.rs")
    } else {
        std::borrow::Cow::Owned(format!("{scope}/src/lib.rs"))
    };
    profiles_contains(lib_path.as_ref()).then(|| {
        let ws = workspace.unwrap_or_else(|| types::workspace_path(&lib_path).0);
        (format_crate_namespace(scope, tail), ws.to_owned())
    })
}

pub(crate) fn resolve_crate_namespace<'a>(
    profile_paths: impl Iterator<Item = &'a str> + Clone,
    profiles_contains: impl Fn(&str) -> bool,
    dependencies: &DependencyRegistry,
    path: &str,
    first: &str,
    tail: &str,
) -> WorkspaceCrateMatch {
    if let Some((pkg_scope, own_lib)) = dependencies.sibling_rust_package_library_for_path(path) {
        if own_lib == first {
            if let Some((ns, ws)) = scope_lib_namespace(&profiles_contains, pkg_scope, None, tail) {
                return WorkspaceCrateMatch::Resolved(ns, ws);
            }
        }
    }
    match lookup_workspace_crate(profile_paths, dependencies, path, first, tail) {
        matched @ (WorkspaceCrateMatch::Resolved(..) | WorkspaceCrateMatch::Isolated) => {
            return matched;
        }
        WorkspaceCrateMatch::Missing => {}
    }
    let local_workspace = types::workspace_path(path).0;
    if dependencies
        .rust_package_for_path(path)
        .is_some_and(|(scope, _)| scope != local_workspace)
    {
        return WorkspaceCrateMatch::Missing;
    }
    let Some(root_crate) = dependencies.root_rust_crate_for_path(path) else {
        return WorkspaceCrateMatch::Missing;
    };
    if first != root_crate {
        return WorkspaceCrateMatch::Missing;
    }
    scope_lib_namespace(
        profiles_contains,
        local_workspace,
        Some(local_workspace),
        tail,
    )
    .map_or(WorkspaceCrateMatch::Missing, |(ns, ws)| {
        WorkspaceCrateMatch::Resolved(ns, ws)
    })
}

fn canonical_namespace<'n>(name: &'n str, crate_root: &str) -> std::borrow::Cow<'n, str> {
    let base: std::borrow::Cow<'n, str> = if let Some(tail) = name.strip_prefix(crate_root) {
        let mut stripped = None;
        for entry in [".lib", ".main"] {
            if let Some(tail) = tail.strip_prefix(entry) {
                if tail.is_empty() || tail.starts_with('.') {
                    stripped = Some(format!("{crate_root}{tail}").into());
                    break;
                }
            }
        }
        stripped.unwrap_or(std::borrow::Cow::Borrowed(name))
    } else {
        std::borrow::Cow::Borrowed(name)
    };
    if base.contains(".mod.") {
        base.replace(".mod.", ".").into()
    } else if let Some(prefix) = base.strip_suffix(".mod") {
        prefix.to_owned().into()
    } else {
        base
    }
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
                node.language == crate::models::tags::LanguageId::Rust
                    && node.kind == crate::models::tags::NodeKind::ModuleDeclaration
                    && node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) != true
            })
        {
            let Some((owner, _)) = declaration.qualname.rsplit_once('.') else {
                continue;
            };
            let crate_root = crate_namespace(&declaration.path);
            let owner = canonical_namespace(owner, &crate_root);
            let scope = declaration
                .detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
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

fn extend_visible_members<'a>(
    all: &'a dyn crate::FactsLookup,
    module: &'a Node,
    name: &str,
    requester: &Node,
    public_only: bool,
    candidates: &mut Vec<&'a Node>,
) -> bool {
    let mut direct = direct_members(all, module, name);
    if public_only {
        direct.retain(|node| is_crate_visible(node, &requester.qualname));
    }
    let found = !direct.is_empty();
    candidates.extend(direct);
    found
}

pub(super) fn is_public_export(module: &Node, name: &str) -> bool {
    module
        .detail_entry(crate::models::details::NodeDetailKey::EXPORTS)
        .as_array()
        .is_some_and(|exports| {
            exports
                .iter()
                .any(|export| export["name"].as_str() == Some(name))
        })
}

pub(super) fn is_public_glob(module: &Node, target: &str) -> bool {
    module
        .detail_entry(crate::models::details::NodeDetailKey::EXPORTS)
        .as_array()
        .is_some_and(|exports| {
            exports
                .iter()
                .any(|export| export["star"] == true && export["module"].as_str() == Some(target))
        })
}

pub(super) fn has_any_public_glob(module: &Node) -> bool {
    module
        .detail_entry(crate::models::details::NodeDetailKey::EXPORTS)
        .as_array()
        .is_some_and(|exports| exports.iter().any(|export| export["star"] == true))
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
                    || normalized
                        .namespace
                        .strip_prefix(&target_root)
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

pub(super) fn is_descendant_module(requester: &str, owner: &Node) -> bool {
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
    let module = receiver
        .qualname
        .rsplit_once('.')
        .map_or("", |(module, _)| module);
    within(requester, module)
        || field.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) == true
        || field
            .detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
            .as_str()
            .and_then(|signature| restricted_scope(signature, module, &field.path))
            .is_some_and(|scope| within(requester, &scope))
}

pub(super) fn is_crate_visible(node: &Node, requester: &str) -> bool {
    node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) == true
        || node
            .detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
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
        let module_declaration = direct.iter().any(|node| {
            node.kind == crate::models::tags::NodeKind::ModuleDeclaration
                && !is_inline_module_declaration(node)
        });
        direct.retain(|node| {
            node.kind != crate::models::tags::NodeKind::ModuleDeclaration
                || is_inline_module_declaration(node)
        });
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
            if node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) != true {
                let scope = node
                    .detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
                    .as_str()
                    .and_then(|signature| restricted_scope(signature, &module.qualname, &node.path))
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
                && self
                    .visiting
                    .iter()
                    .any(|(id, alias, _)| *id == target_module.id && alias == step.name)
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
            if !is_public_export(step.module, step.name) {
                for domain in self.module_visibility.domains(&target_module.qualname) {
                    targets.restrict(Some(&domain.scope));
                }
            }
            *candidates = targets.nodes;
            *scope = targets.scope;
            true
        } else if tail.contains('.') {
            if extend_visible_members(
                self.all,
                target_module,
                tail,
                step.module,
                require_public,
                candidates,
            ) {
                true
            } else {
                self.resolve_nested_member_alias(
                    step,
                    target_module,
                    tail,
                    require_public,
                    scope,
                    candidates,
                )
            }
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

    fn resolve_nested_member_alias(
        &mut self,
        step: &ResolvedImportStep<'a, '_>,
        target_module: &'a Node,
        tail: &str,
        require_public: bool,
        scope: &mut Option<String>,
        candidates: &mut Vec<&'a Node>,
    ) -> bool {
        if step.depth >= RUST_ALIAS_DEPTH {
            return false;
        }
        let Some((head, rest)) = tail.split_once('.') else {
            return false;
        };
        let Some(targets) = self.resolve(target_module, head, require_public, step.depth + 1)
        else {
            return false;
        };
        let mut mid_candidates = Vec::new();
        let mut mid_scope = scope.clone();
        merge_restricted_targets(
            targets,
            &step.module.qualname,
            None,
            &mut mid_scope,
            &mut mid_candidates,
        );
        if mid_candidates.is_empty() {
            return false;
        }
        let mut ok = false;
        for mid in mid_candidates {
            if is_module_scope(mid) {
                let mid_public = step.public_only
                    || (step.module.qualname != mid.qualname
                        && !is_descendant_module(&step.module.qualname, mid));
                if rest.contains('.') {
                    if extend_visible_members(
                        self.all,
                        mid,
                        rest,
                        step.module,
                        mid_public,
                        candidates,
                    ) {
                        ok = true;
                    } else {
                        let next_step = ResolvedImportStep {
                            module: step.module,
                            name: step.name,
                            public_only: step.public_only,
                            named_scope: step.named_scope,
                            depth: step.depth + 1,
                            restricted: step.restricted,
                        };
                        ok |= self.resolve_nested_member_alias(
                            &next_step,
                            mid,
                            rest,
                            mid_public,
                            &mut mid_scope,
                            candidates,
                        );
                    }
                } else if let Some(resolved) = self.resolve(mid, rest, mid_public, step.depth + 1) {
                    if !resolved.nodes.is_empty() {
                        merge_restricted_targets(
                            resolved,
                            &step.module.qualname,
                            step.named_scope,
                            &mut mid_scope,
                            candidates,
                        );
                        ok = true;
                    }
                }
            } else {
                let mut direct = direct_members(self.all, mid, rest);
                if step.public_only {
                    direct.retain(|node| is_crate_visible(node, &step.module.qualname));
                }
                if !direct.is_empty() {
                    candidates.extend(direct);
                    ok = true;
                }
            }
        }
        if ok {
            *scope = mid_scope;
        }
        ok
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
        let (direct_res, module_declaration) =
            self.resolve_direct_alias(module, name, public_only)?;
        if let Some(result) = direct_res {
            return Some(result);
        }
        let facts = self.all.get_facts(module.path.as_str())?;
        let is_private_mod_decl = |reference: &crate::models::Reference| -> bool {
            let Some(alias) = reference.alias.as_deref() else {
                return false;
            };
            facts.nodes.iter().any(|node| {
                node.kind == crate::models::tags::NodeKind::ModuleDeclaration
                    && node.line == reference.line
                    && node.name == alias
                    && node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) != true
                    && node
                        .detail_entry(crate::models::details::NodeDetailKey::SIGNATURE)
                        .as_str()
                        .and_then(|sig| restricted_scope(sig, &module.qualname, &node.path))
                        .is_none()
            })
        };
        let named = facts.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.source == module.id
                && reference.alias.as_deref() == Some(name)
                && (!public_only || !is_private_mod_decl(reference))
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
        let memo_key = ((module.id.as_str(), name.to_owned(), public_only), depth);
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
                && (!public_only || !is_private_mod_decl(reference))
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
            let Some(normalized) = normalize_module_import(module, raw, self.normalized_imports)
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
                &normalized,
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
                    if (!module_declaration && local.nodes.len() == candidates.len()
                        || (module_declaration
                            && local
                                .nodes
                                .iter()
                                .filter(|node| {
                                    node.kind != crate::models::tags::NodeKind::Module
                                        || candidates.iter().any(|c| c.id == node.id)
                                })
                                .count()
                                == candidates.len()))
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

impl crate::languages::profile::LanguageImportState for RustForwarding<'_> {
    fn module_path_visible(&self, namespace: &str, requester: &str) -> bool {
        RustForwarding::module_path_visible(self, namespace, requester)
    }

    fn resolve_path<'r>(
        &'r self,
        module: &Node,
        tail: &str,
        requester: &str,
        by_qual: Option<&HashMap<&'r str, Vec<&'r Node>>>,
    ) -> Vec<&'r Node> {
        RustForwarding::resolve_path(self, module, tail, requester, by_qual)
    }

    fn external_binding_visible(
        &self,
        provider: &Node,
        binding_name: &str,
        requester: &str,
        direct_in_provider: bool,
    ) -> bool {
        RustForwarding::external_binding_visible(
            self,
            provider,
            binding_name,
            requester,
            direct_in_provider,
        )
    }

    fn targets(&self, module: &Node, name: &str, requester: &str) -> &[&Node] {
        RustForwarding::targets(self, module, name, requester)
    }

    fn visible_bindings(&self, module: &Node, requester: &str) -> Vec<(&str, &[&Node])> {
        RustForwarding::visible_bindings(self, module, requester).collect()
    }

    fn workspace_crate_root(
        &self,
        dependencies: &DependencyRegistry,
        path: &str,
        crate_name: &str,
    ) -> Option<&Node> {
        RustForwarding::workspace_crate_root(self, dependencies, path, crate_name)
    }
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
                    node.language == crate::models::tags::LanguageId::Rust
                        && (node.kind == crate::models::tags::NodeKind::Module
                            || (is_inline_module_declaration(node) && !node.is_test))
                })
            })
            .collect();
        let mut names_by_module = HashMap::<&str, HashSet<&str>>::new();
        for module in &modules {
            let Some(facts) = all.get_facts(module.path.as_str()) else {
                continue;
            };
            let names = names_by_module.entry(module.id.as_str()).or_default();
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
                for reference in facts.references.iter().filter(|reference| {
                    reference.kind == "imports"
                        && reference.source == module.id
                        && reference.alias.as_deref() == Some("*")
                }) {
                    let raw = reference.module.as_deref().unwrap_or(&reference.expression);
                    let Some(normalized) = normalize_module_import(module, raw, normalized_imports)
                    else {
                        continue;
                    };
                    let targets = modules_at_import(
                        modules_by_namespace,
                        LanguageFamily("rust"),
                        &normalized,
                        types::workspace_path(&module.path).0,
                        |namespace| module_visibility.visible(namespace, &module.qualname),
                    );
                    if let [(target, tail)] = targets.as_slice() {
                        if matches!(tail.as_str(), "" | "*") {
                            additions.extend(
                                names_by_module
                                    .get(target.id.as_str())
                                    .into_iter()
                                    .flatten()
                                    .map(|name| (module.id.as_str(), *name)),
                            );
                        }
                    }
                }
            }
            let mut changed = false;
            for (module_id, name) in additions {
                changed |= names_by_module.entry(module_id).or_default().insert(name);
            }
            if !changed {
                break;
            }
        }

        for &module in &modules {
            for name in names_by_module
                .get(module.id.as_str())
                .into_iter()
                .flatten()
                .copied()
            {
                let private_result = resolver.resolve(module, name, false, 0);
                if !matches!(&private_result, Some(targets) if targets.nodes.is_empty()) {
                    private.entry(module.id.as_str()).or_default().insert(
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
                            .entry(module.id.as_str())
                            .or_default()
                            .insert(name, targets.clone());
                        continue;
                    }
                    let public_targets =
                        public_result.map_or_else(Vec::new, |targets| targets.nodes);
                    if module.kind == crate::models::tags::NodeKind::Module {
                        insert_package_export(
                            &mut exports,
                            interner,
                            module.path.as_str(),
                            name,
                            (public_targets.len() == 1).then(|| public_targets[0]),
                        );
                    }
                    public
                        .entry(module.id.as_str())
                        .or_default()
                        .insert(name, public_targets);
                }
            }
        }

        let lib_modules = modules
            .iter()
            .copied()
            .filter(|node| {
                node.kind == crate::models::tags::NodeKind::Module
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
        self.targets_in_accessible_module(module, name, requester)
    }

    fn targets_in_accessible_module<'forwarding>(
        &'forwarding self,
        module: &Node,
        name: &str,
        requester: &str,
    ) -> &'forwarding [&'a Node] {
        if let Some(targets) = self
            .restricted
            .get(module.id.as_str())
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
            .get(module.id.as_str())
            .and_then(|aliases| aliases.get(name))
        {
            return targets;
        }
        if requester == module.qualname || is_descendant_module(requester, module) {
            return self
                .private
                .get(module.id.as_str())
                .and_then(|aliases| aliases.get(name))
                .map(Vec::as_slice)
                .unwrap_or_default();
        }
        &[]
    }

    pub(crate) fn resolve_path<'q, 'n>(
        &self,
        module: &Node,
        tail: &str,
        requester: &str,
        by_qual: Option<&HashMap<&'q str, Vec<&'n Node>>>,
    ) -> Vec<&'n Node>
    where
        'a: 'n,
    {
        if !self.module_path_visible(&module.qualname, requester) {
            return Vec::new();
        }
        let segments: Vec<&str> = tail.split('.').collect();
        if segments.len() > RUST_ALIAS_DEPTH || segments.iter().any(|segment| segment.is_empty()) {
            return Vec::new();
        }
        let mut current = vec![module];
        let mut via_reexport = false;
        let mut resolved = Vec::new();
        for (idx, segment) in segments.iter().enumerate() {
            let is_last = idx + 1 == segments.len();
            let rest = &segments[idx..];
            let mut next = Vec::new();
            for candidate in current {
                if is_module_scope(candidate) {
                    let hits = if via_reexport {
                        self.targets_in_accessible_module(candidate, segment, requester)
                    } else {
                        self.targets(candidate, segment, requester)
                    };
                    next.extend_from_slice(hits);
                }
                if let Some(by_qual) =
                    by_qual.filter(|_| via_reexport || !is_module_scope(candidate))
                {
                    let qual = format!("{}.{}", candidate.qualname, rest.join("."));
                    if let Some(nodes) = by_qual.get(qual.as_str()) {
                        resolved.extend(nodes.iter().copied().filter(|node| {
                            is_crate_visible(node, requester)
                                || node
                                    .qualname
                                    .rsplit_once('.')
                                    .is_some_and(|(s, _)| within(requester, module_namespace(s)))
                        }));
                    }
                }
            }
            if is_last {
                resolved.extend(next);
                resolved.sort_by(|left, right| left.id.cmp(&right.id));
                resolved.dedup_by_key(|node| node.id.as_str());
                return resolved;
            }
            next.sort_by(|left, right| left.id.cmp(&right.id));
            next.dedup_by_key(|node| node.id.as_str());
            if next.is_empty() {
                break;
            }
            current = next;
            via_reexport = true;
        }
        resolved.sort_by(|left, right| left.id.cmp(&right.id));
        resolved.dedup_by_key(|node| node.id.as_str());
        resolved
    }

    pub(crate) fn external_binding_visible(
        &self,
        provider: &Node,
        binding_name: &str,
        requester: &str,
        direct_in_provider: bool,
    ) -> bool {
        if requester == provider.qualname || is_descendant_module(requester, provider) {
            return true;
        }
        if is_public_export(provider, binding_name) {
            return true;
        }
        let restricted = restricted_exports(provider);
        if restricted
            .iter()
            .any(|export| export.name == binding_name && within(requester, &export.scope))
        {
            return true;
        }
        if !direct_in_provider {
            return has_any_public_glob(provider)
                || restricted
                    .iter()
                    .any(|export| export.name == "*" && within(requester, &export.scope));
        }
        false
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
        let restricted = self.restricted.get(module.id.as_str());
        let ordinary = bindings
            .get(module.id.as_str())
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
        let Some(module) = facts.nodes.iter().find(|node| {
            node.kind == crate::models::tags::NodeKind::Module
                && node.language == crate::models::tags::LanguageId::Rust
        }) else {
            continue;
        };
        let Some(bindings) = module
            .detail_entry(crate::models::details::NodeDetailKey::EXPORTS)
            .as_array()
        else {
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
