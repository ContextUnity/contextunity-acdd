use super::context::*;
use crate::languages::profile::ProfileLinker;
use crate::languages::{LanguageFamily, LanguageProfile};
use crate::linker::{
    semantic_context,
    traits::{
        import_provider_modules, ImportContext, ImportResolution, ImportScopeState, LanguageLinker,
        GENERIC_LINKER,
    },
    types::qualified,
};
use crate::models::tags::{CoverageStatus, EdgeConfidence, EdgeKind, NodeKind};
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

fn apply_wildcard_import<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    language_linker: &dyn LanguageLinker,
    item: &ImportItem<'a, 'file, '_>,
) where
    'a: 'file,
{
    let mut state = ImportScopeState {
        input: resolver.input,
        path: resolver.path,
        file_module: resolver.file_module,
        facts: resolver.facts,
        references: resolver.references,
        aliases: &mut resolver.aliases,
        external_aliases: &mut resolver.external_aliases,
        lookup_key: &mut resolver.lookup_key,
    };
    if language_linker.apply_wildcard_import(
        &mut state,
        item.r,
        item.profile,
        item.scope,
        &item.modules,
    ) {
        return;
    }
    let input = resolver.input;
    for &target_module in &item.modules {
        if let Some(module_nodes) = input.by_module.get(target_module.path.as_str()) {
            for n in module_nodes {
                if n.kind != crate::models::tags::NodeKind::Module && !n.name.is_empty() {
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
        if let Some(target_facts) = input.all.get_facts(&target_module.path) {
            for import_ref in target_facts.references.iter().filter(|import| {
                import.kind == EdgeKind::Imports.as_str() && import.source == target_module.id
            }) {
                let Some(name) = import_ref.alias.as_deref().filter(|alias| *alias != "*") else {
                    continue;
                };
                let locally_declared = input
                    .by_qual
                    .get(qualified(&mut resolver.lookup_key, item.scope, name))
                    .is_some_and(|nodes| {
                        nodes.iter().any(|n| {
                            n.path == *resolver.path
                                && n.kind != crate::models::tags::NodeKind::ModuleDeclaration
                        })
                    });
                let already_bound = locally_declared
                    || resolver
                        .aliases
                        .get(item.scope)
                        .is_some_and(|entries| entries.contains_key(name))
                    || resolver.references.iter().any(|reference| {
                        reference.kind == EdgeKind::Imports.as_str()
                            && reference.source == item.r.source
                            && reference.alias.as_deref() == Some(name)
                    });
                if already_bound {
                    continue;
                }
                if let Some(target_ext) =
                    external_origin_at(input, target_module, name, item.scope, item.r.line, true)
                {
                    resolver
                        .aliases
                        .entry(item.scope)
                        .or_default()
                        .entry(name)
                        .or_default();
                    resolver
                        .external_aliases
                        .entry(item.scope)
                        .or_default()
                        .insert(name, Some(target_ext));
                }
            }
        }
    }
}

fn external_origin_at<'a>(
    input: &super::ResolutionInput<'a>,
    module: &'a Node,
    name: &str,
    requester_qual: &str,
    line: usize,
    enforce_visibility: bool,
) -> Option<(String, usize)> {
    match input.semantic_context.imported_alias(module, name) {
        Some(semantic_context::Imported::External(origin, _, reference, _)) => {
            if enforce_visibility
                && input
                    .semantic_context
                    .import_state(module.language.as_str())
                    .is_some_and(|state| {
                        !state.external_binding_visible(
                            module,
                            name,
                            requester_qual,
                            reference.source == module.id,
                        )
                    })
            {
                return None;
            }
            Some((origin.replace("::", "."), line))
        }
        _ => None,
    }
}

fn apply_aliased_import<'a, 'file>(
    resolver: &mut FileResolver<'a, 'file>,
    language_linker: &dyn LanguageLinker,
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
            nodes.iter().any(|n| {
                n.path == *resolver.path
                    && n.kind != crate::models::tags::NodeKind::ModuleDeclaration
            })
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
                    .cloned()
                    .or_else(|| {
                        external_origin_at(
                            input,
                            target_module,
                            symbol_name,
                            item.scope,
                            item.r.line,
                            true,
                        )
                    });
            }
        }
        if targets.is_empty() && inherited_ext.is_none() {
            inherited_ext = input
                .interner
                .lookup_node(input.by_id, item.r.source.as_str())
                .and_then(|owner| {
                    external_origin_at(input, owner, alias, item.scope, item.r.line, false)
                });
        }
        if targets.is_empty() {
            if let Some(target_ext) = inherited_ext {
                resolver
                    .external_aliases
                    .entry(item.scope)
                    .or_default()
                    .insert(alias, Some(target_ext));
            }
        }
    }
    if targets.is_empty() && !local_declaration {
        let mut state = ImportScopeState {
            input: resolver.input,
            path: resolver.path,
            file_module: resolver.file_module,
            facts: resolver.facts,
            references: resolver.references,
            aliases: &mut resolver.aliases,
            external_aliases: &mut resolver.external_aliases,
            lookup_key: &mut resolver.lookup_key,
        };
        language_linker.apply_aliased_external_fallback(
            &mut state,
            item.r,
            item.normalized,
            item.scope,
            alias,
        );
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
    language_linker: &dyn LanguageLinker,
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
        .and_then(|owner| {
            language_linker.import_edge_owner(owner, input.by_id, input.interner, path)
        })
        .unwrap_or(r.source.as_str());
    if item.modules.len() == 1 {
        resolver.graph.push_edge(
            path,
            import_owner,
            &item.modules[0].id,
            EdgeKind::Imports,
            r.line,
            r.expression.as_str(),
            EdgeConfidence::Exact,
        );
        if let [declaration] = item.candidates.as_slice() {
            if language_linker.admits_imported_declaration_edge(declaration, item.modules[0]) {
                resolver.graph.push_edge(
                    path,
                    import_owner,
                    &declaration.id,
                    EdgeKind::Imports,
                    r.line,
                    r.expression.as_str(),
                    EdgeConfidence::Exact,
                );
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
                EdgeKind::Imports,
                r.line,
                r.expression.as_str(),
                EdgeConfidence::Exact,
            );
        }
    }
    if item.modules.len() == 1 || (multi_file_allowed && resolved) {
        if item.child_module {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.candidates[0].id,
                crate::models::tags::EdgeKind::Imports,
                r.line,
                r.expression.as_str(),
                crate::models::tags::EdgeConfidence::Exact,
            );
        }
        if item.stub_symbol {
            if let Some(stub) = item.paired_stub {
                resolver.graph.push_edge(
                    path,
                    import_owner,
                    &stub.id,
                    crate::models::tags::EdgeKind::Imports,
                    r.line,
                    r.expression.as_str(),
                    crate::models::tags::EdgeConfidence::Exact,
                );
            }
        }
        if item.reexport_symbol {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.candidates[0].id,
                EdgeKind::Imports,
                r.line,
                r.expression.as_str(),
                EdgeConfidence::Exact,
            );
        } else if language_linker.emits_single_imported_target_edge()
            && !item.child_module
            && !item.stub_symbol
            && item.imported_targets.len() == 1
        {
            resolver.graph.push_edge(
                path,
                import_owner,
                &item.imported_targets[0].id,
                crate::models::tags::EdgeKind::Imports,
                r.line,
                r.expression.as_str(),
                crate::models::tags::EdgeConfidence::Exact,
            );
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
            language_state: input
                .semantic_context
                .import_state(profile.map_or("", LanguageProfile::id)),
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
        candidates.retain(|n| n.kind != NodeKind::Impl);
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
            m.detail_entry(crate::models::details::NodeDetailKey::EXPORTS)
                .as_array()
                .is_some_and(|bindings| {
                    bindings.iter().any(|b| {
                        b["name"].as_str() == Some(r.expression.as_str()) && b["type_only"] == true
                    })
                })
        });
        let is_type_only = matches!(r.receiver_hint.as_ref(), Some(ReceiverHint::TypeOnlyImport))
            || export_type_only;
        let unproven_guard = is_type_only
            && profile.is_some_and(|profile| {
                language_linker.type_only_import_is_unproven(
                    profile,
                    path,
                    facts,
                    input.modules_by_namespace,
                    input.dependency_registry,
                    input.interner,
                    input.node_rebindings,
                )
            });
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
            apply_wildcard_import(resolver, language_linker, &item);
        } else if let Some(alias) = r.alias.as_deref() {
            apply_aliased_import(resolver, language_linker, &mut item, alias);
        }
        let multi_file_allowed = language_linker.allows_multi_file_modules();
        let resolved = emit_import_coverage(resolver, multi_file_allowed, &item);
        emit_import_edges(
            resolver,
            language_linker,
            multi_file_allowed,
            resolved,
            &item,
        );
    }
    let mut state = ImportScopeState {
        input: resolver.input,
        path: resolver.path,
        file_module: resolver.file_module,
        facts: resolver.facts,
        references: resolver.references,
        aliases: &mut resolver.aliases,
        external_aliases: &mut resolver.external_aliases,
        lookup_key: &mut resolver.lookup_key,
    };
    language_linker.apply_module_star_reexports(&mut state);
}
