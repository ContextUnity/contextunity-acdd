//! Read-only lookup and call-resolution helpers on [`ValueFlowIndex`].

use crate::languages::profile::ProfileLinker;
use super::types::{
    normalize_dotted_expression, reaching, ComputedKey, ReadTrace,
    SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex, ValueFlowLookup,
};
use crate::models::{Node, Reference};
use crate::semantic::SourcePosition;

impl<'a> ValueFlowIndex<'a> {
    /// Resolves the reaching type bound to `name` in `owner` at source position `at`.
    pub fn lookup(&self, owner: &Node, name: &str, at: SourcePosition) -> Option<&TypeTarget<'a>> {
        self.lookup_tracked(owner, name, at, None)
    }

    pub(in crate::linker) fn async_return_type(&self, node: &Node) -> Option<&TypeTarget<'a>> {
        self.async_returns.get(node.id.as_str())
    }

    pub(in crate::linker) fn lookup_tracked(
        &self,
        owner: &Node,
        name: &str,
        at: SourcePosition,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        let (id, scope) = self.scopes.get_key_value(owner.id.as_str())?;
        for (is_parent, id) in
            std::iter::once((false, *id)).chain(scope.parents.iter().map(|id| (true, *id)))
        {
            let scope = &self.scopes[id];
            if scope.invalid {
                return Some(&self.unknown);
            }
            if let Some(writes) = scope.bindings.get(name) {
                let linker = crate::languages::by_id(owner.language.as_str()).map(|p| p.linker());
                let setup_complete = is_parent
                    && linker.is_some_and(|l| l.setup_scope_complete(owner, scope.module));
                let immutable_capture = is_parent
                    && linker.is_some_and(|l| l.stable_immutable_capture(writes));
                let uniform_capture = is_parent
                    && writes.first().is_some_and(|first| {
                        !matches!(first.target, TypeTarget::Unknown | TypeTarget::Ambiguous)
                            && writes
                                .iter()
                                .all(|write| super::types::same_target(&first.target, &write.target))
                    });
                if is_parent
                    && writes.len() > 1
                    && !setup_complete
                    && !immutable_capture
                    && !uniform_capture
                {
                    return Some(&self.unknown);
                }
                if let Some(trace) = trace {
                    trace.scope(id);
                }
                let position = if is_parent && setup_complete {
                    SourcePosition {
                        line: usize::MAX,
                        column: usize::MAX,
                    }
                } else {
                    at
                };
                return Some(reaching(writes, position).unwrap_or(&self.unknown));
            }
        }
        None
    }

    pub(super) fn collection_element(
        &self,
        owner: &Node,
        iterable: &str,
        at: SourcePosition,
    ) -> TypeTarget<'a> {
        let Some(scope) = self.scopes.get(owner.id.as_str()) else {
            return TypeTarget::Unknown;
        };
        for (depth, id) in std::iter::once(&owner.id.as_str())
            .chain(scope.parents.iter())
            .enumerate()
        {
            let Some(candidate_scope) = self.scopes.get(*id) else {
                return TypeTarget::Unknown;
            };
            if candidate_scope.invalid {
                return TypeTarget::Unknown;
            }
            let Some(writes) = candidate_scope.bindings.get(iterable) else {
                continue;
            };
            if depth > 0 && writes.len() > 1 {
                return TypeTarget::Unknown;
            }
            let Some(last) = writes.iter().rev().find(|write| write.position < at) else {
                return TypeTarget::Unknown;
            };
            match &last.target {
                TypeTarget::Collection { item, .. }
                | TypeTarget::RustIterator { item }
                | TypeTarget::RustCollection { item }
                | TypeTarget::RustSlice { item }
                | TypeTarget::RustTypedContainer { item, .. } => {
                    return (**item).clone();
                }
                TypeTarget::Builtin(name)
                    if matches!(name.as_str(), "list" | "List" | "Sequence" | "Iterable") => {}
                _ => return TypeTarget::Unknown,
            }
            if let Some(element) = self
                .collection_elements
                .get(*id)
                .and_then(|names| names.get(iterable))
                .and_then(|elements| {
                    elements
                        .iter()
                        .rfind(|(position, _)| *position == last.position)
                })
                .map(|(_, element)| element.clone())
            {
                return element;
            }
        }
        TypeTarget::Unknown
    }

    /// Returns the inferred return type recorded for `callable`.
    pub fn return_type(&self, callable: &Node) -> Option<&TypeTarget<'a>> {
        self.return_type_tracked(callable, None)
    }

    pub(in crate::linker) fn return_type_tracked(
        &self,
        callable: &Node,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        self.returns
            .get_key_value(callable.id.as_str())
            .map(|(id, target)| {
                if let Some(trace) = trace {
                    trace.returned(id);
                }
                target
            })
    }

    /// Returns the precomputed receiver type recorded for call `reference`.
    pub fn computed_receiver(&self, reference: &Reference) -> Option<&TypeTarget<'a>> {
        self.computed.get(&ComputedKey {
            expression: reference.expression.as_str(),
            kind: reference.kind.as_str(),
            source: reference.source.as_str(),
            position: reference.position(),
        })
    }

    /// Returns the inferred field type for `name` on declaration `class`.
    pub fn field_type(&self, class: &Node, name: &str) -> Option<&TypeTarget<'a>> {
        self.field_type_tracked(class, name, None)
    }

    /// Returns a component slot prop type declared in the exact provider module.
    pub fn component_slot_type(
        &self,
        module: &Node,
        slot: &str,
        binding: &str,
    ) -> Option<&TypeTarget<'a>> {
        self.component_slots
            .get(module.id.as_str())
            .and_then(|slots| slots.get(&(slot.to_owned(), binding.to_owned())))
    }

    pub(in crate::linker) fn field_type_tracked(
        &self,
        class: &Node,
        name: &str,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        if let Some(trace) = trace {
            trace.fields();
        }
        self.fields
            .get(class.id.as_str())
            .and_then(|fields| fields.get(name))
    }

    /// Reads only an already reduced, lexically established external callable.
    pub fn configured_callable(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
    ) -> Option<String> {
        let TypeTarget::ExternalCallable {
            result,
            owner: established,
            position,
            member,
        } = self.lookup(owner, expression, at)?
        else {
            return None;
        };
        Some(format!(
            "{}; callable:{}; binding:{}:{}:{}",
            result.evidence, member, established, position.line, position.column
        ))
    }

    pub(in crate::linker) fn type_of_expression_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        let expression = normalize_dotted_expression(expression);
        let expression = expression.as_ref();
        if expression.len() > self.limits.name_bytes
            || expression.matches('.').count() > self.limits.alias_depth
        {
            return Some(&self.unknown);
        }
        let linker = crate::languages::by_id(owner.language.as_str()).map(|p| p.linker());
        if let Some(target) = self.lookup_tracked(owner, expression, at, trace) {
            let returned = match target {
                TypeTarget::Callable(callable) => self.return_type_tracked(callable, trace),
                TypeTarget::CallableReturn(ret) => Some(ret.as_ref()),
                TypeTarget::ClassType(inner) => Some(inner.as_ref()),
                other => linker.and_then(|l| {
                    l.callable_target_return(self, other, owner, at, resolver, trace)
                }),
            };
            if let Some(returned) = returned {
                return Some(returned);
            }
        }
        if let Some(constructor) = linker.and_then(|l| {
            l.constructor_symbol(owner, expression, at, resolver)
        }) {
            if let Some(target) = self.borrow_symbol(constructor, trace) {
                return Some(target);
            }
        }
        self.borrow_return_symbol(
            self.resolve_callable_symbol_tracked(owner, expression, at, resolver, trace),
            trace,
        )
    }

    pub(super) fn callable_expression_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&'a Node> {
        let expression = normalize_dotted_expression(expression);
        let expression = expression.as_ref();
        if expression.len() > self.limits.name_bytes
            || expression.matches('.').count() > self.limits.alias_depth
        {
            return None;
        }
        match self.resolve_callable_symbol_tracked(owner, expression, at, resolver, trace) {
            Symbol::Callable(callable) => Some(callable),
            _ => None,
        }
    }

    fn resolve_callable_symbol_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Symbol<'a> {
        if let Some(TypeTarget::Callable(callable)) =
            self.lookup_tracked(owner, expression, at, trace)
        {
            return Symbol::Callable(callable);
        }
        if let Some((receiver, member)) = expression.rsplit_once('.') {
            if self.lookup_tracked(owner, receiver, at, trace).is_none()
                && matches!(
                    resolver.resolve_symbol(owner, receiver, at, SymbolRole::Type),
                    Symbol::Type(TypeTarget::Local(node))
                        if matches!(node.kind.as_str(), "class" | "struct" | "interface" | "enum")
                )
            {
                return resolver.resolve_symbol(owner, expression, at, SymbolRole::Callable);
            }
            if let Some(target) = self.value_expression(owner, receiver, at, resolver, trace) {
                if let Some(method) = Self::object_member(target, member) {
                    return Symbol::Callable(method);
                }
                return resolver.resolve_member(target, member, owner, at);
            }
        }
        resolver.resolve_symbol(owner, expression, at, SymbolRole::Callable)
    }

    pub(super) fn value_expression(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        if let Some(target) = self.lookup_tracked(owner, expression, at, trace) {
            return Some(target);
        }
        if let Some((receiver, member)) = expression.rsplit_once('.') {
            let linker = crate::languages::by_id(owner.language.as_str()).map(|profile| profile.linker());
            if let Some(class) = self
                .value_expression(owner, receiver, at, resolver, trace)
                .and_then(|target| linker.and_then(|l| l.local_receiver_class(target)))
            {
                return self.field_type_tracked(class, member, trace);
            }
        }
        self.borrow_symbol(
            resolver.resolve_symbol(owner, expression, at, SymbolRole::Type),
            trace,
        )
    }

    fn borrow_symbol(
        &self,
        symbol: Symbol<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        match symbol {
            Symbol::Callable(node) => self.return_type_tracked(node, trace),
            Symbol::Type(TypeTarget::Local(node)) => self.local_types.get(node.id.as_str()),
            Symbol::Ambiguous => Some(&self.ambiguous),
            Symbol::Type(TypeTarget::Ambiguous) => Some(&self.ambiguous),
            _ => None,
        }
    }

    fn borrow_return_symbol(
        &self,
        symbol: Symbol<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        match symbol {
            Symbol::Callable(node) => self.return_type_tracked(node, trace),
            Symbol::Ambiguous => Some(&self.ambiguous),
            _ => None,
        }
    }
}

pub(in crate::linker) fn resolve_type<'a>(
    scope: &'a Node,
    expression: &crate::semantic::TypeExpr,
    at: SourcePosition,
    resolver: &dyn SemanticResolver<'a>,
    limits: super::types::SemanticLimits,
) -> TypeTarget<'a> {
    use super::types::{node_position, symbol_type, valid_type};
    use crate::semantic::TypeExpr;
    use serde::Deserialize;
    use std::borrow::Cow;

    let mut scope = scope;
    let mut expression = Cow::Borrowed(expression);
    let mut at = at;
    let mut visited = Vec::new();
    for _ in 0..limits.alias_depth {
        let linker = crate::languages::by_id(scope.language.as_str()).map(|p| p.linker());
        if valid_type(&expression, limits, 0) {
            if let Some(resolved) = linker.and_then(|l| {
                l.resolve_special_type_expr(scope, &expression, at, resolver, limits)
            }) {
                return resolved;
            }
        }
        if let TypeExpr::Applied { base, args } = &*expression {
            return if valid_type(&expression, limits, 0) {
                resolver.resolve_applied(scope, base, args, at)
            } else {
                TypeTarget::Unknown
            };
        }
        let TypeExpr::Named { name } = &*expression else {
            return TypeTarget::Unknown;
        };
        if name.is_empty() || name.len() > limits.name_bytes {
            return TypeTarget::Unknown;
        }
        let target = symbol_type(resolver.resolve_symbol(scope, name, at, SymbolRole::Type));
        let TypeTarget::Local(node) = &target else {
            return target;
        };
        if let Some(special) = crate::languages::by_id(node.language.as_str())
            .and_then(|profile| profile.linker().special_type_alias_target(node, resolver))
        {
            return special;
        }
        let alias = if let Some(alias) = resolver
            .value_flow(node)
            .and_then(|facts| facts.alias_type.as_ref())
        {
            Cow::Borrowed(alias)
        } else {
            let encoded = match resolver.raw_value_flow(node) {
                Some(raw) => &raw["alias_type"],
                None if resolver.has_typed_flows() => return target,
                None => &node.detail_entry(crate::models::details::NodeDetailKey::VALUE_FLOW)["alias_type"],
            };
            if encoded.is_null() {
                return target;
            }
            let Ok(alias) = TypeExpr::deserialize(encoded) else {
                return TypeTarget::Unknown;
            };
            Cow::Owned(alias)
        };
        if visited.contains(&node.id.as_str()) {
            return TypeTarget::Unknown;
        }
        visited.push(node.id.as_str());
        expression = alias;
        scope = node;
        at = node_position(node);
    }
    TypeTarget::Unknown
}

pub(crate) fn stored_type_expr(annotation: &str, language: &str) -> crate::semantic::TypeExpr {
    use crate::semantic::TypeExpr;
    let annotation = annotation.trim();
    if let Some(expression) = crate::languages::by_id(language)
        .and_then(|profile| profile.linker().stored_type_expr(annotation))
    {
        return expression;
    }
    if let Some((base, rest)) = annotation.split_once('[') {
        if let Some(argument) = rest.strip_suffix(']') {
            let base = base.trim();
            let argument = argument.trim();
            if matches!(base, "list" | "List" | "Sequence" | "Iterable")
                && !argument.is_empty()
                && !argument.contains(['[', ',', ']'])
            {
                return TypeExpr::Applied {
                    base: base.to_owned(),
                    args: vec![TypeExpr::Named {
                        name: argument.to_owned(),
                    }],
                };
            }
        }
    }
    TypeExpr::Named {
        name: annotation.to_owned(),
    }
}

impl<'a> ValueFlowLookup<'a> for ValueFlowIndex<'a> {
    fn lookup_tracked(
        &self,
        owner: &Node,
        name: &str,
        at: SourcePosition,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        ValueFlowIndex::lookup_tracked(self, owner, name, at, trace)
    }

    fn return_type_tracked(
        &self,
        callable: &Node,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        ValueFlowIndex::return_type_tracked(self, callable, trace)
    }

    fn async_return_type(&self, node: &Node) -> Option<&TypeTarget<'a>> {
        ValueFlowIndex::async_return_type(self, node)
    }

    fn field_type_tracked(
        &self,
        class: &Node,
        name: &str,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        ValueFlowIndex::field_type_tracked(self, class, name, trace)
    }

    fn type_of_expression_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &dyn SemanticResolver<'a>,
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        ValueFlowIndex::type_of_expression_tracked(self, owner, expression, at, resolver, trace)
    }
}
