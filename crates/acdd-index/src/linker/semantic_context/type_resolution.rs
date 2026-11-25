use crate::languages::profile::ProfileLinker;
use super::context::*;

fn scope_nodes_shadow_value<'a>(
    owner: &'a Node,
    raw_name: &str,
    head: &str,
    role: SymbolRole,
    scope_nodes: Option<&Vec<&'a Node>>,
) -> bool {
    let value_shadows_role = languages::by_id(owner.language.as_str())
        .is_none_or(|profile| profile.linker().value_shadows_role(raw_name, role));
    value_shadows_role
        && scope_nodes.is_some_and(|nodes| {
            nodes
                .iter()
                .filter(|node| node.path == owner.path)
                .any(|node| node.binding_names().any(|name| name == head))
        })
}

impl<'a> Context<'a> {
    fn resolve_receiver_rule_member(
        &self,
        owner: &'a Node,
        receiver: &TypeTarget<'a>,
        origin: &TypeTarget<'a>,
        pack: &str,
        rule: &crate::languages::manifests::syntax::ReceiverTypeRule,
        member: &str,
    ) -> Option<TypeTarget<'a>> {
        if !rule.members.iter().any(|allowed| allowed == member) {
            return None;
        }
        let spec = rule.member_returns.get(member)?;
        Some(self.project_return_node(
            spec.compiled(),
            &ReturnProjection {
                owner,
                pack: Some(pack),
                receiver: Some(receiver),
                origin,
                import_line: 0,
                is_dom_pack: pack == "web",
            },
            0,
        ))
    }

    fn resolve_configured_receiver_member(
        &self,
        owner: &'a Node,
        receiver: &TypeTarget<'a>,
        authority: &super::super::value_flow::ConfiguredReceiverAuthority,
        origin: &TypeTarget<'a>,
        member: &str,
    ) -> Option<TypeTarget<'a>> {
        use super::super::value_flow::ConfiguredReceiverAuthority as Authority;
        match authority {
            Authority::Nominal {
                pack,
                receiver: nominal,
                ..
            } => self.resolve_receiver_rule_member(
                owner,
                receiver,
                origin,
                pack,
                &nominal.rule,
                member,
            ),
            Authority::Hierarchy { pack, rule } => {
                if !rule.members.iter().any(|allowed| allowed == member) {
                    return None;
                }
                let policy = rule.returns.get(member)?;
                let import_line = match origin {
                    TypeTarget::External { import_line, .. } => *import_line,
                    _ => 0,
                };
                self.project_hierarchy_policy(owner, receiver, pack, policy, import_line)
            }
        }
    }
}

impl<'a> SemanticResolver<'a> for Context<'a> {
    fn unshadowed_global(&self, owner: &'a Node, name: &str, at: SourcePosition) -> bool {
        Context::unshadowed_global(self, owner, name, at)
    }
    fn configured_result_member(
        &self,
        result: &super::super::value_flow::ConfiguredCallResult,
        member: &str,
    ) -> bool {
        self.symbol_id(member)
            .is_some_and(|member| result.members.contains(&member))
    }

    fn resolve_member_result(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        _at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        use super::super::value_flow::ConfiguredReceiverAuthority as Authority;
        if let Some(result) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().special_member_result(self, receiver, member, owner))
        {
            return result;
        }
        if let Some(next) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().member_receiver(receiver, member))
        {
            return next.and_then(|target| self.resolve_member_result(target, member, owner, _at));
        }
        match receiver {
            TypeTarget::Builtin(name) => {
                let lookup_name = crate::languages::by_id(owner.language.as_str())
                    .map_or(name.as_str(), |profile| profile.linker().normalize_builtin_receiver(name));
                let (pack, _rule_id, rule) =
                    self.receiver_rule_with_meta(&owner.path, lookup_name)?;
                self.resolve_receiver_rule_member(owner, receiver, receiver, pack, rule, member)
            }
            TypeTarget::Collection { base, .. } => {
                if let Some((pack, authority)) = self.hierarchy_authority_for_base(&owner.path, base)
                {
                    if let Authority::Hierarchy { rule, .. } = authority.as_ref() {
                        if rule.members.iter().any(|allowed| allowed == member) {
                            if let Some(policy) = rule.returns.get(member) {
                                return self.project_hierarchy_policy(
                                    owner, receiver, &pack, policy, 0,
                                );
                            }
                        }
                    }
                }
                let (pack, _rule_id, rule) = self.receiver_rule_with_meta(&owner.path, base)?;
                self.resolve_receiver_rule_member(owner, receiver, receiver, pack, rule, member)
            }
            TypeTarget::ConfiguredReceiver {
                authority, origin, ..
            } => self.resolve_configured_receiver_member(
                owner,
                receiver,
                authority.as_ref(),
                origin.as_ref(),
                member,
            ),
            TypeTarget::Local(class) => {
                let language = class.language.as_str();
                let (pack, _row, policy) = self.member_state(language)?.return_policy(class, member)?;
                let crate::languages::profile::LanguageMemberResult::External { module, import_line: line, .. } =
                    self.lookup_language_member(language, class, member, false, owner)
                else {
                    return None;
                };
                let origin = TypeTarget::External {
                    module: module.to_owned(),
                    import_line: line,
                };
                self.project_hierarchy_policy_with_origin(
                    owner, receiver, pack, policy, line, &origin,
                )
            }
            _ => None,
        }
    }

    fn resolve_call_result(
        &self,
        owner: &'a Node,
        callee: &str,
        site: Option<&crate::semantic::CallSiteEvidence>,
        at: SourcePosition,
        fallback: Option<TypeTarget<'a>>,
    ) -> Option<TypeTarget<'a>> {
        if let Some(site) = site {
            match self.resolve_argument_factory_call(owner, callee, site, at) {
                Some(Some(TypeTarget::ConfiguredReceiver {
                    authority,
                    origin,
                    evidence,
                    ..
                })) => {
                    return Some(TypeTarget::ConfiguredReceiver {
                        authority,
                        origin,
                        fallback: fallback.map(Box::new),
                        evidence,
                    });
                }
                Some(Some(other)) => return Some(other),
                Some(None) => return fallback,
                None => {}
            }
        }
        let head = callee
            .split('.')
            .next()
            .and_then(|segment| segment.split("::").next())
            .unwrap_or(callee);
        let imported_head = self.imported_alias_in_scope(owner, head, at);
        let unshadowed = !matches!(
            self.resolve_symbol(owner, head, at, SymbolRole::Callable),
            Symbol::Callable(_)
        ) && (self.unshadowed_global(owner, head, at) || imported_head.is_some());
        if !unshadowed {
            return fallback;
        }
        let external_member = callee
            .rsplit_once('.')
            .or_else(|| callee.rsplit_once("::"))
            .and_then(|(alias, member)| {
                let imported = if alias == head {
                    imported_head
                } else {
                    self.imported_alias_in_scope(owner, alias, at)
                };
                match imported {
                    Some(Imported::External(module, _, _, _)) => Some((module.as_str(), member)),
                    _ => None,
                }
            });
        let origin = TypeTarget::External {
            module: callee.to_owned(),
            import_line: site.map_or(at.line, |site| site.position.line),
        };
        for (pack, _rule_id, rule) in self.receiver_types_with_meta_for_path(&owner.path)? {
            let spec = rule.call_returns.get(callee).or_else(|| {
                let (ext_module, member) = external_member?;
                rule.module
                    .as_deref()
                    .is_some_and(|rule_mod| rule_mod == ext_module)
                    .then(|| rule.call_returns.get(member))
                    .flatten()
            });
            if let Some(spec) = spec {
                return Some(self.project_return_node(
                    spec.compiled(),
                    &ReturnProjection {
                        owner,
                        pack: Some(pack),
                        receiver: None,
                        origin: &origin,
                        import_line: 0,
                        is_dom_pack: pack == "web",
                    },
                    0,
                ));
            }
        }
        fallback
    }
    fn property_type(&self, scope: &'a Node, receiver: &str, member: &str) -> Option<String> {
        self.property_type_for_path(&scope.path, receiver, member)
    }
    fn resolve_external_factory(
        &self,
        owner: &'a Node,
        callee: &str,
        at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        if owner.kind == crate::models::tags::NodeKind::Module
            && self
                .symbol_id(&owner.id)
                .and_then(|owner| self.flows.and_then(|flows| flows.get(&owner)))
                .is_some_and(|state| {
                    matches!(
                        state,
                        crate::typed_facts::FlowState::Valid { facts, .. }
                            if facts.component_macros().any(|fact| {
                                fact.callee == callee
                                    && fact.role == crate::semantic::ComponentMacroRole::Props
                                    && fact.position <= at
                            })
                    )
                })
        {
            return Some(TypeTarget::Local(owner));
        }
        if let Some(result) = self.resolve_factory_result(owner, callee, at) {
            return Some(result);
        }
        languages::by_id(owner.language.as_str()).and_then(|profile|
            profile.linker().external_factory_result(self, owner, callee, at))
    }
    fn resolve_factory_result(
        &self,
        owner: &'a Node,
        callee: &str,
        at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        if !languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().tracks_factory_results())
            || callee.contains('.')
        {
            return None;
        }
        let callee_id = self.symbol_id(callee)?;
        if !self.factory_callees.contains(&callee_id) {
            return None;
        }
        let mut scope = owner.qualname.as_str();
        loop {
            for lexical in self.nodes_in_path(scope, &owner.path) {
                let Some(lexical_id) = self.interner.lookup_symbol(&lexical.id) else {
                    return Some(TypeTarget::Unknown);
                };
                let imported = self.factory_imports.get(&(lexical_id, callee_id));
                if imported.is_some()
                    && self.factory_bindings.contains_key(&(lexical_id, callee_id))
                {
                    return Some(TypeTarget::Unknown);
                }
                if matches!(imported, Some(None)) {
                    return Some(TypeTarget::Unknown);
                }
                let (provider, binding) =
                    if let Some(Some((provider, binding, position))) = imported {
                        if *position >= at {
                            return Some(TypeTarget::Unknown);
                        }
                        (*provider, *binding)
                    } else {
                        (lexical, callee_id)
                    };
                if let Some(value) = self.factory_value(provider, binding, owner) {
                    return Some(value);
                }
                if imported.is_some() {
                    return None;
                }
                if self.factory_declares(lexical, callee) {
                    return None;
                }
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        if let Some(result) = self.implicit_factory_result(owner, callee) {
            return Some(result);
        }
        let (Some(path), Some(callee)) = (self.path_id(&owner.path), self.symbol_id(callee)) else {
            return None;
        };
        let mut results = self
            .result_scopes
            .get(&path)
            .into_iter()
            .flatten()
            .filter_map(|scope| scope.calls.get(&callee));
        let result = results.next()?;
        if !self.configured_unshadowed(owner, self.interner.resolve_symbol(callee)) {
            return None;
        }
        if results.next().is_some() {
            return Some(TypeTarget::Ambiguous);
        }
        Some(TypeTarget::ResultObject(result.clone()))
    }
    fn deref_target(&self, receiver: &'a Node) -> Option<&'a Node> {
        LanguageMemberLookup::new(self).deref_target(receiver.language.as_str(), receiver)
    }

    fn value_flow(&self, node: &'a Node) -> Option<&'a crate::semantic::ValueFlowFacts> {
        let node_id = self.symbol_id(&node.id)?;
        match self.flows?.get(&node_id).copied()? {
            crate::typed_facts::FlowState::Valid { facts, .. } => Some(facts),
            crate::typed_facts::FlowState::Invalid(_) => None,
        }
    }

    fn invalid_value_flow(&self, node: &'a Node) -> bool {
        self.symbol_id(&node.id).is_some_and(|node_id| {
            self.flows.is_some_and(|flows| {
                matches!(
                    flows.get(&node_id),
                    Some(crate::typed_facts::FlowState::Invalid(_))
                )
            })
        })
    }

    fn has_typed_flows(&self) -> bool {
        self.flows.is_some()
    }

    fn raw_value_flow(&self, node: &'a Node) -> Option<&'a serde_json::Value> {
        let node_id = self.symbol_id(&node.id)?;
        match self.flows?.get(&node_id).copied()? {
            crate::typed_facts::FlowState::Invalid(value) => Some(value),
            crate::typed_facts::FlowState::Valid { .. } => None,
        }
    }
    fn resolve_applied(
        &self,
        owner: &'a Node,
        base: &str,
        args: &[crate::semantic::TypeExpr],
        at: SourcePosition,
    ) -> TypeTarget<'a> {
        let clean_base = base.rsplit_once('.').map_or(base, |(_, name)| name);
        let nominal = self.resolve_symbol(owner, base, at, SymbolRole::Type);
        if let Some(target) = languages::by_id(owner.language.as_str()).and_then(|profile|
            profile.linker().resolve_applied_type(self, owner, (base, args), &nominal, at)) {
            return target;
        }
        match nominal {
            Symbol::Type(TypeTarget::External {
                module,
                import_line,
            }) if self
                .receiver_type(&owner.path, &module)
                .is_some_and(|receiver| receiver.generic) =>
            {
                TypeTarget::External {
                    module,
                    import_line,
                }
            }
            Symbol::Type(TypeTarget::Builtin(name))
                if languages::by_id(owner.language.as_str())
                    .is_some_and(|profile| profile.builtin_generic(&name)) =>
            {
                TypeTarget::Builtin(name)
            }
            Symbol::Type(TypeTarget::External { module, .. })
                if languages::by_id(owner.language.as_str()).is_some_and(|profile|
                    profile.linker().external_generic_base(&module, clean_base, profile)) =>
            {
                TypeTarget::Builtin(clean_base.to_string())
            }
            _ => TypeTarget::Unknown,
        }
    }
    fn resolve_payload(
        &self,
        owner: &'a Node,
        expression: &crate::semantic::TypeExpr,
        at: SourcePosition,
    ) -> TypeTarget<'a> {
        let crate::semantic::TypeExpr::Applied { base, args } = expression else {
            return TypeTarget::Unknown;
        };
        if args.is_empty() {
            return TypeTarget::Unknown;
        }
        let wrapper = self.resolve_applied(owner, base, args, at);
        if let Some(payload) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().value_payload(&wrapper))
        {
            return payload;
        }
        let admitted = languages::by_id(owner.language.as_str()).is_some_and(|profile|
            profile.linker().admits_type_payload(owner, &wrapper, args.len()));
        if !admitted {
            return TypeTarget::Unknown;
        }
        super::super::value_flow::resolve_type(
            owner,
            &args[0],
            at,
            self,
            super::super::value_flow::SemanticLimits::default(),
        )
    }
    fn resolve_member(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: SourcePosition,
    ) -> Symbol<'a> {
        self.resolve_member_with_mode(receiver, member, owner, at, false)
    }
    fn resolve_associated_member(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: SourcePosition,
    ) -> Symbol<'a> {
        self.resolve_member_with_mode(receiver, member, owner, at, true)
    }
    fn resolve_super_member(&self, owner: &'a Node, member: &str) -> Symbol<'a> {
        languages::by_id(owner.language.as_str()).map_or(Symbol::Unknown, |profile| {
            profile.linker().resolve_super_member(self, owner, member)
        })
    }
    fn callable_signature(&self, node: &'a Node) -> Option<&'a crate::semantic::CallableSignature> {
        self.value_flow(node)?.callable_signature.as_ref()
    }
    fn resolve_symbol(
        &self,
        owner: &'a Node,
        name: &str,
        at: SourcePosition,
        role: SymbolRole,
    ) -> Symbol<'a> {
        if let Some(symbol) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().special_symbol(self, owner, name, at, role))
        {
            return symbol;
        }
        self.resolve_symbol_raw(owner, name, at, role)
    }
    fn resolve_import_module(
        &self,
        owner: &'a Node,
        name: &str,
        at: SourcePosition,
    ) -> Option<&'a Node> {
        let (head, _) = name.split_once('.').unwrap_or((name, ""));
        let mut scope = owner.qualname.as_str();
        loop {
            for node in self.nodes_in_path(scope, &owner.path) {
                if let Some(Imported::Nodes(targets, position, _)) = self
                    .symbol_id(&node.id)
                    .and_then(|node_id| self.imports.get(&node_id))
                    .and_then(|aliases| self.symbol_id(head).and_then(|head| aliases.get(&head)))
                {
                    if *position <= at
                        || languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().import_visible_before_position(owner))
                    {
                        if let [target] = targets.as_slice() {
                            if target.kind == crate::models::tags::NodeKind::Module {
                                return Some(*target);
                            }
                        }
                    }
                }
            }
            match scope.rsplit_once('.') {
                Some((parent, _)) => scope = parent,
                None => break,
            }
        }
        None
    }
}

struct ReturnProjection<'p, 'a> {
    owner: &'a Node,
    pack: Option<&'p str>,
    receiver: Option<&'p TypeTarget<'a>>,
    origin: &'p TypeTarget<'a>,
    import_line: usize,
    is_dom_pack: bool,
}

impl<'a> Context<'a> {
    fn resolve_member_with_mode(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        _at: SourcePosition,
        associated: bool,
    ) -> Symbol<'a> {
        if let Some(next) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().member_receiver(receiver, member))
        {
            return next.map_or(Symbol::Unknown, |target| {
                self.resolve_member_with_mode(target, member, owner, _at, associated)
            });
        }
        match receiver {
            TypeTarget::ConfiguredReceiver {
                authority,
                fallback,
                ..
            } => {
                if let super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                    receiver: nominal,
                    ..
                } = authority.as_ref()
                {
                    if let Some(name) = nominal.rule.properties.get(member) {
                        return Symbol::Type(self.resolve_nominal_target(
                            owner,
                            name,
                            receiver,
                            false,
                        ));
                    }
                }
                if let Some(target) = languages::by_id(owner.language.as_str())
                    .and_then(|profile| profile.linker().synthetic_property_result(owner, receiver, member))
                {
                    return Symbol::Type(target);
                }
                fallback.as_deref().map_or(Symbol::Unknown, |fallback| {
                    self.resolve_member_with_mode(fallback, member, owner, _at, associated)
                })
            }
            TypeTarget::Builtin(receiver) => self
                .property_type_for_path(&owner.path, receiver, member)
                .map_or(Symbol::Unknown, |name| {
                    Symbol::Type(TypeTarget::Builtin(name))
                }),
            TypeTarget::Local(node) => self.member(node, member, associated, owner),
            TypeTarget::Object(members) => match members.get(member) {
                Some(node) => Symbol::Callable(node),
                None => Symbol::Unknown,
            },
            TypeTarget::ClassType(inner) if languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().class_type_uses_associated_lookup()) => {
                self.resolve_member_with_mode(inner, member, owner, _at, associated)
            }
            TypeTarget::External { .. } if languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().external_field_projection()) => self
                .resolve_member_result(receiver, member, owner, _at)
                .map_or(Symbol::Unknown, Symbol::Type),
            _ => Symbol::Unknown,
        }
    }

    fn same_nominal_authority(
        a: &super::super::value_flow::ConfiguredReceiverAuthority,
        b: &super::super::value_flow::ConfiguredReceiverAuthority,
    ) -> bool {
        matches!(
            (a, b),
            (
                super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                    pack: pack_a,
                    rule_id: rule_a,
                    ..
                },
                super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                    pack: pack_b,
                    rule_id: rule_b,
                    ..
                },
            ) if pack_a == pack_b && rule_a == rule_b
        )
    }

    fn resolve_argument_factory_call(
        &self,
        owner: &'a Node,
        callee: &str,
        site: &crate::semantic::CallSiteEvidence,
        at: SourcePosition,
    ) -> Option<Option<TypeTarget<'a>>> {
        use crate::semantic::StaticArgument;
        let (global, _member) = callee.split_once('.')?;
        let path = self.interner.lookup_path(&owner.path)?;
        let mappings = self.argument_factories.get(&path)?;
        let callee_id = self.symbol_id(callee)?;
        let mut selected: Option<
            std::sync::Arc<super::super::value_flow::ConfiguredReceiverAuthority>,
        > = None;
        for (argument, literal) in site.arguments.iter().enumerate() {
            let StaticArgument::String { value } = literal else {
                continue;
            };
            let Some(value_id) = self.symbol_id(value) else {
                continue;
            };
            for candidate in mappings
                .get(&(callee_id, argument, value_id))
                .into_iter()
                .flatten()
            {
                if let Some(previous) = &selected {
                    if !Self::same_nominal_authority(previous.as_ref(), candidate.as_ref()) {
                        return Some(None);
                    }
                    continue;
                }
                if !self.unshadowed_global(owner, global, at) {
                    return None;
                }
                selected = Some(candidate.clone());
            }
        }
        selected.map(|authority| {
            Some(TypeTarget::ConfiguredReceiver {
                authority,
                origin: Box::new(TypeTarget::External {
                    module: callee.to_owned(),
                    import_line: site.position.line,
                }),
                fallback: None,
                evidence: format!("{}:{}", self.dependencies.digest(), owner.path),
            })
        })
    }

    fn imported_alias_in_scope(
        &self,
        owner: &'a Node,
        head: &str,
        at: SourcePosition,
    ) -> Option<&Imported<'a>> {
        let mut scope = owner.qualname.as_str();
        loop {
            let mut shadowed = false;
            for node in self.nodes_in_path(scope, &owner.path) {
                if let Some(imported) = self.imported_alias(node, head) {
                    return Some(imported);
                }
                shadowed |= node.binding_names().any(|candidate| candidate == head)
                    || node.rebinding_names().any(|candidate| candidate == head)
                    || self.value_flow(node).is_some_and(|flow| {
                        flow.bindings
                            .iter()
                            .any(|binding| binding.name == head && binding.position <= at)
                    });
            }
            if shadowed {
                return None;
            }
            let (parent, _) = scope.rsplit_once('.')?;
            scope = parent;
        }
    }

    fn resolve_nominal_target(
        &self,
        owner: &'a Node,
        name: &str,
        origin: &TypeTarget<'a>,
        is_dom_pack: bool,
    ) -> TypeTarget<'a> {
        if !is_dom_pack {
            if let Some((pack, rule_id, nominal)) = self.receiver_rule_with_meta(&owner.path, name) {
                if pack != "web" {
                    return TypeTarget::ConfiguredReceiver {
                        authority: std::sync::Arc::new(
                            super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                                pack: pack.to_owned(),
                                rule_id: rule_id.to_owned(),
                                receiver: std::sync::Arc::new(
                                    super::super::value_flow::ConfiguredNominalReceiver {
                                        rule: nominal.clone(),
                                    },
                                ),
                            },
                        ),
                        origin: Box::new(origin.clone()),
                        fallback: None,
                        evidence: format!("{}:{}", self.dependencies.digest(), owner.path),
                    };
                }
            }
        }
        TypeTarget::Builtin(name.to_owned())
    }

    pub(in crate::linker) fn project_hierarchy_policy(
        &self,
        owner: &'a Node,
        receiver: &TypeTarget<'a>,
        pack: &str,
        policy: &languages::manifests::ReturnPolicy,
        import_line: usize,
    ) -> Option<TypeTarget<'a>> {
        self.project_hierarchy_policy_with_origin(
            owner,
            receiver,
            pack,
            policy,
            import_line,
            receiver,
        )
    }

    fn project_hierarchy_policy_with_origin(
        &self,
        owner: &'a Node,
        receiver: &TypeTarget<'a>,
        pack: &str,
        policy: &languages::manifests::ReturnPolicy,
        import_line: usize,
        origin: &TypeTarget<'a>,
    ) -> Option<TypeTarget<'a>> {
        use languages::manifests::{syntax::ReturnTypeSpec, ReturnPolicy};
        match policy {
            ReturnPolicy::SelfReceiver => Some(receiver.clone()),
            ReturnPolicy::Receiver { base } => {
                let compiled = self
                    .path_id(&owner.path)
                    .and_then(|path_id| self.hierarchy_return_specs.get(&path_id))
                    .and_then(|specs| specs.get(base));
                let parsed;
                let spec = match compiled {
                    Some(spec) => spec,
                    None => {
                        parsed = ReturnTypeSpec::parse(base).ok()?;
                        &parsed
                    }
                };
                let ctx = ReturnProjection {
                    owner,
                    pack: Some(pack),
                    receiver: Some(receiver),
                    origin,
                    import_line,
                    is_dom_pack: false,
                };
                let target = self.project_return_node(spec.compiled(), &ctx, 0);
                (!matches!(target, TypeTarget::Unknown)).then_some(target)
            }
        }
    }

    fn project_return_node(
        &self,
        node: &languages::manifests::syntax::ReturnTypeNode,
        ctx: &ReturnProjection<'_, 'a>,
        depth: usize,
    ) -> TypeTarget<'a> {
        use languages::manifests::syntax::ReturnTypeNode;
        if depth >= 8 {
            return TypeTarget::Unknown;
        }
        match node {
            ReturnTypeNode::SelfReceiver => ctx.receiver.cloned().unwrap_or(TypeTarget::Unknown),
            ReturnTypeNode::Unknown => TypeTarget::Unknown,
            ReturnTypeNode::Reference { mutable, inner } => {
                let projected = self.project_return_node(inner, ctx, depth + 1);
                match languages::by_id(ctx.owner.language.as_str()) {
                    Some(profile) => profile.linker().project_reference(projected, *mutable),
                    None => projected,
                }
            }
            ReturnTypeNode::Tuple(items) => {
                let projected: Vec<_> = items
                    .iter()
                    .map(|item| self.project_return_node(item, ctx, depth + 1))
                    .collect();
                match languages::by_id(ctx.owner.language.as_str()) {
                    Some(profile) => profile.linker().project_tuple(projected),
                    None => TypeTarget::Tuple(projected),
                }
            }
            ReturnTypeNode::Applied { base, args } => {
                self.project_applied_return(base, args, ctx, depth)
            }
            ReturnTypeNode::Named(name) => {
                if name == "Item" {
                    return match ctx.receiver {
                        Some(
                            TypeTarget::Collection { item, .. },
                        ) => (**item).clone(),
                        Some(TypeTarget::ConfiguredReceiver {
                            fallback: Some(fallback),
                            ..
                        }) => match fallback.as_ref() {
                            TypeTarget::Collection { item, .. } => (**item).clone(),
                            _ => TypeTarget::Unknown,
                        },
                        Some(target) => languages::by_id(ctx.owner.language.as_str())
                            .and_then(|profile| profile.linker().collection_item(target)).unwrap_or(TypeTarget::Unknown),
                        _ => TypeTarget::Unknown,
                    };
                }
                self.project_named_return(name, ctx).unwrap_or(TypeTarget::Unknown)
            }
        }
    }

    fn project_applied_return(
        &self,
        constructor: &str,
        args: &[languages::manifests::syntax::ReturnTypeNode],
        ctx: &ReturnProjection<'_, 'a>,
        depth: usize,
    ) -> TypeTarget<'a> {
        let short = constructor
            .rsplit_once("::")
            .or_else(|| constructor.rsplit_once('.'))
            .map_or(constructor, |(_, tail)| tail);
        let first = args
            .first()
            .map(|arg| self.project_return_node(arg, ctx, depth + 1))
            .unwrap_or(TypeTarget::Unknown);
        let linker = languages::by_id(ctx.owner.language.as_str()).map(|profile| profile.linker());
        if let Some(target) = linker.and_then(|linker| linker.project_applied_return(short, &first, &mut ||
            args.get(1).map(|arg| self.project_return_node(arg, ctx, depth + 1)))) {
            return target;
        }
        match short {
            "Promise" => TypeTarget::Promise(Box::new(first)),
            "Option" | "Result" => first,
            "Iterator" => TypeTarget::Collection { base: constructor.to_owned(), item: Box::new(first) },
            "QuerySet" => {
                if let Some(TypeTarget::ConfiguredReceiver {
                    authority,
                    origin,
                    evidence,
                    ..
                }) = self.project_named_return(constructor, ctx)
                {
                    let fallback = (!matches!(first, TypeTarget::Unknown)).then(|| {
                        Box::new(TypeTarget::Collection {
                            base: constructor.to_owned(),
                            item: Box::new(first),
                        })
                    });
                    TypeTarget::ConfiguredReceiver {
                        authority,
                        origin,
                        fallback,
                        evidence,
                    }
                } else {
                    TypeTarget::Collection {
                        base: constructor.to_owned(),
                        item: Box::new(first),
                    }
                }
            }
            "NodeList" | "HTMLCollection" | "Array" | "List" | "list" => {
                TypeTarget::Collection {
                    base: constructor.to_owned(),
                    item: Box::new(first),
                }
            }
            "Set" | "ReadonlyArray" | "Sequence" | "Iterable" if linker.is_none_or(|linker| linker.generic_sequence_return()) => {
                TypeTarget::Collection {
                    base: constructor.to_owned(),
                    item: Box::new(first),
                }
            }
            _ => self
                .project_named_return(constructor, ctx)
                .unwrap_or(TypeTarget::Unknown),
        }
    }

    fn project_named_return(
        &self,
        name: &str,
        ctx: &ReturnProjection<'_, 'a>,
    ) -> Option<TypeTarget<'a>> {
        if ctx.is_dom_pack {
            return Some(TypeTarget::Builtin(name.to_owned()));
        }
        if let Some((pack, rule_id, nominal)) =
            self.receiver_rule_with_meta(&ctx.owner.path, name)
        {
            if pack == "web" {
                return Some(TypeTarget::Builtin(name.to_owned()));
            }
            return Some(TypeTarget::ConfiguredReceiver {
                authority: std::sync::Arc::new(
                    super::super::value_flow::ConfiguredReceiverAuthority::Nominal {
                        pack: pack.to_owned(),
                        rule_id: rule_id.to_owned(),
                        receiver: std::sync::Arc::new(
                            super::super::value_flow::ConfiguredNominalReceiver {
                                rule: nominal.clone(),
                            },
                        ),
                    },
                ),
                origin: Box::new(ctx.origin.clone()),
                fallback: None,
                evidence: format!("{}:{}", self.dependencies.digest(), ctx.owner.path),
            });
        }
        if let Some(result) = languages::by_id(ctx.owner.language.as_str()).and_then(|profile| {
            profile.linker().special_named_return(name, ctx.import_line, &|| {
                ctx.pack.is_some_and(|pack| {
                    self.path_id(&ctx.owner.path)
                        .zip(self.symbol_id(pack))
                        .zip(self.symbol_id(name))
                        .and_then(|((path_id, pack_id), base_id)| self.hierarchy_results.get(&path_id)?.get(&(pack_id, base_id)))
                        .is_some()
                        || self.dependencies.class_hierarchy_authority_for_path(&ctx.owner.path, name, Some(pack)).is_some()
                })
            })
        }) {
            return result;
        }
        if let Some(pack) = ctx.pack {
            let authority = self
                .path_id(&ctx.owner.path)
                .zip(self.symbol_id(pack))
                .zip(self.symbol_id(name))
                .and_then(|((path_id, pack_id), base_id)| {
                    self.hierarchy_results
                        .get(&path_id)?
                        .get(&(pack_id, base_id))
                        .cloned()
                })
                .or_else(|| {
                    self.dependencies
                        .class_hierarchy_authority_for_path(&ctx.owner.path, name, Some(pack))
                        .map(|(resolved_pack, rule)| {
                            std::sync::Arc::new(
                                super::super::value_flow::ConfiguredReceiverAuthority::Hierarchy {
                                    pack: resolved_pack.to_owned(),
                                    rule: std::sync::Arc::new(rule.clone()),
                                },
                            )
                        })
                })?;
            return Some(TypeTarget::ConfiguredReceiver {
                authority,
                origin: Box::new(ctx.origin.clone()),
                fallback: None,
                evidence: format!(
                    "{}:{}:{}:{}",
                    self.dependencies.digest(),
                    ctx.owner.path,
                    pack,
                    match ctx.receiver {
                        Some(TypeTarget::Local(class)) => class.id.as_str(),
                        Some(TypeTarget::ConfiguredReceiver { evidence, .. }) => evidence.as_str(),
                        _ => "",
                    }
                ),
            });
        }
        if languages::by_id(ctx.owner.language.as_str())
            .is_some_and(|profile| profile.builtin_type(name))
        {
            return Some(TypeTarget::Builtin(name.to_owned()));
        }
        None
    }

    fn resolve_receiver_access_symbol(
        &self,
        owner: &'a Node,
        head: &str,
        member: &str,
    ) -> Option<Symbol<'a>> {
        let receiver_access = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().receiver_access(owner, head));
        let (associated, include_interface) = receiver_access.filter(|_| !name_rebound(owner, head))?;
        let mut scope = owner.qualname.as_str();
        while let Some((parent, _)) = scope.rsplit_once('.') {
            if let Some(class) = self
                .nodes_in_path(parent, &owner.path)
                .find(|n| n.kind == crate::models::tags::NodeKind::Class || (include_interface && n.kind == crate::models::tags::NodeKind::Interface))
            {
                return Some(if member.is_empty() {
                    Symbol::Type(TypeTarget::Local(class))
                } else {
                    self.member(class, member, associated, owner)
                });
            }
            scope = parent;
        }
        None
    }

    fn resolve_imported_node_targets(
        &self,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
        role: SymbolRole,
        lookup_key: &mut String,
    ) -> Symbol<'a> {
        if member.is_empty() {
            return self.classify(targets.iter().copied(), role);
        }
        if let Some(result) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().imported_member_path(self, owner, targets, member, role))
        {
            return result;
        }
        if matches!(role, SymbolRole::Callable) {
            if let [target] = targets {
                if matches!(
                    target.kind.as_str(),
                    "class" | "struct" | "enum" | "interface"
                ) {
                    return self.member(target, member, true, owner);
                }
            }
        }
        let mut nodes = Vec::new();
        for target in targets {
            if target.kind == crate::models::tags::NodeKind::Module
                && languages::by_id(target.language.as_str()).is_some_and(|profile| profile.linker().uses_module_exports())
            {
                let (export, rest) = member.split_once('.').unwrap_or((member, ""));
                let Some(exported) = self
                    .exports
                    .get(target.language.as_str())
                    .and_then(|exports| {
                        self.interner
                            .lookup_path(target.path.as_str())
                            .and_then(|path| exports.get(&path))
                    })
                    .and_then(|exports| {
                        self.interner
                            .lookup_symbol(export)
                            .and_then(|name| exports.get(&name))
                    })
                else {
                    continue;
                };
                let Some(exported) = exported else {
                    return Symbol::Ambiguous;
                };
                if rest.is_empty() {
                    nodes.push(*exported);
                } else if targets.len() == 1
                    && matches!(role, SymbolRole::Callable)
                    && matches!(exported.kind.as_str(), "class" | "interface")
                {
                    return self.member(exported, rest, true, owner);
                }
                continue;
            }
            nodes.extend(
                self.by_qual
                    .get(super::super::qualified(lookup_key, &target.qualname, member))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|node| node.path == target.path),
            );
        }
        self.classify(nodes.into_iter(), role)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_imported_symbol_in_scope(
        &self,
        owner: &'a Node,
        scope: &str,
        name: &str,
        head: &str,
        member: &str,
        at: SourcePosition,
        role: SymbolRole,
        import: &Imported<'a>,
        lookup_key: &mut String,
    ) -> Symbol<'a> {
        let imported_at = match import {
            Imported::Nodes(_, position, _) | Imported::External(_, position, _, _) => {
                Some(*position)
            }
            Imported::Unknown => None,
        };
        if let Some(imported_at) = imported_at {
            let local = self
                .by_qual
                .get(super::super::qualified(lookup_key, scope, name))
                .into_iter()
                .flatten()
                .copied()
                .filter(|candidate| {
                    candidate.path == owner.path
                        && SourcePosition {
                            line: candidate.line,
                            column: candidate.detail_entry(crate::models::details::NodeDetailKey::COLUMN).as_u64().unwrap_or(0) as usize,
                        } > imported_at
                        && Self::visible(owner, candidate, at, role)
                });
            if local.clone().next().is_some() {
                return self.classify(local, role);
            }
        }
        let type_only = match import {
            Imported::Nodes(_, _, type_only) => *type_only,
            Imported::External(_, _, reference, _) => matches!(
                reference.receiver_hint.as_ref(),
                Some(ReceiverHint::TypeOnlyImport)
            ),
            Imported::Unknown => false,
        };
        if type_only && matches!(role, SymbolRole::Callable | SymbolRole::Constructor) {
            return Symbol::Unknown;
        }
        match import {
            Imported::Nodes(targets, position, _)
                if *position <= at
                    || languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().import_visible_before_position(owner)) =>
            {
                self.resolve_imported_node_targets(owner, targets, member, role, lookup_key)
            }
            Imported::External(module, position, reference, _) if *position <= at => {
                if let Some(symbol) = languages::by_id(owner.language.as_str()).and_then(|profile|
                    profile.linker().external_import_type(owner, module, reference, (head, member), role, position.line)) {
                    return symbol;
                }
                let matched = self
                    .path_id(&owner.path)
                    .and_then(|path| self.receiver_types.get(&path))
                    .into_iter()
                    .flat_map(|receivers| receivers.iter())
                    .find(|receiver| {
                        matches!(role, SymbolRole::Type)
                            && receiver.module.as_deref() == Some(module)
                            && receiver.export.as_deref().is_some_and(|export| {
                                (reference.expression == export && member.is_empty())
                                    || (reference.expression == *module && member == export)
                            })
                    });
                Symbol::Type(TypeTarget::External {
                    module: matched.map_or_else(|| module.clone(), |r| r.name.clone()),
                    import_line: position.line,
                })
            }
            _ => Symbol::Unknown,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_symbol_in_node(
        &self,
        owner: &'a Node,
        node: &'a Node,
        scope: &str,
        name: &str,
        head: &str,
        member: &str,
        at: SourcePosition,
        role: SymbolRole,
        lookup_key: &mut String,
    ) -> Option<Symbol<'a>> {
        if self.import_states.values().any(|state| {
            state.tracks_binding(node, head) && !state.admits_binding(node, head, owner, at)
        }) {
            return Some(Symbol::Unknown);
        }
        if (node.rebinding_names().any(|n| n == head) || self.node_has_rebinding(node, head))
            && !self.admits_commonjs_binding(node, head, owner, at)
        {
            return Some(Symbol::Unknown);
        }
        let import = self
            .symbol_id(&node.id)
            .and_then(|node_id| self.imports.get(&node_id))
            .and_then(|aliases| self.symbol_id(head).and_then(|head| aliases.get(&head)))?;
        Some(self.resolve_imported_symbol_in_scope(
            owner,
            scope,
            name,
            head,
            member,
            at,
            role,
            import,
            lookup_key,
        ))
    }

    pub(in crate::linker) fn resolve_symbol_raw(
        &self,
        owner: &'a Node,
        raw_name: &str,
        at: SourcePosition,
        role: SymbolRole,
    ) -> Symbol<'a> {
        if let Some(symbol) = self.named_member(owner, raw_name, at) {
            return symbol;
        }
        let Some(name) = crate::linker::types::receiver_type_name(raw_name) else {
            return Symbol::Unknown;
        };
        let lookup = LanguageMemberLookup::new(self);
        if let Some(symbol) = languages::by_id(owner.language.as_str()).and_then(|profile| {
            profile.linker().receiver_symbol(&lookup, owner, &name, &mut |receiver| {
                self.resolve_symbol(owner, receiver, at, SymbolRole::Type)
            })
        }) {
            return symbol;
        }
        let (head, member) = name.split_once('.').unwrap_or((&name, ""));
        if let Some(symbol) = self.resolve_receiver_access_symbol(owner, head, member) {
            return symbol;
        }
        let mut scope = owner.qualname.as_str();
        let mut lookup_key = String::with_capacity(128);
        loop {
            let scope_nodes = self.by_qual.get(scope);
            if let Some(nodes) = scope_nodes {
                for node in nodes.iter().copied().filter(|node| node.path == owner.path) {
                    if let Some(symbol) = self.resolve_symbol_in_node(
                        owner,
                        node,
                        scope,
                        &name,
                        head,
                        member,
                        at,
                        role,
                        &mut lookup_key,
                    ) {
                        return symbol;
                    }
                }
            }
            if !member.is_empty() && matches!(role, SymbolRole::Callable) {
                if let Symbol::Type(TypeTarget::Local(receiver)) = self.classify(
                    self.by_qual
                        .get(super::super::qualified(&mut lookup_key, scope, head))
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|node| {
                            node.path == owner.path && Self::visible(owner, node, at, role)
                        }),
                    SymbolRole::Type,
                ) {
                    return self.member(receiver, member, true, owner);
                }
            }
            let local = self
                .by_qual
                .get(super::super::qualified(&mut lookup_key, scope, &name))
                .into_iter()
                .flatten()
                .copied()
                .filter(|node| node.path == owner.path && Self::visible(owner, node, at, role));
            if local.clone().next().is_some() {
                return self.classify(local, role);
            }
            if !member.is_empty() {
                let owner_ws = languages::workspace_path(&owner.path).0;
                if let Some(submodules) = self
                    .by_qual
                    .get(super::super::qualified(&mut lookup_key, scope, head))
                {
                    let eof = SourcePosition {
                        line: usize::MAX,
                        column: usize::MAX,
                    };
                    let mut nested_key = String::with_capacity(64);
                    for module in submodules.iter().copied().filter(|node| {
                        node.kind == crate::models::tags::NodeKind::Module
                            && node.language == owner.language
                            && languages::workspace_path(&node.path).0 == owner_ws
                    }) {
                        let (sub_head, sub_tail) = member.split_once('.').unwrap_or((member, ""));
                        if let Some(symbol) = self.resolve_symbol_in_node(
                            module,
                            module,
                            module.qualname.as_str(),
                            member,
                            sub_head,
                            sub_tail,
                            eof,
                            role,
                            &mut nested_key,
                        ) {
                            if !matches!(symbol, Symbol::Unknown) {
                                return symbol;
                            }
                        }
                    }
                }
            }
            if scope_nodes_shadow_value(owner, raw_name, head, role, scope_nodes) {
                return Symbol::Unknown;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        if let Some(symbol) = languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().external_symbol(self, owner, &name, role))
        {
            return symbol;
        }
        let builtin = languages::by_id(owner.language.as_str()).is_some_and(|profile| {
            profile.builtin_type(&name)
                || profile.linker().builtin_type_symbol(owner, profile, &name, role)
        });
        if builtin
            || self
                .receiver_type(&owner.path, &name)
                .is_some_and(|receiver| receiver.module.is_none())
        {
            Symbol::Type(TypeTarget::Builtin(name.into_owned()))
        } else {
            Symbol::Unknown
        }
    }

}
