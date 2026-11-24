//! Canonical symbol admission for the shared value-flow reducer.
//!
//! Import providers are selected once using the same language linker and
//! workspace namespaces as ordinary graph linking. No disk or SQL is consulted
//! while reducing values or resolving calls.

mod components;
pub(crate) mod context;
mod factories;
mod preparation;
mod type_resolution;

pub(in crate::linker) use context::{Context, Imported, LanguageMemberLookup, Member};

use context::*;

pub(in crate::linker) fn component_names(
    path: &str,
    project: &str,
    root: &str,
    extension: &str,
) -> Option<(String, String)> {
    components::component_names(path, project, root, extension)
}

impl<'a> Context<'a> {
    pub(in crate::linker) fn with_flows(
        mut self,
        flows: Option<&'a HashMap<SymbolId, &'a crate::typed_facts::FlowState>>,
        uncertainties: Option<
            &'a HashMap<SymbolId, &'a [crate::semantic::NamedDefinitionUncertainty]>,
        >,
        faults: Option<&HashSet<SymbolId>>,
        all: &'a dyn crate::FactsLookup,
        dependencies: &'a languages::dependency_registry::DependencyRegistry,
    ) -> Self {
        self.flows = flows;
        self.named = super::named_bindings::NamedBindings::build_lookup(
            all,
            self.interner,
            flows,
            uncertainties,
            faults,
            dependencies,
            self.by_id,
        );
        self.prepare_registry_bindings();
        self.prepare_factory_bindings(all);
        self.prepare_component_environments(all, dependencies);
        self.prepare_callable_aliases(all);
        let mut factory_callees = HashSet::new();
        factory_callees.extend(self.factory_imports.keys().map(|(_, callee_id)| *callee_id));
        factory_callees.extend(
            self.factory_bindings
                .keys()
                .map(|(_, callee_id)| *callee_id),
        );
        factory_callees.extend(self.named.factories.keys().map(|(_, binding)| *binding));
        for scopes in self.result_scopes.values() {
            for scope in scopes {
                factory_callees.extend(scope.calls.keys().copied());
            }
        }
        for map in self.implicit_builtins.values() {
            factory_callees.extend(map.keys().copied());
        }
        for map in self.component_builtins.values() {
            factory_callees.extend(map.keys().copied());
        }
        self.factory_callees = factory_callees;
        self
    }

    pub(in crate::linker) fn lookup_property<'s>(
        &'s self,
        receiver: &'s TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
    ) -> Member<'s> {
        match receiver {
            #[cfg(feature = "lang-rust")]
            TypeTarget::Local(class) if class.language == "rust" => {
                let mut current = *class;
                for _ in 0..8 {
                    let mut key = String::new();
                    let mut fields = self
                        .by_qual
                        .get(crate::linker::types::qualified(&mut key, &current.qualname, member))
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|field| {
                            field.kind == "field"
                                && field.details["rust_declared_field"] == true
                                && field.path == current.path
                                && super::rust::forwarding::field_visible(
                                    field,
                                    current,
                                    &caller.qualname,
                                )
                        });
                    match (fields.next(), fields.next()) {
                        (Some(field), None) => return Member::Local(field),
                        (Some(_), Some(_)) => return Member::Ambiguous,
                        _ => {}
                    }
                    let Some(next) = self.member_state("rust").and_then(|rust| rust.deref_target(current)) else {
                        break;
                    };
                    current = next;
                }
                Member::Unknown
            }
            TypeTarget::RustReference { target, .. } | TypeTarget::RustDeref { target, .. } => {
                self.lookup_property(target, member, caller)
            }
            TypeTarget::ConfiguredReceiver {
                authority,
                origin,
                fallback,
                evidence,
            } => {
                if let super::value_flow::ConfiguredReceiverAuthority::Nominal {
                    receiver, ..
                } = authority.as_ref()
                {
                    if receiver.rule.properties.contains_key(member) {
                        return Member::ConfiguredExternal {
                            authority,
                            origin,
                            evidence,
                        };
                    }
                }
                fallback.as_deref().map_or(Member::Unknown, |fallback| {
                    self.lookup_property(fallback, member, caller)
                })
            }
            TypeTarget::Builtin(receiver)
                if self
                    .property_type_for_path(&caller.path, receiver, member)
                    .is_some() =>
            {
                Member::Builtin
            }
            TypeTarget::External { .. }
                if caller.language == "rust"
                    && self
                        .resolve_member_result(
                            receiver,
                            member,
                            caller,
                            super::value_flow::node_position(caller),
                        )
                        .is_some() =>
            {
                self.lookup_member(receiver, member, caller)
            }
            TypeTarget::Local(_) => self.lookup_component_template_field(receiver, member),
            _ => Member::Unknown,
        }
    }

    pub(in crate::linker) fn lookup_builtin_member(
        &self,
        path: &str,
        language: &str,
        receiver: &str,
        member: &str,
    ) -> bool {
        let normalized = if matches!(language, "typescript" | "javascript" | "vue") {
            let unprefixed = receiver.strip_prefix("Web.").unwrap_or(receiver);
            crate::linker::typescript::semantic_context::typed_dom_receiver(unprefixed).unwrap_or(unprefixed)
        } else {
            receiver
        };
        if self
            .receiver_type(path, normalized)
            .is_some_and(|rule| rule.members.iter().any(|allowed| allowed == member))
        {
            return true;
        }
        crate::languages::by_id(language).is_some_and(|profile| {
            profile.builtin_member(receiver, member) || profile.builtin_member(normalized, member)
        })
    }

    fn lookup_configured_receiver_member<'s>(
        &'s self,
        (authority, origin, fallback, evidence): (
            &'s crate::linker::value_flow::ConfiguredReceiverAuthority,
            &'s TypeTarget<'a>,
            &'s Option<Box<TypeTarget<'a>>>,
            &'s str,
        ),
        member: &str,
        caller: &'a Node,
        associated: bool,
    ) -> Member<'s> {
        let admitted = match authority {
            crate::linker::value_flow::ConfiguredReceiverAuthority::Hierarchy { rule, .. } => {
                rule.members.iter().any(|allowed| allowed == member)
            }
            crate::linker::value_flow::ConfiguredReceiverAuthority::Nominal {
                receiver, ..
            } => receiver.rule.members.iter().any(|allowed| allowed == member),
        };
        if admitted {
            Member::ConfiguredExternal {
                authority,
                origin,
                evidence,
            }
        } else if let Some(fallback) = fallback {
            self.lookup_member_mode(fallback, member, caller, associated)
        } else {
            Member::Unknown
        }
    }

    #[cfg(feature = "lang-rust")]
    fn lookup_rust_deref_member<'s>(
        &'s self,
        receiver: &'s TypeTarget<'a>,
        wrapper: &str,
        target: &'s TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
        associated: bool,
    ) -> Member<'s> {
        if associated {
            return Member::Unknown;
        }
        if self.rayon_extension_base(receiver, member, caller).is_some() {
            return Member::External("rayon", 0, None);
        }
        if crate::linker::rust::semantic_context::wrapper_value_member(wrapper, member) {
            return Member::Builtin;
        }
        match target {
            TypeTarget::External { module, .. }
                if crate::linker::rust::semantic_context::standard_member(module, member) =>
            {
                Member::Builtin
            }
            _ => self.lookup_member_mode(target, member, caller, associated),
        }
    }

    fn indexed_member_candidates(
        &self,
        owner: SymbolId,
        member: SymbolId,
        associated: bool,
        visited: &mut Vec<SymbolId>,
    ) -> Vec<&'a Node> {
        if visited.contains(&owner) {
            return Vec::new();
        }
        visited.push(owner);
        if let Some(candidates) = self.members.get(&(owner, member, associated)) {
            let candidates = candidates.clone();
            visited.pop();
            return candidates;
        }
        let inherited = self
            .member_bases
            .get(&owner)
            .into_iter()
            .flatten()
            .flat_map(|base| self.indexed_member_candidates(*base, member, associated, visited))
            .collect();
        visited.pop();
        inherited
    }

    fn lookup_local_member_mode<'s>(
        &'s self,
        receiver: &'a Node,
        member: &str,
        caller: &'a Node,
        associated: bool,
    ) -> Member<'s> {
        if receiver.language != "python" {
            if let Some(state) = self.member_state(receiver.language.as_str()) {
                return match state.lookup_member(receiver, member, associated, caller) {
                    crate::languages::profile::LanguageMemberResult::Local(node) => {
                        Member::Local(node)
                    }
                    crate::languages::profile::LanguageMemberResult::Candidates(nodes)
                        if nodes.len() > 1 =>
                    {
                        Member::Ambiguous
                    }
                    crate::languages::profile::LanguageMemberResult::Candidates(nodes) => {
                        nodes.first().copied().map_or(Member::Unknown, Member::Local)
                    }
                    _ if receiver.language == "rust"
                        && !associated
                        && member == "to_string"
                        && state.has_display(receiver) =>
                    {
                        Member::Builtin
                    }
                    _ => Member::Unknown,
                };
            }
        }
        #[cfg(not(feature = "lang-rust"))]
        let _ = caller;
        let candidates = self
            .interner
            .lookup_symbol(&receiver.id)
            .zip(self.interner.lookup_symbol(member))
            .map_or_else(Vec::new, |(receiver, member)| {
                self.indexed_member_candidates(receiver, member, associated, &mut Vec::new())
            });
        match candidates.as_slice() {
            [node] => Member::Local(node),
            nodes if nodes.len() > 1 => {
                let non_stubs: Vec<&Node> = nodes
                    .iter()
                    .copied()
                    .filter(|n| {
                        let is_stub = n
                            .details
                            .get("is_stub")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let is_overload = n
                            .details
                            .get("is_overload")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        !is_stub && !is_overload
                    })
                    .collect();
                if non_stubs.len() == 1 {
                    Member::Local(non_stubs[0])
                } else {
                    Member::Ambiguous
                }
            }
            _ => Member::Unknown,
        }
    }

    fn lookup_external_member_mode<'s>(
        &'s self,
        module: &'s str,
        import_line: usize,
        member: &str,
        caller: &'a Node,
    ) -> Member<'s> {
        if let Some(receiver) = self.receiver_type(&caller.path, module) {
            if receiver.members.iter().any(|allowed| allowed == member) {
                Member::External(module, import_line, Some(module))
            } else {
                Member::Unknown
            }
        } else if let Some(outcome) =
            crate::linker::python::semantic_context::external_member(caller, module, member)
        {
            match outcome {
                crate::linker::python::semantic_context::ExternalMemberOutcome::Supported(origin) => {
                    Member::External(module, import_line, Some(origin))
                }
                crate::linker::python::semantic_context::ExternalMemberOutcome::Unsupported => {
                    Member::Unknown
                }
            }
        } else {
            Member::External(module, import_line, None)
        }
    }

    fn lookup_python_class_member<'s>(&'s self, receiver: &Node, member: &str) -> Member<'s> {
        match self.lookup_language_member("python", receiver, member, false, receiver) {
            crate::languages::profile::LanguageMemberResult::Local(node) => {
                Member::Local(node)
            }
            crate::languages::profile::LanguageMemberResult::External { module, import_line: line, provider, .. } => Member::External(module, line, provider),
            crate::languages::profile::LanguageMemberResult::Ambiguous => {
                Member::Ambiguous
            }
            _ => Member::Unknown,
        }
    }

    fn lookup_collection_member<'s>(&'s self, base: &str, member: &str, caller: &Node) -> Member<'s> {
        if let Some((_, authority)) = self.hierarchy_authority_for_base(&caller.path, base) {
            if let crate::linker::value_flow::ConfiguredReceiverAuthority::Hierarchy {
                rule, ..
            } = authority.as_ref()
            {
                if rule.members.iter().any(|allowed| allowed == member) {
                    return Member::Builtin;
                }
            }
        }
        if self.lookup_builtin_member(&caller.path, &caller.language, base, member) {
            Member::Builtin
        } else {
            Member::Unknown
        }
    }

    pub(in crate::linker) fn lookup_member_mode<'s>(
        &'s self,
        receiver: &'s TypeTarget<'a>,
        member: &str,
        caller: &'a Node,
        associated: bool,
    ) -> Member<'s> {
        #[cfg(feature = "lang-rust")]
        if caller.language == "rust" {
            if let Some(target) = crate::linker::rust::value_flow::reference_target(receiver) {
                return self.lookup_member_mode(target, member, caller, associated);
            }
        }
        match receiver {
            TypeTarget::ConfiguredReceiver {
                authority,
                origin,
                fallback,
                evidence,
            } => self.lookup_configured_receiver_member(
                (authority, origin, fallback, evidence),
                member,
                caller,
                associated,
            ),
            TypeTarget::Local(receiver)
                if receiver.language == "python" && receiver.kind == "class" =>
            {
                self.lookup_python_class_member(receiver, member)
            }
            TypeTarget::ClassType(inner) if caller.language == "python" => {
                self.lookup_member_mode(inner, member, caller, true)
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustCollection { .. } if caller.language == "rust" => {
                if !associated && self.rayon_extension_base(receiver, member, caller).is_some() {
                    return Member::External("rayon", 0, None);
                }
                let standard = !associated
                    && crate::languages::by_id("rust")
                        .is_some_and(|profile| profile.builtin_member("Vec", member));
                self.lookup_rust_builtin_member("Vec", standard, member, associated, caller)
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustTypedContainer { base, .. } if caller.language == "rust" => {
                if !associated && self.rayon_extension_base(receiver, member, caller).is_some() {
                    return Member::External("rayon", 0, None);
                }
                let standard = !associated
                    && crate::languages::by_id("rust")
                        .is_some_and(|profile| profile.builtin_member(base, member));
                self.lookup_rust_builtin_member(base, standard, member, associated, caller)
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustIterator { .. } if caller.language == "rust" => {
                let standard = !associated
                    && crate::languages::by_id("rust")
                        .is_some_and(|profile| profile.builtin_member("Iterator", member));
                if standard {
                    Member::Builtin
                } else {
                    Member::Unknown
                }
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustSlice { .. } if caller.language == "rust" => {
                if !associated && self.rayon_extension_base(receiver, member, caller).is_some() {
                    return Member::External("rayon", 0, None);
                }
                let standard = !associated
                    && crate::languages::by_id("rust")
                        .is_some_and(|profile| profile.builtin_member("Slice", member));
                self.lookup_rust_builtin_member("Slice", standard, member, associated, caller)
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustTuple(_) if caller.language == "rust" => {
                let standard = !associated
                    && crate::languages::by_id("rust")
                        .is_some_and(|profile| profile.builtin_member("Tuple", member));
                self.lookup_rust_builtin_member("Tuple", standard, member, associated, caller)
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustWrapper { wrapper, ok, .. } if caller.language == "rust" => {
                let name = match wrapper {
                    crate::linker::value_flow::RustWrapperKind::Option => "Option",
                    crate::linker::value_flow::RustWrapperKind::Result => "Result",
                };
                let standard = crate::languages::by_id("rust")
                    .is_some_and(|profile| profile.builtin_member(name, member));
                let adapter_shape = !matches!(member, "cloned" | "copied")
                    || matches!(
                        ok.as_ref(),
                        TypeTarget::RustReference { mutable: false, .. }
                    );
                self.lookup_rust_builtin_member(
                    name,
                    !associated && standard && adapter_shape,
                    member,
                    associated,
                    caller,
                )
            }
            #[cfg(feature = "lang-rust")]
            TypeTarget::RustDeref {
                wrapper, target, ..
            } if caller.language == "rust" => {
                self.lookup_rust_deref_member(receiver, wrapper, target, member, caller, associated)
            }
            TypeTarget::Local(receiver) => {
                self.lookup_local_member_mode(receiver, member, caller, associated)
            }
            TypeTarget::Object(members) => members
                .get(member)
                .map_or(Member::Unknown, |node| Member::Local(node)),
            TypeTarget::External {
                module,
                import_line,
            } => self.lookup_external_member_mode(module, *import_line, member, caller),
            TypeTarget::Promise(_) => {
                if self.lookup_builtin_member(&caller.path, &caller.language, "Promise", member) {
                    Member::Builtin
                } else {
                    Member::Unknown
                }
            }
            TypeTarget::Collection { base, .. } => {
                self.lookup_collection_member(base, member, caller)
            }
            TypeTarget::Builtin(name) => {
                #[cfg(feature = "lang-rust")]
                if caller.language == "rust"
                    && !associated
                    && self
                        .rayon_extension_base(receiver, member, caller)
                        .is_some()
                {
                    return Member::External("rayon", 0, None);
                }
                let standard = self.lookup_builtin_member(&caller.path, &caller.language, name, member);
                #[cfg(feature = "lang-rust")]
                if caller.language == "rust" {
                    return self.lookup_rust_builtin_member(
                        name, standard, member, associated, caller,
                    );
                }
                if standard {
                    Member::Builtin
                } else {
                    Member::Unknown
                }
            }
            TypeTarget::Ambiguous => Member::Ambiguous,
            _ => Member::Unknown,
        }
    }

    #[cfg(feature = "lang-rust")]
    fn lookup_rust_builtin_member<'s>(
        &'s self,
        receiver_key: &str,
        standard: bool,
        member: &str,
        associated: bool,
        caller: &Node,
    ) -> Member<'s> {
        if standard {
            return Member::Builtin;
        }
        match self.lookup_language_builtin("rust", receiver_key, member, associated, caller) {
            crate::languages::profile::LanguageMemberResult::Local(node) => Member::Local(node),
            crate::languages::profile::LanguageMemberResult::Candidates(nodes) if nodes.len() > 1 => Member::Ambiguous,
            _ => Member::Unknown,
        }
    }
}
