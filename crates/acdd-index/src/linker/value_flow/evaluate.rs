use super::types::{
    collect_candidate_join, node_position, resolve_type, same_target, symbol_type, Definitions,
    ReadTrace, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};
use crate::models::Node;
use crate::semantic::{SourcePosition, ValueExpr, ValueFlowFacts};
use hashbrown::HashMap;
use std::{borrow::Cow, sync::Arc};

impl<'a> ValueFlowIndex<'a> {
    /// Resolves value.
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
        match value {
            ValueExpr::Annotated { type_expr } => {
                resolve_type(owner, type_expr, at, resolver, self.limits)
            }
            ValueExpr::Construct { callee } => {
                if owner.language == "rust" && callee == "__rust_closure!" {
                    return TypeTarget::Builtin("__rust_closure".to_owned());
                }
                if let Some(target) = super::super::python::value_flow::literal_type(owner, callee)
                {
                    return target;
                }
                if let Some(target) = super::super::javascript::value_flow::construct(owner, callee)
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
                        Symbol::Callable(node) if node.kind == "constant" => self
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
            ValueExpr::Unknown => TypeTarget::Unknown,
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
            if matches!(owner.language.as_str(), "typescript" | "javascript" | "vue") {
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
        if owner.language == "rust"
            && matches!(
                self.lookup_tracked(owner, callee, at, trace),
                Some(TypeTarget::Builtin(name)) if name == "__rust_closure"
            )
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
        if let Some(target) =
            super::super::python::value_flow::constructor_result(owner, callee, at, resolver)
        {
            return target;
        } else if matches!(owner.language.as_str(), "javascript" | "typescript" | "vue") {
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
            if let Some(target) = super::super::javascript::value_flow::evaluate_call(
                self, owner, callee, at, resolver, trace,
            ) {
                return resolver
                    .resolve_call_result(owner, callee, site, at, Some(target.clone()))
                    .unwrap_or(target);
            }
        } else if let Some(target) = super::super::rust::value_flow::constructor_result(
            self, owner, callee, at, resolver, trace,
        ) {
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
        if let Some(target) = super::super::python::value_flow::dotted_member_result(
            self, owner, callee, at, resolver, trace,
        ) {
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
        if owner.language == "python"
            && matches!(
                receiver_expr,
                ValueExpr::Call { callee: super_name, site: None } if super_name == "super"
            )
        {
            if let Symbol::Callable(callable) = resolver.resolve_python_super_member(owner, callee)
            {
                if let Some(result) = self.return_type_tracked(callable, trace) {
                    return result.clone();
                }
            }
            return TypeTarget::Unknown;
        }
        let receiver =
            self.evaluate_tracked(owner, receiver_expr, at, resolver, definitions, trace);
        if let Symbol::Callable(callable) = resolver.resolve_member(&receiver, callee, owner, at) {
            if let Some(result) = self.return_type_tracked(callable, trace) {
                if !(owner.language == "python"
                    && callee == "__aenter__"
                    && matches!(result, TypeTarget::Unknown))
                {
                    return result.clone();
                }
            }
        }
        let member_result = if owner.language == "rust" {
            self.resolve_rust_member_or_adapter(&receiver, callee, owner, at, resolver)
        } else {
            resolver.resolve_member_result(&receiver, callee, owner, at)
        };
        if let Some(target) = member_result {
            return target;
        }
        if let Some(target) =
            super::super::javascript::value_flow::platform_member_return(owner, &receiver, callee)
        {
            return target;
        }
        if let Some(target) = super::super::python::value_flow::synthetic_member_result(
            self, owner, &receiver, callee, at, resolver, trace,
        ) {
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
        if owner.language == "rust"
            && receiver == "self"
            && owner.details["receiver_name"].as_str() == Some("self")
        {
            let target = symbol_type(resolver.resolve_symbol(owner, "Self", at, SymbolRole::Type));
            return super::super::rust::value_flow::local_class(&target)
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
        if let Some(class) = super::super::rust::value_flow::local_class(&receiver_type) {
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
            if matches!(owner.language.as_str(), "javascript" | "typescript") {
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
        if owner.language == "rust" {
            if let Some(target) = self.evaluate_rust_project(
                owner,
                value,
                member,
                at,
                resolver,
                definitions,
                trace,
            ) {
                return target;
            }
            if super::super::rust::value_flow::is_adapter(member) {
                return self.evaluate_rust_adapter(
                    owner,
                    value,
                    member,
                    at,
                    resolver,
                    definitions,
                    trace,
                );
            }
        }
        let source = if owner.language == "rust"
            && matches!(value, ValueExpr::Alias { name } if name == "self")
            && owner.details["receiver_name"].as_str() == Some("self")
        {
            symbol_type(resolver.resolve_symbol(owner, "Self", at, SymbolRole::Type))
        } else {
            self.evaluate_tracked(owner, value, at, resolver, definitions, trace)
        };
        if let Some(index_str) = member.strip_prefix("__index:") {
            return self.project_index(&source, index_str);
        }
        if let Some(field) = member.strip_prefix("__field:") {
            return self.project_field(owner, &source, field, at, resolver, trace);
        }
        if let TypeTarget::RustTuple(items) | TypeTarget::Tuple(items) = &source {
            if let Ok(index) = member.parse::<usize>() {
                return items.get(index).cloned().unwrap_or(TypeTarget::Unknown);
            }
        }
        if let Some(target) = super::super::rust::value_flow::local_class(&source)
            .and_then(|class| self.field_type_tracked(class, member, trace))
        {
            return target.clone();
        }
        if owner.language == "python" {
            if let Some(target) =
                super::super::python::value_flow::synthetic_property_result(owner, &source, member)
            {
                return target;
            }
        }
        self.project_field(owner, &source, member, at, resolver, trace)
    }

    fn project_index(&self, source: &TypeTarget<'a>, index_str: &str) -> TypeTarget<'a> {
        let base = super::super::rust::value_flow::referent(source).0;
        match base {
            TypeTarget::Tuple(items) | TypeTarget::RustTuple(items) => index_str
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .cloned()
                .unwrap_or(TypeTarget::Unknown),
            TypeTarget::Collection { item, .. }
            | TypeTarget::RustCollection { item }
            | TypeTarget::RustSlice { item }
            | TypeTarget::RustTypedContainer { item, .. } => (**item).clone(),
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
        match source {
            TypeTarget::Tuple(items) | TypeTarget::RustTuple(items) => member
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .cloned()
                .unwrap_or(TypeTarget::Unknown),
            TypeTarget::Builtin(name)
                if matches!(owner.language.as_str(), "javascript" | "typescript") =>
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
            TypeTarget::External { .. } if owner.language == "rust" => {
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
            TypeTarget::RustReference { .. } | TypeTarget::RustDeref { .. } => {
                match resolver.resolve_member(source, member, owner, at) {
                    Symbol::Type(target) => target,
                    Symbol::Callable(node) => TypeTarget::Callable(node),
                    Symbol::Ambiguous => TypeTarget::Ambiguous,
                    _ => TypeTarget::Unknown,
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

    #[allow(clippy::too_many_arguments)]
    fn evaluate_rust_project(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        member: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<TypeTarget<'a>> {
        if member == "__rust_generic_call" {
            let ValueExpr::BranchJoin { alternatives } = value else {
                return Some(TypeTarget::Unknown);
            };
            let [call @ ValueExpr::Call { callee, .. }, ValueExpr::Annotated { type_expr }] =
                alternatives.as_slice()
            else {
                return Some(TypeTarget::Unknown);
            };
            return Some(
                super::super::rust::value_flow::deserializer_result(
                    owner,
                    callee,
                    type_expr,
                    at,
                    resolver,
                    self.limits,
                )
                .unwrap_or_else(|| {
                    self.evaluate_tracked(owner, call, at, resolver, definitions, trace)
                }),
            );
        }
        if matches!(member, "__rust_array" | "__rust_tuple" | "__rust_if_join") {
            let ValueExpr::BranchJoin { alternatives } = value else {
                return Some(TypeTarget::Unknown);
            };
            if member == "__rust_tuple" {
                return Some(TypeTarget::RustTuple(
                    alternatives
                        .iter()
                        .map(|item| {
                            self.evaluate_tracked(owner, item, at, resolver, definitions, trace)
                        })
                        .collect(),
                ));
            }
            if member == "__rust_if_join" {
                return Some(join_branch_targets(
                    alternatives
                        .iter()
                        .map(|item| {
                            self.evaluate_tracked(owner, item, at, resolver, definitions, trace)
                        })
                        .collect(),
                    true,
                ));
            }
            let mut items = alternatives
                .iter()
                .map(|item| self.evaluate_tracked(owner, item, at, resolver, definitions, trace));
            let item = items.next().unwrap_or(TypeTarget::Unknown);
            let item = if items.all(|other| same_target(&item, &other)) {
                item
            } else {
                TypeTarget::Unknown
            };
            return Some(TypeTarget::RustCollection {
                item: Box::new(item),
            });
        }
        if matches!(member, "__rust_ref" | "__rust_ref_mut") {
            return Some(TypeTarget::RustReference {
                target: Box::new(self.evaluate_tracked(
                    owner,
                    value,
                    at,
                    resolver,
                    definitions,
                    trace,
                )),
                mutable: member == "__rust_ref_mut",
            });
        }
        let field = member.strip_prefix("__rust_field:")?;
        let source = if matches!(value, ValueExpr::Alias { name } if name == "self")
            && owner.details["receiver_name"].as_str() == Some("self")
        {
            symbol_type(resolver.resolve_symbol(owner, "Self", at, SymbolRole::Type))
        } else {
            self.evaluate_tracked(owner, value, at, resolver, definitions, trace)
        };
        if let TypeTarget::RustTuple(items) = super::super::rust::value_flow::referent(&source).0 {
            return Some(
                field
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| items.get(index))
                    .cloned()
                    .unwrap_or(TypeTarget::Unknown),
            );
        }
        Some(
            super::super::rust::value_flow::local_class(&source)
                .and_then(|class| self.field_type_tracked(class, field, trace))
                .cloned()
                .or_else(|| {
                    if matches!(
                        &source,
                        TypeTarget::External { .. } | TypeTarget::Builtin(_)
                    ) {
                        resolver.resolve_member_result(&source, field, owner, at)
                    } else {
                        None
                    }
                })
                .unwrap_or(TypeTarget::Unknown),
        )
    }

    fn resolve_rust_member_or_adapter(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        owner: &'a Node,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
    ) -> Option<TypeTarget<'a>> {
        match resolver.resolve_member_result(receiver, member, owner, at) {
            Some(
                target @ (TypeTarget::RustWrapper { .. }
                | TypeTarget::RustIterator { .. }
                | TypeTarget::RustTuple(_)),
            ) => Some(target),
            _ => super::super::rust::value_flow::declared_adapter_result(
                receiver, member, owner, at, resolver,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_rust_adapter(
        &self,
        owner: &'a Node,
        value: &ValueExpr,
        member: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
        definitions: Option<&Definitions<'a>>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> TypeTarget<'a> {
        let receiver = self.evaluate_tracked(owner, value, at, resolver, definitions, trace);
        if let Some(projected) = super::super::rust::value_flow::project(&receiver, member, resolver)
        {
            if !matches!(projected, TypeTarget::Unknown) {
                return projected;
            }
            if matches!(member, "?" | "unwrap" | "expect")
                && (matches!(&receiver, TypeTarget::Unknown)
                    || matches!(&receiver, TypeTarget::Builtin(name)
                        if name == "Option" || name == "Result"))
            {
                return self.explicit_payload(owner, value, at, resolver, trace, 0);
            }
            return TypeTarget::Unknown;
        }
        // Prefer known Rust adapter semantics over a generic method signature:
        // library signatures often erase the borrowed payload type.
        if member != "?" {
            if let Symbol::Callable(callable) =
                resolver.resolve_member(&receiver, member, owner, at)
            {
                if let Some(trace) = trace {
                    trace.returned(callable.id.as_str());
                }
                if let Some(target) = self
                    .returns
                    .get(callable.id.as_str())
                    .filter(|t| !matches!(t, TypeTarget::Unknown))
                {
                    return target.clone();
                }
                if let Some(target) = self.resolve_rust_member_or_adapter(
                    &receiver, member, owner, at, resolver,
                ) {
                    return target;
                }
                return TypeTarget::Unknown;
            }
        }
        if let Some(result) = self.resolve_rust_member_or_adapter(
            &receiver, member, owner, at, resolver,
        ) {
            return result;
        }
        if matches!(member, "?" | "unwrap" | "expect")
            && (matches!(&receiver, TypeTarget::Unknown)
                || matches!(&receiver, TypeTarget::Builtin(name)
                    if name == "Option" || name == "Result"))
        {
            return self.explicit_payload(owner, value, at, resolver, trace, 0);
        }
        TypeTarget::Unknown
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
        mut receiver: &TypeTarget<'a>,
        method: &str,
        arg_index: usize,
        param_index: usize,
    ) -> TypeTarget<'a> {
        let valid_arg = arg_index == 0
            || (arg_index == 1 && matches!(method, "map_or" | "map_or_else"));
        if param_index != 0 || !valid_arg {
            return TypeTarget::Unknown;
        }
        while matches!(receiver, TypeTarget::RustReference { .. })
            || matches!(receiver, TypeTarget::RustDeref { wrapper, .. } if *wrapper != "LocalKey")
        {
            match receiver {
                TypeTarget::RustReference { target, .. }
                | TypeTarget::RustDeref { target, .. } => receiver = target,
                _ => break,
            }
        }
        match receiver {
            TypeTarget::RustDeref {
                wrapper: "LocalKey",
                target,
            } if matches!(method, "with" | "try_with") => match target.as_ref() {
                TypeTarget::RustTuple(items) => items.first().map_or(TypeTarget::Unknown, |inner| {
                    TypeTarget::RustReference {
                        mutable: false,
                        target: Box::new(inner.clone()),
                    }
                }),
                _ => TypeTarget::Unknown,
            },
            TypeTarget::Promise(inner) if matches!(method, "then" | "finally") => {
                if method == "then" {
                    (**inner).clone()
                } else {
                    TypeTarget::Unknown
                }
            }
            TypeTarget::Collection { item, .. }
            | TypeTarget::RustIterator { item }
            | TypeTarget::RustCollection { item }
            | TypeTarget::RustSlice { item }
            | TypeTarget::RustTypedContainer { item, .. }
                if matches!(
                    method,
                    "map"
                        | "filter"
                        | "forEach"
                        | "find"
                        | "some"
                        | "every"
                        | "flatMap"
                        | "inspect"
                        | "filter_map"
                        | "flat_map"
                        | "for_each"
                        | "try_for_each"
                        | "position"
                        | "rposition"
                        | "take_while"
                        | "skip_while"
                        | "map_while"
                        | "partition"
                        | "find_map"
                        | "any"
                        | "all"
                        | "retain"
                ) =>
            {
                (**item).clone()
            }
            TypeTarget::RustWrapper { ok, .. }
                if matches!(
                    method,
                    "map"
                        | "and_then"
                        | "inspect"
                        | "filter"
                        | "is_some_and"
                        | "is_none_or"
                        | "ok_or_else"
                        | "map_or"
                        | "map_or_else"
                ) =>
            {
                (**ok).clone()
            }
            TypeTarget::RustWrapper {
                err: Some(err), ..
            } if matches!(method, "map_err" | "or_else" | "inspect_err") => (**err).clone(),
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
                ValueFlowFacts::deserialize_for_scope(&node.details["value_flow"], false)
                    .ok()
                    .map(Cow::Owned)
            } else {
                None
            }
        };
        match value {
            ValueExpr::Annotated { type_expr } => resolver.resolve_payload(owner, type_expr, at),
            ValueExpr::Call { callee, .. } => {
                if matches!(owner.language.as_str(), "typescript" | "javascript" | "vue") {
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
                if matches!(owner.language.as_str(), "typescript" | "javascript" | "vue") {
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
                let reaching_wrapper = match self.lookup_tracked(owner, name, at, trace) {
                    Some(target) if owner.language == "rust" => {
                        matches!(target, TypeTarget::RustWrapper { .. })
                            || matches!(target, TypeTarget::Builtin(wrapper)
                                if matches!(wrapper.as_str(), "Option" | "Result"))
                    }
                    Some(TypeTarget::Promise(_)) => true,
                    Some(TypeTarget::Builtin(wrapper))
                        if matches!(
                            owner.language.as_str(),
                            "typescript" | "javascript" | "vue"
                        ) =>
                    {
                        wrapper == "Promise"
                    }
                    _ => false,
                };
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
                if owner.language == "rust" && super::super::rust::value_flow::is_adapter(member) =>
            {
                let receiver = self.evaluate_tracked(owner, value, at, resolver, None, trace);
                let projected = super::super::rust::value_flow::project(&receiver, member, resolver)
                    .or_else(|| {
                        self.resolve_rust_member_or_adapter(
                            &receiver, member, owner, at, resolver,
                        )
                    })
                    .unwrap_or(TypeTarget::Unknown);
                super::super::rust::value_flow::unwrap_payload(&projected).unwrap_or(projected)
            }
            ValueExpr::Scoped { value, .. } => {
                self.explicit_payload(owner, value, at, resolver, trace, depth + 1)
            }
            _ => TypeTarget::Unknown,
        }
    }
}

fn tuple_items<'a, 'b>(target: &'b TypeTarget<'a>, rust: bool) -> Option<&'b [TypeTarget<'a>]> {
    match target {
        TypeTarget::RustTuple(items) if rust => Some(items),
        TypeTarget::Tuple(items) if !rust => Some(items),
        _ => None,
    }
}

fn join_tuple_elements<'a>(targets: &[TypeTarget<'a>], rust: bool) -> Option<TypeTarget<'a>> {
    let arity = tuple_items(targets.first()?, rust)?.len();
    if !targets
        .iter()
        .all(|t| tuple_items(t, rust).is_some_and(|items| items.len() == arity))
    {
        return None;
    }
    let joined = (0..arity)
        .map(|i| {
            join_branch_targets(
                targets
                    .iter()
                    .filter_map(|t| tuple_items(t, rust).map(|items| items[i].clone()))
                    .collect(),
                true,
            )
        })
        .collect();
    Some(if rust { TypeTarget::RustTuple(joined) } else { TypeTarget::Tuple(joined) })
}

fn join_branch_targets<'a>(
    targets: Vec<TypeTarget<'a>>,
    collapse_identical_local: bool,
) -> TypeTarget<'a> {
    use super::types::RustWrapperKind;
    if targets.is_empty() || targets.iter().all(|t| matches!(t, TypeTarget::Unknown)) {
        return TypeTarget::Unknown;
    }
    if targets.len() == 1 {
        return targets.into_iter().next().unwrap();
    }
    if let Some(tuple) =
        join_tuple_elements(&targets, true).or_else(|| join_tuple_elements(&targets, false))
    {
        return tuple;
    }
    let is_opt = |t: &TypeTarget<'a>| {
        matches!(t, TypeTarget::RustWrapper { wrapper: RustWrapperKind::Option, .. })
    };
    if targets.iter().any(is_opt)
        && targets.iter().all(|t| {
            is_opt(t) || matches!(t, TypeTarget::Unknown) || matches!(t, TypeTarget::Builtin(n) if n == "Option")
        })
    {
        let payloads = targets
            .into_iter()
            .filter_map(|t| match t {
                TypeTarget::RustWrapper { ok, .. } => Some(*ok),
                _ => None,
            })
            .collect();
        return TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Option,
            ok: Box::new(join_branch_targets(payloads, true)),
            err: None,
        };
    }
    if targets.iter().all(|t| {
        matches!(t, TypeTarget::RustWrapper { wrapper: RustWrapperKind::Result, .. })
    }) {
        let mut oks = Vec::with_capacity(targets.len());
        let mut errs = Vec::with_capacity(targets.len());
        for target in targets {
            if let TypeTarget::RustWrapper { ok, err, .. } = target {
                oks.push(*ok);
                errs.extend(err.map(|e| *e));
            }
        }
        let joined_err =
            (errs.len() == oks.len()).then(|| Box::new(join_branch_targets(errs, true)));
        return TypeTarget::RustWrapper {
            wrapper: RustWrapperKind::Result,
            ok: Box::new(join_branch_targets(oks, true)),
            err: joined_err,
        };
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
