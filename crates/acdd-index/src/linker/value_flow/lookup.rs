//! Read-only lookup and call-resolution helpers on [`ValueFlowIndex`].

use super::types::{
    normalize_dotted_expression, reaching, ComputedKey, ReadTrace,
    SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex, ValueFlowLookup,
};
use crate::models::{Node, Reference};
use crate::semantic::SourcePosition;

impl<'a> ValueFlowIndex<'a> {
    /// Performs lookup.
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
                let setup_complete = is_parent
                    && super::super::vue::value_flow::setup_scope_complete(owner, scope.module);
                let immutable_capture = is_parent
                    && matches!(owner.language.as_str(), "javascript" | "typescript")
                    && super::super::javascript::value_flow::stable_immutable_capture(writes);
                if is_parent && writes.len() > 1 && !setup_complete && !immutable_capture {
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

    /// Performs return type.
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

    /// Performs computed receiver.
    pub fn computed_receiver(&self, reference: &Reference) -> Option<&TypeTarget<'a>> {
        self.computed.get(&ComputedKey {
            expression: reference.expression.as_str(),
            kind: reference.kind.as_str(),
            source: reference.source.as_str(),
            position: reference.position(),
        })
    }

    /// Performs field type.
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

    /// Performs type of expression.
    pub fn type_of_expression(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &impl SemanticResolver<'a>,
    ) -> Option<&TypeTarget<'a>> {
        self.type_of_expression_tracked(owner, expression, at, resolver, None)
    }

    pub(in crate::linker) fn type_of_expression_tracked(
        &self,
        owner: &'a Node,
        expression: &str,
        at: SourcePosition,
        resolver: &(impl SemanticResolver<'a> + ?Sized),
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        let expression = normalize_dotted_expression(expression);
        let expression = expression.as_ref();
        if expression.len() > self.limits.name_bytes
            || expression.matches('.').count() > self.limits.alias_depth
        {
            return Some(&self.unknown);
        }
        if let Some(target) = self.lookup_tracked(owner, expression, at, trace) {
            if let Some(returned) = super::super::python::value_flow::callable_target_return(
                self, target, owner, at, resolver, trace,
            ) {
                return Some(returned);
            }
        }
        if let Some(constructor) =
            super::super::python::value_flow::constructor_symbol(owner, expression, at, resolver)
        {
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
        resolver: &(impl SemanticResolver<'a> + ?Sized),
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
        resolver: &(impl SemanticResolver<'a> + ?Sized),
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
        resolver: &(impl SemanticResolver<'a> + ?Sized),
        trace: Option<&ReadTrace<'a, '_>>,
    ) -> Option<&TypeTarget<'a>> {
        if let Some(target) = self.lookup_tracked(owner, expression, at, trace) {
            return Some(target);
        }
        if let Some((receiver, member)) = expression.rsplit_once('.') {
            if let Some(class) = self
                .value_expression(owner, receiver, at, resolver, trace)
                .and_then(super::super::rust::value_flow::local_class)
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
