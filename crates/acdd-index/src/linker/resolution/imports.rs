use super::context::*;
use crate::languages::{self, LanguageFamily, LanguageProfile};
use crate::linker::{
    semantic_context,
    traits::{import_provider_modules, ImportContext, ImportResolution, LanguageLinker, GENERIC_LINKER},
    types::qualified,
};
use crate::models::tags::{CoverageStatus, EdgeConfidence, EdgeKind, LanguageId, NodeKind};
use crate::models::{Node, ReceiverHint, Reference};
use std::borrow::Cow;

struct ImportItem<'a, 'file, 'norm> {
    r: &'a Reference,
    profile: Option<&'static dyn LanguageProfile>,
    normalized: Option<&'norm crate::languages::ImportPath>,
    symbol: String,
    scope: &'file str,
    modules: Vec<&'a Node>,
    candidates: Vec<&'a Node>,
    imported_targets: Vec<&'a Node>,
    paired_stub: Option<&'a Node>,
    child_module: bool,
    stub_symbol: bool,
    reexport_symbol: bool,
    external: Option<String>,
    is_wildcard: bool,
    is_type_only: bool,
    unproven_guard: bool,
}

#[cfg(feature = "lang-rust")]
fn apply_rust_parent_wildcard<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    r: &Reference,
    profile: Option<&'static dyn LanguageProfile>,
    scope: &'file str,
    target_module: &'a Node,
) where
    'a: 'file,
{
    let input = resolver.input;
    let path = resolver.path;
    let file_module = resolver.file_module;
    let Some(target_facts) = input.all.get_facts(&target_module.path) else {
        return;
    };
    let workspace = languages::workspace_path(path).0;
    for parent_import in target_facts.references.iter().filter(|import| {
        import.kind == "imports"
            && import.source == target_module.id
            && import.alias.as_deref().is_some_and(|alias| alias != "*")
    }) {
        let module = parent_import
            .module
            .as_deref()
            .unwrap_or(&parent_import.expression);
        let Some(provider) = input
            .normalized_imports
            .get(&(target_module.path.as_str(), module))
            .and_then(Option::as_ref)
        else {
            continue;
        };
        let alias = parent_import.alias.as_deref().unwrap_or_default();
        if input
            .interner
            .lookup_node(input.by_id, r.source.as_str())
            .is_some_and(|owner| owner.kind != "module")
            && (resolver.references.iter().any(|reference| {
                reference.kind == "imports"
                    && reference.source == r.source
                    && reference.alias.as_deref() == Some(alias)
            }) || input
                .by_qual
                .get(qualified(&mut resolver.lookup_key, scope, alias))
                .is_some_and(|nodes| nodes.iter().any(|node| node.path == *path)))
        {
            continue;
        }
        if target_facts
            .references
            .iter()
            .filter(|import| {
                import.kind == "imports"
                    && import.source == target_module.id
                    && import.alias.as_deref() == Some(alias)
            })
            .count()
            != 1
        {
            continue;
        }
        let mut local_providers: Vec<&Node> = input
            .by_qual
            .get(provider.namespace.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| {
                node.language == "rust" && languages::workspace_path(&node.path).0 == workspace
            })
            .collect();
        if local_providers.is_empty() {
            local_providers.extend(
                input
                    .rust_forwarding
                    .targets(target_module, alias, file_module)
                    .iter()
                    .copied(),
            );
        }
        if !local_providers.is_empty() {
            resolver
                .aliases
                .entry(scope)
                .or_default()
                .entry(alias)
                .or_default()
                .extend(local_providers);
        } else if !provider.relative
            && !resolver
                .aliases
                .get(scope)
                .is_some_and(|entries| entries.contains_key(alias))
            && profile
                .and_then(|p| {
                    input.dependency_registry.classification_for_path(
                        p,
                        module,
                        &target_module.path,
                    )
                })
                .is_some()
        {
            resolver.aliases.entry(scope).or_default().entry(alias).or_default();
            resolver
                .external_aliases
                .entry(scope)
                .or_default()
                .insert(alias, Some((provider.namespace.clone(), r.line)));
        }
    }
}

#[cfg(feature = "lang-rust")]
fn apply_rust_scope_wildcard_bindings<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    r: &Reference,
    scope: &'file str,
    target_module: &'a Node,
) where
    'a: 'file,
{
    let input = resolver.input;
    for (name, targets) in input
        .rust_forwarding
        .visible_bindings(target_module, resolver.file_module)
    {
        if resolver.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.source == r.source
                && reference.alias.as_deref() == Some(name)
        }) {
            continue;
        }
        if input
            .by_qual
            .get(qualified(&mut resolver.lookup_key, scope, name))
            .is_some_and(|nodes| nodes.iter().any(|node| node.path == *resolver.path))
        {
            resolver.aliases.entry(scope).or_default().remove(name);
            continue;
        }
        use hashbrown::hash_map::Entry;
        match resolver.aliases.entry(scope).or_default().entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(targets.to_vec());
            }
            Entry::Occupied(mut entry) => {
                if entry.get().is_empty() || targets.is_empty() {
                    entry.get_mut().clear();
                } else {
                    entry.get_mut().extend_from_slice(targets);
                    entry.get_mut().sort_by_key(|node| node.id.as_str());
                    entry.get_mut().dedup_by_key(|node| node.id.as_str());
                }
                if !targets.is_empty() {
                    if let Some(origin) = resolver
                        .external_aliases
                        .get_mut(scope)
                        .and_then(|bindings| bindings.get_mut(name))
                    {
                        *origin = None;
                    }
                }
            }
        }
    }
}

fn apply_wildcard_import<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    item: &ImportItem<'a, 'file, '_>,
) where
    'a: 'file,
{
    let input = resolver.input;
    for &target_module in &item.modules {
        if let Some(module_nodes) = input
            .by_module
            .get(target_module.path.as_str())
            .filter(|_| !item.profile.is_some_and(|profile| profile.id() == "rust"))
        {
            for n in module_nodes {
                if n.kind != "module" && !n.name.is_empty() {
                    resolver
                        .aliases
                        .entry(item.scope)
                        .or_default()
                        .entry(n.name.as_str())
                        .or_default()
                        .push(n);
                }
            }
        }
        #[cfg(feature = "lang-rust")]
        if item.modules.len() == 1
            && item.profile.is_some_and(|profile| profile.id() == "rust")
            && resolver
                .file_module
                .strip_prefix(&target_module.qualname)
                .is_some_and(|suffix| suffix.starts_with('.'))
        {
            apply_rust_parent_wildcard(resolver, item.r, item.profile, item.scope, target_module);
        }
    }
    #[cfg(feature = "lang-rust")]
    if item.profile.is_some_and(|profile| profile.id() == "rust")
        && item.modules.len() == 1
        && input
            .interner
            .lookup_node(input.by_id, item.r.source.as_str())
            .is_some_and(|owner| owner.kind != "module")
    {
        apply_rust_scope_wildcard_bindings(resolver, item.r, item.scope, item.modules[0]);
    }
}

fn apply_aliased_import<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    item: &mut ImportItem<'a, 'file, '_>,
    alias: &'a str,
) where
    'a: 'file,
{
    let input = resolver.input;
    let local_declaration = input
        .by_qual
        .get(qualified(&mut resolver.lookup_key, item.scope, alias))
        .is_some_and(|nodes| {
            nodes
                .iter()
                .any(|n| n.path == *resolver.path && n.kind != "module_declaration")
        });
    let mut targets = if local_declaration {
        Vec::new()
    } else {
        item.candidates.clone()
    };
    let symbol_name = item.symbol.rsplit('.').next().unwrap_or(&item.symbol);
    if targets.is_empty() && !local_declaration {
        let mut inherited_ext = None;
        for target_module in &item.modules {
            let target_scope = target_module.qualname.as_str();
            if let Some(target_alias) = resolver
                .aliases
                .get(target_scope)
                .and_then(|entries| entries.get(symbol_name))
            {
                targets.extend(target_alias.iter().copied());
            }
            if inherited_ext.is_none() {
                inherited_ext = resolver
                    .external_aliases
                    .get(target_scope)
                    .and_then(|entries| entries.get(symbol_name))
                    .and_then(Option::as_ref)
                    .cloned();
            }
        }
        if let Some(target_ext) = inherited_ext {
            resolver
                .external_aliases
                .entry(item.scope)
                .or_default()
                .insert(alias, Some(target_ext));
        }
    }
    #[cfg(feature = "lang-rust")]
    if item.profile.is_some_and(|profile| profile.id() == "rust")
        && targets.is_empty()
        && !local_declaration
        && item.normalized.is_some_and(|import| import.relative)
    {
        if let Some(semantic_context::Imported::External(origin, position, _, _)) = input
            .interner
            .lookup_symbol(&item.r.source)
            .and_then(|owner| input.semantic_context.imports.get(&owner))
            .and_then(|bindings| {
                input
                    .interner
                    .lookup_symbol(alias)
                    .and_then(|name| bindings.get(&name))
            })
        {
            resolver
                .external_aliases
                .entry(item.scope)
                .or_default()
                .insert(alias, Some((origin.replace("::", "."), position.line)));
        }
    }
    let alias_taken = resolver
        .aliases
        .get(item.scope)
        .is_some_and(|entries| entries.contains_key(alias));
    if alias_taken {
        if let Some(provenance) = resolver
            .external_aliases
            .get_mut(item.scope)
            .and_then(|entries| entries.get_mut(alias))
        {
            *provenance = None;
        }
    } else if item.external.is_some() && !local_declaration {
        resolver
            .external_aliases
            .entry(item.scope)
            .or_default()
            .insert(alias, Some((item.symbol.clone(), item.r.line)));
    }
    if item.is_type_only && !item.unproven_guard {
        resolver
            .type_only_aliases
            .entry(item.scope)
            .or_default()
            .insert(alias);
    }
    if !item.unproven_guard {
        resolver
            .aliases
            .entry(item.scope)
            .or_default()
            .entry(alias)
            .or_default()
            .extend(targets.clone());
        item.imported_targets = targets;
    }
}

fn emit_import_coverage<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    multi_file_allowed: bool,
    item: &ImportItem<'a, 'file, '_>,
) -> bool
where
    'a: 'file,
{
    let path = resolver.path;
    let r = item.r;
    let resolved = !item.unproven_guard
        && !item.modules.is_empty()
        && ((item.modules.len() == 1
            && (r.alias.is_none()
                || item.is_wildcard
                || item.candidates.len() == 1
                || item.imported_targets.len() == 1))
            || (multi_file_allowed
                && (item.candidates.len() == 1
                    || item.imported_targets.len() == 1
                    || item.is_wildcard
                    || r.alias.is_none()
                    || (item.normalized.is_some_and(|n| !n.symbol_path)
                        && item.candidates.len() == item.modules.len()))));
    let is_external_alias = r.alias.as_deref().is_some_and(|a| {
        resolver
            .external_aliases
            .get(item.scope)
            .and_then(|e| e.get(a))
            .is_some_and(Option::is_some)
    });
    let status = if item.unproven_guard {
        CoverageStatus::Unresolved.as_str()
    } else if resolved {
        CoverageStatus::Resolved.as_str()
    } else if (!multi_file_allowed && item.modules.len() > 1)
        || item.candidates.len() > 1
        || item.imported_targets.len() > 1
    {
        CoverageStatus::Ambiguous.as_str()
    } else if item.external.is_some() || is_external_alias {
        CoverageStatus::External.as_str()
    } else {
        CoverageStatus::Unresolved.as_str()
    };
    let evidence: Cow<'static, str> = if item.unproven_guard {
        "type guard condition is unproven or shadowed".into()
    } else if item.is_wildcard && resolved {
        format!(
            "wildcard import from {}: imported from {}",
            item.symbol, item.modules[0].path
        )
        .into()
    } else if item.reexport_symbol {
        format!("import {}: explicit local package re-export", item.symbol).into()
    } else if item.stub_symbol {
        format!(
            "import {}: declaration from paired type stub; runtime declaration not indexed",
            item.symbol
        )
        .into()
    } else if let Some(kind) = item.external.as_deref() {
        format!("{kind}; no indexed provider for {}", item.symbol).into()
    } else if item.modules.is_empty() && item.normalized.is_some_and(|n| !n.relative) {
        format!(
            "no indexed provider for absolute import {}; external dependency or missing source remains unverified",
            item.symbol
        )
        .into()
    } else {
        format!(
            "import {}: {} modules, {} alias targets",
            item.symbol,
            item.modules.len(),
            item.candidates.len()
        )
        .into()
    };
    resolver.graph.reference_coverage(path, r, status, evidence);
    resolved
}

fn emit_import_edges<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    multi_file_allowed: bool,
    resolved: bool,
    item: &ImportItem<'a, 'file, '_>,
) where
    'a: 'file,
{
    let input = resolver.input;
    let path = resolver.path;
    let r = item.r;
    let import_owner = input
        .interner
        .lookup_node(input.by_id, r.source.as_str())
        .filter(|owner| {
            owner.language == LanguageId::Html.as_str()
                || (owner.kind == NodeKind::TemplateScope.as_str()
                    && owner.details["embedded_language"] == LanguageId::Javascript.as_str())
        })
        .and_then(|owner| owner.details["html_import_owner"].as_str())
        .filter(|id| {
            input.interner.lookup_node(input.by_id, id).is_some_and(|node| {
                node.kind == NodeKind::Module.as_str()
                    && node.language == LanguageId::Html.as_str()
                    && node.path == *path
            })
        })
        .unwrap_or(r.source.as_str());
    if item.modules.len() == 1 {
        resolver.graph.push_edge(
            path,
            import_owner,
            &item.modules[0].id,
            EdgeKind::Imports.as_str(),
            r.line,
            r.expression.as_str(),
            EdgeConfidence::Exact.as_str(),
        );
        if item
            .profile
            .is_some_and(|profile| profile.id() == LanguageId::Typescript.as_str())
        {
            if let [declaration] = item.candidates.as_slice() {
                if matches!(
                    declaration.kind.as_str(),
                    k if k == NodeKind::Type.as_str() || k == NodeKind::Interface.as_str()
                ) && declaration.path == item.modules[0].path
                {
                    resolver.graph.push_edge(
                        path,
                        import_owner,
                        &declaration.id,
                        EdgeKind::Imports.as_str(),
                        r.line,
                        r.expression.as_str(),
                        EdgeConfidence::Exact.as_str(),
                    );
                }
            }
        }
    } else if multi_file_allowed && resolved {
        let single_path = match item.candidates.as_slice() {
            [declaration] => Some(declaration.path.as_str()),
            _ => None,
        };
        for target_module in item
            .modules
            .iter()
            .copied()
            .filter(|m| single_path.is_none_or(|p| m.path == p))
        {
            resolver.graph.push_edge(
                path,
                import_owner,
                &target_module.id,
                EdgeKind::Imports.as_str(),
                r.line,
                r.expression.as_str(),
                EdgeConfidence::Exact.as_str(),
            );
        }
    }
    if item.modules.len() == 1 || (multi_file_allowed && resolved) {
        if item.child_module {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.candidates[0].id,
                "imports",
                r.line,
                r.expression.as_str(),
                "exact",
            );
        }
        if item.stub_symbol {
            if let Some(stub) = item.paired_stub {
                resolver.graph.push_edge(
                    path,
                    import_owner,
                    &stub.id,
                    "imports",
                    r.line,
                    r.expression.as_str(),
                    "exact",
                );
            }
        }
        if item.reexport_symbol {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.candidates[0].id,
                EdgeKind::Imports.as_str(),
                r.line,
                r.expression.as_str(),
                EdgeConfidence::Exact.as_str(),
            );
        } else if item.profile.is_some_and(|profile| profile.id() == "python")
            && !item.child_module
            && !item.stub_symbol
            && item.imported_targets.len() == 1
        {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.imported_targets[0].id,
                "imports",
                r.line,
                r.expression.as_str(),
                "exact",
            );
        }
    }
}

#[cfg(feature = "lang-rust")]
fn apply_rust_module_star_reexports<'a, 'file>(resolver: &mut FileResolver<'a, 'file>)
where
    'a: 'file,
{
    let input = resolver.input;
    let Some(module) = resolver
        .facts
        .nodes
        .iter()
        .find(|node| node.kind == "module" && node.language == "rust")
    else {
        return;
    };
    if resolver.references.iter().any(|reference| {
        reference.kind == "imports"
            && reference.source == module.id
            && reference.alias.as_deref() == Some("*")
    }) {
        let bindings = resolver.aliases.entry(module.qualname.as_str()).or_default();
        for (name, targets) in input
            .rust_forwarding
            .visible_bindings(module, &module.qualname)
        {
            if input
                .by_qual
                .get(qualified(&mut resolver.lookup_key, &module.qualname, name))
                .is_some_and(|nodes| {
                    nodes
                        .iter()
                        .any(|node| node.path == *resolver.path && node.kind != "module_declaration")
                })
            {
                bindings.remove(name);
            } else {
                bindings.insert(name, targets.to_vec());
            }
        }
    }
    let bindings = resolver.aliases.entry(module.qualname.as_str()).or_default();
    if let Some(imports) = input
        .interner
        .lookup_symbol(&module.id)
        .and_then(|id| input.semantic_context.imports.get(&id))
    {
        for imported in imports.values() {
            if let semantic_context::Imported::External(origin, position, reference, _) = imported {
                if let Some(name) = reference.alias.as_deref() {
                    if bindings.get(name).is_none_or(Vec::is_empty) {
                        resolver
                            .external_aliases
                            .entry(module.qualname.as_str())
                            .or_default()
                            .entry(name)
                            .or_insert_with(|| Some((origin.replace("::", "."), position.line)));
                    }
                }
            }
        }
    }
}

pub(super) fn phase_imports<'a, 'file>(resolver: &mut FileResolver<'a, 'file>)
where
    'a: 'file,
{
    let input = resolver.input;
    let path = resolver.path;
    let facts = resolver.facts;
    let profile = input.profiles.get(path).copied();
    let language_linker = profile.map_or(&GENERIC_LINKER as &dyn LanguageLinker, |p| p.linker());
    let language_package_exports = input
        .package_exports
        .get(profile.map_or("", |profile| profile.id()))
        .expect("package export index exists for every linked language");
    for r in facts
        .references
        .iter()
        .filter(|r| r.kind == EdgeKind::Imports.as_str())
    {
        let callable_path = input.semantic_context.callable_import_path(path, r);
        let normalized = callable_path.as_ref().or_else(|| {
            input
                .normalized_imports
                .get(&(path, r.module.as_deref().unwrap_or(&r.expression)))
                .expect("imports are normalized before parallel resolution")
                .as_ref()
        });
        let mut modules = Vec::new();
        let mut selected_namespace = "";
        if let (Some(profile), Some(normalized)) = (profile, normalized) {
            (modules, selected_namespace) = import_provider_modules(
                path,
                normalized,
                profile.family(),
                input.modules_by_namespace,
            );
        }
        let symbol = normalized
            .as_ref()
            .map_or_else(|| r.expression.clone(), |n| n.namespace.clone());
        let mut candidates = Vec::new();
        let ImportResolution {
            paired_stub,
            child_module,
            stub_symbol,
            reexport_symbol,
        } = language_linker.resolve_import(&mut ImportContext {
            path,
            interner: input.interner,
            dependencies: input.dependency_registry,
            reference: r,
            #[cfg(feature = "lang-rust")]
            rust_forwarding: Some(input.rust_forwarding),
            normalized,
            selected_namespace,
            family: profile.map_or(LanguageFamily(""), |profile| profile.family()),
            modules: &mut modules,
            candidates: &mut candidates,
            modules_by_namespace: input.modules_by_namespace,
            by_module: input.by_module,
            by_qual: input.by_qual,
            package_exports: language_package_exports,
            lookup_key: &mut resolver.lookup_key,
        });
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        candidates.dedup_by_key(|n| &n.id);
        candidates.retain(|n| n.kind != NodeKind::Impl.as_str());
        crate::linker::traits::filter_stubs_and_overloads(&mut candidates);
        let external = if modules.is_empty() && normalized.as_ref().is_some_and(|n| !n.relative) {
            profile.and_then(|p| {
                input.dependency_registry.classification_for_path(
                    p,
                    r.module.as_deref().unwrap_or(&r.expression),
                    path,
                )
            })
        } else {
            None
        };
        let scope = input
            .interner
            .lookup_node(input.by_id, r.source.as_str())
            .map_or(resolver.file_module, |n| n.qualname.as_str());
        let is_wildcard = r.alias.as_deref() == Some("*")
            || (r.expression == "*" && r.alias.is_none())
            || r.module
                .as_deref()
                .is_some_and(|m| m.ends_with(".*") || m.ends_with("::*"));
        let export_type_only = modules.first().is_some_and(|m| {
            m.details["exports"].as_array().is_some_and(|bindings| {
                bindings.iter().any(|b| {
                    b["name"].as_str() == Some(r.expression.as_str()) && b["type_only"] == true
                })
            })
        });
        let is_type_only = matches!(r.receiver_hint.as_ref(), Some(ReceiverHint::TypeOnlyImport))
            || export_type_only;
        let unproven_guard = is_type_only
            && profile.is_some_and(|p| p.id() == "python")
            && !super::super::python::semantic_context::has_proven_type_checking(
                path,
                facts,
                input.modules_by_namespace,
                input.dependency_registry,
                profile.unwrap().family(),
                input.interner,
                input.node_rebindings,
            );
        let mut item = ImportItem {
            r,
            profile,
            normalized,
            symbol,
            scope,
            modules,
            candidates,
            imported_targets: Vec::new(),
            paired_stub,
            child_module,
            stub_symbol,
            reexport_symbol,
            external,
            is_wildcard,
            is_type_only,
            unproven_guard,
        };
        if item.is_wildcard {
            apply_wildcard_import(resolver, &item);
        } else if let Some(alias) = r.alias.as_deref() {
            apply_aliased_import(resolver, &mut item, alias);
        }
        let multi_file_allowed = language_linker.allows_multi_file_modules();
        let resolved = emit_import_coverage(resolver, multi_file_allowed, &item);
        emit_import_edges(resolver, multi_file_allowed, resolved, &item);
    }
    #[cfg(feature = "lang-rust")]
    apply_rust_module_star_reexports(resolver);
}
