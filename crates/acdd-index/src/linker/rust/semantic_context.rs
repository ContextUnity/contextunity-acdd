use crate::languages;
#[cfg(feature = "lang-rust")]
use crate::languages::ImportPath;
#[cfg(feature = "lang-rust")]
use crate::languages::{dependency_registry::DependencyRegistry, LanguageProfile};
#[cfg(feature = "lang-rust")]
use crate::linker::traits::ModulesByNamespace;
use crate::linker::types::{receiver_type_name, ReceiverType};
use crate::linker::value_flow::types::{import_path, Symbol, TypeTarget};
use crate::models::{Facts, Node};
#[cfg(feature = "lang-rust")]
use hashbrown::HashMap;

use super::super::semantic_context::LanguageMemberLookup;
#[cfg(feature = "lang-rust")]
use super::super::semantic_context::{Context, Imported};

#[cfg(feature = "lang-rust")]
impl<'a> Context<'a> {
    fn rayon_extension_base(
        &self,
        receiver: &TypeTarget<'_>,
        member: &str,
        caller: &Node,
    ) -> Option<&'static str> {
        let receiver = super::value_flow::reference_target(receiver).unwrap_or(receiver);
        let container_base = |name: &str| -> Option<&'static str> {
            match name {
                "HashMap" => Some("HashMap"),
                "BTreeMap" => Some("BTreeMap"),
                "HashSet" => Some("HashSet"),
                "BTreeSet" => Some("BTreeSet"),
                _ => None,
            }
        };
        let base = match receiver {
            TypeTarget::RustCollection { .. } => "Vec",
            TypeTarget::RustSlice { .. } => "Slice",
            TypeTarget::RustTypedContainer { base, .. } => container_base(base)?,
            TypeTarget::RustDeref { wrapper, .. } => container_base(wrapper)?,
            TypeTarget::Builtin(name) if name == "Vec" => "Vec",
            TypeTarget::Builtin(name) => container_base(name.as_str())?,
            _ => return None,
        };
        if !self.dependencies.declares_for_path(
            languages::LanguageFamily("rust"),
            &caller.path,
            "rayon",
        ) {
            return None;
        }
        let mut scope = caller.qualname.as_str();
        let mut imported = false;
        loop {
            let module_boundary = self
                .nodes_in_path(scope, &caller.path)
                .any(|owner| matches!(owner.kind.as_str(), "module" | "module_declaration"));
            imported |= self.nodes_in_path(scope, &caller.path).any(|owner| {
                self.symbol_id(&owner.id).and_then(|id| self.imports.get(&id)).is_some_and(|aliases| {
                    aliases.values().any(|import| matches!(import, Imported::External(origin, _, reference, _)
                        if reference.alias.as_deref() == Some("*") && origin.trim_end_matches("::*") == "rayon::prelude"))
                })
            });
            if imported || module_boundary {
                break;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        if !imported {
            return None;
        }
        let lookup_base = if matches!(base, "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet") {
            "Vec"
        } else {
            base
        };
        let (_, rule) = self.dependencies.class_hierarchy_authority_for_path(
            &caller.path,
            lookup_base,
            Some("rust-cargo"),
        )?;
        (rule.provider == "rayon" && rule.members.iter().any(|allowed| allowed == member))
            .then_some(base)
    }
    pub(in crate::linker) fn rust_alias_member(
        &self,
        owner: &'a Node,
        reference: &crate::models::Reference,
    ) -> Option<Symbol<'a>> {
        let (base, member) = reference.expression.rsplit_once("::")?;
        let TypeTarget::Local(target) =
            self.rust_alias_target(owner, base, reference.position())?
        else {
            return None;
        };
        Some(self.member(target, member, true, owner))
    }

    pub(in crate::linker) fn resolve_rust_symbol(
        &self,
        owner: &'a Node,
        name: &str,
        at: crate::semantic::SourcePosition,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Option<Symbol<'a>> {
        use crate::linker::value_flow::SymbolRole;
        if let Some((base, member)) = name.rsplit_once("::").or_else(|| name.rsplit_once('.')) {
            if let Some(TypeTarget::Local(node)) =
                self.rust_alias_target(owner, base, at).or_else(|| {
                    match self.resolve_symbol_raw(owner, base, at, SymbolRole::Type) {
                        Symbol::Type(target) => Some(target),
                        _ => None,
                    }
                })
            {
                if matches!(node.kind.as_str(), "struct" | "enum" | "trait" | "type") {
                    let resolved = self.member(node, member, true, owner);
                    if !matches!(resolved, Symbol::Unknown) {
                        return Some(resolved);
                    }
                }
            }
        }
        if matches!(role, SymbolRole::Type | SymbolRole::Constructor) {
            if let Some(target) = self.rust_alias_target(owner, name, at) {
                return Some(Symbol::Type(target));
            }
        }
        None
    }

    fn rust_alias_target(
        &self,
        owner: &'a Node,
        name: &str,
        at: crate::semantic::SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        use crate::linker::value_flow::SymbolRole;
        let mut symbol = self.resolve_symbol_raw(owner, name, at, SymbolRole::Type);
        let mut visited = Vec::new();
        for _ in 0..16 {
            match symbol {
                Symbol::Type(TypeTarget::Local(alias))
                    if alias.language == crate::models::tags::LanguageId::Rust
                        && alias.kind == crate::models::tags::NodeKind::Type =>
                {
                    if *alias.detail_entry(crate::models::details::NodeDetailKey::DEREF_UNPROVEN)
                        == true
                    {
                        return None;
                    }
                    if visited.contains(&alias.id.as_str()) {
                        return Some(TypeTarget::Unknown);
                    }
                    let Some(target) = alias
                        .detail_entry(crate::models::details::NodeDetailKey::ALIAS_TARGET)
                        .as_str()
                    else {
                        return Some(TypeTarget::Unknown);
                    };
                    visited.push(alias.id.as_str());
                    symbol = self.resolve_symbol_raw(
                        alias,
                        target,
                        crate::linker::value_flow::node_position(alias),
                        SymbolRole::Type,
                    );
                }
                Symbol::Type(target) if !visited.is_empty() => return Some(target),
                _ => return (!visited.is_empty()).then_some(TypeTarget::Unknown),
            }
        }
        Some(TypeTarget::Unknown)
    }
    pub(in crate::linker) fn resolve_rust_external_or_crate_symbol(
        &self,
        owner: &'a Node,
        name: &str,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Option<Symbol<'a>> {
        if let Some((crate_name, tail)) = name.split_once('.') {
            if let Some(root_module) = self.import_state("rust").and_then(|state| {
                state.workspace_crate_root(self.dependencies, &owner.path, crate_name)
            }) {
                if tail.is_empty() {
                    return Some(self.classify(std::iter::once(root_module), role));
                }
                let resolved = self.resolve_rust_member_path(owner, &[root_module], tail, role);
                if !matches!(resolved, Symbol::Unknown) {
                    return Some(resolved);
                }
            }
        }
        let canonical = name.replace('.', "::");
        let crate_name = canonical.split("::").next().unwrap_or("");
        if ((canonical.contains("::") && matches!(crate_name, "std" | "core" | "alloc"))
            || self.dependencies.declares_for_path(
                languages::LanguageFamily("rust"),
                &owner.path,
                crate_name,
            ))
            && self
                .dependencies
                .class_hierarchy_authority_for_path(&owner.path, &canonical, None)
                .is_some()
        {
            return Some(Symbol::Type(TypeTarget::External {
                module: canonical,
                import_line: 0,
            }));
        }
        None
    }
    pub(in crate::linker) fn resolve_rust_imported_call(
        &self,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
    ) -> Vec<&'a Node> {
        match self.resolve_rust_member_path(
            owner,
            targets,
            member,
            crate::linker::value_flow::SymbolRole::Callable,
        ) {
            Symbol::Callable(node) => vec![node],
            _ => Vec::new(),
        }
    }

    fn step_rust_module_member(
        &self,
        owner: &'a Node,
        target: &'a Node,
        segment: &str,
        next: &mut Vec<&'a Node>,
    ) {
        let visible = self.import_state("rust").map_or(&[][..], |state| {
            state.targets(target, segment, &owner.qualname)
        });
        if !visible.is_empty() {
            next.extend_from_slice(visible);
            return;
        }
        if let Some(Imported::Nodes(imported, _, _)) = self.imported_alias(target, segment) {
            if imported
                .iter()
                .all(|node| node.kind == crate::models::tags::NodeKind::Module)
            {
                next.extend(imported.iter().copied().filter(|node| {
                    self.import_state("rust").is_none_or(|state| {
                        state.module_path_visible(&node.qualname, &owner.qualname)
                    })
                }));
                return;
            }
        }
        let sub_qual = format!("{}.{}", target.qualname, segment);
        let target_pkg = target.path.split_once("/src/").map_or("", |(p, _)| p);
        if let Some(candidates) = self.by_qual.get(sub_qual.as_str()) {
            next.extend(candidates.iter().copied().filter(|node| {
                node.language == crate::models::tags::LanguageId::Rust
                    && node.kind == crate::models::tags::NodeKind::Module
                    && node.path.split_once("/src/").map_or("", |(p, _)| p) == target_pkg
                    && self.import_state("rust").is_none_or(|state| {
                        state.module_path_visible(&node.qualname, &owner.qualname)
                    })
            }));
        }
    }

    pub(in crate::linker) fn resolve_rust_member_path(
        &self,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
        role: crate::linker::value_flow::SymbolRole,
    ) -> Symbol<'a> {
        use crate::languages::profile::ProfileLinker;
        use crate::linker::value_flow::SymbolRole;
        let mut current = targets.to_vec();
        let segment_count = member.split('.').count();
        for (index, segment) in member.split('.').enumerate() {
            if index >= 64 {
                return Symbol::Unknown;
            }
            let mut next = Vec::new();
            for target in &current {
                if target.kind == crate::models::tags::NodeKind::Module {
                    self.step_rust_module_member(owner, target, segment, &mut next);
                } else if segment_count == index + 1 && matches!(role, SymbolRole::Callable) {
                    if target.kind == crate::models::tags::NodeKind::Constant {
                        if let Some(method) =
                            languages::by_id(owner.language.as_str()).and_then(|profile| {
                                profile
                                    .linker()
                                    .explicit_constant_member(target, segment, owner, self)
                            })
                        {
                            next.push(method);
                        }
                    } else if let Symbol::Callable(method) =
                        self.member(target, segment, true, owner)
                    {
                        next.push(method);
                    } else if let Symbol::Callable(method) =
                        self.member(target, segment, false, owner)
                    {
                        next.push(method);
                    }
                }
            }
            next.sort_by_key(|node| node.id.as_str());
            next.dedup_by_key(|node| node.id.as_str());
            current = next;
        }
        self.classify(current.into_iter(), role)
    }
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn special_member_result<'a>(
    context: &Context<'a>,
    receiver: &TypeTarget<'a>,
    member: &str,
    owner: &'a Node,
) -> Option<Option<TypeTarget<'a>>> {
    let extension = context.rayon_extension_base(receiver, member, owner);
    let extension_receiver = extension.map(|base| TypeTarget::External {
        module: base.to_owned(),
        import_line: 0,
    });
    let receiver = extension_receiver.as_ref().unwrap_or(receiver);
    let TypeTarget::External {
        module,
        import_line,
    } = receiver
    else {
        return None;
    };
    Some((|| {
        let base = module.replace('.', "::");
        let (pack, rule) = context.dependencies.class_hierarchy_authority_for_path(
            &owner.path,
            &base,
            extension.map(|_| "rust-cargo"),
        )?;
        if !rule.members.iter().any(|allowed| allowed == member) {
            return None;
        }
        context.project_hierarchy_policy(
            owner,
            receiver,
            pack,
            rule.returns.get(member)?,
            *import_line,
        )
    })())
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn special_property_lookup<'s, 'a>(
    context: &'s Context<'a>,
    receiver: &'s TypeTarget<'a>,
    member: &str,
    caller: &'a Node,
) -> Option<super::super::semantic_context::Member<'s>> {
    use crate::linker::semantic_context::Member;
    use crate::linker::value_flow::SemanticResolver;
    match receiver {
        TypeTarget::RustReference { target, .. } | TypeTarget::RustDeref { target, .. } => {
            Some(context.lookup_property(target, member, caller))
        }
        TypeTarget::Local(class) => {
            let mut current = *class;
            for _ in 0..8 {
                let mut key = String::new();
                let mut fields = context
                    .by_qual
                    .get(crate::linker::types::qualified(
                        &mut key,
                        &current.qualname,
                        member,
                    ))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|field| {
                        field.kind == crate::models::tags::NodeKind::Field
                            && *field.detail_entry(
                                crate::models::details::NodeDetailKey::RUST_DECLARED_FIELD,
                            ) == true
                            && field.path == current.path
                            && super::forwarding::field_visible(field, current, &caller.qualname)
                    });
                match (fields.next(), fields.next()) {
                    (Some(field), None) => return Some(Member::Local(field)),
                    (Some(_), Some(_)) => return Some(Member::Ambiguous),
                    _ => {}
                }
                let Some(next) = context
                    .member_state("rust")
                    .and_then(|state| state.deref_target(current))
                else {
                    break;
                };
                current = next;
            }
            Some(Member::Unknown)
        }
        TypeTarget::External { .. }
            if context
                .resolve_member_result(
                    receiver,
                    member,
                    caller,
                    crate::linker::value_flow::node_position(caller),
                )
                .is_some() =>
        {
            Some(context.lookup_member(receiver, member, caller))
        }
        _ => None,
    }
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn special_member_lookup<'s, 'a>(
    context: &'s Context<'a>,
    receiver: &'s TypeTarget<'a>,
    member: &str,
    caller: &'a Node,
    associated: bool,
) -> Option<super::super::semantic_context::Member<'s>> {
    use crate::languages::profile::{LanguageMemberResult, ProfileLinker};
    use crate::linker::semantic_context::Member;
    use crate::linker::value_flow::RustWrapperKind;

    let (name, standard) = match receiver {
        TypeTarget::RustCollection { .. } => ("Vec", !associated),
        TypeTarget::RustTypedContainer { base, .. } => (*base, !associated),
        TypeTarget::RustSlice { .. } => ("Slice", !associated),
        TypeTarget::RustTuple(_) => ("Tuple", !associated),
        TypeTarget::RustIterator { .. } => {
            return Some(
                if !associated
                    && languages::by_id("rust")
                        .is_some_and(|p| p.builtin_member("Iterator", member))
                {
                    Member::Builtin
                } else {
                    Member::Unknown
                },
            );
        }
        TypeTarget::RustWrapper { wrapper, ok, .. } => {
            let name = match wrapper {
                RustWrapperKind::Option => "Option",
                RustWrapperKind::Result => "Result",
            };
            let shape = !matches!(member, "cloned" | "copied")
                || matches!(
                    ok.as_ref(),
                    TypeTarget::RustReference { mutable: false, .. }
                );
            (name, !associated && shape)
        }
        TypeTarget::RustDeref {
            wrapper, target, ..
        } => {
            if associated {
                return Some(Member::Unknown);
            }
            if context
                .rayon_extension_base(receiver, member, caller)
                .is_some()
            {
                return Some(Member::External("rayon", 0, None));
            }
            let linker = languages::by_id(caller.language.as_str()).map(|profile| profile.linker());
            if linker.is_some_and(|linker| linker.wrapper_value_member(wrapper, member))
                || matches!(target.as_ref(), TypeTarget::External { module, .. } if linker.is_some_and(|linker| linker.standard_external_member(module, member)))
            {
                return Some(Member::Builtin);
            }
            return Some(context.lookup_member_mode(target, member, caller, associated));
        }
        TypeTarget::Builtin(name) => (name.as_str(), true),
        _ => return None,
    };
    if !associated
        && context
            .rayon_extension_base(receiver, member, caller)
            .is_some()
    {
        return Some(Member::External("rayon", 0, None));
    }
    let standard = standard
        && if matches!(receiver, TypeTarget::Builtin(_)) {
            context.lookup_builtin_member(&caller.path, caller.language.as_str(), name, member)
        } else {
            languages::by_id("rust").is_some_and(|p| p.builtin_member(name, member))
        };
    Some(if standard {
        Member::Builtin
    } else {
        match context.lookup_language_builtin("rust", name, member, associated, caller) {
            LanguageMemberResult::Local(node) => Member::Local(node),
            LanguageMemberResult::Candidates(nodes) if nodes.len() > 1 => Member::Ambiguous,
            _ => Member::Unknown,
        }
    })
}

pub(in crate::linker) fn receiver_symbol<'a>(
    lookup: Option<&LanguageMemberLookup<'_, 'a>>,
    owner: &'a Node,
    name: &str,
    mut resolve_type: impl FnMut(&str) -> Symbol<'a>,
) -> Option<Symbol<'a>> {
    if name == "Self" {
        #[cfg(feature = "lang-rust")]
        if let Some(receiver) = lookup.and_then(|lookup| lookup.receiver_for("rust", owner)) {
            return Some(Symbol::Type(TypeTarget::Local(receiver)));
        }
        if let Some(receiver_type) = owner
            .detail_entry(crate::models::details::NodeDetailKey::RECEIVER_TYPE)
            .as_str()
            .filter(|name| *name != "Self")
        {
            let clean = receiver_type_name(receiver_type)
                .unwrap_or(std::borrow::Cow::Borrowed(receiver_type));
            return Some(resolve_type(clean.as_ref()));
        }
        return None;
    }
    #[cfg(feature = "lang-rust")]
    {
        let clean = name.replace("::", ".");
        if let Some((first, _)) = clean.split_once('.') {
            if (super::forwarding::is_registered_cargo_crate(first)
                || crate::languages::rust::visibility::is_rust_stdlib(first))
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
        let Some(receiver) = lookup.receiver_for("rust", owner) else {
            return Some(Symbol::Unknown);
        };
        let methods = lookup
            .lookup_member("rust", receiver, member, true, owner)
            .into_nodes();
        match methods.as_slice() {
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
) -> Option<ReceiverType<'a>> {
    let profile = profile.filter(|profile| profile.id() == "rust")?;
    if !dependencies.declares_for_path(profile.family(), path, head) {
        return None;
    }
    let original = annotation.trim().trim_start_matches('&').trim_start();
    let original = original.strip_prefix("mut ").unwrap_or(original);
    Some(match original.split_once("::") {
        Some((origin, _)) if origin == head => ReceiverType::External(origin, 0),
        _ => ReceiverType::Unknown,
    })
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn external_annotation_type<'a>(
    profile: Option<&dyn LanguageProfile>,
    raw_head: &'a str,
    head: &str,
    path: &str,
    dependencies: &DependencyRegistry,
) -> Option<ReceiverType<'a>> {
    let profile = profile.filter(|profile| profile.id() == "rust")?;
    (profile.external_import(head).is_some()
        || dependencies.declares_for_path(profile.family(), path, head))
    .then_some(ReceiverType::External(raw_head, 0))
}

#[cfg(feature = "lang-rust")]
fn rust_crate_root_prefix(path: &str) -> Option<&str> {
    if path.starts_with("src/bin/") || path.contains("/src/bin/") {
        None
    } else if path.starts_with("src/") {
        Some("")
    } else {
        path.split_once("/src/").map(|(prefix, _)| prefix)
    }
}

#[cfg(feature = "lang-rust")]
fn register_rust_crate_anchors<'a>(
    context: &mut Context<'a>,
    rust_modules: &[(&'a Facts, &'a Node)],
) {
    let mut module_by_qualname: HashMap<&'a str, &'a Node> = HashMap::new();
    let mut crate_roots: HashMap<&'a str, &'a Node> = HashMap::new();
    for &(_, module) in rust_modules {
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

    let zero_pos = crate::semantic::SourcePosition { line: 0, column: 0 };
    for &(_, module) in rust_modules {
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
                (parent == src_ns).then_some(crate_root).flatten()
            })
        });
        let anchors = [
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
}

#[cfg(feature = "lang-rust")]
fn has_local_scope_member(
    facts: &Facts,
    scope_qualname: &str,
    name: &str,
    skip_mod_decl: bool,
) -> bool {
    facts.nodes.iter().any(|node| {
        (!skip_mod_decl || node.kind != crate::models::tags::NodeKind::ModuleDeclaration)
            && node
                .qualname
                .rsplit_once('.')
                .is_some_and(|(scope, candidate)| scope == scope_qualname && candidate == name)
    })
}

#[cfg(feature = "lang-rust")]
fn apply_forwarded_module_globs<'a>(
    context: &mut Context<'a>,
    rust_forwarding: &'a (dyn crate::languages::profile::LanguageImportState + 'a),
    rust_modules: &[(&'a Facts, &'a Node)],
) {
    for &(facts, module) in rust_modules {
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
            if has_local_scope_member(facts, &module.qualname, name, true) {
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
}

#[cfg(feature = "lang-rust")]
fn insert_nested_glob_binding<'a>(
    aliases: &mut HashMap<crate::interner::SymbolId, Imported<'a>>,
    name_id: crate::interner::SymbolId,
    targets: &[&'a Node],
    position: crate::semantic::SourcePosition,
) {
    match aliases.entry(name_id) {
        hashbrown::hash_map::Entry::Vacant(entry) => {
            entry.insert(if targets.is_empty() {
                Imported::Unknown
            } else {
                Imported::Nodes(targets.to_vec(), position, false)
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

#[cfg(feature = "lang-rust")]
fn resolve_single_glob_provider<'a>(
    imports: &RustGlobImports<'a, '_>,
    module: &Node,
    reference: &crate::models::Reference,
) -> Option<(&'a Node, String)> {
    let raw = reference.module.as_deref().unwrap_or(&reference.expression);
    let normalized = imports
        .normalized_imports
        .get(&(module.path.as_str(), raw))
        .and_then(Option::as_ref)?;
    let providers = super::forwarding::modules_at_import(
        imports.modules,
        languages::LanguageFamily("rust"),
        normalized,
        languages::workspace_path(&module.path).0,
        |namespace| {
            imports
                .rust_forwarding
                .module_path_visible(namespace, &module.qualname)
        },
    );
    let [(provider, tail)] = providers.as_slice() else {
        return None;
    };
    let (prefix, resolved_tail) = if reference.alias.as_deref() == Some("*") {
        let Some(prefix) = tail
            .strip_suffix(".*")
            .or_else(|| (!matches!(tail.as_str(), "" | "*")).then_some(tail.as_str()))
        else {
            return Some((*provider, tail.clone()));
        };
        (prefix, "*")
    } else if let Some((prefix, last)) = tail.rsplit_once('.') {
        (prefix, last)
    } else {
        return Some((*provider, tail.clone()));
    };
    let mods: Vec<_> = imports
        .rust_forwarding
        .resolve_path(provider, prefix, &module.qualname, None)
        .into_iter()
        .filter(|node| node.kind == crate::models::tags::NodeKind::Module)
        .collect();
    match mods.as_slice() {
        [target_mod] => Some((*target_mod, resolved_tail.to_owned())),
        _ => None,
    }
}

#[cfg(feature = "lang-rust")]
fn apply_nested_use_globs<'a>(context: &mut Context<'a>, imports: &RustGlobImports<'a, '_>) {
    for &(facts, module) in imports.rust_modules {
        for reference in facts.references.iter().filter(|reference| {
            reference.kind == "imports"
                && reference.source != module.id
                && reference.alias.as_deref() == Some("*")
        }) {
            let Some(owner) = context
                .symbol_id(&reference.source)
                .and_then(|id| imports.by_id.get(&id).copied())
            else {
                continue;
            };
            let Some((provider, tail)) = resolve_single_glob_provider(imports, module, reference)
            else {
                continue;
            };
            if !matches!(tail.as_str(), "" | "*") {
                continue;
            }
            let owner_id = context
                .symbol_id(&owner.id)
                .expect("Rust import owner id is interned");
            for (name, targets) in imports
                .rust_forwarding
                .visible_bindings(provider, &module.qualname)
            {
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
                if has_local_scope_member(facts, &owner.qualname, name, false) {
                    aliases.remove(&name_id);
                    continue;
                }
                insert_nested_glob_binding(aliases, name_id, targets, reference.position());
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
fn apply_registered_cargo_aliases<'a>(context: &mut Context<'a>, all: &'a dyn crate::FactsLookup) {
    let alias_counts = collect_external_alias_counts(all);
    for facts in all.values_facts() {
        for reference in facts.references.iter().filter(|r| r.kind == "imports") {
            let expr = reference.module.as_deref().unwrap_or(&reference.expression);
            let first = expr.split("::").next().unwrap_or(expr);
            let has_local_module = facts.nodes.iter().any(|node| {
                (node.kind == crate::models::tags::NodeKind::Module
                    || node.kind == crate::models::tags::NodeKind::ModuleDeclaration)
                    && (node.name == first
                        || node
                            .qualname
                            .rsplit_once('.')
                            .is_some_and(|(_, n)| n == first))
            });
            if !has_local_module && super::forwarding::is_registered_cargo_crate(first) {
                register_cargo_import_aliases(context, reference, expr, &alias_counts);
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
pub(in crate::linker) fn import_globs<'a>(
    context: &mut Context<'a>,
    all: &'a dyn crate::FactsLookup,
    by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules: &'a ModulesByNamespace<'a>,
) {
    let Some(rust_forwarding) = context.import_state("rust") else {
        return;
    };
    let rust_modules: Vec<_> = all
        .values_facts()
        .filter_map(|facts| {
            facts
                .nodes
                .iter()
                .find(|node| {
                    node.kind == crate::models::tags::NodeKind::Module
                        && node.language == crate::models::tags::LanguageId::Rust
                })
                .map(|module| (facts, module))
        })
        .collect();

    register_rust_crate_anchors(context, &rust_modules);
    apply_forwarded_module_globs(context, rust_forwarding, &rust_modules);

    let glob_imports = RustGlobImports {
        all,
        by_id,
        normalized_imports,
        modules,
        rust_forwarding,
        rust_modules: &rust_modules,
    };
    apply_nested_use_globs(context, &glob_imports);
    apply_registered_cargo_aliases(context, all);

    // Carry external provenance through ancestor imports independently of file order.
    // Only ancestor imports can expose private bindings; unrelated glob providers
    // continue to use the public forwarding projection above.
    propagate_ancestor_external_imports(context, &glob_imports);
}

#[cfg(feature = "lang-rust")]
struct RustGlobImports<'a, 'b> {
    all: &'a dyn crate::FactsLookup,
    by_id: &'a HashMap<crate::interner::SymbolId, &'a Node>,
    normalized_imports: &'a HashMap<(&'a str, &'a str), Option<ImportPath>>,
    modules: &'a ModulesByNamespace<'a>,
    rust_forwarding: &'a (dyn crate::languages::profile::LanguageImportState + 'a),
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
        node.kind != crate::models::tags::NodeKind::ModuleDeclaration
            && node.kind != crate::models::tags::NodeKind::Impl
            && node
                .qualname
                .rsplit_once('.')
                .is_some_and(|(scope, candidate)| scope == module_qualname && candidate == name)
    })
}

#[cfg(feature = "lang-rust")]
type ExternalImportAddition<'a> = (
    crate::interner::SymbolId,
    crate::interner::SymbolId,
    String,
    crate::semantic::SourcePosition,
    &'a crate::models::Reference,
);

#[cfg(feature = "lang-rust")]
fn collect_super_glob_additions<'a>(
    context: &Context<'a>,
    imports: &RustGlobImports<'a, '_>,
    additions: &mut Vec<ExternalImportAddition<'a>>,
) {
    for facts in imports.all.values_facts() {
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
            let Some(owner) = imports.by_id.get(&owner_id) else {
                continue;
            };
            let Some(module) = find_owner_module(context, owner) else {
                continue;
            };
            let namespace = if module.kind == crate::models::tags::NodeKind::Module {
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
                    imports
                        .modules
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
                    additions.push((
                        owner_id,
                        name,
                        origin.clone(),
                        reference.position(),
                        *original,
                    ));
                }
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
fn collect_scoped_reexport_additions<'a>(
    context: &Context<'a>,
    imports: &RustGlobImports<'a, '_>,
    conflicts: &hashbrown::HashSet<(crate::interner::SymbolId, crate::interner::SymbolId)>,
    additions: &mut Vec<ExternalImportAddition<'a>>,
) {
    for (facts, module) in imports.rust_modules {
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.kind == "imports" && reference.alias.is_some())
        {
            let glob = reference.alias.as_deref() == Some("*");
            let Some((provider, tail)) = resolve_single_glob_provider(imports, module, reference)
            else {
                continue;
            };
            if (glob && !matches!(tail.as_str(), "" | "*"))
                || (!glob && (tail.is_empty() || tail.contains('.') || tail == "*"))
            {
                continue;
            }
            let (Some(owner_id), Some(provider_id)) = (
                context.symbol_id(&reference.source),
                context.symbol_id(&provider.id),
            ) else {
                continue;
            };
            let requester_qual = imports
                .by_id
                .get(&owner_id)
                .map_or(module.qualname.as_str(), |owner| owner.qualname.as_str());
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
                if !imports.rust_forwarding.external_binding_visible(
                    provider,
                    provider_name,
                    requester_qual,
                    original.source == provider.id,
                ) {
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
                    additions.push((
                        owner_id,
                        name_id,
                        origin.clone(),
                        reference.position(),
                        src_ref,
                    ));
                }
            }
        }
    }
}

#[cfg(feature = "lang-rust")]
fn propagate_ancestor_external_imports<'a>(
    context: &mut Context<'a>,
    imports: &RustGlobImports<'a, '_>,
) {
    let mut conflicts = hashbrown::HashSet::new();
    for _ in 0..64 {
        let mut additions = Vec::new();
        collect_super_glob_additions(context, imports, &mut additions);
        collect_scoped_reexport_additions(context, imports, &conflicts, &mut additions);
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

pub(in crate::linker) fn nominal_application<'a>(
    owner: &Node,
    nominal: &Symbol<'a>,
) -> Option<TypeTarget<'a>> {
    if owner.language != crate::models::tags::LanguageId::Rust {
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
    owner.language == crate::models::tags::LanguageId::Rust
        && matches!(wrapper, TypeTarget::Builtin(name)
            if (name == "Option" && argument_count == 1)
                || (name == "Result" && argument_count <= 2))
}

pub(in crate::linker) fn standard_member(module: &str, member: &str) -> bool {
    if matches!(member, "new" | "from" | "with_capacity" | "default") {
        return false;
    }
    let module = import_path(module);
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
            languages::by_id("rust")
                .is_some_and(|profile| profile.builtin_member("HashMap", member))
                || matches!(
                    member,
                    "get_index"
                        | "get_index_mut"
                        | "sort_keys"
                        | "shift_remove"
                        | "swap_remove"
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
