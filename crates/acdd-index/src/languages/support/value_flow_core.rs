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
        let Some(name_node) = binding_target(node) else {
            return;
        };
        let Some(name) = identifier_name(name_node, self.source) else {
            return;
        };
        let explicit_type = node
            .child_by_field_name("type")
            .and_then(|ty| parse_type_expr(ty, self.source, 0));
        let inferred_value = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("initializer"))
            .or_else(|| node.child_by_field_name("right"))
            .and_then(|value| self.value_expr(value));
        let value = explicit_type
            .map(|type_expr| ValueExpr::Annotated { type_expr })
            .or(inferred_value);
        if let Some(value) = value {
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
            let member_target = node.child_by_field_name("function").unwrap_or(node);
            let receiver = member_target
                .child_by_field_name("object")
                .or_else(|| member_target.child_by_field_name("receiver"))
                .or_else(|| member_target.child_by_field_name("operand"))
                .or_else(|| member_target.child_by_field_name("value"))?;
            let member = member_target
                .child_by_field_name("name")
                .or_else(|| member_target.child_by_field_name("field"))
                .or_else(|| member_target.child_by_field_name("property"))?;
            let inner = self.value_expr(receiver)?;
            return Some(ValueExpr::Call {
                callee: text(member, self.source).trim().to_owned(),
                site: Some(CallSiteEvidence {
                    position: position(node),
                    receiver: Some(Box::new(inner)),
                    arguments: Vec::new(),
                }),
            });
        }
        if let Some(name) = identifier_name(node, self.source) {
            return Some(ValueExpr::Alias {
                name: name.to_owned(),
            });
        }
        None
    }
}

fn position(node: Syntax<'_>) -> SourcePosition {
    crate::languages::source_start(node)
}

fn binding_target(node: Syntax<'_>) -> Option<Syntax<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        .or_else(|| node.child_by_field_name("left"))
        .or_else(|| node.child_by_field_name("declarator"))
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
