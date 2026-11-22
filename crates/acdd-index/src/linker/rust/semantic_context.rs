use crate::languages;
#[cfg(feature = "lang-rust")]
use crate::languages::ImportPath;
#[cfg(feature = "lang-rust")]
use crate::languages::{dependency_registry::DependencyRegistry, LanguageProfile};
#[cfg(feature = "lang-rust")]
use crate::linker::traits::ModulesByNamespace;
use crate::linker::value_flow::{Symbol, TypeTarget};
use crate::models::{Facts, Node};
#[cfg(feature = "lang-rust")]
use hashbrown::HashMap;

use super::super::semantic_context::LanguageMemberLookup;
#[cfg(feature = "lang-rust")]
use super::super::semantic_context::{Context, Imported};

pub(in crate::linker) fn deref_target<'a>(
    lookup: &LanguageMemberLookup<'_, 'a>,
    receiver: &Node,
) -> Option<&'a Node> {
    #[cfg(feature = "lang-rust")]
    {
        lookup.rust_deref_target(receiver)
    }
    #[cfg(not(feature = "lang-rust"))]
    {
        let _ = (lookup, receiver);
        None
    }
}

pub(in crate::linker) fn receiver_symbol<'a>(
    lookup: Option<&LanguageMemberLookup<'_, 'a>>,
    owner: &'a Node,
    name: &str,
    mut resolve_type: impl FnMut(&str) -> Symbol<'a>,
) -> Option<Symbol<'a>> {
    if name == "Self" {
        #[cfg(feature = "lang-rust")]
        if let Some(receiver) = lookup.and_then(|lookup| lookup.rust_receiver(owner)) {
            return Some(Symbol::Type(TypeTarget::Local(receiver)));
        }
        if let Some(receiver_type) = owner.details["receiver_type"]
            .as_str()
            .filter(|name| *name != "Self")
        {
            let clean = super::super::receiver_type_name(receiver_type)
                .unwrap_or(std::borrow::Cow::Borrowed(receiver_type));
            return Some(resolve_type(clean.as_ref()));
        }
        return None;
    }
    #[cfg(feature = "lang-rust")]
    {
        let clean = name.replace("::", ".");
        if let Some((first, _)) = clean.split_once('.') {
            if (super::resolution::is_registered_cargo_crate(first)
                || crate::languages::rust::RUST.is_stdlib(first))
                && crate::languages::rust::is_registered_rust_type(name)
                && matches!(resolve_type(first), Symbol::Unknown)
            {
                return Some(Symbol::Type(TypeTarget::External {
                    module: name.replace('.', "::"),
                    import_line: 0,
                }));
            }
        }
    }
    #[cfg(feature = "lang-rust")]
    {
        let member = name
            .strip_prefix("Self::")
            .or_else(|| name.strip_prefix("Self."))?;
        let Some(lookup) = lookup else {
            return Some(Symbol::Unknown);
        };
        let Some(receiver) = lookup.rust_receiver(owner) else {
            return Some(Symbol::Unknown);
        };
        let methods = lookup.rust(receiver, member, true, owner);
        match methods {
            [target] => Some(Symbol::Callable(target)),
            [] => Some(Symbol::Unknown),
            _ => Some(Symbol::Ambiguous),
        }
    }
    #[cfg(not(feature = "lang-rust"))]
    {
        let _ = lookup;
        None
    }
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn qualified_external_annotation<'a>(
    profile: Option<&dyn LanguageProfile>,
    annotation: &'a str,
    head: &str,
    path: &str,
    dependencies: &DependencyRegistry,
) -> Option<super::super::ReceiverType<'a>> {
    let profile = profile.filter(|profile| profile.id() == "rust")?;
    if !dependencies.declares_for_path(profile.family(), path, head) {
        return None;
    }
    let original = annotation.trim().trim_start_matches('&').trim_start();
    let original = original.strip_prefix("mut ").unwrap_or(original);
    Some(match original.split_once("::") {
        Some((origin, _)) if origin == head => super::super::ReceiverType::External(origin, 0),
        _ => super::super::ReceiverType::Unknown,
    })
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn external_annotation_type<'a>(
    profile: Option<&dyn LanguageProfile>,
    raw_head: &'a str,
    head: &str,
    path: &str,
    dependencies: &DependencyRegistry,
) -> Option<super::super::ReceiverType<'a>> {
    let profile = profile.filter(|profile| profile.id() == "rust")?;
    (profile.external_import(head).is_some()
        || dependencies.declares_for_path(profile.family(), path, head))
    .then_some(super::super::ReceiverType::External(raw_head, 0))
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn import_globs<'a>(
    context: &mut Context<'a>,
    all: &'a dyn crate::FactsLookup,
    by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules: &'a ModulesByNamespace<'a>,
    rust_forwarding: &'a languages::rust::linker::RustForwarding<'a>,
) {
    let rust_modules: Vec<_> = all
        .values_facts()
        .filter_map(|facts| {
            facts
                .nodes
                .iter()
                .find(|node| node.kind == "module" && node.language == "rust")
                .map(|module| (facts, module))
        })
        .collect();

    fn rust_crate_root_prefix(path: &str) -> Option<&str> {
        if path.starts_with("src/bin/") || path.contains("/src/bin/") {
            None
        } else if path.starts_with("src/") {
            Some("")
        } else {
            path.split_once("/src/").map(|(prefix, _)| prefix)
        }
    }

    let mut module_by_qualname: HashMap<&'a str, &'a Node> = HashMap::new();
    let mut crate_roots: HashMap<&'a str, &'a Node> = HashMap::new();
    for &(_, module) in &rust_modules {
        module_by_qualname.insert(module.qualname.as_str(), module);
        let Some(prefix) = rust_crate_root_prefix(&module.path) else {
            continue;
        };
        let is_lib = module.path == "src/lib.rs" || module.path.ends_with("/src/lib.rs");
        let is_main = module.path == "src/main.rs" || module.path.ends_with("/src/main.rs");
        if is_lib {
            crate_roots.insert(prefix, module);
        } else if is_main {
            crate_roots.entry(prefix).or_insert(module);
        }
    }

    for &(_, module) in &rust_modules {
        let Some(module_id) = context.symbol_id(&module.id) else {
            continue;
        };
        let crate_root = rust_crate_root_prefix(&module.path)
            .and_then(|prefix| crate_roots.get(prefix).copied());
        let super_module = module.qualname.rsplit_once('.').and_then(|(parent, _)| {
            module_by_qualname.get(parent).copied().or_else(|| {
                let prefix = rust_crate_root_prefix(&module.path)?;
                let src_ns = if prefix.is_empty() {
                    "src".to_owned()
                } else {
                    format!("{}.src", prefix.replace('/', "."))
                };
                if parent == src_ns {
                    crate_root
                } else {
                    None
                }
            })
        });
        let zero_pos = crate::semantic::SourcePosition { line: 0, column: 0 };
        let anchors: [Option<(crate::interner::SymbolId, &'a Node)>; 3] = [
            ("self", Some(module)),
            ("crate", crate_root),
            ("super", super_module),
        ]
        .map(|(anchor, target)| {
            target.and_then(|node| context.symbol_id(anchor).map(|id| (id, node)))
        });
        let aliases = context.imports.entry(module_id).or_default();
        for (anchor_id, target) in anchors.into_iter().flatten() {
            aliases
                .entry(anchor_id)
                .or_insert_with(|| Imported::Nodes(vec![target], zero_pos, false));
        }
    }

    for &(facts, module) in &rust_modules {
        let Some(reference) = facts.references.iter().find(|reference| {
            reference.kind == "imports"
                && reference.source == module.id
                && reference.alias.as_deref() == Some("*")
        }) else {
            continue;
        };
        let glob_position = reference.position();
        let module_id = context
            .symbol_id(&module.id)
            .expect("Rust module id is interned");
        for (name, targets) in rust_forwarding.visible_bindings(module, &module.qualname) {
            let Some(name_id) = context.symbol_id(name) else {
                continue;
            };
            let aliases = context.imports.entry(module_id).or_default();
            if facts.nodes.iter().any(|node| {
                node.kind != "module_declaration"
                    && node
                        .qualname
                        .rsplit_once('.')
                        .is_some_and(|(scope, candidate)| {
                            scope == module.qualname && candidate == name
                        })
            }) {
                aliases.remove(&name_id);
                continue;
            }
            if targets.is_empty() && matches!(aliases.get(&name_id), Some(Imported::External(..))) {
                continue;
            }
            let position = match aliases.get(&name_id) {
                Some(Imported::Nodes(_, position, _)) => *position,
                _ => glob_position,
            };
            aliases.insert(
                name_id,
                if targets.is_empty() {
                    Imported::Unknown
                } else {
                    Imported::Nodes(targets.to_vec(), position, false)
                },
            );
        }
    }

    for (facts, module) in rust_modules.clone() {
        for reference in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && reference.source != module.id
                && reference.alias.as_deref() == Some("*")
        }) {
            let Some(owner) = context
                .symbol_id(&reference.source)
                .and_then(|id| by_id.get(&id).copied())
            else {
                continue;
            };
            let raw = reference.module.as_deref().unwrap_or(&reference.expression);
            let Some(normalized) = normalized_imports
                .get(&(module.path.as_str(), raw))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            let providers = languages::rust::linker::modules_at_import(
                modules,
                languages::LanguageFamily("rust"),
                normalized,
                languages::workspace_path(&module.path).0,
                |namespace| rust_forwarding.module_path_visible(namespace, &module.qualname),
            );
            let [(provider, tail)] = providers.as_slice() else {
                continue;
            };
            if !matches!(tail.as_str(), "" | "*") {
                continue;
            }
            let owner_id = context
                .symbol_id(&owner.id)
                .expect("Rust import owner id is interned");
            for (name, targets) in rust_forwarding.visible_bindings(provider, &module.qualname) {
                if facts.references.iter().any(|candidate| {
                    candidate.kind == "imports"
                        && candidate.source == owner.id
                        && candidate.alias.as_deref() == Some(name)
                }) {
                    continue;
                }
                let Some(name_id) = context.symbol_id(name) else {
                    continue;
                };
                let aliases = context.imports.entry(owner_id).or_default();
                if facts.nodes.iter().any(|node| {
                    node.qualname
                        .rsplit_once('.')
                        .is_some_and(|(scope, candidate)| {
                            scope == owner.qualname && candidate == name
                        })
                }) {
                    aliases.remove(&name_id);
                    continue;
                }
                match aliases.entry(name_id) {
                    hashbrown::hash_map::Entry::Vacant(entry) => {
                        entry.insert(if targets.is_empty() {
                            Imported::Unknown
                        } else {
                            Imported::Nodes(targets.to_vec(), reference.position(), false)
                        });
                    }
                    hashbrown::hash_map::Entry::Occupied(mut entry) => {
                        if let Imported::Nodes(nodes, _, _) = entry.get_mut() {
                            if targets.is_empty() {
                                entry.insert(Imported::Unknown);
                            } else {
                                nodes.extend_from_slice(targets);
                                nodes.sort_by_key(|node| node.id.as_str());
                                nodes.dedup_by_key(|node| node.id.as_str());
                            }
                        } else {
                            entry.insert(Imported::Unknown);
                        }
                    }
                }
            }
        }
    }

    // Build alias counts per owner across all import references to detect conflicting aliases
    let alias_counts = collect_external_alias_counts(all);
    for facts in all.values_facts() {
        for reference in facts.references.iter().filter(|r| r.kind == "imports") {
            let expr = reference.module.as_deref().unwrap_or(&reference.expression);
            let first = expr.split("::").next().unwrap_or(expr);
            let has_local_module = facts.nodes.iter().any(|node| {
                (node.kind == "module" || node.kind == "module_declaration")
                    && (node.name == first
                        || node
                            .qualname
                            .rsplit_once('.')
                            .is_some_and(|(_, n)| n == first))
            });
            if !has_local_module && super::resolution::is_registered_cargo_crate(first) {
                register_cargo_import_aliases(context, reference, expr, &alias_counts);
            }
        }
    }

    // Carry external provenance through ancestor imports independently of file order.
    // Only ancestor imports can expose private bindings; unrelated glob providers
    // continue to use the public forwarding projection above.
    propagate_ancestor_external_imports(
        context,
        &RustGlobImports {
            all,
            by_id,
            normalized_imports,
            modules,
            rust_forwarding,
            rust_modules: &rust_modules,
        },
    );
}

#[cfg(feature = "lang-rust")]
struct RustGlobImports<'a, 'b> {
    all: &'a dyn crate::FactsLookup,
    by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules: &'a ModulesByNamespace<'a>,
    rust_forwarding: &'a languages::rust::linker::RustForwarding<'a>,
    rust_modules: &'b [(&'a Facts, &'a Node)],
}

#[cfg(feature = "lang-rust")]
fn collect_external_alias_counts(
    all: &dyn crate::FactsLookup,
) -> hashbrown::HashMap<(&str, &str), usize> {
    let mut alias_counts = hashbrown::HashMap::new();
    for facts in all.values_facts() {
        for reference in facts.references.iter().filter(|r| r.kind == "imports") {
            let bound_alias = reference
                .alias
                .as_deref()
                .or_else(|| reference.expression.rsplit("::").next());
            if let Some(alias) = bound_alias.filter(|a| *a != "*") {
                *alias_counts
                    .entry((reference.source.as_str(), alias))
                    .or_default() += 1;
            }
        }
    }
    alias_counts
}

#[cfg(feature = "lang-rust")]
fn register_cargo_import_aliases<'a>(
    context: &mut Context<'a>,
    reference: &'a crate::models::Reference,
    expr: &str,
    alias_counts: &hashbrown::HashMap<(&str, &str), usize>,
) {
    let Some(owner_id) = context.symbol_id(&reference.source) else {
        return;
    };
    let Some(alias) = reference
        .alias
        .as_deref()
        .or_else(|| reference.expression.rsplit("::").next())
    else {
        return;
    };
    if alias == "*" {
        if expr.trim_end_matches("::*") == "rayon::prelude" {
            if let Some(namespace) = context.symbol_id(expr) {
                context.imports.entry(owner_id).or_default().insert(
                    namespace,
                    Imported::External(expr.to_owned(), reference.position(), reference, true),
                );
            }
        }
        return;
    }
    if alias_counts
        .get(&(reference.source.as_str(), alias))
        .copied()
        .unwrap_or(0)
        > 1
    {
        return;
    }
    if let Some(alias_id) = context.symbol_id(alias) {
        let aliases = context.imports.entry(owner_id).or_default();
        if matches!(aliases.get(&alias_id), None | Some(Imported::Unknown)) {
            aliases.insert(
                alias_id,
                Imported::External(expr.to_owned(), reference.position(), reference, true),
            );
        }
    }
}

#[cfg(feature = "lang-rust")]
fn find_owner_module<'a>(context: &Context<'a>, owner: &'a Node) -> Option<&'a Node> {
    let mut scope = owner.qualname.as_str();
    loop {
        if let Some(found) = context
            .nodes_in_path(scope, &owner.path)
            .find(|node| matches!(node.kind.as_str(), "module" | "module_declaration"))
        {
            return Some(found);
        }
        let (parent, _) = scope.rsplit_once('.')?;
        scope = parent;
    }
}

#[cfg(feature = "lang-rust")]
fn has_conflicting_local_binding(
    facts: &Facts,
    module_qualname: &str,
    reference: &crate::models::Reference,
    name: &str,
) -> bool {
    facts.references.iter().any(|candidate| {
        !std::ptr::eq(candidate, reference)
            && candidate.kind == "imports"
            && candidate.source == reference.source
            && candidate.alias.as_deref() == Some(name)
    }) || facts.nodes.iter().any(|node| {
        node.kind != "module_declaration"
            && node.kind != "impl"
            && node
                .qualname
                .rsplit_once('.')
                .is_some_and(|(scope, candidate)| scope == module_qualname && candidate == name)
    })
}

#[cfg(feature = "lang-rust")]
fn propagate_ancestor_external_imports<'a>(
    context: &mut Context<'a>,
    imports: &RustGlobImports<'a, '_>,
) {
    let RustGlobImports {
        all,
        by_id,
        normalized_imports,
        modules,
        rust_forwarding,
        rust_modules,
    } = *imports;
    let mut conflicts = hashbrown::HashSet::new();
    for _ in 0..64 {
        let mut additions = Vec::new();
        for facts in all.values_facts() {
            for reference in facts.references.iter().filter(|reference| {
                reference.kind == "imports"
                    && reference.alias.as_deref() == Some("*")
                    && reference
                        .module
                        .as_deref()
                        .unwrap_or(&reference.expression)
                        .trim_end_matches("::*")
                        == "super"
            }) {
                let Some(owner_id) = context.symbol_id(&reference.source) else {
                    continue;
                };
                let Some(owner) = by_id.get(&owner_id) else {
                    continue;
                };
                let Some(module) = find_owner_module(context, owner) else {
                    continue;
                };
                let namespace = if module.kind == "module" {
                    module
                        .qualname
                        .strip_suffix(".lib")
                        .or_else(|| module.qualname.strip_suffix(".main"))
                        .or_else(|| module.qualname.strip_suffix(".mod"))
                        .unwrap_or(&module.qualname)
                } else {
                    &module.qualname
                };
                let Some((parent, _)) = namespace.rsplit_once('.') else {
                    continue;
                };
                let parent_lib = format!("{parent}.lib");
                let parent_main = format!("{parent}.main");
                let parent_mod = format!("{parent}.mod");
                let mut providers: Vec<_> = [
                    parent,
                    parent_lib.as_str(),
                    parent_main.as_str(),
                    parent_mod.as_str(),
                ]
                    .into_iter()
                    .flat_map(|scope| {
                        context.nodes_in_path(scope, &owner.path).chain(
                            modules
                                .get(&languages::LanguageFamily("rust"))
                                .and_then(|modules| modules.get(scope))
                                .into_iter()
                                .flatten()
                                .copied(),
                        )
                    })
                    .filter(|node| matches!(node.kind.as_str(), "module" | "module_declaration"))
                    .collect();
                providers.sort_by_key(|node| node.id.as_str());
                providers.dedup_by_key(|node| node.id.as_str());
                let [provider] = providers.as_slice() else {
                    continue;
                };
                let Some(provider_id) = context.symbol_id(&provider.id) else {
                    continue;
                };
                for (&name, imported) in context.imports.get(&provider_id).into_iter().flatten() {
                    let Imported::External(origin, _, original, _) = imported else {
                        continue;
                    };
                    if original.alias.as_deref() == Some("*")
                        && origin.trim_end_matches("::*") == "rayon::prelude"
                        && !context
                            .imports
                            .get(&owner_id)
                            .is_some_and(|aliases| aliases.contains_key(&name))
                    {
                        additions.push((owner_id, name, origin.clone(), reference.position(), *original));
                    }
                }
            }
        }
        for (facts, module) in rust_modules {
            for reference in facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports" && reference.alias.is_some())
            {
                let glob = reference.alias.as_deref() == Some("*");
                let raw = reference.module.as_deref().unwrap_or(&reference.expression);
                let Some(normalized) = normalized_imports
                    .get(&(module.path.as_str(), raw))
                    .and_then(Option::as_ref)
                else {
                    continue;
                };
                let providers = languages::rust::linker::modules_at_import(
                    modules,
                    languages::LanguageFamily("rust"),
                    normalized,
                    languages::workspace_path(&module.path).0,
                    |namespace| rust_forwarding.module_path_visible(namespace, &module.qualname),
                );
                let [(provider, tail)] = providers.as_slice() else {
                    continue;
                };
                let namespace = provider
                    .qualname
                    .strip_suffix(".lib")
                    .or_else(|| provider.qualname.strip_suffix(".main"))
                    .or_else(|| provider.qualname.strip_suffix(".mod"))
                    .unwrap_or(&provider.qualname);
                if (glob && !matches!(tail.as_str(), "" | "*"))
                    || (!glob && (tail.is_empty() || tail.contains('.') || tail == "*"))
                    || !module
                        .qualname
                        .strip_prefix(namespace)
                        .is_some_and(|suffix| suffix.starts_with('.'))
                {
                    continue;
                }
                let (Some(owner_id), Some(provider_id)) = (
                    context.symbol_id(&reference.source),
                    context.symbol_id(&provider.id),
                ) else {
                    continue;
                };
                for (&provider_name_id, imported) in
                    context.imports.get(&provider_id).into_iter().flatten()
                {
                    let Imported::External(origin, _, original, _) = imported else {
                        continue;
                    };
                    if original.alias.as_deref() == Some("*") {
                        continue;
                    }
                    let provider_name = context.symbol_name(provider_name_id);
                    if !glob && provider_name != tail {
                        continue;
                    }
                    let Some(name) = (if glob {
                        Some(provider_name)
                    } else {
                        reference.alias.as_deref()
                    }) else {
                        continue;
                    };
                    let Some(name_id) = context.symbol_id(name) else {
                        continue;
                    };
                    if conflicts.contains(&(owner_id, name_id))
                        || has_conflicting_local_binding(facts, &module.qualname, reference, name)
                    {
                        continue;
                    }
                    let existing = context
                        .imports
                        .get(&owner_id)
                        .and_then(|aliases| aliases.get(&name_id));
                    if matches!(existing, None | Some(Imported::Unknown))
                        || matches!(existing, Some(Imported::External(existing, ..)) if existing != origin)
                    {
                        let src_ref = if glob { *original } else { reference };
                        additions.push((owner_id, name_id, origin.clone(), reference.position(), src_ref));
                    }
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        for (owner, name, origin, position, reference) in additions {
            let aliases = context.imports.entry(owner).or_default();
            if matches!(aliases.get(&name), Some(Imported::External(existing, ..)) if existing != &origin)
            {
                conflicts.insert((owner, name));
            }
            if conflicts.contains(&(owner, name)) {
                aliases.insert(name, Imported::Unknown);
            } else {
                aliases.insert(name, Imported::External(origin, position, reference, true));
            }
        }
        for &(owner, name) in &conflicts {
            context
                .imports
                .entry(owner)
                .or_default()
                .insert(name, Imported::Unknown);
        }
    }
}

pub(in crate::linker) fn visible(owner: &Node, candidate: &Node) -> Option<bool> {
    if candidate.language == "rust" && candidate.kind == "impl" {
        Some(false)
    } else if owner.language == "rust" {
        Some(true)
    } else {
        None
    }
}

pub(in crate::linker) fn nominal_application<'a>(
    owner: &Node,
    nominal: &Symbol<'a>,
) -> Option<TypeTarget<'a>> {
    if owner.language != "rust" {
        return None;
    }
    match nominal {
        Symbol::Type(TypeTarget::Local(node))
            if matches!(node.kind.as_str(), "struct" | "enum") =>
        {
            Some(TypeTarget::Local(node))
        }
        _ => None,
    }
}

pub(in crate::linker) fn admits_payload(
    owner: &Node,
    wrapper: &TypeTarget<'_>,
    argument_count: usize,
) -> bool {
    owner.language == "rust"
        && matches!(wrapper, TypeTarget::Builtin(name)
            if (name == "Option" && argument_count == 1)
                || (name == "Result" && argument_count <= 2))
}

pub(in crate::linker) fn reference_target<'target, 'node>(
    target: &'target TypeTarget<'node>,
) -> Option<&'target TypeTarget<'node>> {
    match target {
        TypeTarget::RustReference { target, .. } => Some(target),
        _ => None,
    }
}

pub(in crate::linker) fn standard_member(module: &str, member: &str) -> bool {
    if matches!(member, "new" | "from" | "with_capacity" | "default") {
        return false;
    }
    let module = super::value_flow::import_path(module);
    let nominal = match module.as_ref() {
        "std.collections.HashMap"
        | "std.collections.HashSet"
        | "std.collections.BTreeMap"
        | "std.collections.BTreeSet"
        | "std.collections.VecDeque" => module.rsplit_once('.').map_or("", |(_, name)| name),
        "std.string.String" | "alloc.string.String" => "String",
        "std.vec.Vec" | "alloc.vec.Vec" => "Vec",
        "std.path.Path" => "Path",
        "std.path.PathBuf" => "PathBuf",
        "std.option.Option" | "core.option.Option" => "Option",
        "std.result.Result" | "core.result.Result" => "Result",
        "std.borrow.Cow" | "alloc.borrow.Cow" => "Cow",
        "std.ffi.OsStr" => "OsStr",
        "std.ffi.OsString" => "OsString",
        "std.fs.File" => "std::fs::File",
        "std.io.Error" => "std::io::Error",
        "std.sync.OnceLock" => "std::sync::OnceLock",
        "std.sync.Mutex" => "std::sync::Mutex",
        "std.sync.RwLock" => "std::sync::RwLock",
        "std.cell.RefCell" => "std::cell::RefCell",
        "std.thread.LocalKey" => "std::thread::LocalKey",
        "std.fmt.Formatter" | "core.fmt.Formatter" => "std::fmt::Formatter",
        _ => return false,
    };
    languages::by_id("rust").is_some_and(|profile| profile.builtin_member(nominal, member))
}

pub(in crate::linker) fn wrapper_value_member(wrapper: &str, member: &str) -> bool {
    match wrapper {
        "Box" => matches!(member, "as_ref" | "as_mut"),
        "Arc" | "Rc" => matches!(member, "clone" | "as_ref"),
        "Cow" => matches!(member, "into_owned" | "as_ref" | "to_mut" | "clone"),
        "HashMap" | "BTreeMap" | "Range" => {
            languages::by_id("rust").is_some_and(|profile| profile.builtin_member(wrapper, member))
        }
        "IndexMap" => {
            languages::by_id("rust").is_some_and(|profile| profile.builtin_member("HashMap", member))
                || matches!(
                    member,
                    "get_index" | "get_index_mut" | "sort_keys" | "shift_remove" | "swap_remove"
                        | "pop"
                )
        }
        "MapEntry" => matches!(
            member,
            "or_insert"
                | "or_insert_with"
                | "or_insert_with_key"
                | "or_default"
                | "and_modify"
                | "key"
                | "insert"
                | "insert_entry"
                | "get"
                | "get_mut"
                | "into_mut"
                | "remove"
                | "remove_entry"
        ),
        "OnceLock" => matches!(
            member,
            "get" | "get_mut" | "get_or_init" | "get_or_try_init" | "set" | "take" | "into_inner"
        ),
        "LocalKey" => matches!(member, "with" | "try_with"),
        "Mutex" => matches!(
            member,
            "lock" | "try_lock" | "into_inner" | "get_mut" | "is_poisoned" | "clear_poison"
        ),
        "RwLock" => matches!(
            member,
            "read"
                | "write"
                | "try_read"
                | "try_write"
                | "into_inner"
                | "get_mut"
                | "is_poisoned"
                | "clear_poison"
        ),
        "RefCell" => matches!(
            member,
            "borrow"
                | "borrow_mut"
                | "try_borrow"
                | "try_borrow_mut"
                | "into_inner"
                | "replace"
                | "replace_with"
                | "take"
                | "swap"
        ),
        "JoinHandle" => matches!(member, "join" | "abort" | "is_finished"),
        _ => false,
    }
}
