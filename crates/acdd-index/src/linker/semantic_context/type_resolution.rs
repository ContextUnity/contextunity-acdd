use super::context::*;

impl<'a> Context<'a> {
    pub(in crate::linker) fn rust_alias_member(
        &self,
        owner: &'a Node,
        reference: &Reference,
    ) -> Option<Symbol<'a>> {
        let (base, member) = reference.expression.rsplit_once("::")?;
        let TypeTarget::Local(target) =
            self.rust_alias_target(owner, base, reference.position())?
        else {
            return None;
        };
        Some(self.member(target, member, true, owner))
    }
}

fn scope_nodes_shadow_value<'a>(
    owner: &'a Node,
    raw_name: &str,
    head: &str,
    role: SymbolRole,
    scope_nodes: Option<&Vec<&'a Node>>,
) -> bool {
    let value_shadows_role = !(owner.language == "rust"
        && (matches!(role, SymbolRole::Type)
            || raw_name
                .split_once('.')
                .map_or(raw_name, |(before_dot, _)| before_dot)
                .contains("::")));
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
        #[cfg(feature = "lang-rust")]
        let extension = (owner.language == "rust")
            .then(|| self.rayon_extension_base(receiver, member, owner))
            .flatten();
        #[cfg(not(feature = "lang-rust"))]
        let extension: Option<&str> = None;
        let extension_receiver = extension.map(|base| TypeTarget::External {
            module: base.to_owned(),
            import_line: 0,
        });
        let receiver = extension_receiver.as_ref().unwrap_or(receiver);
        match receiver {
            TypeTarget::External {
                module,
                import_line,
            } if owner.language == "rust" => {
                let base = module.replace('.', "::");
                let (pack, rule) = self.dependencies.class_hierarchy_authority_for_path(
                    &owner.path,
                    &base,
                    extension.map(|_| "rust-cargo"),
                )?;
                if !rule.members.iter().any(|allowed| allowed == member) {
                    return None;
                }
                self.project_hierarchy_policy(
                    owner,
                    receiver,
                    pack,
                    rule.returns.get(member)?,
                    *import_line,
                )
            }
            TypeTarget::Builtin(name) => {
                let lookup_name =
                    if matches!(owner.language.as_str(), "typescript" | "javascript" | "vue") {
                        let unprefixed = name.strip_prefix("Web.").unwrap_or(name);
                        super::super::typescript::semantic_context::typed_dom_receiver(unprefixed)
                            .unwrap_or(unprefixed)
                    } else {
                        name.as_str()
                    };
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
            TypeTarget::RustReference { target, .. } => {
                self.resolve_member_result(target, member, owner, _at)
            }
            TypeTarget::RustDeref { wrapper, target } => {
                if super::super::rust::semantic_context::wrapper_value_member(wrapper, member) {
                    None
                } else {
                    self.resolve_member_result(target, member, owner, _at)
                }
            }
            TypeTarget::Local(class) if class.language == "python" => {
                let (pack, _row, policy) = self.member_state("python")?.return_policy(class, member)?;
                let crate::languages::profile::LanguageMemberResult::External { module, import_line: line, .. } =
                    self.lookup_language_member("python", class, member, false, owner)
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
        if let Some(global) = crate::languages::by_id(&owner.language)
            .and_then(|profile| profile.linker().identity_call_global(callee))
        {
            return Some(if self.unshadowed_global(owner, global, at) {
                fallback.unwrap_or(TypeTarget::Unknown)
            } else {
                TypeTarget::Unknown
            });
        }
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
        if owner.kind == "module"
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
        if let Some(result) = super::super::javascript::semantic_context::external_factory_result(
            self, owner, callee, at,
        ) {
            return Some(result);
        }
        super::super::python::semantic_context::external_factory_result(self, owner, callee, at)
    }
    fn resolve_factory_result(
        &self,
        owner: &'a Node,
        callee: &str,
        at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        if !matches!(owner.language.as_str(), "javascript" | "typescript" | "vue")
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
        if receiver.language == "rust" {
            return super::super::rust::semantic_context::deref_target(
                &LanguageMemberLookup::new(self),
                receiver,
            );
        }
        let _ = receiver;
        None
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
        if owner.language == "python"
            && matches!(clean_base, "tuple" | "Tuple")
            && !args.is_empty()
            && !args.iter().any(
                |arg| matches!(arg, crate::semantic::TypeExpr::Named { name } if name == "..."),
            )
        {
            return TypeTarget::Tuple(
                args.iter()
                    .map(|arg| {
                        super::super::value_flow::resolve_type(
                            owner,
                            arg,
                            at,
                            self,
                            super::super::value_flow::SemanticLimits::default(),
                        )
                    })
                    .collect(),
            );
        }
        let nominal = self.resolve_symbol(owner, base, at, SymbolRole::Type);
        if let Some(target) = super::super::python::semantic_context::resolve_applied(
            self, owner, base, args, &nominal, at,
        ) {
            return target;
        }
        if let Some(target) =
            super::super::rust::semantic_context::nominal_application(owner, &nominal)
        {
            return target;
        }
        if let Some(target) =
            super::super::typescript::semantic_context::resolve_applied(owner, args, &nominal)
        {
            return target;
        }
        if let Some(target) =
            super::super::rust::value_flow::resolve_applied(owner, base, args, &nominal, at, self)
        {
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
                if languages::by_id(&owner.language)
                    .is_some_and(|profile| profile.builtin_generic(&name)) =>
            {
                TypeTarget::Builtin(name)
            }
            Symbol::Type(TypeTarget::External { module, .. })
                if super::super::python::semantic_context::is_external_generic_base(
                    &module,
                    clean_base,
                    languages::by_id(&owner.language),
                ) =>
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
        if let TypeTarget::RustWrapper { ok, .. } = &wrapper {
            return (**ok).clone();
        }
        let admitted =
            super::super::rust::semantic_context::admits_payload(owner, &wrapper, args.len())
                || super::super::typescript::semantic_context::admits_payload(
                    owner,
                    &wrapper,
                    args.len(),
                );
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
        _at: SourcePosition,
    ) -> Symbol<'a> {
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
                if let Some(target) = languages::by_id(&owner.language)
                    .and_then(|profile| profile.linker().synthetic_property_result(owner, receiver, member))
                {
                    return Symbol::Type(target);
                }
                fallback.as_deref().map_or(Symbol::Unknown, |fallback| {
                    self.resolve_member(fallback, member, owner, _at)
                })
            }
            TypeTarget::Builtin(receiver) => self
                .property_type_for_path(&owner.path, receiver, member)
                .map_or(Symbol::Unknown, |name| {
                    Symbol::Type(TypeTarget::Builtin(name))
                }),
            TypeTarget::Local(node) => self.member(node, member, false, owner),
            TypeTarget::RustReference { target, .. } => {
                self.resolve_member(target, member, owner, _at)
            }
            TypeTarget::RustDeref {
                wrapper, target, ..
            } => {
                if super::super::rust::semantic_context::wrapper_value_member(wrapper, member) {
                    Symbol::Unknown
                } else {
                    self.resolve_member(target, member, owner, _at)
                }
            }
            TypeTarget::Object(members) => match members.get(member) {
                Some(node) => Symbol::Callable(node),
                None => Symbol::Unknown,
            },
            TypeTarget::ClassType(inner) if owner.language == "python" => {
                self.resolve_member(inner, member, owner, _at)
            }
            TypeTarget::External { .. } if owner.language == "rust" => self
                .resolve_member_result(receiver, member, owner, _at)
                .map_or(Symbol::Unknown, Symbol::Type),
            _ => Symbol::Unknown,
        }
    }
    fn resolve_python_super_member(&self, owner: &'a Node, member: &str) -> Symbol<'a> {
        self.python_super_member(owner, member)
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
        if owner.language == "rust" {
            if let Some((base, member)) = name.rsplit_once("::").or_else(|| name.rsplit_once('.')) {
                if let Some(TypeTarget::Local(node)) = self.rust_alias_target(owner, base, at).or_else(
                    || match self.resolve_symbol_raw(owner, base, at, SymbolRole::Type) {
                        Symbol::Type(target) => Some(target),
                        _ => None,
                    },
                ) {
                    if matches!(node.kind.as_str(), "struct" | "enum" | "trait" | "type") {
                        let resolved = self.member(node, member, true, owner);
                        if !matches!(resolved, Symbol::Unknown) {
                            return resolved;
                        }
                    }
                }
            }
            if matches!(role, SymbolRole::Type | SymbolRole::Constructor) {
                if let Some(target) = self.rust_alias_target(owner, name, at) {
                    return Symbol::Type(target);
                }
            }
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
                        || super::super::vue::semantic_context::template_import_visible(owner)
                    {
                        if let [target] = targets.as_slice() {
                            if target.kind == "module" {
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

    fn project_hierarchy_policy(
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
                if ctx.owner.language == "rust" {
                    TypeTarget::RustReference {
                        mutable: *mutable,
                        target: Box::new(projected),
                    }
                } else {
                    projected
                }
            }
            ReturnTypeNode::Tuple(items) => {
                let projected: Vec<_> = items
                    .iter()
                    .map(|item| self.project_return_node(item, ctx, depth + 1))
                    .collect();
                if ctx.owner.language == "rust" {
                    TypeTarget::RustTuple(projected)
                } else {
                    TypeTarget::Tuple(projected)
                }
            }
            ReturnTypeNode::Applied { base, args } => {
                self.project_applied_return(base, args, ctx, depth)
            }
            ReturnTypeNode::Named(name) => {
                if name == "Item" {
                    return match ctx.receiver {
                        Some(
                            TypeTarget::Collection { item, .. }
                            | TypeTarget::RustIterator { item }
                            | TypeTarget::RustCollection { item }
                            | TypeTarget::RustSlice { item }
                            | TypeTarget::RustTypedContainer { item, .. },
                        ) => (**item).clone(),
                        Some(TypeTarget::ConfiguredReceiver {
                            fallback: Some(fallback),
                            ..
                        }) => match fallback.as_ref() {
                            TypeTarget::Collection { item, .. } => (**item).clone(),
                            _ => TypeTarget::Unknown,
                        },
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
        match short {
            "Promise" => TypeTarget::Promise(Box::new(first)),
            "Option" => {
                if ctx.owner.language == "rust" {
                    TypeTarget::RustWrapper {
                        wrapper: super::super::value_flow::RustWrapperKind::Option,
                        ok: Box::new(first),
                        err: None,
                    }
                } else {
                    first
                }
            }
            "Result" => {
                if ctx.owner.language == "rust" {
                    let err = args
                        .get(1)
                        .map(|arg| Box::new(self.project_return_node(arg, ctx, depth + 1)));
                    TypeTarget::RustWrapper {
                        wrapper: super::super::value_flow::RustWrapperKind::Result,
                        ok: Box::new(first),
                        err,
                    }
                } else {
                    first
                }
            }
            "Iterator" => {
                if ctx.owner.language == "rust" {
                    TypeTarget::RustIterator {
                        item: Box::new(first),
                    }
                } else {
                    TypeTarget::Collection {
                        base: constructor.to_owned(),
                        item: Box::new(first),
                    }
                }
            }
            "Vec" if ctx.owner.language == "rust" => TypeTarget::RustCollection {
                item: Box::new(first),
            },
            "Box" | "Arc" | "Rc" | "Cow" if ctx.owner.language == "rust" => TypeTarget::RustDeref {
                wrapper: match short {
                    "Box" => "Box",
                    "Arc" => "Arc",
                    "Rc" => "Rc",
                    _ => "Cow",
                },
                target: Box::new(first),
            },
            "VecDeque" | "HashSet" | "BTreeSet" if ctx.owner.language == "rust" => {
                TypeTarget::RustTypedContainer {
                    base: match short {
                        "VecDeque" => "VecDeque",
                        "HashSet" => "HashSet",
                        _ => "BTreeSet",
                    },
                    item: Box::new(first),
                }
            }
            "Slice" if ctx.owner.language == "rust" => TypeTarget::RustSlice {
                item: Box::new(first),
            },
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
            "Set" | "ReadonlyArray" | "Sequence" | "Iterable" if ctx.owner.language != "rust" => {
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
        if ctx.owner.language == "rust" {
            if !name.contains([':', '.'])
                && languages::by_id("rust").is_some_and(|profile| profile.builtin_type(name))
            {
                return Some(TypeTarget::Builtin(name.to_owned()));
            }
            if !matches!(name.split("::").next(), Some("std" | "core" | "alloc"))
                && !name.starts_with("serde::ser::")
            {
                let pack = ctx.pack?;
                let in_hierarchy = self
                    .path_id(&ctx.owner.path)
                    .zip(self.symbol_id(pack))
                    .zip(self.symbol_id(name))
                    .and_then(|((path_id, pack_id), base_id)| {
                        self.hierarchy_results
                            .get(&path_id)?
                            .get(&(pack_id, base_id))
                    })
                    .is_some()
                    || self
                        .dependencies
                        .class_hierarchy_authority_for_path(&ctx.owner.path, name, Some(pack))
                        .is_some();
                if !in_hierarchy {
                    return None;
                }
            }
            return Some(TypeTarget::External {
                module: name.to_owned(),
                import_line: ctx.import_line,
            });
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
        if languages::by_id(&ctx.owner.language)
            .is_some_and(|profile| profile.builtin_type(name))
        {
            return Some(TypeTarget::Builtin(name.to_owned()));
        }
        None
    }

    fn rust_alias_target(
        &self,
        owner: &'a Node,
        name: &str,
        at: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        let mut symbol = self.resolve_symbol_raw(owner, name, at, SymbolRole::Type);
        let mut visited = Vec::new();
        for _ in 0..16 {
            match symbol {
                Symbol::Type(TypeTarget::Local(alias))
                    if alias.language == "rust" && alias.kind == "type" =>
                {
                    if alias.details["deref_unproven"] == true {
                        return None;
                    }
                    if visited.contains(&alias.id.as_str()) {
                        return Some(TypeTarget::Unknown);
                    }
                    let Some(target) = alias.details["alias_target"].as_str() else {
                        return Some(TypeTarget::Unknown);
                    };
                    visited.push(alias.id.as_str());
                    symbol = self.resolve_symbol_raw(
                        alias,
                        target,
                        super::super::value_flow::node_position(alias),
                        SymbolRole::Type,
                    );
                }
                Symbol::Type(target) if !visited.is_empty() => return Some(target),
                _ => return (!visited.is_empty()).then_some(TypeTarget::Unknown),
            }
        }
        Some(TypeTarget::Unknown)
    }
    fn resolve_receiver_access_symbol(
        &self,
        owner: &'a Node,
        head: &str,
        member: &str,
    ) -> Option<Symbol<'a>> {
        let receiver_access = super::super::python::semantic_context::receiver_access(owner, head)
            .or_else(|| super::super::javascript::semantic_context::receiver_access(owner, head));
        let (associated, include_interface) = receiver_access.filter(|_| !name_rebound(owner, head))?;
        let mut scope = owner.qualname.as_str();
        while let Some((parent, _)) = scope.rsplit_once('.') {
            if let Some(class) = self
                .nodes_in_path(parent, &owner.path)
                .find(|n| n.kind == "class" || (include_interface && n.kind == "interface"))
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
        #[cfg(feature = "lang-rust")]
        if owner.language == "rust" {
            return self.resolve_rust_member_path(owner, targets, member, role);
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
            if target.kind == "module"
                && matches!(
                    target.language.as_str(),
                    "typescript" | "javascript" | "vue"
                )
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
                            column: candidate.details["column"].as_u64().unwrap_or(0) as usize,
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
                    || super::super::vue::semantic_context::template_import_visible(owner) =>
            {
                self.resolve_imported_node_targets(owner, targets, member, role, lookup_key)
            }
            Imported::External(module, position, reference, _) if *position <= at => {
                if matches!(module.as_str(), "typing" | "typing_extensions")
                    && (member == "Any" || head == "Any")
                {
                    return Symbol::Unknown;
                }
                if let Some(symbol) = super::super::python::semantic_context::external_import_type(
                    owner,
                    module,
                    reference,
                    head,
                    member,
                    role,
                    position.line,
                ) {
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

    fn resolve_rust_external_or_crate_symbol(
        &self,
        owner: &'a Node,
        name: &str,
        role: SymbolRole,
    ) -> Option<Symbol<'a>> {
        if owner.language != "rust" {
            return None;
        }
        #[cfg(feature = "lang-rust")]
        if let Some((crate_name, tail)) = name.split_once('.') {
            if let Some(root_module) = self
                .import_state("rust")
                .and_then(|state| state.workspace_crate_root(self.dependencies, &owner.path, crate_name))
            {
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
        if self.import_state("javascript").is_some_and(|state| {
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

    fn resolve_symbol_raw(
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
        if owner.language == "rust" {
            let lookup = LanguageMemberLookup::new(self);
            if let Some(symbol) = super::super::rust::semantic_context::receiver_symbol(
                Some(&lookup),
                owner,
                &name,
                |receiver| self.resolve_symbol(owner, receiver, at, SymbolRole::Type),
            ) {
                return symbol;
            }
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
            if scope_nodes_shadow_value(owner, raw_name, head, role, scope_nodes) {
                return Symbol::Unknown;
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            scope = parent;
        }
        if let Some(symbol) = self.resolve_rust_external_or_crate_symbol(owner, &name, role) {
            return symbol;
        }
        let builtin = languages::by_id(&owner.language).is_some_and(|profile| {
            profile.builtin_type(&name)
                || super::super::typescript::semantic_context::is_builtin_promise_type(
                    owner, profile, &name, role,
                )
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

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn resolve_rust_imported_call(
        &self,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
    ) -> Vec<&'a Node> {
        match self.resolve_rust_member_path(owner, targets, member, SymbolRole::Callable) {
            Symbol::Callable(node) => vec![node],
            _ => Vec::new(),
        }
    }

    #[cfg(feature = "lang-rust")]
    pub(in crate::linker) fn resolve_rust_constant_member(
        &self,
        constant: &'a Node,
        member: &str,
        owner: &'a Node,
    ) -> Option<&'a Node> {
        let alias_ty = self
            .value_flow(constant)
            .and_then(|facts| facts.alias_type.as_ref())?;
        let target_ty = super::super::value_flow::resolve_type(
            constant,
            alias_ty,
            super::super::value_flow::node_position(constant),
            self,
            super::super::value_flow::SemanticLimits::default(),
        );
        match self.resolve_member(
            &target_ty,
            member,
            owner,
            super::super::value_flow::node_position(owner),
        ) {
            Symbol::Callable(node) => Some(node),
            _ => None,
        }
    }

    #[cfg(feature = "lang-rust")]
    fn step_rust_module_member(
        &self,
        owner: &'a Node,
        target: &'a Node,
        segment: &str,
        next: &mut Vec<&'a Node>,
    ) {
        if let Some(Imported::Nodes(imported, _, _)) = self.imported_alias(target, segment) {
            if imported.iter().all(|node| node.kind == "module") {
                next.extend(imported.iter().copied().filter(|node| {
                    self.import_state("rust")
                        .is_none_or(|state| state.module_path_visible(&node.qualname, &owner.qualname))
                }));
                return;
            }
        }
        let visible = self
            .import_state("rust")
            .map_or(&[][..], |state| state.targets(target, segment, &owner.qualname));
        if visible.is_empty() {
            let sub_qual = format!("{}.{}", target.qualname, segment);
            let target_pkg = target.path.split_once("/src/").map_or("", |(p, _)| p);
            if let Some(candidates) = self.by_qual.get(sub_qual.as_str()) {
                next.extend(candidates.iter().copied().filter(|node| {
                    node.language == "rust"
                        && node.kind == "module"
                        && node.path.split_once("/src/").map_or("", |(p, _)| p) == target_pkg
                        && self.import_state("rust").is_none_or(|state| {
                            state.module_path_visible(&node.qualname, &owner.qualname)
                        })
                }));
            }
            return;
        }
        if let Some(Imported::Nodes(imported, _, _)) = self.imported_alias(target, segment) {
            next.extend(imported.iter().copied());
        } else {
            next.extend_from_slice(visible);
        }
    }

    #[cfg(feature = "lang-rust")]
    fn resolve_rust_member_path(
        &self,
        owner: &'a Node,
        targets: &[&'a Node],
        member: &str,
        role: SymbolRole,
    ) -> Symbol<'a> {
        let mut current = targets.to_vec();
        let segment_count = member.split('.').count();
        for (index, segment) in member.split('.').enumerate() {
            if index >= 64 {
                return Symbol::Unknown;
            }
            let mut next = Vec::new();
            for target in &current {
                if target.kind == "module" {
                    self.step_rust_module_member(owner, target, segment, &mut next);
                } else if segment_count == index + 1 && matches!(role, SymbolRole::Callable) {
                    if target.kind == "constant" {
                        if let Some(method) =
                            self.resolve_rust_constant_member(target, segment, owner)
                        {
                            next.push(method);
                        }
                    } else if let Symbol::Callable(method) = self.member(target, segment, true, owner)
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
