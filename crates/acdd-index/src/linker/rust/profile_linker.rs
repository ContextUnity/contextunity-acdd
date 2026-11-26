use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::{
    dependency_registry::DependencyRegistry,
    profile::{LanguageMemberIndex, LanguageMemberResult},
    types as languages,
};
use crate::models::{Facts, Node};
use hashbrown::{HashMap, HashSet};

use super::forwarding::{is_crate_visible, resolve_qualified_path, RustForwarding};

const RUST_ALIAS_DEPTH: usize = 64;

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
    trait_associated: HashMap<(&'a str, &'a str), &'a Node>,
    dynamic_implementors: HashMap<(&'a str, &'a str), Vec<&'a Node>>,
    display_impls: HashSet<&'a str>,
}

fn builtin_receiver_key(raw: &str) -> Option<&'static str> {
    let name = crate::linker::types::strip_rust_references(raw, true)?;
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
    super::linker_impl::disambiguate_rust_cfg_siblings(methods);
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
                    out.extend(
                        targets
                            .iter()
                            .filter(|n| n.kind == crate::models::tags::NodeKind::Macro),
                    );
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
            .filter(|n| n.kind == crate::models::tags::NodeKind::Macro)
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
                    .filter(|n| {
                        n.kind == crate::models::tags::NodeKind::Macro && n.path == owner_mod.path
                    }),
            );
            scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
        }
        for import in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && (reference.source == owner_mod.id
                    || owner_mod.qualname.starts_with(reference.source.as_str()))
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
    tables: &RustMemberBuildTables<'a>,
    forwarding: &RustForwarding<'a>,
    dependencies: &DependencyRegistry,
) -> Option<&'a Node> {
    if name.is_empty() || name.contains(['<', '>', '&', '[', '(', '!']) {
        return None;
    }
    let module = facts
        .nodes
        .iter()
        .filter(|node| {
            node.path == scope.path
                && node.language == crate::models::tags::LanguageId::Rust
                && matches!(node.kind.as_str(), "module" | "module_declaration")
                && (scope.qualname == node.qualname
                    || scope
                        .qualname
                        .strip_prefix(node.qualname.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.')))
        })
        .max_by_key(|node| node.qualname.len())
        .or_else(|| {
            facts.nodes.iter().find(|node| {
                node.kind == crate::models::tags::NodeKind::Module
                    && node.language == crate::models::tags::LanguageId::Rust
            })
        })?;
    let workspace = languages::workspace_path(&scope.path).0;
    fn pick_unique_slice<'a>(nodes: &[&'a Node], workspace: &str) -> Option<&'a Node> {
        let mut candidates = nodes.iter().copied().filter(|node| {
            languages::workspace_path(&node.path).0 == workspace
                && matches!(node.kind.as_str(), "struct" | "enum" | "trait" | "type")
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    }
    let pick_unique = |qualified: &str| -> Option<&'a Node> {
        tables
            .by_qual
            .get(qualified)
            .and_then(|nodes| pick_unique_slice(nodes, workspace))
    };

    let is_inline_submodule = facts.nodes.iter().any(|node| {
        node.path == scope.path
            && node.language == crate::models::tags::LanguageId::Rust
            && node.kind == crate::models::tags::NodeKind::Module
            && node.id != module.id
            && module.qualname.starts_with(&format!("{}.", node.qualname))
    });

    if let Some((head, tail)) = name.split_once("::") {
        let tail_dotted = tail.replace("::", ".");
        let mut forwarded = Vec::new();
        for provider in forwarding.targets(module, head, &module.qualname) {
            if super::forwarding::is_module_scope(provider) {
                forwarded.extend(forwarding.resolve_path(
                    provider,
                    &tail_dotted,
                    &module.qualname,
                    Some(&tables.by_qual),
                ));
            }
        }
        forwarded.sort_by_key(|node| node.id.as_str());
        forwarded.dedup_by_key(|node| node.id.as_str());
        if let Some(found) = pick_unique_slice(&forwarded, workspace) {
            return Some(found);
        }
        let local_imports: Vec<_> = if !is_inline_submodule && scope.id == module.id {
            Vec::new()
        } else {
            facts
                .references
                .iter()
                .filter(|reference| {
                    reference.kind == "imports"
                        && (reference.source == scope.id || reference.source == module.id)
                        && reference.alias.as_deref() == Some(head)
                })
                .collect()
        };
        let qualified = match local_imports.as_slice() {
            [] => resolve_qualified_path(
                &scope.path,
                &module.qualname,
                name,
                dependencies,
                &tables.profile_paths,
            ),
            [import] => {
                let target = import.module.as_deref()?;
                resolve_qualified_path(
                    &scope.path,
                    &module.qualname,
                    &format!("{target}::{tail}"),
                    dependencies,
                    &tables.profile_paths,
                )
            }
            _ => return None,
        };
        if let Some(found) = pick_unique(&qualified) {
            return Some(found);
        }
        let mut cursor = qualified.as_str();
        while let Some((mod_ns, _)) = cursor.rsplit_once('.') {
            let rel_tail = &qualified[mod_ns.len() + 1..];
            if let Some(providers) = tables.modules_by_ns.get(mod_ns) {
                let mut reexported = Vec::new();
                for provider in providers {
                    reexported.extend(forwarding.resolve_path(
                        provider,
                        rel_tail,
                        &module.qualname,
                        Some(&tables.by_qual),
                    ));
                }
                reexported.sort_by_key(|node| node.id.as_str());
                reexported.dedup_by_key(|node| node.id.as_str());
                if let Some(found) = pick_unique_slice(&reexported, workspace) {
                    return Some(found);
                }
            }
            cursor = mod_ns;
        }
        return None;
    }

    if let Some(found) = pick_unique(&format!("{}.{name}", module.qualname)) {
        return Some(found);
    }
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
    if !is_inline_submodule {
        if let Some(found) = pick_unique_slice(
            forwarding.targets(module, name, &module.qualname),
            workspace,
        ) {
            return Some(found);
        }
    }
    if scope.id != module.id || is_inline_submodule {
        let local_imports: Vec<_> = facts
            .references
            .iter()
            .filter(|reference| {
                reference.kind == "imports"
                    && (reference.source == scope.id || reference.source == module.id)
                    && reference.alias.as_deref() == Some(name)
            })
            .collect();
        match local_imports.as_slice() {
            [] => {}
            [import] => {
                let target = import.module.as_deref()?;
                let qualified = resolve_qualified_path(
                    &scope.path,
                    &module.qualname,
                    target,
                    dependencies,
                    &tables.profile_paths,
                );
                return pick_unique(&qualified);
            }
            _ => return None,
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
        .filter(|node| node.language == crate::models::tags::LanguageId::Rust)
    {
        by_qual.entry(&node.qualname).or_default().push(node);
        by_id.insert(node.id.as_str(), node);
        if node.kind == crate::models::tags::NodeKind::Module {
            modules_by_ns.entry(&node.qualname).or_default().push(node);
            let stripped = node
                .qualname
                .strip_suffix(".lib")
                .or_else(|| node.qualname.strip_suffix(".main"))
                .or_else(|| node.qualname.strip_suffix(".mod"));
            if let Some(stripped) = stripped {
                modules_by_ns.entry(stripped).or_default().push(node);
            }
        } else if node.kind == crate::models::tags::NodeKind::Trait {
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

type ImplReceiverMap<'a> = HashMap<&'a str, (&'a Node, bool, Option<&'a Node>)>;
type BuiltinImplReceiverMap<'a> = HashMap<&'a str, (&'static str, &'a Node)>;

struct RustMemberBuildEnv<'a, 'env> {
    tables: &'env RustMemberBuildTables<'a>,
    interner: &'env LinkerInterner<'a>,
    flows: Option<&'env HashMap<SymbolId, &'env crate::typed_facts::FlowState>>,
    forwarding: &'env RustForwarding<'a>,
    dependencies: &'env DependencyRegistry,
}

fn collect_trait_decl_tables<'a>(
    all: &'a dyn crate::FactsLookup,
    tables: &RustMemberBuildTables<'a>,
    index: &mut RustMembers<'a>,
    trait_defaults: &mut HashMap<&'a str, Vec<&'a Node>>,
    trait_methods: &mut HashMap<&'a str, Vec<&'a Node>>,
) {
    for facts in all.values_facts() {
        for edge in facts.edges.iter().filter(|edge| edge.kind == "contains") {
            let (Some(owner), Some(method)) = (
                tables.by_id.get(edge.src.as_str()),
                tables.by_id.get(edge.dst.as_str()),
            ) else {
                continue;
            };
            if owner.kind == crate::models::tags::NodeKind::Trait
                && method.kind == crate::models::tags::NodeKind::Method
            {
                if method.detail_entry(crate::models::details::NodeDetailKey::UNPROVEN_BOUNDS)
                    != true
                {
                    trait_methods
                        .entry(owner.id.as_str())
                        .or_default()
                        .push(method);
                    if method.detail_entry(crate::models::details::NodeDetailKey::TRAIT_DEFAULT)
                        == true
                    {
                        trait_defaults
                            .entry(owner.id.as_str())
                            .or_default()
                            .push(method);
                    }
                }
                if method.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC) == true {
                    index
                        .trait_associated
                        .insert((owner.id.as_str(), method.name.as_str()), method);
                } else {
                    index
                        .dynamic_declarations
                        .insert((owner.id.as_str(), method.name.as_str()), method);
                }
            }
        }
    }
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
    let trait_name = owner
        .detail_entry(crate::models::details::NodeDetailKey::IMPL_TRAIT)
        .as_str()?;
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
    let receiver = owner
        .detail_entry(crate::models::details::NodeDetailKey::RECEIVER_TYPE)
        .as_str()
        .and_then(|name| resolve_type(facts, owner, name, tables, forwarding, dependencies))
        .filter(|node| {
            node.kind == crate::models::tags::NodeKind::Struct
                && node.detail_entry(crate::models::details::NodeDetailKey::DEREF_UNPROVEN) != true
        })?;
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
        && owner.detail_entry(crate::models::details::NodeDetailKey::GENERIC_IMPL) != true
        && owner.detail_entry(crate::models::details::NodeDetailKey::DEREF_UNPROVEN) != true)
    {
        return Some((receiver, None));
    }
    let targets: Vec<_> = facts
        .edges
        .iter()
        .filter(|edge| edge.kind == "contains" && edge.src == owner.id)
        .filter_map(|edge| tables.by_id.get(edge.dst.as_str()).copied())
        .filter(|node| node.kind == crate::models::tags::NodeKind::Type && node.name == "Target")
        .collect();
    let [target] = targets.as_slice() else {
        return Some((receiver, None));
    };
    let alias = if target.detail_entry(crate::models::details::NodeDetailKey::DEREF_UNPROVEN)
        == true
    {
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
        crate::semantic::ValueFlowFacts::deserialize_for_scope(
            target.detail_entry(crate::models::details::NodeDetailKey::VALUE_FLOW),
            false,
        )
        .ok()
        .and_then(|facts| facts.alias_type.clone())
    };
    let resolved = match alias {
        Some(crate::semantic::TypeExpr::Named { name }) => {
            resolve_type(facts, target, &name, tables, forwarding, dependencies).filter(|node| {
                node.kind == crate::models::tags::NodeKind::Struct
                    && node.detail_entry(crate::models::details::NodeDetailKey::DEREF_UNPROVEN)
                        != true
            })
        }
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
        let Some(module) = facts.nodes.iter().find(|node| {
            node.kind == crate::models::tags::NodeKind::Module
                && node.language == crate::models::tags::LanguageId::Rust
        }) else {
            continue;
        };
        for owner in facts
            .nodes
            .iter()
            .filter(|node| node.kind == crate::models::tags::NodeKind::Impl)
        {
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
                        .filter(|target| target.kind == crate::models::tags::NodeKind::Trait)
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
            .filter(|target| {
                target.path == module.path
                    || target.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) == true
            })
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
        if let Some(target) =
            resolve_type(facts, import_scope, name, tables, forwarding, dependencies)
                .filter(|node| node.kind == crate::models::tags::NodeKind::Trait)
        {
            visible_traits
                .entry(&module.path)
                .or_default()
                .insert(&target.id);
        }
    }
    for node in &facts.nodes {
        if node.kind == crate::models::tags::NodeKind::Trait
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
    forwarding: &RustForwarding<'a>,
    dependencies: &DependencyRegistry,
    index: &mut RustMembers<'a>,
) -> (ImplReceiverMap<'a>, BuiltinImplReceiverMap<'a>) {
    let mut impl_receivers = HashMap::new();
    let mut builtin_impl_receivers = HashMap::new();
    for owner in facts.nodes.iter().filter(|node| {
        node.kind == crate::models::tags::NodeKind::Impl
            && (node.detail_entry(crate::models::details::NodeDetailKey::GENERIC_IMPL) != true
                || node.detail_entry(crate::models::details::NodeDetailKey::UNIFORM_GENERIC_IMPL)
                    == true)
    }) {
        let raw_receiver = owner
            .detail_entry(crate::models::details::NodeDetailKey::RECEIVER_TYPE)
            .as_str();
        let local_receiver = raw_receiver
            .and_then(|name| {
                let nominal = if owner
                    .detail_entry(crate::models::details::NodeDetailKey::UNIFORM_GENERIC_IMPL)
                    == true
                    || !super::linker_impl::rust_has_non_lifetime_generics(name)
                {
                    name.split_once('<').map_or(name, |(base, _)| base.trim())
                } else {
                    name
                };
                resolve_type(facts, owner, nominal, tables, forwarding, dependencies)
            })
            .filter(|node| matches!(node.kind.as_str(), "struct" | "enum"));
        let impl_trait = owner
            .detail_entry(crate::models::details::NodeDetailKey::IMPL_TRAIT)
            .as_str();
        let trait_node = impl_trait.and_then(|name| {
            let clean_trait = name.split('<').next().unwrap_or(name).trim();
            resolve_type(facts, owner, clean_trait, tables, forwarding, dependencies)
                .filter(|node| node.kind == crate::models::tags::NodeKind::Trait)
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

fn collect_file_impl_members<'a>(
    facts: &'a Facts,
    tables: &RustMemberBuildTables<'a>,
    (impl_receivers, builtin_impl_receivers): (&ImplReceiverMap<'a>, &BuiltinImplReceiverMap<'a>),
    trait_defaults: &HashMap<&'a str, Vec<&'a Node>>,
    index: &mut RustMembers<'a>,
    traits: &mut TraitImplMap<'a>,
    builtin_traits: &mut BuiltinTraitImplMap<'a>,
) -> Vec<(&'a Node, &'a Node, &'a Node, bool)> {
    let mut unproven_deref_members = Vec::new();
    let mut implemented = HashMap::<&str, HashSet<(&str, bool)>>::new();
    for edge in facts.edges.iter().filter(|edge| edge.kind == "contains") {
        let (Some(owner), Some(method)) = (
            tables.by_id.get(edge.src.as_str()),
            tables.by_id.get(edge.dst.as_str()),
        ) else {
            continue;
        };
        let is_member_owner = (owner.kind == crate::models::tags::NodeKind::Impl
            && matches!(method.kind.as_str(), "method" | "constant" | "type"))
            || (matches!(owner.kind.as_str(), "struct" | "enum")
                && matches!(method.kind.as_str(), "method" | "constant"))
            || (owner.kind == crate::models::tags::NodeKind::Trait
                && method.kind == crate::models::tags::NodeKind::Method);
        if !is_member_owner {
            continue;
        }
        if owner.kind == crate::models::tags::NodeKind::Trait {
            index.receivers_by_method.insert(&method.id, *owner);
            continue;
        }
        if owner.kind == crate::models::tags::NodeKind::Impl {
            if let Some(&(builtin_key, trait_node)) = builtin_impl_receivers.get(owner.id.as_str())
            {
                let associated = matches!(method.kind.as_str(), "constant" | "type")
                    || method.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC)
                        == true;
                builtin_traits
                    .entry((builtin_key, method.name.as_str(), associated))
                    .or_default()
                    .push((method, trait_node));
                continue;
            }
        }
        let (receiver, has_impl_trait, trait_node) =
            if owner.kind == crate::models::tags::NodeKind::Impl {
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
        let associated = matches!(method.kind.as_str(), "constant" | "type")
            || method.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC) == true;
        if method.detail_entry(crate::models::details::NodeDetailKey::UNPROVEN_BOUNDS) == true {
            let deref_wrapper = index.deref_targets.contains_key(receiver.id.as_str());
            if deref_wrapper {
                if let Some(trait_node) = trait_node {
                    unproven_deref_members.push((receiver, *method, trait_node, associated));
                } else if !has_impl_trait {
                    index
                        .inherent
                        .entry((receiver.id.as_str(), method.name.as_str(), associated))
                        .or_default();
                }
                continue;
            }
            if has_impl_trait
                || owner.detail_entry(crate::models::details::NodeDetailKey::GENERIC_IMPL) == true
            {
                continue;
            }
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
            let associated =
                method.detail_entry(crate::models::details::NodeDetailKey::IS_STATIC) == true;
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
    unproven_deref_members
}

fn apply_macro_type_invocations<'a>(
    all: &'a dyn crate::FactsLookup,
    env: &RustMemberBuildEnv<'a, '_>,
    trait_methods: &HashMap<&'a str, Vec<&'a Node>>,
    traits: &mut TraitImplMap<'a>,
) {
    let RustMemberBuildEnv {
        tables,
        forwarding,
        dependencies,
        ..
    } = *env;
    let mut macro_traits = HashMap::<&str, Vec<&Node>>::new();
    let mut crate_exported_macros = HashMap::<(&str, &str), Vec<&Node>>::new();
    for node in all
        .values_facts()
        .flat_map(|facts| &facts.nodes)
        .filter(|node| {
            node.language == crate::models::tags::LanguageId::Rust
                && node.kind == crate::models::tags::NodeKind::Macro
        })
    {
        if node.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC) == true {
            let crate_root = node
                .path
                .split_once("/src/")
                .map_or("", |(prefix, _)| prefix);
            crate_exported_macros
                .entry((crate_root, node.name.as_str()))
                .or_default()
                .push(node);
        }
        let Some(impl_traits) = node
            .detail_entry(crate::models::details::NodeDetailKey::RUST_MACRO_IMPL_TRAITS)
            .as_array()
        else {
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
                for cand in candidates
                    .iter()
                    .copied()
                    .filter(|n| n.kind == crate::models::tags::NodeKind::Trait)
                {
                    macro_traits.entry(node.id.as_str()).or_default().push(cand);
                }
            }
        }
    }
    for facts in all.values_facts() {
        for owner_mod in facts
            .nodes
            .iter()
            .filter(|node| matches!(node.kind.as_str(), "module" | "module_declaration"))
        {
            let Some(invocations) = owner_mod
                .detail_entry(crate::models::details::NodeDetailKey::RUST_MACRO_TYPE_INVOCATIONS)
                .as_array()
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
                    &crate_exported_macros,
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
                    tables,
                    forwarding,
                    dependencies,
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
                        let associated = method
                            .detail_entry(crate::models::details::NodeDetailKey::IS_STATIC)
                            == true;
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
        if methods.iter().any(|method| {
            method.detail_entry(crate::models::details::NodeDetailKey::SYNTHESIZED) != true
        }) {
            methods.retain(|method| {
                method.detail_entry(crate::models::details::NodeDetailKey::SYNTHESIZED) != true
            });
        }
        super::linker_impl::disambiguate_rust_cfg_siblings(methods);
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
            for path in trait_consumers
                .get(trait_node.id.as_str())
                .into_iter()
                .flatten()
            {
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
            for path in trait_consumers
                .get(trait_node.id.as_str())
                .into_iter()
                .flatten()
            {
                index
                    .builtin_trait_members
                    .entry((*receiver_key, *member, *associated, path))
                    .or_default()
                    .push(method);
            }
        }
    }
    for (receiver, method, trait_node, associated) in unproven_deref_members {
        for path in trait_consumers
            .get(trait_node.id.as_str())
            .into_iter()
            .flatten()
        {
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
        if methods.len() > crate::linker::value_flow::types::MAX_CANDIDATE_BRANCH_JOINS {
            methods.clear();
        }
    }
}

impl<'a> RustMembers<'a> {
    pub(crate) fn build(
        all: &'a dyn crate::models::FactsLookup,
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
            trait_associated: HashMap::new(),
            dynamic_implementors: HashMap::new(),
            display_impls: HashSet::new(),
        };
        let mut traits: TraitImplMap<'a> = HashMap::new();
        let mut builtin_traits: BuiltinTraitImplMap<'a> = HashMap::new();
        let mut unproven_deref_members = Vec::new();
        let mut visible_traits: HashMap<&'a str, HashSet<&'a str>> = HashMap::new();
        let mut trait_defaults: HashMap<&'a str, Vec<&'a Node>> = HashMap::new();
        let mut trait_methods: HashMap<&'a str, Vec<&'a Node>> = HashMap::new();
        collect_trait_decl_tables(
            all,
            &tables,
            &mut index,
            &mut trait_defaults,
            &mut trait_methods,
        );
        let env = RustMemberBuildEnv {
            tables: &tables,
            interner,
            flows,
            forwarding,
            dependencies,
        };
        populate_deref_targets(all, &env, &mut index);
        for facts in all.values_facts() {
            let Some(module) = facts.nodes.iter().find(|node| {
                node.kind == crate::models::tags::NodeKind::Module
                    && node.language == crate::models::tags::LanguageId::Rust
            }) else {
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
            let receivers =
                collect_file_impl_receivers(facts, &tables, forwarding, dependencies, &mut index);
            unproven_deref_members.extend(collect_file_impl_members(
                facts,
                &tables,
                (&receivers.0, &receivers.1),
                &trait_defaults,
                &mut index,
                &mut traits,
                &mut builtin_traits,
            ));
        }
        apply_macro_type_invocations(all, &env, &trait_methods, &mut traits);
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
        if receiver.kind == crate::models::tags::NodeKind::Trait {
            let map = if associated {
                &self.trait_associated
            } else {
                &self.dynamic_declarations
            };
            return map
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

impl LanguageMemberIndex for RustMembers<'_> {
    fn language_ids(&self) -> &'static [&'static str] {
        &["rust"]
    }

    fn lookup_member<'a>(
        &'a self,
        receiver: &Node,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> LanguageMemberResult<'a> {
        let candidates = self.lookup(receiver, member, associated, caller);
        if candidates.is_empty() {
            LanguageMemberResult::Unknown
        } else if let [node] = candidates {
            LanguageMemberResult::Local(node)
        } else {
            LanguageMemberResult::Candidates(candidates.to_vec())
        }
    }

    fn lookup_builtin<'a>(
        &'a self,
        receiver: &str,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> LanguageMemberResult<'a> {
        let candidates = self.lookup_builtin(receiver, member, associated, caller);
        if candidates.is_empty() {
            LanguageMemberResult::Unknown
        } else if let [node] = candidates {
            LanguageMemberResult::Local(node)
        } else {
            LanguageMemberResult::Candidates(candidates.to_vec())
        }
    }

    fn receiver_for(&self, method: &Node) -> Option<&Node> {
        RustMembers::receiver_for(self, method)
    }

    fn deref_target(&self, receiver: &Node) -> Option<&Node> {
        RustMembers::deref_target(self, receiver)
    }

    fn is_inherent_member(&self, method: &Node) -> bool {
        RustMembers::is_inherent_member(self, method)
    }

    fn builtin_local_member(&self, receiver: &Node, member: &str, associated: bool) -> bool {
        !associated && member == "to_string" && RustMembers::has_display(self, receiver)
    }

    fn dynamic_dispatch(&self, receiver: &Node, member: &str) -> Option<(&Node, Vec<&Node>)> {
        RustMembers::dynamic_dispatch(self, receiver, member)
            .map(|(declaration, implementors)| (declaration, implementors.to_vec()))
    }
}

pub(super) fn required_full_facts(
    facts: &dyn crate::models::FactsLookup,
    affected: &std::collections::BTreeSet<String>,
    catalog: &[(&str, &str)],
) -> std::collections::BTreeSet<String> {
    let workspaces: HashSet<_> = facts
        .iter_facts()
        .filter(|(path, facts)| {
            affected.contains(*path)
                && facts
                    .nodes
                    .iter()
                    .any(|node| node.language == crate::models::tags::LanguageId::Rust)
                && facts.references.iter().any(|reference| {
                    reference.kind == "calls"
                        && (reference.expression.contains('.')
                            || reference.expression.contains("::"))
                })
        })
        .map(|(path, _)| languages::workspace_path(path).0)
        .collect();
    catalog
        .iter()
        .filter(|(path, language)| {
            *language == "rust" && workspaces.contains(languages::workspace_path(path).0)
        })
        .map(|(path, _)| (*path).to_owned())
        .collect()
}
