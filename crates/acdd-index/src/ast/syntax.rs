use std::collections::HashSet;
pub(crate) use tree_sitter::Node as Syntax;

/// Slices source text covered by `node`.
pub fn text<'a>(node: Syntax<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// Extracts child field text by field name.
pub fn field<'a>(node: Syntax<'_>, source: &'a str, name: &str) -> Option<&'a str> {
    node.child_by_field_name(name).map(|n| text(n, source))
}

pub(crate) fn strip_outer_parentheses(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        let mut depth = 0_usize;
        let mut closed_at_end = false;
        for (index, character) in trimmed.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && index == trimmed.len() - 1 {
                        closed_at_end = true;
                    }
                }
                _ => {}
            }
        }
        if closed_at_end {
            trimmed[1..trimmed.len() - 1].trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    }
}

pub(crate) fn qualified(module: &str, scopes: &[String], name: &str) -> String {
    std::iter::once(module)
        .chain(scopes.iter().map(String::as_str))
        .chain(std::iter::once(name))
        .collect::<Vec<_>>()
        .join(".")
}

/// Extracts doc comments preceding the syntax node.
pub fn doc_comment(node: Syntax<'_>, source: &str) -> String {
    let mut anchor = node;
    while let Some(parent) = anchor.parent() {
        if matches!(
            parent.kind(),
            "export_statement"
                | "variable_declarator"
                | "lexical_declaration"
                | "variable_declaration"
                | "decorated_definition"
        ) {
            anchor = parent;
        } else {
            break;
        }
    }
    let mut comments = Vec::new();
    let mut previous = anchor.prev_named_sibling();
    while let Some(n) = previous {
        if !n.kind().contains("comment") {
            break;
        }
        comments.push(text(n, source));
        previous = n.prev_named_sibling();
    }
    comments.reverse();
    comments.join("\n")
}

/// Produces a length-bounded string expression representation.
pub fn bounded_expression(value: &str) -> String {
    if value.len() <= 512 {
        return value.into();
    }
    format!(
        "{}… [sha256:{}]",
        value.chars().take(256).collect::<String>(),
        crate::commitments::hash(value.as_bytes())
    )
}

/// Names visible at a scope boundary and names rebound within that scope.
#[derive(Debug)]
pub struct ScopeBindings {
    /// All bindings visible after processing the scope.
    pub all: Vec<String>,
    /// Names assigned or otherwise rebound in this scope.
    pub rebindings: Vec<String>,
}

/// Trait implemented by language-specific incremental scope visitors.
pub trait ScopeVisitor<'source, 'tree> {
    fn enter(&mut self, node: Syntax<'tree>);
    fn enter_with_kind(&mut self, node: Syntax<'tree>, _kind: &str) {
        self.enter(node);
    }
    fn exit(&mut self, node: Syntax<'tree>);
    fn observe_module_event(&mut self, _node: Syntax<'tree>, _entering: bool) {}
    fn module_exports(
        &self,
        _bindings: &ScopeBindings,
    ) -> Option<(Vec<crate::semantic::ExportBinding>, serde_json::Value)> {
        None
    }
    fn enter_suspended(&mut self, _node: Syntax<'tree>) {}
    fn exit_suspended(&mut self, _node: Syntax<'tree>) {}
    fn finish(
        self: Box<Self>,
        collect_module_aliases: bool,
    ) -> (crate::semantic::ValueFlowFacts, Option<HashSet<String>>);
}

#[cfg(any(
    feature = "lang-python",
    feature = "lang-rust",
    feature = "lang-typescript"
))]
pub(crate) trait SyntaxCollector<'tree> {
    type Output;
    fn enter(&mut self, node: Syntax<'tree>);
    fn exit(&mut self, node: Syntax<'tree>);
    fn finish(self) -> Self::Output;
}

#[cfg(any(
    feature = "lang-python",
    feature = "lang-rust",
    feature = "lang-typescript"
))]
pub(crate) fn collect<'tree, C: SyntaxCollector<'tree>>(
    root: Syntax<'tree>,
    mut collector: C,
) -> C::Output {
    walk(root, &mut |node, entering| {
        if entering {
            collector.enter(node);
        } else {
            collector.exit(node);
        }
    });
    collector.finish()
}

/// Delivers balanced enter/exit events in source order for standalone profile hooks.
pub(crate) fn walk<'tree>(root: Syntax<'tree>, visitor: &mut impl FnMut(Syntax<'tree>, bool)) {
    walk_in_order(root, false, visitor);
}

#[cfg(feature = "lang-vue")]
pub(crate) fn walk_reversed<'tree>(
    root: Syntax<'tree>,
    visitor: &mut impl FnMut(Syntax<'tree>, bool),
) {
    walk_in_order(root, true, visitor);
}

pub(crate) fn walk_in_order<'tree>(
    root: Syntax<'tree>,
    reversed: bool,
    visitor: &mut impl FnMut(Syntax<'tree>, bool),
) {
    walk_in_order_impl(root, reversed, false, &mut |node, _ancestors, entering| {
        visitor(node, entering);
    });
}

pub(crate) fn walk_in_order_with_ancestors<'tree>(
    root: Syntax<'tree>,
    reversed: bool,
    visitor: &mut impl FnMut(Syntax<'tree>, &[Syntax<'tree>], bool),
) {
    walk_in_order_impl(root, reversed, true, visitor);
}

fn walk_in_order_impl<'tree>(
    root: Syntax<'tree>,
    reversed: bool,
    track_ancestors: bool,
    visitor: &mut impl FnMut(Syntax<'tree>, &[Syntax<'tree>], bool),
) {
    fn first_named_child(cursor: &mut tree_sitter::TreeCursor<'_>, reversed: bool) -> bool {
        if !cursor.goto_first_child() {
            return false;
        }
        if reversed {
            while cursor.goto_next_sibling() {}
            loop {
                if cursor.node().is_named() {
                    return true;
                }
                if !cursor.goto_previous_sibling() {
                    cursor.goto_parent();
                    return false;
                }
            }
        }
        loop {
            if cursor.node().is_named() {
                return true;
            }
            if !cursor.goto_next_sibling() {
                cursor.goto_parent();
                return false;
            }
        }
    }

    fn next_named_sibling(cursor: &mut tree_sitter::TreeCursor<'_>, reversed: bool) -> bool {
        let mut moved = if reversed {
            cursor.goto_previous_sibling()
        } else {
            cursor.goto_next_sibling()
        };
        while moved && !cursor.node().is_named() {
            moved = if reversed {
                cursor.goto_previous_sibling()
            } else {
                cursor.goto_next_sibling()
            };
        }
        moved
    }

    let mut ancestors: Vec<Syntax<'tree>> = Vec::new();
    if track_ancestors {
        if let Some(parent) = root.parent() {
            ancestors.push(parent);
        }
    }
    let mut cursor = root.walk();
    visitor(root, &ancestors, true);
    if track_ancestors {
        ancestors.push(root);
    }
    if !first_named_child(&mut cursor, reversed) {
        if track_ancestors {
            ancestors.pop();
        }
        visitor(root, &ancestors, false);
        return;
    }

    loop {
        let node = cursor.node();
        visitor(node, &ancestors, true);
        if first_named_child(&mut cursor, reversed) {
            if track_ancestors {
                ancestors.push(node);
            }
            continue;
        }

        visitor(node, &ancestors, false);
        loop {
            if next_named_sibling(&mut cursor, reversed) {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            if track_ancestors {
                ancestors.pop();
            }
            let parent = cursor.node();
            visitor(parent, &ancestors, false);
            if parent.id() == root.id() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BitStack {
    words: [u64; 4],
    depth: u16,
}

impl BitStack {
    #[inline(always)]
    pub(crate) fn new() -> Self {
        Self {
            words: [0; 4],
            depth: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, bit: bool) {
        let d = self.depth as usize;
        if d < 256 {
            let idx = d / 64;
            let offset = d % 64;
            if bit {
                self.words[idx] |= 1 << offset;
            } else {
                self.words[idx] &= !(1 << offset);
            }
        }
        self.depth += 1;
    }

    #[inline(always)]
    pub(crate) fn pop(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
        }
    }

    #[inline(always)]
    pub(crate) fn last(&self) -> Option<bool> {
        if self.depth == 0 {
            None
        } else {
            let d = (self.depth - 1) as usize;
            if d < 256 {
                let idx = d / 64;
                let offset = d % 64;
                Some((self.words[idx] & (1 << offset)) != 0)
            } else {
                Some(false)
            }
        }
    }
}

#[inline]
pub(crate) fn advance_cursor(cursor: &mut tree_sitter::TreeCursor<'_>, descend: bool) -> bool {
    if descend && cursor.goto_first_child() {
        return true;
    }
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
    }
}

#[cfg(feature = "lang-rust")]
pub(crate) fn rust_pattern_bindings<'tree>(
    pattern: Syntax<'tree>,
    mut emit: impl FnMut(Syntax<'tree>),
) {
    if matches!(pattern.kind(), "identifier" | "shorthand_field_identifier") {
        emit(pattern);
        return;
    }
    let mut cursor = pattern.walk();
    loop {
        let node = cursor.node();
        let descend = if cursor.field_name() == Some("type") {
            false
        } else {
            match node.kind() {
                "identifier" | "shorthand_field_identifier" => {
                    emit(node);
                    false
                }
                "tuple_struct_pattern"
                | "struct_pattern"
                | "field_pattern"
                | "tuple_pattern"
                | "slice_pattern"
                | "reference_pattern"
                | "ref_pattern"
                | "mut_pattern"
                | "captured_pattern"
                | "or_pattern" => true,
                _ => false,
            }
        };
        if !advance_cursor(&mut cursor, descend) {
            return;
        }
    }
}

pub(crate) struct BindingCollector<'source, 'tree> {
    source: &'source str,
    body: Option<Syntax<'tree>>,
    states: BitStack,
    params: HashSet<&'source str>,
    names: HashSet<&'source str>,
}

fn binding_names<'a>(node: Syntax<'_>, source: &'a str, out: &mut HashSet<&'a str>) {
    match node.kind() {
        #[cfg(feature = "lang-rust")]
        "tuple_struct_pattern" | "struct_pattern" | "ref_pattern" | "captured_pattern" => {
            rust_pattern_bindings(node, |binding| {
                out.insert(text(binding, source));
            });
        }
        "identifier"
        | "shorthand_property_identifier_pattern"
        | "simple_identifier"
        | "variable_name" => {
            out.insert(text(node, source));
        }
        "default_parameter"
        | "typed_default_parameter"
        | "required_parameter"
        | "optional_parameter"
        | "parameter"
        | "formal_parameter"
        | "spread_parameter"
        | "catch_formal_parameter"
        | "class_parameter"
        | "simple_parameter"
        | "variadic_parameter"
        | "property_promotion_parameter"
        | "keyword_parameter"
        | "splat_parameter"
        | "hash_splat_parameter"
        | "block_parameter"
        | "parameter_declaration"
        | "optional_parameter_declaration"
        | "variadic_parameter_declaration" => {
            let mut found = false;
            for field in ["name", "pattern", "declarator"] {
                let mut c = node.walk();
                for child in node.children_by_field_name(field, &mut c) {
                    found = true;
                    binding_names(child, source, out);
                }
            }
            if !found {
                let mut c = node.walk();
                for child in node.named_children(&mut c) {
                    if matches!(
                        child.kind(),
                        "identifier" | "simple_identifier" | "variable_name"
                    ) {
                        out.insert(text(child, source));
                        break;
                    }
                }
            }
        }
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "function_declarator"
        | "init_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                binding_names(inner, source, out);
            }
        }
        "parenthesized_declarator" => {
            if let Some(inner) = node.named_child(0) {
                binding_names(inner, source, out);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(n) = node.child_by_field_name("left") {
                binding_names(n, source, out);
            }
        }
        "pair_pattern" => {
            if let Some(n) = node.child_by_field_name("value") {
                binding_names(n, source, out);
            }
        }
        "typed_parameter" => {
            if let Some(n) = node.named_child(0) {
                if Some(n) != node.child_by_field_name("type") {
                    binding_names(n, source, out);
                }
            }
        }
        "parameters"
        | "method_parameters"
        | "block_parameters"
        | "function_value_parameters"
        | "lambda_parameters"
        | "as_pattern_target"
        | "formal_parameters"
        | "parameter_list"
        | "tuple_pattern"
        | "list_pattern"
        | "pattern_list"
        | "expression_list"
        | "array_pattern"
        | "object_pattern"
        | "rest_pattern"
        | "list_splat_pattern"
        | "dictionary_splat_pattern"
        | "slice_pattern"
        | "reference_pattern"
        | "mut_pattern" => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                binding_names(child, source, out);
            }
        }
        _ => {}
    }
}

impl<'source, 'tree> BindingCollector<'source, 'tree> {
    pub(crate) fn new(node: Syntax<'tree>, source: &'source str) -> Self {
        Self::new_with_kind(node, source, node.kind())
    }

    pub(crate) fn new_with_kind(node: Syntax<'tree>, source: &'source str, kind: &str) -> Self {
        let is_callable = callable_scope_kind(kind);
        let is_module = matches!(
            kind,
            "module" | "program" | "source_file" | "class_definition" | "document" | "stylesheet"
        );
        if !is_callable && !is_module {
            return Self {
                source,
                body: None,
                states: BitStack::new(),
                params: Default::default(),
                names: Default::default(),
            };
        }
        let mut param_names = HashSet::new();
        if is_callable {
            for field in ["parameters", "parameter"] {
                if let Some(parameters) = node.child_by_field_name(field) {
                    binding_names(parameters, source, &mut param_names);
                }
            }
            if let Some(mut decl) = node.child_by_field_name("declarator") {
                loop {
                    if let Some(parameters) = decl.child_by_field_name("parameters") {
                        binding_names(parameters, source, &mut param_names);
                        break;
                    }
                    let Some(inner) = decl.child_by_field_name("declarator") else {
                        break;
                    };
                    decl = inner;
                }
            }
            if kind == "function_declaration" {
                let mut c = node.walk();
                for child in node
                    .named_children(&mut c)
                    .filter(|n| n.kind() == "function_value_parameters")
                {
                    binding_names(child, source, &mut param_names);
                }
            }
        }
        let body = node
            .child_by_field_name("body")
            .or_else(|| is_module.then_some(node));
        Self {
            source,
            body,
            states: BitStack::new(),
            params: param_names,
            names: Default::default(),
        }
    }

    pub(crate) fn enter(&mut self, n: Syntax<'tree>) {
        self.enter_with_kind(n, n.kind());
    }

    pub(crate) fn enter_with_kind(&mut self, n: Syntax<'tree>, kind: &str) {
        let source = self.source;
        let active = self.body == Some(n)
            || (self.states.last().unwrap_or(false) && !binding_scope_kind(kind));
        self.states.push(active);
        if !active {
            return;
        }
        let lhs = match kind {
            "assignment"
            | "augmented_assignment"
            | "assignment_expression"
            | "augmented_assignment_expression"
            | "assignment_statement"
            | "for_statement"
            | "for_in_statement"
            | "for_in_clause"
            | "short_var_declaration" => n.child_by_field_name("left"),
            "as_pattern" => n.child_by_field_name("alias"),
            "variable_declarator" | "let_declaration" | "named_expression" | "init_declarator" => n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("pattern"))
                .or_else(|| n.child_by_field_name("declarator"))
                .or_else(|| {
                    if kind != "variable_declarator" {
                        return None;
                    }
                    let mut c = n.walk();
                    let found = n.named_children(&mut c).find(|child| {
                        matches!(
                            child.kind(),
                            "identifier" | "simple_identifier" | "variable_name"
                        )
                    });
                    found
                }),
            "variable_declaration" => {
                let mut c = n.walk();
                let found = n
                    .named_children(&mut c)
                    .find(|child| matches!(child.kind(), "simple_identifier" | "identifier"));
                found
            }
            _ => None,
        };
        if let Some(lhs) = lhs {
            let callable = n
                .child_by_field_name("value")
                .or_else(|| n.child_by_field_name("right"))
                .is_some_and(|n| {
                    matches!(
                        n.kind(),
                        "arrow_function" | "function_expression" | "lambda"
                    )
                });
            if !callable {
                binding_names(lhs, source, &mut self.names);
            }
        }
        if kind == "delete_statement" {
            let mut c = n.walk();
            for child in n.named_children(&mut c) {
                binding_names(child, source, &mut self.names);
            }
        }
    }

    pub(crate) fn exit(&mut self) {
        self.states.pop();
    }

    pub(crate) fn finish(self) -> ScopeBindings {
        let mut rebindings: Vec<String> = self.names.into_iter().map(str::to_owned).collect();
        rebindings.sort_unstable();
        let mut all: Vec<String> = self
            .params
            .into_iter()
            .map(str::to_owned)
            .chain(rebindings.iter().cloned())
            .collect();
        all.sort_unstable();
        all.dedup();
        ScopeBindings { all, rebindings }
    }
}

pub(crate) fn binding_scope(n: Syntax<'_>) -> bool {
    binding_scope_kind(n.kind())
}

fn callable_scope_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "lambda"
            | "function_declaration"
            | "function_item"
            | "function_signature_item"
            | "arrow_function"
            | "function_expression"
            | "method_definition"
            | "method_declaration"
            | "method"
            | "singleton_method"
            | "constructor_declaration"
            | "local_function_statement"
            | "closure_expression"
            | "func_literal"
    )
}

pub(crate) fn binding_scope_kind(kind: &str) -> bool {
    kind == "class_definition" || callable_scope_kind(kind)
}

pub(crate) fn bindings(node: Syntax<'_>, source: &str) -> ScopeBindings {
    let mut collector = BindingCollector::new(node, source);
    walk(node, &mut |node, entering| {
        if entering {
            collector.enter(node);
        } else {
            collector.exit();
        }
    });
    collector.finish()
}

/// Extracts symbol name from a syntax node.
pub fn symbol_name<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    let inferred = node.parent().and_then(|p| {
        if matches!(p.kind(), "variable_declarator" | "pair" | "assignment") {
            field(p, source, "name")
                .or_else(|| field(p, source, "left"))
                .or_else(|| field(p, source, "key"))
        } else {
            None
        }
    });
    field(node, source, "name")
        .or_else(|| field(node, source, "type"))
        .or(inferred)
}

/// Extracts scope bindings for a syntax node.
pub fn scope_bindings(node: Syntax<'_>, source: &str) -> ScopeBindings {
    bindings(node, source)
}

pub(crate) fn parse_island(
    parser: &mut tree_sitter::Parser,
    input: &str,
    offset: usize,
    position: tree_sitter::Point,
) -> Option<tree_sitter::Tree> {
    let mut padded = String::with_capacity(input.len() + 1);
    padded.push('\n');
    padded.push_str(input);
    let mut tree = parser.parse(&padded, None)?;
    // A virtual prefix keeps parent traversal in the same coordinate system.
    tree.edit(&tree_sitter::InputEdit {
        start_byte: 0,
        old_end_byte: 1,
        new_end_byte: offset,
        start_position: tree_sitter::Point::new(0, 0),
        old_end_position: tree_sitter::Point::new(1, 0),
        new_end_position: position,
    });
    Some(tree)
}
