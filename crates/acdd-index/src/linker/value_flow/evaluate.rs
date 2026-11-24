use crate::languages::profile::ProfileLinker;
use super::lookup::resolve_type;
use super::types::{
    collect_candidate_join, node_position, same_target, symbol_type, Definitions, ReadTrace,
    SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};
use crate::models::Node;
use crate::semantic::{SourcePosition, ValueExpr, ValueFlowFacts};
use hashbrown::HashMap;
use std::{borrow::Cow, sync::Arc};

impl<'a> ValueFlowIndex<'a> {
    pub fn resolve_value(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
    ) -> TypeTarget<'a> {
        self.evaluate(owner, value, at, resolver, None)
    }

    pub(super) fn evaluate(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
    ) -> TypeTarget<'a> {
        self.evaluate_tracked(owner, value, at, resolver, definitions, None)
    }
    pub(super) fn evaluate_tracked(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
        let eval = crate::languages::profile::CallEvalContext { index: self, owner, at, resolver, trace };
        if let Some(result) = linker.and_then(|linker| linker.evaluate_language_value(
            &eval, value, self.limits,
            &|value| self.evaluate_tracked(owner, value, at, resolver, definitions, trace),
            &|value| self.explicit_payload(owner, value, at, resolver, trace, 0),
            &|targets| join_branch_targets(targets, true, Some(linker)),
        )) {
            return result;
        }
        match value {
            ValueExpr::Annotated { type_expr } => {
                resolve_type(owner, type_expr, at, resolver, self.limits)
            }
            ValueExpr::Construct { callee } => {
                if let Some(target) = crate::languages::by_id(owner.language.as_str())
                    .and_then(|profile| profile.linker().construct_value(owner, callee))
                {
                    return target;
                }
                symbol_type(resolver.resolve_symbol(owner, callee, at, SymbolRole::Constructor))
            }
            ValueExpr::Alias { name } => self
                .lookup_tracked(owner, name, at, trace)
                .cloned()
                .unwrap_or_else(|| {
                    match resolver.resolve_symbol(owner, name, at, SymbolRole::Callable) {
                        Symbol::Callable(node) if node.kind == crate::models::tags::NodeKind::Constant => self
                            .returns
                            .get(node.id.as_str())
                            .cloned()
                            .unwrap_or(TypeTarget::Unknown),
                        Symbol::Callable(node) => TypeTarget::Callable(node),
                        _ => TypeTarget::Unknown,
                    }
                }),
            ValueExpr::LoopElement { iterable, .. } => self.collection_element(owner, iterable, at),
            ValueExpr::Await { value } => {
                self.evaluate_await(owner, value, at, resolver, definitions, trace)
            }
            ValueExpr::Scoped { value, .. } => {
                self.evaluate_tracked(owner, value, at, resolver, definitions, trace)
            }
            ValueExpr::BranchJoin { alternatives } => join_branch_targets(
                alternatives
                    .iter()
                    .map(|alternative| {
                        self.evaluate_tracked(owner, alternative, at, resolver, definitions, trace)
                    })
                    .collect(),
                false,
                crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker()),
            ),
            ValueExpr::Call { callee, site } => self.evaluate_call(
                owner,
                callee,
                site.as_ref(),
                at,
                resolver,
                definitions,
                trace,
            ),
            ValueExpr::Field { receiver, member } => {
                self.evaluate_field(owner, receiver, member, at, resolver, trace)
            }
            ValueExpr::Project { value, member } => {
                self.evaluate_project(owner, value, member, at, resolver, definitions, trace)
            }
            ValueExpr::Object {
                members,
                builtin_guard,
            } => self.evaluate_object(owner, members, *builtin_guard, at, resolver, definitions),
            ValueExpr::CallbackParam {
                callee,
                arg_index,
                param_index,
            } => self.evaluate_callback_param(
                owner,
                callee,
                *arg_index,
                *param_index,
                at,
                resolver,
                definitions,
                trace,
            ),
            _ => TypeTarget::Unknown,
        }
    }

    fn evaluate_await(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let evaluated = self.evaluate_tracked(owner, value, at, resolver, definitions, trace);
        if let TypeTarget::Promise(inner) = evaluated {
            return *inner;
        }
        let ValueExpr::Call { callee, .. } = value else {
            if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().payload_uses_async_return()) {
                return self.explicit_payload(owner, value, at, resolver, trace, 0);
            }
            return TypeTarget::Unknown;
        };
        let callable = self.callable_expression_tracked(owner, callee, at, resolver, trace);
        callable.map_or(TypeTarget::Unknown, |callable| {
            self.async_returns
                .get(callable.id.as_str())
                .cloned()
                .unwrap_or(TypeTarget::Unknown)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_call(
        &self,
        owner: &'a Node,
        callee: &str,
        site: Option<&crate::semantic::CallSiteEvidence>,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        if self
            .lookup_tracked(owner, callee, at, trace)
            .is_some_and(|receiver| {
                crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| {
                    profile
                        .linker()
                        .callable_marker_evidence(receiver)
                        .is_some()
                })
            })
        {
            return TypeTarget::Unknown;
        }
        let at = site.map_or(at, |site| site.position);
        if let Some(receiver_expr) = site.and_then(|site| site.receiver.as_deref()) {
            return self.evaluate_member_call(
                owner,
                callee,
                receiver_expr,
                at,
                resolver,
                definitions,
                trace,
            );
        }
        if let Some(target) = resolver.resolve_factory_result(owner, callee, at) {
            return target;
        }
        let linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
        if let Some(target) = linker.and_then(|l| l.constructor_result(owner, callee, at, resolver)) {
            return target;
        }
        if linker.is_some_and(|linker| linker.payload_uses_async_return()) {
            if self
                .callable_expression_tracked(owner, callee, at, resolver, trace)
                .is_some_and(|callable| self.async_returns.contains_key(callable.id.as_str()))
            {
                // Retain the wrapper so aliases can await it without exposing the payload ordinarily.
                return TypeTarget::Builtin("Promise".to_owned());
            }
            if let Some((receiver_name, member)) = callee.rsplit_once('.') {
                if let Some(receiver_target) =
                    self.value_expression(owner, receiver_name, at, resolver, trace)
                {
                    if let Some(target) =
                        resolver.resolve_member_result(receiver_target, member, owner, at)
                    {
                        return target;
                    }
                }
            }
        }
        let eval = crate::languages::profile::CallEvalContext {
            index: self,
            owner,
            at,
            resolver,
            trace,
        };
        if let Some(target) = linker.and_then(|l| l.evaluate_call_fallback(&eval, callee, site)) {
            return target;
        }
        let existing = self
            .type_of_expression_tracked(owner, callee, at, resolver, trace)
            .cloned();
        if existing
            .as_ref()
            .is_some_and(|target| !matches!(target, TypeTarget::Unknown))
        {
            return existing.unwrap();
        }
        if let Some(target) = linker.and_then(|l| l.evaluate_dotted_call(&eval, callee)) {
            return target;
        }
        resolver
            .resolve_call_result(owner, callee, site, at, None)
            .unwrap_or(TypeTarget::Unknown)
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_member_call(
        &self,
        owner: &'a Node,
        callee: &str,
        receiver_expr: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
        let eval = crate::languages::profile::CallEvalContext {
            index: self, owner, at, resolver, trace,
        };
        if let Some(result) = linker.and_then(|linker| linker.special_member_call(&eval, receiver_expr, callee)) {
            return result;
        }
        let receiver =
            self.evaluate_tracked(owner, receiver_expr, at, resolver, definitions, trace);
        if let Symbol::Callable(callable) = resolver.resolve_member(&receiver, callee, owner, at) {
            if let Some(result) = self.return_type_tracked(callable, trace) {
                if linker.is_none_or(|linker| linker.accept_member_return(callee, result)) {
                    return result.clone();
                }
            }
        }
        let member_result = self.resolve_member_call(&receiver, callee, owner, at, resolver);
        if let Some(target) = member_result {
            return target;
        }
        if let Some(target) = linker.and_then(|linker| linker.evaluate_member_call_fallback(&eval, &receiver, callee))
        {
            return target;
        }
        TypeTarget::Unknown
    }

    fn evaluate_field(
        &self,
        owner: &'a Node,
        receiver: &str,
        member: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
        if linker.is_some_and(|linker| linker.lexical_receiver_field(owner, receiver)) {
            let target = symbol_type(resolver.resolve_symbol(owner, "Self", at, SymbolRole::Type));
            return linker
                .and_then(|l| l.local_receiver_class(&target))
                .and_then(|class| self.field_type_tracked(class, member, trace))
                .cloned()
                .unwrap_or(TypeTarget::Unknown);
        }
        let binding = self.lookup_tracked(owner, receiver, at, trace);
        let receiver_type = binding.cloned().unwrap_or_else(|| {
            symbol_type(resolver.resolve_symbol(owner, receiver, at, SymbolRole::Type))
        });
        if binding.is_none()
            && matches!(&receiver_type, TypeTarget::Local(node) if matches!(node.kind.as_str(), "class" | "struct" | "interface" | "enum"))
        {
            return match resolver.resolve_symbol(
                owner,
                &format!("{receiver}.{member}"),
                at,
                SymbolRole::Callable,
            ) {
                Symbol::Callable(node) => TypeTarget::Callable(node),
                _ => TypeTarget::Unknown,
            };
        }
        if let Some(class) = linker.and_then(|l| l.local_receiver_class(&receiver_type)) {
            if let Some(target) = self.field_type_tracked(class, member, trace) {
                return target.clone();
            }
        }
        if let TypeTarget::Local(class) = &receiver_type {
            if let Some(target) = self.field_type_tracked(class, member, trace) {
                return target.clone();
            }
        }
        if let TypeTarget::Builtin(name) = &receiver_type {
            if linker.is_some_and(|linker| linker.builtin_field_projection(owner)) {
                if let Some(prop_type) = resolver.property_type(owner, name, member) {
                    return TypeTarget::Builtin(prop_type);
                }
            }
        }
        match resolver.resolve_member(&receiver_type, member, owner, at) {
            Symbol::Type(target) => target,
            Symbol::Callable(node) => TypeTarget::Callable(node),
            _ => TypeTarget::Unknown,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_project(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        member: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let language_linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
        let source = self.evaluate_tracked(owner, value, at, resolver, definitions, trace);
        if let Some(index_str) = member.strip_prefix("__index:") {
            return self.project_index(owner, &source, index_str);
        }
        if let Some(field) = member.strip_prefix("__field:") {
            return self.project_field(owner, &source, field, at, resolver, trace);
        }
        if let TypeTarget::Tuple(items) = &source {
            if let Ok(index) = member.parse::<usize>() {
                return items.get(index).cloned().unwrap_or(TypeTarget::Unknown);
            }
        }
        if let Some(target) = language_linker
            .and_then(|l| l.local_receiver_class(&source))
            .and_then(|class| self.field_type_tracked(class, member, trace))
        {
            return target.clone();
        }
        if let Some(target) =
            language_linker.and_then(|l| l.synthetic_property_result(owner, &source, member))
        {
            return target;
        }
        self.project_field(owner, &source, member, at, resolver, trace)
    }

    fn project_index(&self, owner: &Node, source: &TypeTarget<'a>, index_str: &str) -> TypeTarget<'a> {
        let base = crate::languages::by_id(owner.language.as_str())
            .map_or(source, |profile| profile.linker().unwrap_reference(source));
        if let Some(target) = crate::languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().special_index_projection(base, index_str))
        {
            return target;
        }
        match base {
            TypeTarget::Tuple(items) => index_str
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .cloned()
                .unwrap_or(TypeTarget::Unknown),
            TypeTarget::Collection { item, .. } => (**item).clone(),
            _ => TypeTarget::Unknown,
        }
    }

    fn project_field(
        &self,
        owner: &'a Node,
        source: &TypeTarget<'a>,
        member: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        if let Some(target) = crate::languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().special_field_projection(source, member, owner, at, resolver))
        {
            return target;
        }
        match source {
            TypeTarget::Tuple(items) => member
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .cloned()
                .unwrap_or(TypeTarget::Unknown),
            TypeTarget::Builtin(name)
                if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().builtin_field_projection(owner)) =>
            {
                resolver
                    .property_type(owner, name, member)
                    .map_or(TypeTarget::Unknown, TypeTarget::Builtin)
            }
            TypeTarget::ConfiguredReceiver { .. } => {
                match resolver.resolve_member(source, member, owner, at) {
                    Symbol::Type(target) => target,
                    _ => TypeTarget::Unknown,
                }
            }
            TypeTarget::External { .. } if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().external_field_projection()) => {
                match resolver.resolve_member(source, member, owner, at) {
                    Symbol::Type(target) => target,
                    _ => TypeTarget::Unknown,
                }
            }
            TypeTarget::ResultObject(result) if resolver.configured_result_member(result, member) => {
                TypeTarget::ExternalCallable {
                    result: result.clone(),
                    owner: owner.id.clone(),
                    position: at,
                    member: member.to_owned(),
                }
            }
            TypeTarget::Object(members) => members.get(member).map_or(TypeTarget::Unknown, |node| {
                if matches!(node.kind.as_str(), "function" | "method") {
                    TypeTarget::Callable(node)
                } else {
                    TypeTarget::Unknown
                }
            }),
            TypeTarget::Local(class) => self
                .field_type_tracked(class, member, trace)
                .cloned()
                .unwrap_or_else(|| match resolver.resolve_member(source, member, owner, at) {
                    Symbol::Callable(node) => TypeTarget::Callable(node),
                    Symbol::Ambiguous => TypeTarget::Ambiguous,
                    _ => TypeTarget::Unknown,
                }),
            TypeTarget::Ambiguous => TypeTarget::Ambiguous,
            _ => TypeTarget::Unknown,
        }
    }

    fn resolve_member_call(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        crate::languages::by_id(owner.language.as_str()).map_or_else(
            || resolver.resolve_member_result(receiver, member, owner, at),
            |profile| profile.linker().member_call_result(receiver, member, owner, at, resolver),
        )
    }

    fn evaluate_object(
        &self,
        owner: &'a Node,
        members: &[crate::semantic::ObjectMember],
        builtin_guard: bool,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
    ) -> TypeTarget<'a> {
        if builtin_guard
            && !matches!(
                resolver.resolve_symbol(owner, "Object", at, SymbolRole::Type),
                Symbol::Type(TypeTarget::Builtin(name)) if name == "Object"
            )
        {
            return TypeTarget::Unknown;
        }
        let Some(definitions) = definitions else {
            return TypeTarget::Unknown;
        };
        let mut resolved = HashMap::new();
        for member in members {
            let Some([node]) = definitions
                .get(&(owner.path.as_str(), member.position))
                .map(Vec::as_slice)
            else {
                return TypeTarget::Unknown;
            };
            if !matches!(node.kind.as_str(), "function" | "method" | "field")
                || resolved.contains_key(&member.name)
            {
                return TypeTarget::Unknown;
            }
            resolved.insert(member.name.clone(), *node);
        }
        TypeTarget::Object(Arc::new(resolved))
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_callback_param(
        &self,
        owner: &'a Node,
        callee_expr: &ValueExpr,
        arg_index: usize,
        param_index: usize,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let ValueExpr::Call { callee, site } = callee_expr else {
            return TypeTarget::Unknown;
        };
        let call_at = site.as_ref().map_or(at, |site| site.position);
        if let Some(receiver_expr) = site.as_ref().and_then(|site| site.receiver.as_deref()) {
            let receiver = self.evaluate_tracked(
                owner,
                receiver_expr,
                call_at,
                resolver,
                definitions,
                trace,
            );
            if let Symbol::Callable(callable) =
                resolver.resolve_member(&receiver, callee, owner, call_at)
            {
                if let Some(target) = self.callback_param_from_signature(
                    callable,
                    arg_index,
                    param_index,
                    resolver,
                ) {
                    return target;
                }
            }
            return self.callback_param_from_receiver(
                owner,
                &receiver,
                callee,
                arg_index,
                param_index,
            );
        }
        let Some(callable) =
            self.callable_expression_tracked(owner, callee, call_at, resolver, trace)
        else {
            return TypeTarget::Unknown;
        };
        self.callback_param_from_signature(callable, arg_index, param_index, resolver)
            .unwrap_or(TypeTarget::Unknown)
    }

    fn callback_param_from_signature(
        &self,
        callable: &'a Node,
        arg_index: usize,
        param_index: usize,
        resolver: &impl SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        let sig = resolver.callable_signature(callable)?;
        let param = sig.params.get(arg_index)?;
        let callback_ty = param.callback_params.get(param_index)?;
        let resolved = resolve_type(
            callable,
            callback_ty,
            node_position(callable),
            resolver,
            self.limits,
        );
        (!matches!(resolved, TypeTarget::Unknown)).then_some(resolved)
    }

    fn callback_param_from_receiver(
        &self,
        owner: &Node,
        receiver: &TypeTarget<'a>,
        method: &str,
        arg_index: usize,
        param_index: usize,
    ) -> TypeTarget<'a> {
        let valid_arg = arg_index == 0
            || (arg_index == 1 && matches!(method, "map_or" | "map_or_else"));
        if param_index != 0 || !valid_arg {
            return TypeTarget::Unknown;
        }
        if let Some(result) = crate::languages::by_id(owner.language.as_str())
            .and_then(|profile| profile.linker().callback_receiver(receiver, method))
        {
            return result;
        }
        match receiver {
            TypeTarget::Promise(inner) if matches!(method, "then" | "finally") => {
                if method == "then" {
                    (**inner).clone()
                } else {
                    TypeTarget::Unknown
                }
            }
            TypeTarget::Collection { item, .. } if collection_callback(method) =>
            {
                (**item).clone()
            }
            _ => TypeTarget::Unknown,
        }
    }

    pub(super) fn explicit_payload(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
        depth: usize,
    ) -> TypeTarget<'a> {
        if depth >= self.limits.alias_depth.min(8) {
            return TypeTarget::Unknown;
        }
        let flow = |node: &'a Node| {
            if self
                .scopes
                .get(node.id.as_str())
                .is_none_or(|scope| scope.invalid)
            {
                return None;
            }
            if let Some(facts) = resolver.value_flow(node) {
                Some(Cow::Borrowed(facts))
            } else if !resolver.has_typed_flows() {
                ValueFlowFacts::deserialize_for_scope(node.detail_entry(crate::models::details::NodeDetailKey::VALUE_FLOW), false)
                    .ok()
                    .map(Cow::Owned)
            } else {
                None
            }
        };
        match value {
            ValueExpr::Annotated { type_expr } => resolver.resolve_payload(owner, type_expr, at),
            ValueExpr::Call { callee, .. } => {
                if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().payload_uses_async_return()) {
                    if let TypeTarget::Promise(inner) =
                        self.evaluate_tracked(owner, value, at, resolver, None, trace)
                    {
                        return *inner;
                    }
                }
                let Some(callable) =
                    self.callable_expression_tracked(owner, callee, at, resolver, trace)
                else {
                    return TypeTarget::Unknown;
                };
                if let Some(trace) = trace {
                    trace.returned(callable.id.as_str());
                }
                if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().payload_uses_async_return()) {
                    return self
                        .async_returns
                        .get(callable.id.as_str())
                        .cloned()
                        .unwrap_or(TypeTarget::Unknown);
                }
                flow(callable)
                    .and_then(|facts| {
                        facts.return_type.as_ref().map(|ty| {
                            resolver.resolve_payload(callable, ty, node_position(callable))
                        })
                    })
                    .unwrap_or(TypeTarget::Unknown)
            }
            ValueExpr::Alias { name } => {
                // Reaching-value uncertainty still fences projection; a write cannot revive an old annotation.
                let reaching_wrapper = self.lookup_tracked(owner, name, at, trace).is_some_and(|target| {
                    crate::languages::by_id(owner.language.as_str()).map_or(
                        matches!(target, TypeTarget::Promise(_)),
                        |profile| profile.linker().reaching_payload_wrapper(target),
                    )
                });
                if !reaching_wrapper {
                    return TypeTarget::Unknown;
                }
                let Some(scope) = self.scopes.get(owner.id.as_str()) else {
                    return TypeTarget::Unknown;
                };
                for id in std::iter::once(owner.id.as_str()).chain(scope.parents.iter().copied()) {
                    let Some(TypeTarget::Local(declaration)) = self.local_types.get(id) else {
                        continue;
                    };
                    let Some(facts) = flow(declaration) else {
                        return TypeTarget::Unknown;
                    };
                    if let Some(binding) = facts
                        .bindings
                        .iter()
                        .filter(|binding| binding.name == *name && binding.position < at)
                        .max_by_key(|binding| binding.position)
                    {
                        if binding.conditional {
                            return TypeTarget::Unknown;
                        }
                        return self.explicit_payload(
                            declaration,
                            &binding.value,
                            binding.position,
                            resolver,
                            trace,
                            depth + 1,
                        );
                    }
                    if self.scopes[id].bindings.contains_key(name.as_str()) {
                        return TypeTarget::Unknown;
                    }
                }
                TypeTarget::Unknown
            }
            ValueExpr::Project { value, member }
                if crate::languages::by_id(owner.language.as_str()).is_some_and(|profile| profile.linker().is_value_adapter(member)) =>
            {
                let receiver = self.evaluate_tracked(owner, value, at, resolver, None, trace);
                let linker = crate::languages::by_id(owner.language.as_str()).unwrap().linker();
                let projected = linker.adapter_value(&receiver, member, resolver)
                    .or_else(|| {
                        self.resolve_member_call(
                            &receiver, member, owner, at, resolver,
                        )
                    })
                    .unwrap_or(TypeTarget::Unknown);
                linker.value_payload(&projected).unwrap_or(projected)
            }
            ValueExpr::Scoped { value, .. } => {
                self.explicit_payload(owner, value, at, resolver, trace, depth + 1)
            }
            _ => TypeTarget::Unknown,
        }
    }
}

pub(in crate::linker) fn collection_callback(method: &str) -> bool {
    matches!(method, "map" | "filter" | "forEach" | "find" | "some" | "every" | "flatMap"
        | "inspect" | "filter_map" | "flat_map" | "for_each" | "try_for_each" | "position"
        | "rposition" | "take_while" | "skip_while" | "map_while" | "partition" | "find_map"
        | "any" | "all" | "retain")
}

fn join_tuple_elements<'a>(targets: &[TypeTarget<'a>], linker: Option<&dyn crate::languages::profile::LanguageLinker>) -> Option<TypeTarget<'a>> {
    let TypeTarget::Tuple(first) = targets.first()? else { return None; };
    let arity = first.len();
    if !targets
        .iter()
        .all(|t| matches!(t, TypeTarget::Tuple(items) if items.len() == arity))
    {
        return None;
    }
    let joined = (0..arity)
        .map(|i| {
            join_branch_targets(
                targets
                    .iter()
                    .filter_map(|t| match t { TypeTarget::Tuple(items) => Some(items[i].clone()), _ => None })
                    .collect(),
                true,
                linker,
            )
        })
        .collect();
    Some(TypeTarget::Tuple(joined))
}

fn join_branch_targets<'a>(
    targets: Vec<TypeTarget<'a>>,
    collapse_identical_local: bool,
    linker: Option<&dyn crate::languages::profile::LanguageLinker>,
) -> TypeTarget<'a> {
    if targets.is_empty() || targets.iter().all(|t| matches!(t, TypeTarget::Unknown)) {
        return TypeTarget::Unknown;
    }
    if targets.len() == 1 {
        return targets.into_iter().next().unwrap();
    }
    if let Some(special) = linker.and_then(|linker| linker.join_language_targets(&targets, &|targets| join_branch_targets(targets, true, Some(linker)))) {
        return special;
    }
    if let Some(tuple) = join_tuple_elements(&targets, linker) {
        return tuple;
    }
    let first = &targets[0];
    let allow_collapse = !matches!(
        first,
        TypeTarget::Unknown | TypeTarget::Ambiguous | TypeTarget::Candidates { .. }
    ) && (collapse_identical_local || !matches!(first, TypeTarget::Local(_)));
    if allow_collapse && targets[1..].iter().all(|other| same_target(first, other)) {
        return targets.into_iter().next().unwrap();
    }
    collect_candidate_join(targets)
}
