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

use crate::languages::profile::ProfileLinker;
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
        let language = match receiver {
            TypeTarget::Local(class) => class.language,
            _ => caller.language,
        };
        if let Some(result) = crate::languages::by_id(language.as_str()).and_then(|profile| {
            profile
                .linker()
                .special_property_lookup(self, receiver, member, caller)
        }) {
            return result;
        }
        match receiver {
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
        let normalized = crate::languages::by_id(language).map_or(receiver, |profile| {
            profile.linker().normalize_builtin_receiver(receiver)
        });
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
            } => receiver
                .rule
                .members
                .iter()
                .any(|allowed| allowed == member),
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
        if let Some(state) = self
            .member_state(receiver.language.as_str())
            .filter(|state| state.handles_receiver(receiver))
        {
            return match state.lookup_local_member(receiver, member, associated, caller) {
                crate::languages::profile::LanguageMemberResult::Local(node) => Member::Local(node),
                crate::languages::profile::LanguageMemberResult::Candidates(nodes)
                    if nodes.len() > 1 =>
                {
                    Member::Ambiguous
                }
                crate::languages::profile::LanguageMemberResult::Candidates(nodes) => nodes
                    .first()
                    .copied()
                    .map_or(Member::Unknown, Member::Local),
                crate::languages::profile::LanguageMemberResult::External {
                    module,
                    import_line,
                    provider,
                    ..
                } => Member::External(module, import_line, provider),
                crate::languages::profile::LanguageMemberResult::Ambiguous => Member::Ambiguous,
                _ if state.builtin_local_member(receiver, member, associated) => Member::Builtin,
                _ => Member::Unknown,
            };
        }
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
                            .detail_get(crate::models::details::NodeDetailKey::IS_STUB)
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let is_overload = n
                            .detail_get(crate::models::details::NodeDetailKey::IS_OVERLOAD)
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
            crate::languages::by_id(caller.language.as_str()).and_then(|profile| {
                profile
                    .linker()
                    .external_member_policy(caller, module, import_line, member)
            })
        {
            match outcome {
                crate::languages::profile::LanguageMemberResult::External { provider, .. } => {
                    Member::External(module, import_line, provider)
                }
                _ => Member::Unknown,
            }
        } else {
            Member::External(module, import_line, None)
        }
    }

    fn lookup_collection_member<'s>(
        &'s self,
        base: &str,
        member: &str,
        caller: &Node,
    ) -> Member<'s> {
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
        if self.lookup_builtin_member(&caller.path, caller.language.as_str(), base, member) {
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
        if let Some(profile) = crate::languages::by_id(caller.language.as_str()) {
            let target = profile.linker().unwrap_reference(receiver);
            if !std::ptr::eq(target, receiver) {
                return self.lookup_member_mode(target, member, caller, associated);
            }
            if let Some(result) = profile
                .linker()
                .special_member_lookup(self, receiver, member, caller, associated)
            {
                return result;
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
            TypeTarget::ClassType(inner)
                if crate::languages::by_id(caller.language.as_str()).is_some_and(|profile| {
                    profile.linker().class_type_uses_associated_lookup()
                }) =>
            {
                self.lookup_member_mode(inner, member, caller, true)
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
                if self.lookup_builtin_member(
                    &caller.path,
                    caller.language.as_str(),
                    "Promise",
                    member,
                ) {
                    Member::Builtin
                } else {
                    Member::Unknown
                }
            }
            TypeTarget::Collection { base, .. } => {
                self.lookup_collection_member(base, member, caller)
            }
            TypeTarget::Builtin(name) => {
                let standard = self.lookup_builtin_member(
                    &caller.path,
                    caller.language.as_str(),
                    name,
                    member,
                );
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
}
