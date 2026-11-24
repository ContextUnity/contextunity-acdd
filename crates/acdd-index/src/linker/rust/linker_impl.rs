use super::forwarding::{
    crate_namespace, declared_exports, is_crate_visible, module_namespace,
    resolve_crate_namespace, within, WorkspaceCrateMatch,
};
use crate::interner::LinkerInterner;
use crate::languages::{
    dependency_registry::DependencyRegistry,
    profile::LanguageProfile,
    rust::visibility::{normalize_root_crate_import, rust_module_name},
    types::{self as languages, ImportPath, LanguageFamily},
};
use crate::linker::traits::{
    package_export, resolve_default_import, ImportContext, ImportNormalizationContext,
    ImportResolution, LanguageLinker, ModulesByNamespace, PackageExports,
};
use crate::linker::types::NodeBindingIndex;
use crate::models::{
    tags::{EdgeKind, LanguageId},
    Facts, Node,
};
use hashbrown::{HashMap, HashSet};

fn relative_import(namespace: String) -> Option<ImportPath> {
    Some(ImportPath {
        namespace,
        relative: true,
        symbol_path: true,
    })
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
            Some("lib.rs" | "main.rs" | "mod.rs" | "build.rs")
        )
        || parent.file_name().is_some_and(|name| name == "bin")
    {
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
            let mut namespace = rust_module_name(&local);
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
    match resolve_crate_namespace(
        ctx.profiles.keys().copied(),
        |candidate| ctx.profiles.contains_key(candidate),
        ctx.dependencies,
        path,
        first,
        tail,
    ) {
        WorkspaceCrateMatch::Resolved(namespace, _) => return relative_import(namespace),
        WorkspaceCrateMatch::Isolated => return None,
        WorkspaceCrateMatch::Missing => {}
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
        root_rust_crate.and_then(|name| normalize_root_crate_import(name, module))
    {
        return Some(normalized);
    }
    profile.normalize_import_with_root(ctx.root, path, module)
}

pub(crate) struct RustLinker;
pub(crate) static RUST_LINKER: RustLinker = RustLinker;
impl LanguageLinker for RustLinker {
    fn callable_marker_evidence(
        &self,
        receiver: &crate::linker::value_flow::TypeTarget<'_>,
    ) -> Option<&'static str> {
        matches!(receiver, crate::linker::value_flow::TypeTarget::Builtin(name) if name == "__rust_closure")
            .then_some("invocation of a lexically proven Rust closure")
    }

    fn identity_call_global(&self, callee: &str) -> Option<&'static str> {
        (callee == "__rust_identity_some").then_some("Some")
    }

    fn filter_call_candidates(
        &self,
        member_state: Option<&dyn crate::languages::profile::LanguageMemberIndex>,
        edge_kind: &str,
        candidates: &mut Vec<&Node>,
    ) {
        let Some(state) = member_state else {
            return;
        };
        if candidates.len() > 1
            && (edge_kind == EdgeKind::Calls.as_str() || edge_kind == EdgeKind::Handles.as_str())
            && candidates.iter().any(|node| state.is_inherent_member(node))
            && candidates.iter().any(|node| !state.is_inherent_member(node))
        {
            candidates.retain(|node| state.is_inherent_member(node));
        }
    }

    fn admits_dynamic_member(
        &self,
        member_state: Option<&dyn crate::languages::profile::LanguageMemberIndex>,
        dynamic: bool,
        node: &Node,
    ) -> bool {
        !dynamic || member_state.is_some_and(|state| state.is_inherent_member(node))
    }

    fn value_projection(
        &self,
        member: &str,
    ) -> Option<crate::languages::profile::LanguageValueProjection> {
        use crate::languages::profile::LanguageValueProjection as Projection;
        Some(match member {
            "__rust_generic_call" => Projection::GenericCall,
            "__rust_array" => Projection::Array,
            "__rust_tuple" => Projection::Tuple,
            "__rust_if_join" => Projection::BranchJoin,
            "__rust_ref" => Projection::Reference { mutable: false },
            "__rust_ref_mut" => Projection::Reference { mutable: true },
            field if field.starts_with("__rust_field:") => {
                Projection::Field(field["__rust_field:".len()..].to_owned())
            }
            adapter if super::value_flow::is_adapter(adapter) => Projection::Adapter,
            _ => return None,
        })
    }

    fn construct_value<'a>(
        &self,
        _owner: &'a Node,
        callee: &str,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        (callee == "__rust_closure!")
            .then(|| crate::linker::value_flow::TypeTarget::Builtin("__rust_closure".to_owned()))
    }

    fn evaluate_call_fallback<'a>(
        &self,
        eval: &crate::languages::profile::CallEvalContext<'_, 'a, '_>,
        callee: &str,
        _site: Option<&crate::semantic::CallSiteEvidence>,
    ) -> Option<crate::linker::value_flow::TypeTarget<'a>> {
        super::value_flow::constructor_result(
            eval.index,
            eval.owner,
            callee,
            eval.at,
            eval.resolver,
            eval.trace,
        )
    }

    fn local_receiver_class<'a>(
        &self,
        target: &crate::linker::value_flow::TypeTarget<'a>,
    ) -> Option<&'a Node> {
        super::value_flow::local_class(target)
    }

    fn unwrap_reference<'t, 'a>(
        &self,
        target: &'t crate::linker::value_flow::TypeTarget<'a>,
    ) -> &'t crate::linker::value_flow::TypeTarget<'a> {
        super::value_flow::referent(target).0
    }

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
        if context.language_state.is_some_and(|state| {
            context.normalized.is_some_and(|normalized| {
                !state.module_path_visible(&normalized.namespace, requester)
            })
        }) {
            context.modules.clear();
            context.candidates.clear();
            return ImportResolution::default();
        }
        let mut resolution = resolve_default_import(context);
        context.candidates.retain(|node| {
            node.kind != "impl"
                && (node.kind == "module"
                    && context.language_state.is_none_or(|state| {
                        state.module_path_visible(&node.qualname, requester)
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
        let Some(forwarding) = context.language_state else {
            return resolution;
        };
        let requester_node = context
            .by_module
            .get(context.path)
            .into_iter()
            .flatten()
            .find(|node| node.kind == "module");
        if context.modules.is_empty() && context.normalized.is_none() {
            let raw = context
                .reference
                .module
                .as_deref()
                .unwrap_or(&context.reference.expression);
            if let Some(req_mod) = requester_node.filter(|_| raw.contains("::")) {
                let resolved = forwarding.resolve_path(
                    req_mod,
                    &raw.replace("::", "."),
                    requester,
                    Some(context.by_qual),
                );
                if !resolved.is_empty() {
                    context.modules.push(req_mod);
                    context.candidates.extend(resolved);
                    resolution.reexport_symbol = true;
                }
            }
            return resolution;
        }
        let Some(name) = context
            .tail()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
        else {
            return resolution;
        };
        if context.modules.len() != 1 {
            return resolution;
        }
        let module = context.modules[0];
        if module.language != "rust" {
            return resolution;
        }
        let requester = requester_node.map_or(module.qualname.as_str(), |owner| owner.qualname.as_str());
        context.candidates.extend(forwarding.resolve_path(
            module,
            &name,
            requester,
            Some(context.by_qual),
        ));
        if !context.candidates.is_empty()
            && context
                .candidates
                .iter()
                .all(|node| node.kind != "module" || node.path != module.path)
        {
            resolution.reexport_symbol = true;
        }
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
        _node_rebindings: &NodeBindingIndex,
        all: &'a dyn crate::models::FactsLookup,
        _modules_by_namespace: &ModulesByNamespace<'a>,
        _by_module: &HashMap<&'a str, Vec<&'a Node>>,
        _root: Option<&std::path::Path>,
    ) -> PackageExports<'a> {
        declared_exports(all.values_facts(), interner)
    }

    fn required_full_facts(
        &self,
        facts: &dyn crate::models::FactsLookup,
        affected: &std::collections::BTreeSet<String>,
        catalog: &[(&str, &str)],
    ) -> std::collections::BTreeSet<String> {
        super::profile_linker::required_full_facts(facts, affected, catalog)
    }
}
