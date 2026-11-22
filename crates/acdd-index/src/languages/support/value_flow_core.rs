use crate::ast::text;
use crate::languages::{
    CompiledSyntaxGrammar, CompiledSyntaxKind, DeclarationRole, MAX_VALUE_FLOW_TYPE_DEPTH, Syntax,
};
use crate::semantic::{
    BindingFact, CallSiteEvidence, FieldFact, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use std::sync::Arc;

/// Typed per-frame flags tracked across scope-aware AST traversals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeFrameFlags {
    bits: u8,
}

impl ScopeFrameFlags {
    const ACTIVE: u8 = 1 << 0;
    const BRANCH_CONDITIONAL: u8 = 1 << 1;
    const TRY_CONDITIONAL: u8 = 1 << 2;
    const COLLECT_FACTS: u8 = 1 << 3;
    const LEXICAL_SCOPE: u8 = 1 << 4;
    const ANNOTATION_OWNER: u8 = 1 << 5;
    const SUSPENDED: u8 = 1 << 6;

    /// Returns a cleared frame flag set.
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    const fn set_bit(mut self, bit: u8, enabled: bool) -> Self {
        if enabled {
            self.bits |= bit;
        } else {
            self.bits &= !bit;
        }
        self
    }

    /// Sets or clears the active-scope flag.
    pub const fn with_active(self, enabled: bool) -> Self {
        self.set_bit(Self::ACTIVE, enabled)
    }

    /// Sets or clears the branch-conditional flag.
    pub const fn with_branch_conditional(self, enabled: bool) -> Self {
        self.set_bit(Self::BRANCH_CONDITIONAL, enabled)
    }

    /// Sets or clears the try/catch-conditional flag.
    pub const fn with_try_conditional(self, enabled: bool) -> Self {
        self.set_bit(Self::TRY_CONDITIONAL, enabled)
    }

    /// Sets or clears the fact-collection flag.
    pub const fn with_collect_facts(self, enabled: bool) -> Self {
        self.set_bit(Self::COLLECT_FACTS, enabled)
    }

    /// Sets or clears the lexical-scope flag.
    pub const fn with_lexical_scope(self, enabled: bool) -> Self {
        self.set_bit(Self::LEXICAL_SCOPE, enabled)
    }

    /// Sets or clears the annotation-owner flag.
    pub const fn with_annotation_owner(self, enabled: bool) -> Self {
        self.set_bit(Self::ANNOTATION_OWNER, enabled)
    }

    /// Sets or clears the suspended-collection flag.
    pub const fn with_suspended(self, enabled: bool) -> Self {
        self.set_bit(Self::SUSPENDED, enabled)
    }

    /// Reports whether this frame belongs to the active analyzed body.
    pub const fn is_active(self) -> bool {
        self.bits & Self::ACTIVE != 0
    }

    /// Reports whether this frame lies inside a control-flow branch.
    pub const fn is_branch_conditional(self) -> bool {
        self.bits & Self::BRANCH_CONDITIONAL != 0
    }

    /// Reports whether this frame lies inside a `try` block.
    pub const fn is_try_conditional(self) -> bool {
        self.bits & Self::TRY_CONDITIONAL != 0
    }

    /// Reports whether branch or try conditionality applies.
    pub const fn is_conditional(self) -> bool {
        self.is_branch_conditional() || self.is_try_conditional()
    }

    /// Reports whether facts should be emitted inside this frame.
    pub const fn collects_facts(self) -> bool {
        self.bits & Self::COLLECT_FACTS != 0
    }

    /// Reports whether this frame introduces a lexical scope.
    pub const fn is_lexical_scope(self) -> bool {
        self.bits & Self::LEXICAL_SCOPE != 0
    }

    /// Reports whether annotations here belong to the enclosing declaration.
    pub const fn is_annotation_owner(self) -> bool {
        self.bits & Self::ANNOTATION_OWNER != 0
    }

    /// Reports whether collection is temporarily suspended in this frame.
    pub const fn is_suspended(self) -> bool {
        self.bits & Self::SUSPENDED != 0
    }
}

/// Explicit stack of [`ScopeFrameFlags`] shared across value-flow collectors.
#[derive(Clone, Debug, Default)]
pub struct ScopeState {
    frames: Vec<ScopeFrameFlags>,
}

impl ScopeState {
    /// Allocates a scope stack with the given frame capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            frames: Vec::with_capacity(capacity),
        }
    }

    /// Pushes one frame onto the scope stack.
    pub fn push(&mut self, flags: ScopeFrameFlags) {
        self.frames.push(flags);
    }

    /// Pops the top frame from the scope stack.
    pub fn pop(&mut self) -> Option<ScopeFrameFlags> {
        self.frames.pop()
    }

    /// Returns the active top frame if present.
    pub fn current(&self) -> Option<ScopeFrameFlags> {
        self.frames.last().copied()
    }

    /// Returns the active top frame or cleared defaults.
    pub fn current_or_default(&self) -> ScopeFrameFlags {
        self.current().unwrap_or_default()
    }

    /// Returns the number of frames currently on the scope stack.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Reports whether the scope stack is empty.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Reports whether the active top frame is conditional.
    pub fn is_conditional(&self) -> bool {
        self.current_or_default().is_conditional()
    }
}

/// Shared value-flow collector accumulator and scope stack.
#[derive(Debug, Default)]
pub struct ValueFlowCollectorCore {
    scope_state: ScopeState,
    facts: ValueFlowFacts,
}

impl ValueFlowCollectorCore {
    /// Creates a collector core with preallocated scope-stack capacity.
    pub fn with_scope_capacity(capacity: usize) -> Self {
        Self {
            scope_state: ScopeState::with_capacity(capacity),
            facts: ValueFlowFacts::default(),
        }
    }

    /// Borrows the scope stack immutably.
    pub fn scope_state(&self) -> &ScopeState {
        &self.scope_state
    }

    /// Borrows the scope stack mutably.
    pub fn scope_state_mut(&mut self) -> &mut ScopeState {
        &mut self.scope_state
    }

    /// Borrows the accumulated value-flow facts immutably.
    pub fn facts(&self) -> &ValueFlowFacts {
        &self.facts
    }

    /// Borrows the accumulated value-flow facts mutably.
    pub fn facts_mut(&mut self) -> &mut ValueFlowFacts {
        &mut self.facts
    }

    /// Appends a lexical binding fact.
    pub fn push_binding(&mut self, name: String, position: SourcePosition, value: ValueExpr, conditional: bool) {
        self.facts.bindings.push(BindingFact { name, position, value, conditional });
    }

    /// Appends a field assignment fact.
    pub fn push_field(&mut self, name: String, position: SourcePosition, value: ValueExpr, conditional: bool) {
        self.facts.fields.push(FieldFact { name, position, value, conditional });
    }

    /// Appends a bounded branch-join binding fact across enumerated `alternatives`.
    pub fn push_branch_join(
        &mut self,
        name: String,
        position: SourcePosition,
        alternatives: Vec<ValueExpr>,
        conditional: bool,
    ) {
        self.push_binding(name, position, ValueExpr::BranchJoin { alternatives }, conditional);
    }

    /// Sets the declared return type if not already recorded.
    pub fn set_return_type_if_none(&mut self, return_type: Option<TypeExpr>) {
        if self.facts.return_type.is_none() {
            self.facts.return_type = return_type;
        }
    }

    /// Sets the declared type alias if not already recorded.
    pub fn set_alias_type_if_none(&mut self, alias_type: Option<TypeExpr>) {
        if self.facts.alias_type.is_none() {
            self.facts.alias_type = alias_type;
        }
    }

    /// Records the latest return source position.
    pub fn set_return_position(&mut self, position: SourcePosition) {
        self.facts.return_position = Some(position);
    }

    /// Deterministically sorts accumulated facts and returns them.
    pub fn finish(mut self) -> ValueFlowFacts {
        self.facts.bindings.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| left.name.cmp(&right.name))
        });
        self.facts.fields.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| left.name.cmp(&right.name))
        });
        self.facts
    }
}

/// Declarative single-pass scope analyzer driven by [`CompiledSyntaxGrammar`] selectors.
pub struct GenericScopeAnalysis<'source, 'tree> {
    root: Syntax<'tree>,
    source: &'source str,
    compiled: Arc<CompiledSyntaxGrammar>,
    core: ValueFlowCollectorCore,
}

impl<'source, 'tree> GenericScopeAnalysis<'source, 'tree> {
    /// Creates a declarative analyzer bound to `root` and `compiled` grammar selectors.
    pub(crate) fn new(
        root: Syntax<'tree>,
        source: &'source str,
        compiled: Arc<CompiledSyntaxGrammar>,
    ) -> Self {
        Self {
            root,
            source,
            compiled,
            core: ValueFlowCollectorCore::with_scope_capacity(16),
        }
    }

    /// Processes an entering AST node during the shared `TreeCursor` traversal.
    pub fn enter(&mut self, node: Syntax<'tree>) {
        let parent = self.core.scope_state().current_or_default();
        let classified = self.compiled.classify(node.kind_id());
        let is_root = node.id() == self.root.id();
        let nested_scope = !is_root
            && classified.is_some_and(|kind| kind.declaration().is_some() || kind.is_scope());
        let active = is_root || (parent.is_active() && !nested_scope);
        let branch_conditional = parent.is_branch_conditional()
            || (!is_root && classified.is_some_and(CompiledSyntaxKind::is_conditional));
        let flags = ScopeFrameFlags::new()
            .with_active(active)
            .with_branch_conditional(branch_conditional)
            .with_collect_facts(active);
        self.core.scope_state_mut().push(flags);
        if !flags.collects_facts() {
            return;
        }

        let conditional = self.core.scope_state().is_conditional();
        if classified.is_some_and(CompiledSyntaxKind::is_typed_binding) {
            self.record_typed_binding(node, conditional);
        } else if classified.is_some_and(CompiledSyntaxKind::is_assignment) {
            if let Some((name_node, value_node)) = self.binding_pair(node) {
                if let Some(value) = self.value_expr(value_node) {
                    if let Some(name) = identifier_name(name_node, self.source) {
                        self.core
                            .push_binding(name.to_owned(), position(name_node), value, conditional);
                    } else if let Some(field_node) = self.receiver_field_target(name_node) {
                        self.core.push_field(
                            text(field_node, self.source).trim().to_owned(),
                            position(field_node),
                            value,
                            conditional,
                        );
                    }
                }
            }
        }
        if is_root {
            if let Some(return_type) = node
                .child_by_field_name("return_type")
                .or_else(|| node.child_by_field_name("result"))
                .and_then(|ty| parse_type_expr(ty, self.source, 0))
            {
                self.core.set_return_type_if_none(Some(return_type));
            }
            if classified.is_some_and(|kind| kind.declaration_role() == Some(DeclarationRole::Type)) {
                let alias_type = node
                    .child_by_field_name("type")
                    .or_else(|| node.child_by_field_name("value"))
                    .and_then(|ty| parse_type_expr(ty, self.source, 0));
                self.core.set_alias_type_if_none(alias_type);
            }
        }
        if node.kind().starts_with("return") {
            self.core.set_return_position(position(node));
        }
    }

    /// Processes an exiting AST node during the shared `TreeCursor` traversal.
    pub fn exit(&mut self, _node: Syntax<'tree>) {
        self.core.scope_state_mut().pop();
    }

    /// Finalizes deterministically sorted [`ValueFlowFacts`].
    pub fn finish(self) -> ValueFlowFacts {
        self.core.finish()
    }

    fn record_typed_binding(&mut self, node: Syntax<'tree>, conditional: bool) {
        if node.kind() == "property_declaration" {
            let mut cursor = node.walk();
            let mut var_decl = None;
            let mut initializer = None;
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "variable_declaration" => var_decl = Some(child),
                    "modifiers" | "binding_pattern_kind" | "type_constraints" | "getter"
                    | "setter" => {}
                    _ => initializer = Some(child),
                }
            }
            if let Some(decl) = var_decl {
                self.record_kotlin_decl(decl, initializer, conditional);
            }
            return;
        }
        if node.kind() == "variable_declaration"
            && node
                .parent()
                .is_some_and(|p| p.kind() == "property_declaration")
        {
            return;
        }
        if node.kind() == "declaration"
            && node
                .child_by_field_name("declarator")
                .is_some_and(|d| d.kind() == "init_declarator")
        {
            return;
        }
        if matches!(node.kind(), "parameter" | "variable_declaration")
            && node.child_by_field_name("name").is_none()
            && node.child_by_field_name("type").is_none()
        {
            self.record_kotlin_decl(node, None, conditional);
            return;
        }
        let Some(name_node) = binding_target(node).map(unwrap_declarator) else {
            return;
        };
        let explicit_type = node
            .child_by_field_name("type")
            .or_else(|| {
                matches!(node.kind(), "variable_declarator" | "init_declarator")
                    .then(|| node.parent().and_then(|p| p.child_by_field_name("type")))
                    .flatten()
            })
            .filter(|ty| !matches!(ty.kind(), "implicit_type") && text(*ty, self.source).trim() != "var")
            .and_then(|ty| parse_type_expr(ty, self.source, 0));
        let inferred_value = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("initializer"))
            .or_else(|| node.child_by_field_name("right"))
            .or_else(|| {
                let mut cursor = node.walk();
                let found = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "equals_value_clause")
                    .and_then(|c| c.named_child(0));
                found
            })
            .or_else(|| {
                find_variable_declarator_child(node, |child| {
                    child != name_node && child.kind() != "argument_list"
                })
            })
            .and_then(|value| self.value_expr(value));
        self.push_typed_binding(name_node, explicit_type, inferred_value, conditional);
    }

    fn record_kotlin_decl(
        &mut self,
        decl: Syntax<'tree>,
        initializer: Option<Syntax<'tree>>,
        conditional: bool,
    ) {
        let mut cursor = decl.walk();
        let mut name_node = None;
        let mut type_node = None;
        for child in decl.named_children(&mut cursor) {
            if matches!(child.kind(), "simple_identifier" | "identifier") && name_node.is_none() {
                name_node = Some(child);
            } else if matches!(
                child.kind(),
                "user_type" | "nullable_type" | "function_type" | "parenthesized_type"
            ) && type_node.is_none()
            {
                type_node = Some(child);
            }
        }
        let Some(name_node) = name_node else {
            return;
        };
        let explicit_type = type_node.and_then(|ty| parse_type_expr(ty, self.source, 0));
        let inferred_value = initializer.and_then(|value| self.value_expr(value));
        self.push_typed_binding(name_node, explicit_type, inferred_value, conditional);
    }

    fn push_typed_binding(
        &mut self,
        name_node: Syntax<'tree>,
        explicit_type: Option<TypeExpr>,
        inferred_value: Option<ValueExpr>,
        conditional: bool,
    ) {
        let Some(name) = identifier_name(name_node, self.source) else {
            return;
        };
        if let Some(value) = explicit_type
            .map(|type_expr| ValueExpr::Annotated { type_expr })
            .or(inferred_value)
        {
            self.core
                .push_binding(name.to_owned(), position(name_node), value, conditional);
        }
    }

    fn binding_pair(&self, node: Syntax<'tree>) -> Option<(Syntax<'tree>, Syntax<'tree>)> {
        let target = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.child_by_field_name("declarator"))?;
        let value = node
            .child_by_field_name("right")
            .or_else(|| node.child_by_field_name("value"))
            .or_else(|| node.child_by_field_name("initializer"))?;
        let target = unwrap_single_expression_list(unwrap_declarator(target));
        let value = unwrap_single_expression_list(value);
        Some((target, value))
    }

    fn receiver_field_target(&self, node: Syntax<'tree>) -> Option<Syntax<'tree>> {
        let receiver = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("receiver"))
            .or_else(|| node.child_by_field_name("value"))?;
        if !matches!(text(receiver, self.source).trim(), "this" | "self") {
            return None;
        }
        node.child_by_field_name("field")
            .or_else(|| node.child_by_field_name("property"))
            .or_else(|| node.child_by_field_name("attribute"))
    }

    fn value_expr(&self, node: Syntax<'tree>) -> Option<ValueExpr> {
        let classified = self.compiled.classify(node.kind_id());
        if classified.is_some_and(CompiledSyntaxKind::is_constructor) {
            let callee_node = node
                .child_by_field_name("type")
                .or_else(|| node.child_by_field_name("constructor"))
                .or_else(|| node.child_by_field_name("function"))
                .or_else(|| node.named_child(0))?;
            let callee = clean_type_name(text(callee_node, self.source))?;
            return Some(ValueExpr::Construct { callee });
        }
        if classified.is_some_and(CompiledSyntaxKind::is_member_call) {
            let member_target = node
                .child_by_field_name("function")
                .or_else(|| {
                    (node.kind() == "call_expression")
                        .then(|| node.named_child(0))
                        .flatten()
                        .filter(|c| c.kind() == "navigation_expression")
                })
                .unwrap_or(node);
            if member_target.kind() == "navigation_expression" {
                let receiver = member_target.named_child(0)?;
                let member = member_target
                    .named_child(1)
                    .filter(|s| s.kind() == "navigation_suffix")
                    .and_then(|s| s.named_child(0))
                    .or_else(|| member_target.named_child(1))?;
                return self.member_call_expr(node, receiver, text(member, self.source).trim());
            }
            if let (Some(receiver), Some(member)) = (
                member_target
                    .child_by_field_name("object")
                    .or_else(|| member_target.child_by_field_name("receiver"))
                    .or_else(|| member_target.child_by_field_name("operand"))
                    .or_else(|| member_target.child_by_field_name("expression"))
                    .or_else(|| member_target.child_by_field_name("argument"))
                    .or_else(|| member_target.child_by_field_name("value")),
                member_target
                    .child_by_field_name("name")
                    .or_else(|| member_target.child_by_field_name("field"))
                    .or_else(|| member_target.child_by_field_name("property"))
                    .or_else(|| member_target.child_by_field_name("method")),
            ) {
                let member_name = text(member, self.source).trim();
                if receiver.kind() == "constant" && member_name == "new" {
                    if let Some(callee) = clean_type_name(text(receiver, self.source)) {
                        return Some(ValueExpr::Construct { callee });
                    }
                }
                return self.member_call_expr(node, receiver, member_name);
            }
            if let Some(callee) = node
                .child_by_field_name("function")
                .or_else(|| node.named_child(0))
                .and_then(|n| identifier_name(n, self.source))
            {
                return Some(ValueExpr::Call {
                    callee: callee.to_owned(),
                    site: None,
                });
            }
            return None;
        }
        if let Some(name) = identifier_name(node, self.source) {
            return Some(ValueExpr::Alias {
                name: name.to_owned(),
            });
        }
        None
    }

    fn member_call_expr(
        &self,
        call_node: Syntax<'tree>,
        receiver: Syntax<'tree>,
        member_name: &str,
    ) -> Option<ValueExpr> {
        let inner = self.value_expr(receiver)?;
        Some(ValueExpr::Call {
            callee: member_name.to_owned(),
            site: Some(CallSiteEvidence {
                position: position(call_node),
                receiver: Some(Box::new(inner)),
                arguments: Vec::new(),
            }),
        })
    }
}

fn find_variable_declarator_child<'tree>(
    node: Syntax<'tree>,
    mut pred: impl FnMut(Syntax<'tree>) -> bool,
) -> Option<Syntax<'tree>> {
    if node.kind() != "variable_declarator" {
        return None;
    }
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|child| pred(*child));
    found
}

fn position(node: Syntax<'_>) -> SourcePosition {
    crate::languages::source_start(node)
}

fn binding_target(node: Syntax<'_>) -> Option<Syntax<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        .or_else(|| node.child_by_field_name("left"))
        .or_else(|| node.child_by_field_name("declarator"))
        .or_else(|| {
            find_variable_declarator_child(node, |child| {
                matches!(
                    child.kind(),
                    "identifier" | "simple_identifier" | "variable_name"
                )
            })
        })
}

fn unwrap_declarator(mut node: Syntax<'_>) -> Syntax<'_> {
    while matches!(
        node.kind(),
        "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "function_declarator"
    ) {
        let Some(inner) = node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
        else {
            break;
        };
        node = inner;
    }
    node
}

fn unwrap_single_expression_list(node: Syntax<'_>) -> Syntax<'_> {
    if node.kind() == "expression_list" && node.named_child_count() == 1 {
        return node.named_child(0).unwrap_or(node);
    }
    node
}

fn identifier_name<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    if matches!(
        node.kind(),
        "identifier" | "simple_identifier" | "variable_name" | "field_identifier"
    ) {
        let name = text(node, source).trim();
        return (!name.is_empty()).then_some(name);
    }
    None
}

fn clean_type_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches(['*', '&']).trim();
    if trimmed.is_empty()
        || trimmed.chars().any(char::is_whitespace)
        || !trimmed
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | ':' | '\\' | '$'))
    {
        return None;
    }
    let normalized = trimmed.replace("::", ".").replace('\\', ".");
    (!normalized.is_empty()).then_some(normalized)
}

fn parse_type_expr(node: Syntax<'_>, source: &str, depth: usize) -> Option<TypeExpr> {
    if depth >= MAX_VALUE_FLOW_TYPE_DEPTH {
        return None;
    }
    let mut cursor = node.walk();
    if matches!(
        node.kind(),
        "struct_specifier" | "class_specifier" | "enum_specifier" | "union_specifier"
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            return parse_type_expr(name, source, depth + 1);
        }
    }
    if node.kind().ends_with("pointer_type")
        || node.kind().ends_with("reference_type")
        || node.kind() == "type_annotation"
        || node.kind() == "nullable_type"
    {
        for child in node.named_children(&mut cursor) {
            if let Some(ty) = parse_type_expr(child, source, depth + 1) {
                return Some(ty);
            }
        }
    }
    if let (Some(base), Some(args_node)) = (
        node.child_by_field_name("type")
            .or_else(|| node.child_by_field_name("name")),
        node.child_by_field_name("type_arguments")
            .or_else(|| node.child_by_field_name("arguments")),
    ) {
        let base = clean_type_name(text(base, source))?;
        let mut arg_cursor = args_node.walk();
        let mut args = Vec::new();
        for arg in args_node.named_children(&mut arg_cursor) {
            if args.len() >= 4 {
                return None;
            }
            if let Some(parsed) = parse_type_expr(arg, source, depth + 1) {
                args.push(parsed);
            }
        }
        if !args.is_empty() {
            return Some(TypeExpr::Applied { base, args });
        }
        return Some(TypeExpr::Named { name: base });
    }
    clean_type_name(text(node, source)).map(|name| TypeExpr::Named { name })
}

#[cfg(test)]
mod tests {
    use super::{GenericScopeAnalysis, ScopeFrameFlags, ScopeState, ValueFlowCollectorCore};
    use crate::semantic::{CallSiteEvidence, SourcePosition, TypeExpr, ValueExpr};

    #[test]
    fn scope_state_and_collector_core_sort_and_track_conditionality() {
        let mut core = ValueFlowCollectorCore::with_scope_capacity(4);
        assert!(core.scope_state().is_empty());
        assert_eq!(core.scope_state().len(), 0);
        assert!(!core.scope_state().is_conditional());

        let root_flags = ScopeFrameFlags::new()
            .with_active(true)
            .with_collect_facts(true)
            .with_lexical_scope(true)
            .with_annotation_owner(true);
        assert!(root_flags.is_active());
        assert!(root_flags.collects_facts());
        assert!(root_flags.is_lexical_scope());
        assert!(root_flags.is_annotation_owner());
        assert!(!root_flags.is_suspended());
        assert!(!root_flags.is_conditional());
        core.scope_state_mut().push(root_flags);
        assert!(!core.scope_state().is_empty());
        assert_eq!(core.scope_state().len(), 1);

        let branch_flags = ScopeFrameFlags::new()
            .with_active(true)
            .with_branch_conditional(true)
            .with_collect_facts(true);
        core.scope_state_mut().push(branch_flags);
        assert!(core.scope_state().is_conditional());

        let try_flags = ScopeFrameFlags::new()
            .with_active(true)
            .with_try_conditional(true)
            .with_suspended(true);
        assert!(try_flags.is_try_conditional());
        assert!(try_flags.is_conditional());
        assert!(try_flags.is_suspended());

        core.push_binding(
            "later".into(),
            SourcePosition { line: 4, column: 2 },
            ValueExpr::Alias { name: "a".into() },
            core.scope_state().is_conditional(),
        );
        core.push_binding(
            "earlier".into(),
            SourcePosition { line: 2, column: 2 },
            ValueExpr::Construct { callee: "Svc".into() },
            false,
        );
        core.push_branch_join(
            "joined".into(),
            SourcePosition { line: 4, column: 6 },
            vec![ValueExpr::Alias { name: "a".into() }],
            true,
        );
        core.push_field(
            "client".into(),
            SourcePosition { line: 3, column: 2 },
            ValueExpr::Construct { callee: "Client".into() },
            false,
        );
        assert_eq!(core.facts().bindings.len(), 3);
        core.facts_mut().bindings.reserve(1);
        core.set_return_type_if_none(Some(TypeExpr::Named { name: "Svc".into() }));
        core.set_alias_type_if_none(Some(TypeExpr::Named { name: "Alias".into() }));
        core.set_return_position(SourcePosition { line: 5, column: 2 });
        core.scope_state_mut().pop();
        assert!(!core.scope_state().is_conditional());

        let facts = core.finish();
        assert_eq!(facts.bindings[0].name, "earlier");
        assert_eq!(facts.bindings[1].name, "later");
        assert!(facts.bindings[1].conditional);
        assert_eq!(facts.bindings[2].name, "joined");
        assert_eq!(facts.fields.len(), 1);
        assert_eq!(
            facts.return_type,
            Some(TypeExpr::Named { name: "Svc".into() })
        );
        assert_eq!(
            facts.alias_type,
            Some(TypeExpr::Named { name: "Alias".into() })
        );
        assert_eq!(facts.return_position, Some(SourcePosition { line: 5, column: 2 }));

        let mut stack = ScopeState::with_capacity(2);
        stack.push(branch_flags);
        assert_eq!(stack.pop(), Some(branch_flags));
        assert!(stack.current().is_none());
    }

    #[cfg(feature = "lang-typescript")]
    #[test]
    fn generic_scope_analysis_extracts_bindings_from_compiled_selectors() {
        use crate::languages::{DeclarationRole, SyntaxGrammarSpec};
        use std::sync::Arc;

        let spec = SyntaxGrammarSpec {
            declaration_roles: &[("function_declaration", DeclarationRole::Function)],
            typed_bindings: &["required_parameter", "variable_declarator"],
            assignments: &["assignment_expression"],
            constructors: &["new_expression"],
            member_calls: &["call_expression"],
            conditionals: &["if_statement"],
            ..SyntaxGrammarSpec::DEFAULT
        };
        let grammar = tree_sitter_typescript::language_typescript();
        let compiled = Arc::new(spec.compile(&grammar).expect("spec compiles"));
        let source = "\
function build(svc: Service, other: Service, flag: boolean): Service {
  let built = new Service();
  let opened = svc.open();
  this.client = svc;
  if (svc == other) {
    let sum = a + b;
    const alias = built;
  }
  return svc;
}
";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let func = tree.root_node().named_child(0).unwrap();

        let mut analysis = GenericScopeAnalysis::new(func, source, compiled);
        let mut cursor = func.walk();
        loop {
            analysis.enter(cursor.node());
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                analysis.exit(cursor.node());
                if cursor.node().id() == func.id() || cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node().id() == func.id() {
                break;
            }
        }
        let facts = analysis.finish();
        assert_eq!(
            facts
                .bindings
                .iter()
                .map(|b| (b.name.as_str(), b.conditional))
                .collect::<Vec<_>>(),
            [
                ("svc", false),
                ("other", false),
                ("flag", false),
                ("built", false),
                ("opened", false),
                ("alias", true),
            ],
            "non-assignment comparisons/arithmetic (svc == other, a + b) must emit zero bindings"
        );
        assert_eq!(
            facts.bindings.iter().find(|b| b.name == "opened").map(|b| &b.value),
            Some(&ValueExpr::Call {
                callee: "open".into(),
                site: Some(CallSiteEvidence {
                    position: SourcePosition { line: 3, column: 15 },
                    receiver: Some(Box::new(ValueExpr::Alias { name: "svc".into() })),
                    arguments: Vec::new(),
                }),
            })
        );
        assert_eq!(
            facts.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["client"]
        );
        assert!(facts.return_type.is_some());
        assert!(facts.return_position.is_some());
    }
}
