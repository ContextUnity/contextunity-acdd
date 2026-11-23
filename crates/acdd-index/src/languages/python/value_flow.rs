use crate::ast::syntax::{
    collect, field, strip_outer_parentheses, text, BitStack, ScopeBindings, ScopeVisitor,
    SyntaxCollector,
};
use crate::languages::support::value_flow_core::{ScopeFrameFlags, ValueFlowCollectorCore};
use crate::semantic::{
    AnnotationReferenceFact, CollectionElementFact, GuardImportFact, SourcePosition, TypeExpr,
    TypeOnlyImportFact, ValueExpr, ValueFlowFacts,
};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

pub(crate) struct AliasWrites {
    writes: HashMap<String, (usize, bool)>,
    active: BitStack,
}
impl AliasWrites {
    pub(crate) fn new() -> Self {
        Self {
            writes: Default::default(),
            active: BitStack::new(),
        }
    }
    fn targets(
        node: Node<'_>,
        source: &str,
        alias: bool,
        writes: &mut std::collections::HashMap<String, (usize, bool)>,
    ) {
        if node.kind() == "identifier" {
            let name = text(node, source);
            if let Some(entry) = writes.get_mut(name) {
                entry.0 += 1;
                entry.1 &= alias;
            } else {
                writes.insert(name.to_owned(), (1, alias));
            }
        } else if matches!(
            node.kind(),
            "tuple_pattern"
                | "list_pattern"
                | "pattern_list"
                | "list_splat_pattern"
                | "dictionary_splat_pattern"
                | "expression_list"
        ) {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                Self::targets(child, source, false, writes);
            }
        }
    }
    pub(crate) fn enter(&mut self, node: Node<'_>, source: &str) {
        let active = self.active.last().unwrap_or(true)
            && !matches!(
                node.kind(),
                "function_definition" | "class_definition" | "lambda"
            );
        self.active.push(active);
        if !active {
            return;
        }
        let target = match node.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                node.child_by_field_name("left")
            }
            "named_expression" => node.child_by_field_name("name"),
            "as_pattern" => node.child_by_field_name("alias"),
            _ => None,
        };
        if let Some(target) = target {
            Self::targets(
                target,
                source,
                super::common::type_alias(node, source),
                &mut self.writes,
            );
        }
        if node.kind() == "delete_statement" {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                Self::targets(child, source, false, &mut self.writes);
            }
        }
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            let mut cursor = node.walk();
            for child in node.children_by_field_name("name", &mut cursor) {
                let name = field(child, source, "alias")
                    .or_else(|| field(child, source, "name"))
                    .unwrap_or_else(|| text(child, source));
                let bound = if node.kind() == "import_statement" && !child.kind().contains("alias")
                {
                    name.split('.').next().unwrap_or(name)
                } else {
                    name
                };
                let entry = self.writes.entry(bound.to_owned()).or_insert((0, true));
                entry.0 += 1;
                entry.1 = false;
            }
        }
    }
    pub(crate) fn exit(&mut self) {
        self.active.pop();
    }
    pub(crate) fn aliases(&self) -> HashSet<String> {
        self.writes
            .iter()
            .filter(|(_, (count, alias))| *count == 1 && *alias)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

fn position(node: Node<'_>) -> SourcePosition {
    crate::languages::types::source_end(node)
}

fn annotation_reference_positions(node: Node<'_>, source: &str, facts: &mut ValueFlowFacts) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let is_reference = match current.kind() {
            "scoped_type_identifier" | "attribute" => true,
            "type_identifier" | "identifier" => {
                !matches!(
                    text(current, source),
                    "_" | "self" | "cls" | "this" | "true" | "false" | "None" | "nil" | "null"
                ) && !text(current, source).starts_with('\'')
            }
            _ => false,
        };
        if is_reference {
            let start = current.start_position();
            facts.push_annotation_reference(AnnotationReferenceFact {
                expression: text(current, source).to_owned(),
                value_query: false,
                position: SourcePosition {
                    line: start.row + 1,
                    column: start.column,
                },
            });
        } else {
            let mut cursor = current.walk();
            for child in current.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }
}

fn type_only_imports(module: Node<'_>, source: &str) -> Vec<TypeOnlyImportFact> {
    fn target_names(node: Node<'_>, source: &str, names: &mut HashSet<String>) {
        if matches!(node.kind(), "identifier" | "attribute") {
            names.insert(text(node, source).to_owned());
        } else if matches!(
            node.kind(),
            "tuple_pattern" | "list_pattern" | "pattern_list"
        ) {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                target_names(child, source, names);
            }
        }
    }

    fn import_aliases(node: Node<'_>, source: &str, mut emit: impl FnMut(&str)) {
        let mut cursor = node.walk();
        for child in node.children_by_field_name("name", &mut cursor) {
            let name = field(child, source, "name").unwrap_or_else(|| text(child, source));
            let alias = field(child, source, "alias").unwrap_or_else(|| {
                if node.kind() == "import_statement" {
                    name.split('.').next().unwrap_or(name)
                } else {
                    name
                }
            });
            emit(alias);
        }
    }

    fn written_names(node: Node<'_>, source: &str, names: &mut HashSet<String>) {
        match node.kind() {
            "function_definition" | "class_definition" => {
                if let Some(name) = field(node, source, "name") {
                    names.insert(name.to_owned());
                }
                return;
            }
            "import_from_statement" | "import_statement" => {
                import_aliases(node, source, |alias| {
                    names.insert(alias.to_owned());
                });
                return;
            }
            "assignment" | "augmented_assignment" | "for_statement" => {
                if let Some(target) = node.child_by_field_name("left") {
                    target_names(target, source, names);
                }
            }
            "named_expression" => {
                if let Some(target) = node.child_by_field_name("name") {
                    target_names(target, source, names);
                }
            }
            "as_pattern" => {
                if let Some(target) = node.child_by_field_name("alias") {
                    target_names(target, source, names);
                }
            }
            "delete_statement" => {
                let mut cursor = node.walk();
                for target in node.named_children(&mut cursor) {
                    target_names(target, source, names);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            written_names(child, source, names);
        }
    }

    fn guarded_imports(
        node: Node<'_>,
        source: &str,
        guard: Option<&GuardImportFact>,
        output: &mut Vec<TypeOnlyImportFact>,
    ) {
        match node.kind() {
            "import_from_statement" | "import_statement" => {
                import_aliases(node, source, |alias| {
                    output.push(TypeOnlyImportFact {
                        alias: alias.to_owned(),
                        position: crate::languages::types::source_start(node),
                        guard_import: guard.cloned(),
                    });
                });
            }
            "function_definition" | "class_definition" => {}
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    guarded_imports(
                        child,
                        source,
                        guard.filter(|_| node.kind() == "block"),
                        output,
                    );
                }
            }
        }
    }

    fn nested_guards(
        node: Node<'_>,
        source: &str,
        known: &HashSet<String>,
        output: &mut Vec<TypeOnlyImportFact>,
    ) {
        if node.kind() == "if_statement" {
            if let Some(condition) = field(node, source, "condition") {
                let condition = condition.trim();
                if condition == "TYPE_CHECKING"
                    || condition.ends_with(".TYPE_CHECKING")
                    || known.contains(condition)
                {
                    if let Some(consequence) = node.child_by_field_name("consequence") {
                        guarded_imports(consequence, source, None, output);
                    }
                    return;
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            nested_guards(child, source, known, output);
        }
    }

    let mut known_guards = HashSet::new();
    let mut safe_guards: HashMap<String, GuardImportFact> = HashMap::new();
    let mut output = Vec::new();
    let mut cursor = module.walk();
    for statement in module.named_children(&mut cursor) {
        if !safe_guards.is_empty() {
            let mut writes = HashSet::new();
            written_names(statement, source, &mut writes);
            safe_guards.retain(|guard, _| {
                !writes.contains(guard)
                    && !writes.contains(guard.split('.').next().unwrap_or(guard.as_str()))
            });
        }
        match statement.kind() {
            "import_from_statement"
                if matches!(
                    field(statement, source, "module_name"),
                    Some("typing" | "typing_extensions")
                ) =>
            {
                let mut names = statement.walk();
                for child in statement.children_by_field_name("name", &mut names) {
                    if field(child, source, "name").unwrap_or_else(|| text(child, source))
                        == "TYPE_CHECKING"
                    {
                        let alias = field(child, source, "alias").unwrap_or("TYPE_CHECKING");
                        known_guards.insert(alias.to_owned());
                        safe_guards.insert(
                            alias.to_owned(),
                            GuardImportFact {
                                alias: alias.to_owned(),
                                position: crate::languages::types::source_start(statement),
                            },
                        );
                    }
                }
            }
            "import_statement" => {
                let mut names = statement.walk();
                for child in statement.children_by_field_name("name", &mut names) {
                    let name = field(child, source, "name").unwrap_or_else(|| text(child, source));
                    if matches!(name, "typing" | "typing_extensions") {
                        let alias = field(child, source, "alias").unwrap_or(name);
                        let guard = format!("{alias}.TYPE_CHECKING");
                        known_guards.insert(guard.clone());
                        safe_guards.insert(
                            guard,
                            GuardImportFact {
                                alias: alias.to_owned(),
                                position: crate::languages::types::source_start(statement),
                            },
                        );
                    }
                }
            }
            "if_statement" => {
                if let Some(condition) = field(statement, source, "condition") {
                    let condition = condition.trim();
                    if condition == "TYPE_CHECKING"
                        || condition.ends_with(".TYPE_CHECKING")
                        || known_guards.contains(condition)
                    {
                        if let Some(consequence) = statement.child_by_field_name("consequence") {
                            guarded_imports(
                                consequence,
                                source,
                                safe_guards.get(condition),
                                &mut output,
                            );
                        }
                    } else {
                        nested_guards(statement, source, &known_guards, &mut output);
                    }
                }
            }
            "function_definition"
            | "class_definition"
            | "decorated_definition"
            | "try_statement"
            | "with_statement"
            | "for_statement"
            | "while_statement"
            | "match_statement" => {
                nested_guards(statement, source, &known_guards, &mut output);
            }
            _ => {}
        }
    }
    output
}

fn direct_branch_end(node: Node<'_>) -> Option<SourcePosition> {
    let statement = node.parent()?;
    let body = if statement.kind() == "expression_statement" {
        statement.parent()?
    } else {
        statement
    };
    let body = (body.kind() == "block").then_some(body)?;
    let branch = body.parent()?;
    let conditional = match branch.kind() {
        "if_statement" => branch,
        "elif_clause" | "else_clause" => branch
            .parent()
            .filter(|parent| parent.kind() == "if_statement")?,
        _ => return None,
    };
    let parent = conditional.parent()?;
    let enclosing = if parent.kind() == "block" {
        parent.parent()?
    } else {
        parent
    };
    matches!(enclosing.kind(), "function_definition" | "module").then(|| position(body))
}

fn static_name(node: Node<'_>) -> bool {
    node.kind() == "identifier"
        || (node.kind() == "attribute"
            && node.child_by_field_name("object").is_some_and(static_name)
            && node
                .child_by_field_name("attribute")
                .is_some_and(|member| member.kind() == "identifier"))
}

pub(crate) use super::common::split_python_pipe_types;

fn parse_annotation_generic(
    node: Node<'_>,
    source: &str,
    depth: usize,
    remaining: &mut usize,
    parse_fn: fn(Node<'_>, &str, usize, &mut usize) -> TypeExpr,
) -> TypeExpr {
    let base = if node.kind() == "subscript" {
        node.child_by_field_name("value")
    } else {
        node.named_child(0)
    };
    let Some(base) = base.filter(|base| static_name(*base)) else {
        return TypeExpr::Unknown;
    };
    let base = text(base, source);
    if base.len() > 256 {
        return TypeExpr::Unknown;
    }
    let mut cursor = node.walk();
    let arguments: Vec<_> = if node.kind() == "subscript" {
        let subs: Vec<_> = node
            .children_by_field_name("subscript", &mut cursor)
            .collect();
        if subs.len() == 1 && subs[0].kind() == "tuple" {
            let mut tuple_cursor = subs[0].walk();
            subs[0].named_children(&mut tuple_cursor).collect()
        } else {
            subs
        }
    } else {
        let Some(parameters) = node.named_child(1) else {
            return TypeExpr::Unknown;
        };
        parameters.named_children(&mut cursor).collect()
    };
    if arguments.is_empty() || arguments.len() > *remaining {
        return TypeExpr::Unknown;
    }
    let args = arguments
        .into_iter()
        .map(|arg| parse_fn(arg, source, depth + 1, remaining))
        .collect();
    TypeExpr::Applied {
        base: base.to_owned(),
        args,
    }
}

fn parse_annotation_pipe_union(
    node: Node<'_>,
    source: &str,
    depth: usize,
    remaining: &mut usize,
    parse_fn: fn(Node<'_>, &str, usize, &mut usize) -> TypeExpr,
) -> Option<TypeExpr> {
    let mut operands = Vec::new();
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "binary_operator"
            && curr
                .child_by_field_name("operator")
                .is_some_and(|op| text(op, source) == "|")
        {
            if let Some(right) = curr.child_by_field_name("right") {
                stack.push(right);
            }
            if let Some(left) = curr.child_by_field_name("left") {
                stack.push(left);
            }
        } else if curr.kind() == "parenthesized_expression" {
            if let Some(inner) = curr.named_child(0) {
                stack.push(inner);
            } else {
                operands.push(curr);
            }
        } else {
            operands.push(curr);
        }
    }
    if !operands.is_empty() && operands.len() <= *remaining {
        let args = operands
            .into_iter()
            .map(|arg| parse_fn(arg, source, depth + 1, remaining))
            .collect();
        Some(TypeExpr::Applied {
            base: "Union".to_owned(),
            args,
        })
    } else {
        None
    }
}

fn annotation(node: Node<'_>, source: &str) -> TypeExpr {
    fn parse(node: Node<'_>, source: &str, depth: usize, remaining: &mut usize) -> TypeExpr {
        if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH || *remaining == 0 {
            return TypeExpr::Unknown;
        }
        *remaining -= 1;
        if node.kind() == "type" {
            return node.named_child(0).map_or(TypeExpr::Unknown, |child| {
                parse(child, source, depth + 1, remaining)
            });
        }
        if matches!(node.kind(), "generic_type" | "subscript") {
            return parse_annotation_generic(node, source, depth, remaining, parse);
        }
        if node.kind() == "parenthesized_expression" {
            if let Some(inner) = node.named_child(0) {
                return parse(inner, source, depth, remaining);
            }
        }
        if node.kind() == "binary_operator"
            && node
                .child_by_field_name("operator")
                .is_some_and(|op| text(op, source) == "|")
        {
            if let Some(union_ty) =
                parse_annotation_pipe_union(node, source, depth, remaining, parse)
            {
                return union_ty;
            }
        }
        let value = text(node, source).trim();
        let value = value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .or_else(|| {
                value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
            })
            .unwrap_or(value);
        let value = strip_outer_parentheses(value);
        if let Some(parts) = split_python_pipe_types(value) {
            if parts.len() >= 2 && parts.len() <= *remaining {
                let args = parts
                    .into_iter()
                    .map(|part| TypeExpr::Named {
                        name: strip_outer_parentheses(part).to_owned(),
                    })
                    .collect();
                return TypeExpr::Applied {
                    base: "Union".to_owned(),
                    args,
                };
            }
        }
        if !value.is_empty()
            && value.len() <= 256
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part.chars().enumerate().all(|(i, c)| {
                        c == '_'
                            || if i == 0 {
                                c.is_alphabetic()
                            } else {
                                c.is_alphanumeric()
                            }
                    })
            })
        {
            TypeExpr::Named {
                name: value.to_owned(),
            }
        } else {
            TypeExpr::Unknown
        }
    }
    parse(node, source, 0, &mut 32)
}

fn literal_value(node: Node<'_>, source: &str) -> Option<ValueExpr> {
    if node.kind() == "string" {
        let bytes = node
            .named_child(0)
            .filter(|start| start.kind() == "string_start")
            .and_then(|start| text(start, source).split(['\'', '"']).next())
            .is_some_and(|prefix| prefix.bytes().any(|b| matches!(b, b'b' | b'B')));
        return Some(ValueExpr::Construct {
            callee: if bytes { "b\"\"" } else { "\"\"" }.to_owned(),
        });
    }
    let kind = if node.kind() == "dictionary" {
        "dict"
    } else {
        node.kind()
    };
    crate::semantic::python_literal_constructor(kind).map(|callee| ValueExpr::Construct {
        callee: callee.to_owned(),
    })
}

pub(super) fn valid_super_arguments(arguments: Node<'_>, source: &str) -> bool {
    let mut cursor = arguments.walk();
    let mut args = arguments.named_children(&mut cursor);
    let Some(first) = args.next() else {
        return true;
    };
    let Some(second) = args.next() else {
        return false;
    };
    args.next().is_none()
        && static_name(first)
        && second.kind() == "identifier"
        && matches!(text(second, source), "self" | "cls")
}

fn attribute_chain<'tree>(mut node: Node<'tree>, source: &str) -> Option<(Node<'tree>, String)> {
    let mut members = Vec::new();
    while node.kind() == "attribute" {
        if members.len() >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
            return None;
        }
        let member = node
            .child_by_field_name("attribute")
            .filter(|member| member.kind() == "identifier")?;
        members.push(text(member, source));
        node = node.child_by_field_name("object")?;
    }
    (!members.is_empty()).then(|| {
        members.reverse();
        (node, members.join("."))
    })
}

pub(super) fn call_value(node: Node<'_>, source: &str) -> ValueExpr {
    call_value_depth(node, source, 0)
}

pub(super) fn computed_receiver_value(node: Node<'_>, source: &str) -> Option<ValueExpr> {
    let expr = value_depth(node, source, 0);
    (!matches!(expr, ValueExpr::Unknown)).then_some(expr)
}

fn receiver_call(receiver: ValueExpr, callee: impl Into<String>, node: Node<'_>) -> ValueExpr {
    if matches!(receiver, ValueExpr::Unknown) {
        return ValueExpr::Unknown;
    }
    ValueExpr::Call {
        callee: callee.into(),
        site: Some(crate::semantic::CallSiteEvidence {
            position: crate::languages::types::source_start(node),
            receiver: Some(Box::new(receiver)),
            arguments: Vec::new(),
        }),
    }
}

fn call_value_depth(node: Node<'_>, source: &str, depth: usize) -> ValueExpr {
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        return ValueExpr::Unknown;
    }
    let Some(callee) = node.child_by_field_name("function") else {
        return ValueExpr::Unknown;
    };
    if static_name(callee) {
        let name = text(callee, source);
        if name == "super"
            && !node
                .child_by_field_name("arguments")
                .is_some_and(|args| valid_super_arguments(args, source))
        {
            return ValueExpr::Unknown;
        }
        return ValueExpr::Call {
            callee: name.to_owned(),
            site: None,
        };
    }
    if callee.kind() == "call" {
        return receiver_call(
            call_value_depth(callee, source, depth + 1),
            "__call__",
            node,
        );
    }
    if callee.kind() != "attribute" {
        return ValueExpr::Unknown;
    }
    let Some((base, member_chain)) = attribute_chain(callee, source) else {
        return ValueExpr::Unknown;
    };
    receiver_call(value_depth(base, source, depth + 1), member_chain, node)
}

fn value(node: Node<'_>, source: &str) -> ValueExpr {
    value_depth(node, source, 0)
}

fn value_depth(node: Node<'_>, source: &str, depth: usize) -> ValueExpr {
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        return ValueExpr::Unknown;
    }
    if let Some(literal) = literal_value(node, source) {
        return literal;
    }
    match node.kind() {
        "parenthesized_expression" => node
            .named_child(0)
            .map_or(ValueExpr::Unknown, |inner| value_depth(inner, source, depth + 1)),
        "identifier" => ValueExpr::Alias {
            name: text(node, source).to_owned(),
        },
        "await" => node
            .named_child(0)
            .map_or(ValueExpr::Unknown, |expression| ValueExpr::Await {
                value: Box::new(value_depth(expression, source, depth + 1)),
            }),
        "call" => call_value_depth(node, source, depth + 1),
        "binary_operator" => {
            let Some(op) = node.child_by_field_name("operator") else {
                return ValueExpr::Unknown;
            };
            if text(op, source) != "/" {
                return ValueExpr::Unknown;
            }
            let Some(left) = node.child_by_field_name("left") else {
                return ValueExpr::Unknown;
            };
            receiver_call(value_depth(left, source, depth + 1), "__truediv__", node)
        }
        "subscript" => {
            let mut cursor = node.walk();
            let has_slice = node
                .children_by_field_name("subscript", &mut cursor)
                .any(|child| child.kind() == "slice");
            if !has_slice {
                return ValueExpr::Unknown;
            }
            let Some(object) = node.child_by_field_name("value") else {
                return ValueExpr::Unknown;
            };
            receiver_call(value_depth(object, source, depth + 1), "__slice__", node)
        }
        "attribute" => {
            let Some(object) = node.child_by_field_name("object") else {
                return ValueExpr::Unknown;
            };
            let Some(member) = field(node, source, "attribute") else {
                return ValueExpr::Unknown;
            };
            if static_name(object) {
                return ValueExpr::Field {
                    receiver: text(object, source).to_owned(),
                    member: member.to_owned(),
                };
            }
            let receiver = value_depth(object, source, depth + 1);
            if matches!(receiver, ValueExpr::Unknown) {
                return ValueExpr::Unknown;
            }
            ValueExpr::Project {
                value: Box::new(receiver),
                member: member.to_owned(),
            }
        }
        _ => ValueExpr::Unknown,
    }
}

fn direct_assignment<'tree>(
    block: Node<'tree>,
    source: &str,
) -> Result<Option<(String, Node<'tree>)>, ()> {
    let mut assignment = None;
    let mut cursor = block.walk();
    for statement in block.named_children(&mut cursor) {
        let statement = if statement.kind() == "expression_statement" {
            let Some(expression) = statement.named_child(0) else {
                return Err(());
            };
            expression
        } else {
            statement
        };
        match statement.kind() {
            "comment" | "pass_statement" => {}
            "assignment" => {
                let Some(left) = statement
                    .child_by_field_name("left")
                    .filter(|left| left.kind() == "identifier")
                else {
                    return Err(());
                };
                if assignment.is_some() {
                    return Err(());
                }
                assignment = Some((text(left, source).to_owned(), statement));
            }
            _ => return Err(()),
        }
    }
    Ok(assignment)
}

fn branch_blocks<'tree>(node: Node<'tree>) -> Option<(Vec<Node<'tree>>, bool)> {
    let consequence = node.child_by_field_name("consequence")?;
    let mut branches = vec![consequence];
    let mut has_else = false;
    let mut cursor = node.walk();
    for alternative in node.children_by_field_name("alternative", &mut cursor) {
        match alternative.kind() {
            "elif_clause" => branches.push(alternative.child_by_field_name("consequence")?),
            "else_clause" => {
                branches.push(alternative.child_by_field_name("body")?);
                has_else = true;
            }
            _ => return None,
        }
    }
    Some((branches, has_else))
}

pub(crate) struct PythonValueFlowCollector<'a, 'tree> {
    root: Node<'tree>,
    module_export_facts: Option<super::module_exports::ModuleExportCollector<'tree>>,
    body: Option<Node<'tree>>,
    body_depth: Option<usize>,
    core: ValueFlowCollectorCore,
    return_last: Option<Node<'tree>>,
    return_prefix_valid: bool,
    aliases: Option<AliasWrites>,
    source: &'a str,
    class_scope: bool,
    local_scope: bool,
    receiver: Option<&'a str>,
    init: bool,
    uncertain_callable: bool,
    collect_summary: bool,
    collect_return_summary: bool,
    summary: BodySummary,
}

impl<'a, 'tree> PythonValueFlowCollector<'a, 'tree> {
    fn is_known_local(&mut self, name: &str) -> bool {
        if self
            .core
            .facts_mut()
            .bindings
            .iter()
            .any(|binding| binding.name == name)
        {
            return true;
        }
        self.root
            .child_by_field_name("parameters")
            .is_some_and(|params| {
                let mut cursor = params.walk();
                let found = params.named_children(&mut cursor).any(|param| {
                    let pname = if param.kind() == "identifier" {
                        Some(text(param, self.source))
                    } else {
                        field(param, self.source, "name").or_else(|| {
                            param
                                .named_child(0)
                                .filter(|n| n.kind() == "identifier")
                                .map(|n| text(n, self.source))
                        })
                    };
                    pname == Some(name)
                });
                found
            })
    }

    fn bind(&mut self, target: Node<'_>, value: ValueExpr, at: SourcePosition, conditional: bool) {
        if target.kind() == "identifier" {
            let name = text(target, self.source).to_owned();
            if self.class_scope {
                self.core.push_field(name, at, value, conditional);
            } else {
                self.core.push_binding(name, at, value, conditional);
            }
        } else if target.kind() == "attribute" {
            if target
                .child_by_field_name("object")
                .is_some_and(|receiver| {
                    receiver.kind() == "identifier"
                        && Some(text(receiver, self.source)) == self.receiver
                })
            {
                if let Some(member) = field(target, self.source, "attribute") {
                    let stored = if self.init && !self.uncertain_callable {
                        value
                    } else {
                        ValueExpr::Unknown
                    };
                    self.core
                        .push_field(member.to_owned(), at, stored, conditional);
                }
            }
        } else if matches!(
            target.kind(),
            "tuple_pattern"
                | "list_pattern"
                | "pattern_list"
                | "list_splat_pattern"
                | "dictionary_splat_pattern"
        ) {
            let source_value = match &value {
                ValueExpr::Alias { name } if !self.is_known_local(name) => ValueExpr::Unknown,
                _ => value,
            };
            let mut bindings = Vec::new();
            ValueFlowCollectorCore::collect_destructuring_bindings(
                target,
                source_value,
                self.source,
                &mut bindings,
            );
            if self.class_scope {
                for binding in bindings {
                    self.core
                        .push_field(binding.name, at, binding.value, conditional);
                }
            } else {
                self.core
                    .push_destructured_bindings(bindings, at, None, conditional, |expr| expr);
            }
        }
    }

    fn bind_if_join(&mut self, node: Node<'_>, inherited_conditional: bool) {
        let Some((branches, has_else)) = branch_blocks(node) else {
            return;
        };
        let mut branch_assignments = Vec::with_capacity(branches.len());
        let mut joined_name: Option<String> = None;
        for branch in branches {
            let assignment = match direct_assignment(branch, self.source) {
                Ok(assignment) => assignment,
                Err(()) => return,
            };
            if let Some((name, _)) = &assignment {
                if joined_name.as_ref().is_some_and(|joined| joined != name) {
                    return;
                }
                joined_name.get_or_insert_with(|| name.clone());
            }
            branch_assignments.push(assignment);
        }
        let Some(name) = joined_name else {
            return;
        };

        let start = node.start_position();
        let prior_binding = self
            .core
            .facts()
            .bindings
            .iter()
            .filter(|binding| {
                binding.name == name
                    && !binding.conditional
                    && (binding.position.line, binding.position.column)
                        < (start.row + 1, start.column)
            })
            .map(|binding| (binding.position, &binding.value));
        let prior_join = self
            .core
            .facts()
            .branch_joins
            .iter()
            .filter(|join| {
                join.name == name
                    && !join.conditional
                    && (join.position.line, join.position.column) < (start.row + 1, start.column)
            })
            .map(|join| (join.position, &join.value));
        let incoming = prior_binding
            .into_iter()
            .chain(prior_join)
            .max_by_key(|(position, _)| (position.line, position.column))
            .map(|(_, value)| value.clone())
            .unwrap_or(ValueExpr::Unknown);
        let mut alternatives = branch_assignments
            .into_iter()
            .map(|assignment| match assignment {
                Some((_, statement)) => {
                    let right = statement.child_by_field_name("right");
                    let literal = right.and_then(|right| literal_value(right, self.source));
                    if let Some(literal) = literal {
                        literal
                    } else {
                        right.map_or(ValueExpr::Unknown, |right| value(right, self.source))
                    }
                }
                None => incoming.clone(),
            })
            .collect::<Vec<_>>();
        if !has_else {
            alternatives.push(incoming);
        }

        let mut joined = ValueExpr::BranchJoin { alternatives };
        if inherited_conditional {
            let Some(body_end) = direct_branch_end(node) else {
                return;
            };
            joined = ValueExpr::Scoped {
                value: Box::new(joined),
                body_end,
            };
        }
        let mut at = position(node);
        at.column = at.column.saturating_add(1);
        self.core
            .facts_mut()
            .branch_joins
            .push(crate::semantic::BranchJoinFact {
                name,
                value: joined,
                position: at,
                conditional: inherited_conditional,
            });
    }

    fn record_enter_summary(&mut self, node: Node<'tree>, lexical_scope: bool) {
        if self.collect_return_summary
            && matches!(
                node.kind(),
                "named_expression"
                    | "conditional_expression"
                    | "boolean_operator"
                    | "await"
                    | "yield"
                    | "lambda"
            )
        {
            self.summary.has_uncertain_expression = true;
        }
        if !lexical_scope {
            return;
        }
        if node.kind() == "yield" {
            self.summary.has_yield = true;
        }
        if matches!(node.kind(), "global_statement" | "nonlocal_statement") {
            let mut cursor = node.walk();
            self.summary.nonlocal_names.extend(
                node.named_children(&mut cursor)
                    .filter(|name| name.kind() == "identifier")
                    .map(|name| text(name, self.source).to_owned()),
            );
        }
    }

    fn collect_assignment_fact(&mut self, node: Node<'tree>, conditional: bool) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let right = node.child_by_field_name("right");
        let type_node = node.child_by_field_name("type");
        if !conditional && left.kind() == "identifier" {
            if let Some(TypeExpr::Applied { base, args }) =
                type_node.map(|ty| annotation(ty, self.source))
            {
                if matches!(
                    base.as_str(),
                    "list" | "List" | "Sequence" | "Iterable"
                ) && args.len() == 1
                {
                    self.core.facts_mut().push_collection_element(
                        CollectionElementFact {
                            name: text(left, self.source).to_owned(),
                            position: position(node),
                            element_type: args[0].clone(),
                        },
                    );
                }
            }
        }
        let literal = right.and_then(|right| literal_value(right, self.source));
        let assigned = if let Some(literal) = literal {
            literal
        } else if let (true, Some(ty)) = (self.class_scope, type_node) {
            ValueExpr::Annotated {
                type_expr: annotation(ty, self.source),
            }
        } else {
            right.map_or(ValueExpr::Unknown, |right| {
                type_node.map_or_else(
                    || value(right, self.source),
                    |ty| ValueExpr::Annotated {
                        type_expr: annotation(ty, self.source),
                    },
                )
            })
        };
        let branch_end = conditional.then(|| direct_branch_end(node)).flatten();
        let assigned = if let Some(body_end) = branch_end {
            ValueExpr::Scoped {
                value: Box::new(assigned),
                body_end,
            }
        } else {
            assigned
        };
        self.bind(left, assigned, position(node), conditional);
    }

    fn collect_as_pattern_fact(&mut self, node: Node<'tree>, conditional: bool) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        let name = if alias.kind() == "identifier" {
            alias
        } else {
            alias
                .named_child(0)
                .filter(|child| child.kind() == "identifier")
                .unwrap_or(alias)
        };
        // `with expr as name` binds name to expr. __enter__ that
        // returns self is that value; except-as stays unknown.
        let in_with = node
            .parent()
            .is_some_and(|parent| parent.kind() == "with_item");
        let async_with = in_with && {
            let mut cur = node.parent();
            let mut is_async = false;
            for _ in 0..3 {
                let Some(parent) = cur else { break };
                if parent.kind() == "with_statement" {
                    is_async = text(parent, self.source)
                        .trim_start()
                        .starts_with("async ");
                    break;
                }
                cur = parent.parent();
            }
            is_async
        };
        let bound = if in_with {
            node.named_child(0)
                .filter(|child| child.id() != alias.id())
                .map_or(ValueExpr::Unknown, |expr| {
                    receiver_call(
                        value(expr, self.source),
                        if async_with { "__aenter__" } else { "__enter__" },
                        expr,
                    )
                })
        } else {
            ValueExpr::Unknown
        };
        self.bind(name, bound, position(name), !in_with || conditional);
    }

    fn collect_node_facts(&mut self, node: Node<'tree>, conditional: bool) {
        match node.kind() {
            "call" => {
                if let Some(context) = super::syntax_operations::extract(node, self.source) {
                    self.core.facts_mut().push_template_context(context);
                }
            }
            "assignment" => self.collect_assignment_fact(node, conditional),
            "augmented_assignment" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, ValueExpr::Unknown, position(node), conditional);
                }
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.bind(
                        name,
                        node.child_by_field_name("value")
                            .map_or(ValueExpr::Unknown, |right| value(right, self.source)),
                        position(node),
                        conditional,
                    );
                }
            }
            "for_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let value = match (
                        node.child_by_field_name("right"),
                        node.child_by_field_name("body"),
                    ) {
                        (Some(iterable), Some(body))
                            if left.kind() == "identifier"
                                && iterable.kind() == "identifier" =>
                        {
                            ValueExpr::LoopElement {
                                iterable: text(iterable, self.source).to_owned(),
                                body_end: position(body),
                            }
                        }
                        _ => ValueExpr::Unknown,
                    };
                    let bounded = matches!(value, ValueExpr::LoopElement { .. });
                    self.bind(left, value, position(left), !bounded);
                }
            }
            "as_pattern" => self.collect_as_pattern_fact(node, conditional),
            "delete_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.bind(child, ValueExpr::Unknown, position(node), conditional);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn enter(&mut self, node: Node<'tree>) {
        if let Some(aliases) = &mut self.aliases {
            aliases.enter(node, self.source);
        }
        let prev = self.core.scope_state().current_or_default();
        let is_body = self.body == Some(node);
        let active = is_body || prev.is_active();
        let conditional = if is_body {
            false
        } else {
            prev.is_branch_conditional()
        };
        let collect_facts = is_body || prev.collects_facts();
        let lexical_scope = is_body || prev.is_lexical_scope();
        if !active {
            self.core.scope_state_mut().push(ScopeFrameFlags::new());
            return;
        }
        if is_body {
            self.body_depth = Some(self.core.scope_state().depth());
        }
        let is_direct_child = self
            .body_depth
            .is_some_and(|d| self.core.scope_state().depth() == d + 1);
        if is_direct_child && node.kind() != "comment" {
            if let Some(previous) = self.return_last {
                self.return_prefix_valid &= valid_return_prefix(previous);
            }
            self.return_last = Some(node);
        }
        if self.collect_summary {
            self.record_enter_summary(node, lexical_scope);
        }

        let nested_scope = matches!(
            node.kind(),
            "function_definition" | "class_definition" | "lambda"
        );
        if !self.local_scope && !self.class_scope && super::common::type_alias(node, self.source) {
            self.core.scope_state_mut().push(
                ScopeFrameFlags::new().with_branch_conditional(conditional),
            );
            return;
        }
        if collect_facts
            && self.local_scope
            && matches!(node.kind(), "function_definition" | "class_definition")
        {
            if let Some(name) = node.child_by_field_name("name") {
                self.bind(name, ValueExpr::Unknown, position(node), conditional);
            }
        }
        if (collect_facts && nested_scope) || (!collect_facts && !self.collect_summary) {
            self.core.scope_state_mut().push(
                ScopeFrameFlags::new()
                    .with_active(collect_facts && self.collect_summary && self.collect_return_summary)
                    .with_branch_conditional(conditional),
            );
            return;
        }
        let conditional = conditional
            || matches!(
                node.kind(),
                "if_statement"
                    | "elif_clause"
                    | "else_clause"
                    | "for_statement"
                    | "while_statement"
                    | "try_statement"
                    | "except_clause"
                    | "finally_clause"
                    | "match_statement"
                    | "conditional_expression"
                    | "boolean_operator"
            );
        if collect_facts {
            self.collect_node_facts(node, conditional);
        }
        self.core.scope_state_mut().push(
            ScopeFrameFlags::new()
                .with_active(true)
                .with_branch_conditional(conditional)
                .with_collect_facts(collect_facts && !nested_scope)
                .with_lexical_scope(lexical_scope && !nested_scope),
        );
    }

    pub(crate) fn exit(&mut self, node: Node<'tree>) {
        if self.body == Some(node) {
            self.body_depth = None;
        }
        let collect_facts = self
            .core
            .scope_state_mut()
            .pop()
            .is_some_and(|flags| flags.collects_facts());
        let inherited_conditional = self.core.scope_state().is_conditional();
        if collect_facts && self.local_scope && !self.class_scope && node.kind() == "if_statement" {
            self.bind_if_join(node, inherited_conditional);
        }
        if let Some(aliases) = &mut self.aliases {
            aliases.exit();
        }
    }
}

#[derive(Default)]
struct BodySummary {
    has_yield: bool,
    has_uncertain_expression: bool,
    nonlocal_names: HashSet<String>,
}

fn valid_return_prefix(previous: Node<'_>) -> bool {
    match previous.kind() {
        "pass_statement" | "import_statement" | "import_from_statement" => true,
        "expression_statement" => previous.named_child(0).is_some_and(|expression| {
            expression.kind() == "string"
                || (expression.kind() == "assignment"
                    && expression
                        .child_by_field_name("left")
                        .is_some_and(|left| left.kind() == "identifier"))
        }),
        _ => false,
    }
}

fn returned_value(statement: Node<'_>, source: &str) -> Option<(ValueExpr, SourcePosition)> {
    if statement.kind() != "return_statement" {
        return None;
    }
    let expression = statement.named_child(0)?;
    let returned = value(expression, source);
    (!matches!(returned, ValueExpr::Unknown)).then(|| (returned, position(expression)))
}

impl<'a, 'tree> PythonValueFlowCollector<'a, 'tree> {
    fn empty(
        node: Node<'tree>,
        source: &'a str,
        collect_aliases: bool,
        facts: ValueFlowFacts,
    ) -> Self {
        let mut core = ValueFlowCollectorCore::with_scope_capacity(32);
        *core.facts_mut() = facts;
        Self {
            root: node,
            module_export_facts: (node.kind() == "module")
                .then(|| super::module_exports::ModuleExportCollector::new(node)),
            body: None,
            body_depth: None,
            core,
            return_last: None,
            return_prefix_valid: true,
            aliases: collect_aliases.then(AliasWrites::new),
            source,
            class_scope: false,
            local_scope: false,
            receiver: None,
            init: false,
            uncertain_callable: false,
            collect_summary: false,
            collect_return_summary: false,
            summary: BodySummary::default(),
        }
    }
    pub(crate) fn new(node: Node<'tree>, source: &'a str, collect_aliases: bool) -> Self {
        if super::common::type_alias(node, source) {
            let right = node.child_by_field_name("right").or_else(|| {
                (node.kind() == "type_alias_statement")
                    .then(|| node.named_child(node.named_child_count().saturating_sub(1)))
                    .flatten()
            });
            let alias_type = right
                .map(|right| {
                    if right.kind() == "call" {
                        if field(right, source, "function").is_some_and(|callee| {
                            super::common::typing_name(callee) == "TypeAliasType"
                        }) {
                            right
                                .child_by_field_name("arguments")
                                .and_then(|arguments| arguments.named_child(1))
                                .map_or(TypeExpr::Unknown, |value| annotation(value, source))
                        } else {
                            TypeExpr::Unknown
                        }
                    } else {
                        annotation(right, source)
                    }
                })
                .unwrap_or(TypeExpr::Unknown);
            let facts = ValueFlowFacts {
                framework: Some(Box::new(crate::semantic::FrameworkScopeFacts {
                    alias_type: Some(alias_type),
                    ..Default::default()
                })),
                ..Default::default()
            };
            return Self::empty(node, source, collect_aliases, facts);
        }
        if !matches!(
            node.kind(),
            "module" | "class_definition" | "function_definition" | "lambda"
        ) {
            return Self::empty(node, source, collect_aliases, ValueFlowFacts::default());
        }
        let is_lambda = node.kind() == "lambda";
        let callable = node.kind() == "function_definition";
        let body = if node.kind() == "module" {
            Some(node)
        } else {
            node.child_by_field_name("body")
        };
        let decorated = node
            .parent()
            .is_some_and(|parent| parent.kind() == "decorated_definition");
        let initially_uncertain =
            callable && (decorated || text(node, source).trim_start().starts_with("async "));
        let collect_return_summary = callable && !initially_uncertain;
        let receiver = if callable {
            node.child_by_field_name("parameters")
                .and_then(|params| params.named_child(0))
                .and_then(|parameter| {
                    if parameter.kind() == "identifier" {
                        Some(text(parameter, source))
                    } else {
                        field(parameter, source, "name").or_else(|| {
                            parameter
                                .named_child(0)
                                .filter(|name| name.kind() == "identifier")
                                .map(|name| text(name, source))
                        })
                    }
                })
                .filter(|name| matches!(*name, "self" | "cls"))
        } else {
            None
        };
        let mut collector = PythonValueFlowCollector {
            root: node,
            module_export_facts: (node.kind() == "module")
                .then(|| super::module_exports::ModuleExportCollector::new(node)),
            body,
            body_depth: None,
            core: ValueFlowCollectorCore::with_scope_capacity(32),
            return_last: None,
            return_prefix_valid: true,
            aliases: collect_aliases.then(AliasWrites::new),
            source,
            class_scope: node.kind() == "class_definition",
            local_scope: callable || is_lambda,
            receiver,
            init: field(node, source, "name") == Some("__init__"),
            uncertain_callable: initially_uncertain,
            collect_summary: callable,
            collect_return_summary,
            summary: BodySummary::default(),
        };
        if is_lambda {
            record_python_lambda_callback_params(node, source, &mut collector.core);
            return collector;
        }
        if callable {
            Self::populate_callable_signature(node, source, receiver, &mut collector.core);
        }
        collector
    }

    fn populate_callable_signature(
        node: Node<'tree>,
        source: &'a str,
        receiver: Option<&'a str>,
        core: &mut ValueFlowCollectorCore,
    ) {
        let mut sig_params = Vec::new();
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for (idx, parameter) in parameters.named_children(&mut cursor).enumerate() {
                if idx == 0 && receiver.is_some() {
                    continue;
                }
                let param_name = if parameter.kind() == "identifier" {
                    Some(text(parameter, source))
                } else {
                    field(parameter, source, "name").or_else(|| {
                        parameter
                            .named_child(0)
                            .filter(|name| name.kind() == "identifier")
                            .map(|name| text(name, source))
                    })
                };
                let mut param_ty = TypeExpr::Unknown;
                let mut callback_params = Vec::new();
                if let Some(ty) = parameter.child_by_field_name("type") {
                    annotation_reference_positions(ty, source, core.facts_mut());
                    let parsed = annotation(ty, source);
                    callback_params = extract_python_callback_params(ty, source);
                    if let Some(name) = param_name {
                        if let TypeExpr::Applied { base, args } = &parsed {
                            if matches!(
                                base.as_str(),
                                "list" | "List" | "Sequence" | "Iterable"
                            ) && args.len() == 1
                            {
                                core.facts_mut().push_collection_element(CollectionElementFact {
                                    name: name.to_owned(),
                                    position: crate::languages::types::source_start(node),
                                    element_type: args[0].clone(),
                                });
                            }
                        }
                    }
                    param_ty = parsed;
                }
                if sig_params.len() < 128 {
                    sig_params.push(crate::semantic::CallableParameter {
                        name: param_name.map(str::to_owned),
                        type_expr: param_ty,
                        callback_params,
                    });
                }
            }
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            annotation_reference_positions(ret, source, core.facts_mut());
        }
        if !sig_params.iter().any(|p| !p.callback_params.is_empty()) {
            return;
        }
        let sig = crate::semantic::CallableSignature {
            has_receiver: receiver.is_some(),
            params: sig_params,
            return_type: node
                .child_by_field_name("return_type")
                .map(|ret| annotation(ret, source)),
        };
        if sig.valid() {
            core.facts_mut().callable_signature = Some(sig);
        }
    }
    pub(crate) fn module_aliases(&self) -> HashSet<String> {
        self.aliases
            .as_ref()
            .map_or_else(HashSet::new, AliasWrites::aliases)
    }
    pub(crate) fn observe_module_event(&mut self, node: Node<'tree>, entering: bool) {
        if let Some(exports) = &mut self.module_export_facts {
            exports.observe(node, entering, self.source);
        }
    }
    pub(crate) fn module_exports(
        &self,
        bindings: &ScopeBindings,
    ) -> Option<(
        Vec<crate::semantic::ExportBinding>,
        Vec<crate::languages::types::LazyExport>,
    )> {
        self.module_export_facts
            .as_ref()
            .map(|exports| exports.outputs(self.source, bindings))
    }
    pub(crate) fn finish(mut self) -> ValueFlowFacts {
        let node = self.root;
        let source = self.source;
        let callable = self.local_scope;
        let decorated = node
            .parent()
            .is_some_and(|parent| parent.kind() == "decorated_definition");
        let return_statement = self
            .return_last
            .filter(|last| self.return_prefix_valid && last.kind() == "return_statement");
        self.uncertain_callable |= callable && self.summary.has_yield;
        if self.uncertain_callable && self.init {
            for field in &mut self.core.facts_mut().fields {
                field.value = ValueExpr::Unknown;
            }
        }
        for name in &self.summary.nonlocal_names {
            for binding in &mut self.core.facts_mut().bindings {
                if binding.name == *name {
                    binding.value = ValueExpr::Unknown;
                }
            }
        }
        let mut scoped_names: Vec<_> = self.summary.nonlocal_names.iter().collect();
        scoped_names.sort_unstable();
        for name in scoped_names {
            let start = node.start_position();
            self.core.push_binding(
                name.clone(),
                SourcePosition {
                    line: start.row + 1,
                    column: start.column,
                },
                ValueExpr::Unknown,
                false,
            );
        }
        if callable {
            let async_callable = text(node, source).trim_start().starts_with("async ");
            if async_callable {
                if !decorated && !self.summary.has_yield {
                    // An async function's declared type describes the awaited
                    // result, not the ordinary call result (a coroutine).
                    self.core.facts_mut().async_return_type = node
                        .child_by_field_name("return_type")
                        .map(|ty| annotation(ty, source));
                }
            } else if !self.uncertain_callable {
                let return_type = node
                    .child_by_field_name("return_type")
                    .map(|ty| annotation(ty, source));
                self.core.set_return_type_if_none(return_type);
            }
        }
        if callable
            && !self.uncertain_callable
            && self.summary.nonlocal_names.is_empty()
            && self.collect_return_summary
            && !self.summary.has_uncertain_expression
        {
            if let Some((returned, at)) =
                return_statement.and_then(|statement| returned_value(statement, source))
            {
                self.core.facts_mut().return_value = Some(returned);
                self.core.set_return_position(at);
            }
        }
        if node.kind() == "module" {
            let imports = type_only_imports(node, source);
            for import in imports {
                self.core.facts_mut().push_type_only_import(import);
            }
        }
        self.core.finish()
    }
}

impl<'tree> SyntaxCollector<'tree> for PythonValueFlowCollector<'_, 'tree> {
    type Output = ValueFlowFacts;
    fn enter(&mut self, node: Node<'tree>) {
        PythonValueFlowCollector::enter(self, node);
    }
    fn exit(&mut self, node: Node<'tree>) {
        PythonValueFlowCollector::exit(self, node);
    }
    fn finish(self) -> ValueFlowFacts {
        PythonValueFlowCollector::finish(self)
    }
}

impl<'source, 'tree> ScopeVisitor<'source, 'tree> for PythonValueFlowCollector<'source, 'tree> {
    fn enter(&mut self, node: Node<'tree>) {
        PythonValueFlowCollector::enter(self, node);
    }

    fn exit(&mut self, node: Node<'tree>) {
        PythonValueFlowCollector::exit(self, node);
    }

    fn observe_module_event(&mut self, node: Node<'tree>, entering: bool) {
        PythonValueFlowCollector::observe_module_event(self, node, entering);
    }

    fn module_exports(
        &self,
        bindings: &ScopeBindings,
    ) -> Option<(Vec<crate::semantic::ExportBinding>, serde_json::Value)> {
        PythonValueFlowCollector::module_exports(self, bindings)
            .map(|(exports, lazy)| (exports, serde_json::json!(lazy)))
    }

    fn finish(
        self: Box<Self>,
        collect_module_aliases: bool,
    ) -> (ValueFlowFacts, Option<HashSet<String>>) {
        let aliases = collect_module_aliases.then(|| self.module_aliases());
        ((*self).finish(), aliases)
    }
}

pub(super) fn extract(node: Node<'_>, source: &str) -> ValueFlowFacts {
    collect(node, PythonValueFlowCollector::new(node, source, false))
}

fn extract_python_callback_params(ty: Node<'_>, source: &str) -> Vec<TypeExpr> {
    let mut target = ty;
    if target.kind() == "type" {
        let Some(inner) = target.named_child(0) else {
            return Vec::new();
        };
        target = inner;
    }
    if target.kind() != "generic_type" && target.kind() != "subscript" {
        return Vec::new();
    }
    let Some(base_node) = target
        .named_child(0)
        .filter(|n| matches!(n.kind(), "identifier" | "attribute"))
    else {
        return Vec::new();
    };
    let base_name = text(base_node, source).trim();
    if base_name.rsplit('.').next() != Some("Callable") {
        return Vec::new();
    }
    let args_list = target
        .child_by_field_name("type_parameter")
        .or_else(|| target.named_child(1));
    let Some(args_list) = args_list else {
        return Vec::new();
    };
    fn unwrap_type(n: Node<'_>) -> Node<'_> {
        if n.kind() == "type" {
            n.named_child(0).unwrap_or(n)
        } else {
            n
        }
    }
    let mut cursor = args_list.walk();
    let Some(params_list) = args_list
        .named_children(&mut cursor)
        .map(unwrap_type)
        .find(|n| n.kind() == "list")
        .or_else(|| (args_list.kind() == "list").then_some(args_list))
    else {
        return Vec::new();
    };
    let mut p_cursor = params_list.walk();
    params_list
        .named_children(&mut p_cursor)
        .take(32)
        .map(|arg| annotation(arg, source))
        .collect()
}

fn record_python_lambda_callback_params(
    node: Node<'_>,
    source: &str,
    core: &mut ValueFlowCollectorCore,
) {
    let Some(arguments) = node.parent().filter(|p| p.kind() == "argument_list") else {
        return;
    };
    let Some(call) = arguments.parent().filter(|p| p.kind() == "call") else {
        return;
    };
    let mut cursor = arguments.walk();
    let Some(arg_index) = arguments
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .position(|n| n.id() == node.id())
    else {
        return;
    };
    let callee_expr = call_value(call, source);
    if matches!(callee_expr, ValueExpr::Unknown) {
        return;
    }
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return;
    };
    let at = crate::languages::types::source_start(node);
    let mut p_cursor = parameters.walk();
    for (param_index, param) in parameters.named_children(&mut p_cursor).enumerate() {
        let Some(ident) = (param.kind() == "identifier")
            .then_some(param)
            .or_else(|| param.child_by_field_name("name"))
            .filter(|n| n.kind() == "identifier")
        else {
            continue;
        };
        core.push_callback_param(
            text(ident, source),
            at,
            callee_expr.clone(),
            arg_index,
            param_index,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::extract;

    #[test]
    fn test_persistence_type_only_imports() {
        let source = "from typing import TYPE_CHECKING\n\nclass PersistenceMixin:\n    if TYPE_CHECKING:\n        from contextunity.router.service.payloads import GraphEntry\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::language())
            .unwrap();
        for ordinary in ["", "value = 1\n", "def plain():\n    pass\n"] {
            let tree = parser.parse(ordinary, None).unwrap();
            let facts = extract(tree.root_node(), ordinary);
            assert!(
                facts.framework.is_none(),
                "ordinary module needs no framework allocation: {ordinary:?}"
            );
        }
        for source in [
            "def plain() -> None:\n    pass\n",
            "def plain(value: None):\n    pass\n",
        ] {
            let tree = parser.parse(source, None).unwrap();
            let function = tree.root_node().named_child(0).unwrap();
            let facts = extract(function, source);
            assert!(
                facts.framework.is_none(),
                "None annotation needs no framework allocation: {source:?}"
            );
        }
        let annotated = "def plain(value: Input) -> Output:\n    pass\n";
        let tree = parser.parse(annotated, None).unwrap();
        let facts = extract(tree.root_node().named_child(0).unwrap(), annotated);
        assert!(facts.framework.is_some());
        let annotation_references: Vec<_> = facts.annotation_references().collect();
        assert_eq!(annotation_references.len(), 2);
        assert_eq!(annotation_references[0].expression, "Input");
        assert_eq!(annotation_references[1].expression, "Output");
        let tree = parser.parse(source, None).unwrap();
        let facts = extract(tree.root_node(), source);
        assert!(facts.framework.is_some());
        let type_only_imports: Vec<_> = facts.type_only_imports().collect();
        assert_eq!(type_only_imports.len(), 1);
        assert_eq!(type_only_imports[0].alias, "GraphEntry");
    }
}
