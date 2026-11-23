use super::commonjs::required_module;
use super::setup_summaries;
use super::types::kind;
use crate::ast::syntax::{field, text, Syntax};
use crate::languages::support::value_flow_core::{ScopeFrameFlags, ValueFlowCollectorCore};
use crate::semantic::{
    ExportBinding, ObjectMember, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use std::collections::{BTreeMap, HashSet};

/// Builds the same bounded call-result receiver identity for calls and property reads.
pub(super) fn computed_member_hint(
    node: Syntax<'_>,
    source: &str,
    offset: usize,
    property_read: bool,
) -> Option<crate::models::ReceiverHint> {
    let mut receiver = node;
    let mut members = [""; crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH];
    let mut count = 0;
    for _ in 0..crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        if receiver.kind() == "parenthesized_expression" {
            receiver = receiver.named_child(0)?;
            continue;
        }
        if receiver.kind() != "member_expression" {
            break;
        }
        if property_read && receiver.child_by_field_name("optional_chain").is_some() {
            return None;
        }
        let member = receiver
            .child_by_field_name("property")
            .filter(|member| member.kind() == "property_identifier")?;
        members[count] = text(member, source);
        count += 1;
        receiver = receiver.child_by_field_name("object")?;
    }
    if count == 0
        || !matches!(
            receiver.kind(),
            "call_expression" | "new_expression" | "string"
        )
    {
        return None;
    }
    let mut member = String::with_capacity(
        members[..count]
            .iter()
            .map(|member| member.len())
            .sum::<usize>()
            + count
            - 1,
    );
    for part in members[..count].iter().rev() {
        if !member.is_empty() {
            member.push('.');
        }
        member.push_str(part);
    }
    match receiver.kind() {
        "call_expression" => {
            let value = call_value(receiver, source, offset);
            matches!(value, ValueExpr::Call { .. })
                .then_some(crate::models::ReceiverHint::TypedCallResult { value, member })
        }
        "new_expression" => receiver.child_by_field_name("constructor").map(|callee| {
            crate::models::ReceiverHint::ConstructorResult {
                callee: text(callee, source).to_owned(),
                member,
            }
        }),
        "string" => Some(crate::models::ReceiverHint::StringLiteral { member }),
        _ => None,
    }
}

fn position(node: Syntax<'_>) -> SourcePosition {
    crate::languages::types::source_start(node)
}

fn completed(node: Syntax<'_>) -> SourcePosition {
    crate::languages::types::source_end(node)
}

pub(crate) fn named_type(node: Syntax<'_>, source: &str) -> TypeExpr {
    type_expr(node, source, 0)
}

fn type_expr(node: Syntax<'_>, source: &str, depth: usize) -> TypeExpr {
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        return TypeExpr::Unknown;
    }
    if node.kind() == "type_annotation" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .next()
            .map_or(TypeExpr::Unknown, |child| {
                type_expr(child, source, depth + 1)
            });
    }
    if node.kind() == "generic_type" {
        let (Some(base), Some(arguments)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("type_arguments"),
        ) else {
            return TypeExpr::Unknown;
        };
        let TypeExpr::Named { name: base } = type_expr(base, source, depth + 1) else {
            return TypeExpr::Unknown;
        };
        let mut cursor = arguments.walk();
        let mut args = Vec::new();
        for argument in arguments.named_children(&mut cursor) {
            if args.len() >= 16 {
                return TypeExpr::Unknown;
            }
            args.push(type_expr(argument, source, depth + 1));
        }
        return if args.is_empty() {
            TypeExpr::Unknown
        } else {
            TypeExpr::Applied { base, args }
        };
    }
    if node.kind() == "array_type" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .next()
            .map_or(TypeExpr::Unknown, |element| TypeExpr::Applied {
                base: "Array".into(),
                args: vec![type_expr(element, source, depth + 1)],
            });
    }
    if node.kind() == "union_type" {
        let mut cursor = node.walk();
        let non_null: Vec<_> = node
            .named_children(&mut cursor)
            .filter(|child| {
                let txt = text(*child, source).trim();
                !matches!(txt, "null" | "undefined" | "void")
                    && !matches!(child.kind(), "null_type" | "undefined_type" | "void_type")
            })
            .collect();
        if non_null.len() == 1 {
            return type_expr(non_null[0], source, depth + 1);
        }
        return TypeExpr::Unknown;
    }
    let value = text(node, source).trim().trim_start_matches(':').trim();
    if !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
    {
        TypeExpr::Named {
            name: value.to_owned(),
        }
    } else {
        TypeExpr::Unknown
    }
}

pub(super) fn jsdoc(
    node: Syntax<'_>,
    source: &str,
) -> (BTreeMap<String, String>, Option<TypeExpr>, Option<TypeExpr>) {
    let mut anchor = node;
    while let Some(parent) = anchor.parent().filter(|parent| {
        matches!(
            parent.kind(),
            "export_statement"
                | "variable_declarator"
                | "lexical_declaration"
                | "variable_declaration"
        )
    }) {
        anchor = parent;
    }
    let Some(comment) = anchor
        .prev_named_sibling()
        .filter(|comment| comment.kind() == "comment")
    else {
        return (BTreeMap::new(), None, None);
    };
    let comment = text(comment, source);
    if !comment.starts_with("/**") || comment.len() > 8192 {
        return (BTreeMap::new(), None, None);
    }
    let mut parameters = BTreeMap::new();
    let mut duplicates = HashSet::new();
    let mut returned = None;
    let mut declared = None;
    let mut returns = 0;
    let mut types = 0;
    let mut invalid_parameters = false;
    for line in comment.lines() {
        let line = line
            .trim()
            .trim_start_matches("/**")
            .trim_start_matches('*')
            .trim();
        let (tag, rest) = if let Some(rest) = line.strip_prefix("@param ") {
            ("param", rest)
        } else if let Some(rest) = line
            .strip_prefix("@returns ")
            .or_else(|| line.strip_prefix("@return "))
        {
            ("return", rest)
        } else if let Some(rest) = line.strip_prefix("@type ") {
            ("type", rest)
        } else {
            continue;
        };
        if tag == "return" {
            returns += 1;
            if returns > 1 {
                returned = Some(TypeExpr::Unknown);
            }
        }
        if tag == "type" {
            types += 1;
            if types > 1 {
                declared = Some(TypeExpr::Unknown);
            }
        }
        let Some(rest) = rest.trim_start().strip_prefix('{') else {
            if tag == "param" {
                invalid_parameters = true;
            }
            continue;
        };
        let Some((ty, tail)) = rest.split_once('}') else {
            if tag == "param" {
                invalid_parameters = true;
            }
            continue;
        };
        let ty = ty.trim();
        if ty.is_empty()
            || !ty.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            })
        {
            match tag {
                "return" => returned = Some(TypeExpr::Unknown),
                "type" => declared = Some(TypeExpr::Unknown),
                _ => invalid_parameters = true,
            }
            continue;
        }
        if tag == "param" {
            let Some(name) = tail.split_whitespace().next().filter(|name| {
                name.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            }) else {
                continue;
            };
            if parameters.insert(name.to_owned(), ty.to_owned()).is_some() {
                duplicates.insert(name.to_owned());
            }
        } else if tag == "return" {
            if returns != 1 {
                returned = Some(TypeExpr::Unknown);
            } else {
                returned = Some(TypeExpr::Named {
                    name: ty.to_owned(),
                });
            }
        } else if types != 1 {
            declared = Some(TypeExpr::Unknown);
        } else {
            declared = Some(TypeExpr::Named {
                name: ty.to_owned(),
            });
        }
    }
    for duplicate in duplicates {
        parameters.remove(&duplicate);
    }
    if invalid_parameters {
        parameters.clear();
    }
    (parameters, returned, declared)
}

pub(super) fn object_value(
    node: Syntax<'_>,
    source: &str,
    definitions: Option<&hashbrown::HashMap<&str, Syntax<'_>>>,
) -> ValueExpr {
    if node.named_child_count() > 128 {
        return ValueExpr::Unknown;
    }
    let mut members = Vec::new();
    let mut names = hashbrown::HashSet::new();
    for item in node.named_children(&mut node.walk()) {
        let (name, declaration) = if item.kind() == "method_definition" {
            if text(item, source).trim_start().starts_with("get ")
                || text(item, source).trim_start().starts_with("set ")
            {
                return ValueExpr::Unknown;
            }
            let Some(name) = item
                .child_by_field_name("name")
                .filter(|name| matches!(name.kind(), "property_identifier" | "identifier"))
            else {
                return ValueExpr::Unknown;
            };
            (text(name, source), item)
        } else if item.kind() == "pair" {
            let (Some(name), Some(declaration)) = (
                item.child_by_field_name("key"),
                item.child_by_field_name("value"),
            ) else {
                return ValueExpr::Unknown;
            };
            if !matches!(name.kind(), "property_identifier" | "identifier") {
                return ValueExpr::Unknown;
            }
            (
                text(name, source),
                if matches!(declaration.kind(), "arrow_function" | "function_expression") {
                    declaration
                } else {
                    item
                },
            )
        } else if item.kind() == "shorthand_property_identifier" {
            let name = text(item, source);
            let Some(declaration) = definitions.and_then(|definitions| definitions.get(name))
            else {
                return ValueExpr::Unknown;
            };
            (name, *declaration)
        } else {
            return ValueExpr::Unknown;
        };
        if !names.insert(name) {
            return ValueExpr::Unknown;
        }
        members.push(ObjectMember {
            name: name.to_owned(),
            position: position(declaration),
            target: Default::default(),
        });
    }
    if members.is_empty() {
        ValueExpr::Unknown
    } else {
        ValueExpr::Object {
            members,
            builtin_guard: false,
        }
    }
}

fn value(node: Syntax<'_>, source: &str) -> ValueExpr {
    match node.kind() {
        "await_expression" => node
            .named_child(0)
            .map(|expression| {
                if expression
                    .child_by_field_name("function")
                    .is_some_and(|callee| {
                        callee.kind() == "identifier" && text(callee, source) == "fetch"
                    })
                {
                    ValueExpr::Call {
                        callee: "await fetch".to_owned(),
                        site: None,
                    }
                } else {
                    ValueExpr::Await {
                        value: Box::new(value(expression, source)),
                    }
                }
            })
            .unwrap_or(ValueExpr::Unknown),
        "identifier" => ValueExpr::Alias {
            name: text(node, source).to_owned(),
        },
        "array" => ValueExpr::Construct {
            callee: "[]".to_owned(),
        },
        "new_expression" => node
            .child_by_field_name("constructor")
            .map(|callee| ValueExpr::Construct {
                callee: text(callee, source).to_owned(),
            })
            .unwrap_or(ValueExpr::Unknown),
        "call_expression" => {
            let Some(callee) = node.child_by_field_name("function") else {
                return ValueExpr::Unknown;
            };
            if text(callee, source) == "Object.freeze" {
                let Some(arguments) = node.child_by_field_name("arguments") else {
                    return ValueExpr::Unknown;
                };
                let mut cursor = arguments.walk();
                let mut values = arguments.named_children(&mut cursor);
                return match (values.next(), values.next()) {
                    (Some(object), None) if object.kind() == "object" => {
                        match value(object, source) {
                            ValueExpr::Object { members, .. } => ValueExpr::Object {
                                members,
                                builtin_guard: true,
                            },
                            _ => ValueExpr::Unknown,
                        }
                    }
                    _ => ValueExpr::Unknown,
                };
            }
            call_value(node, source, 0)
        }
        "member_expression" => member_value(node, source, 0),
        "object" => object_value(node, source, None),
        _ => ValueExpr::Unknown,
    }
}

fn member_value(node: Syntax<'_>, source: &str, depth: usize) -> ValueExpr {
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH {
        return ValueExpr::Unknown;
    }
    let (Some(receiver), Some(member)) = (
        node.child_by_field_name("object"),
        node.child_by_field_name("property")
            .filter(|member| member.kind() == "property_identifier"),
    ) else {
        return ValueExpr::Unknown;
    };
    let member = text(member, source);
    if receiver.kind() == "identifier" {
        return ValueExpr::Field {
            receiver: text(receiver, source).to_owned(),
            member: member.to_owned(),
        };
    }
    if node.child_by_field_name("optional_chain").is_some() {
        return ValueExpr::Unknown;
    }
    let value = match receiver.kind() {
        "call_expression" => call_value_at(receiver, source, 0, depth + 1),
        "member_expression" => member_value(receiver, source, depth + 1),
        _ => return ValueExpr::Unknown,
    };
    if matches!(value, ValueExpr::Unknown) {
        ValueExpr::Unknown
    } else {
        ValueExpr::Project {
            value: Box::new(value),
            member: member.to_owned(),
        }
    }
}

/// Canonical bounded call evidence shared by assigned values and direct receivers.
pub(super) fn call_value(node: Syntax<'_>, source: &str, offset: usize) -> ValueExpr {
    call_value_at(node, source, offset, 0)
}

fn call_value_at(node: Syntax<'_>, source: &str, offset: usize, depth: usize) -> ValueExpr {
    use crate::semantic::{CallSiteEvidence, StaticArgument};
    if depth >= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH
        || node.kind() != "call_expression"
    {
        return ValueExpr::Unknown;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return ValueExpr::Unknown;
    };
    let mut callee = text(function, source);
    let mut receiver = None;
    if function.kind() == "member_expression" {
        if let Some(object) = function
            .child_by_field_name("object")
            .filter(|object| object.kind() == "call_expression")
        {
            let nested = call_value_at(object, source, offset, depth + 1);
            if matches!(nested, ValueExpr::Call { .. }) {
                if let Some(member) = function
                    .child_by_field_name("property")
                    .filter(|member| member.kind() == "property_identifier")
                {
                    callee = text(member, source);
                    receiver = Some(Box::new(nested));
                }
            }
        }
    }
    let mut arguments = Vec::new();
    let mut useful = receiver.is_some();
    let mut slots = 0;
    let mut bytes = 0;
    let mut valid = function.child_by_field_name("optional_chain").is_none()
        && node.child_by_field_name("optional_chain").is_none();
    if !valid {
        return ValueExpr::Call {
            callee: text(function, source).to_owned(),
            site: None,
        };
    }
    if let Some(values) = node.child_by_field_name("arguments") {
        for argument in values.named_children(&mut values.walk()) {
            if argument.kind() == "comment" {
                continue;
            }
            if slots == 4 || argument.kind() == "spread_element" {
                valid = false;
                break;
            }
            let literal = (argument.kind() == "string")
                .then(|| text(argument, source))
                .and_then(|raw| {
                    let quote = raw.as_bytes().first().copied()?;
                    (raw.len() >= 2
                        && matches!(quote, b'\'' | b'"')
                        && raw.as_bytes().last() == Some(&quote))
                    .then(|| &raw[1..raw.len() - 1])
                })
                .filter(|value| {
                    value.len() <= 256
                        && !value.contains('\\')
                        && !value.chars().any(char::is_control)
                });
            if argument.kind() == "string" && literal.is_none() {
                valid = false;
                break;
            }
            if let Some(literal) = literal {
                bytes += literal.len();
                useful = true;
                arguments.resize(slots, StaticArgument::Unknown);
                arguments.push(StaticArgument::String {
                    value: literal.to_owned(),
                });
            } else if useful {
                arguments.push(StaticArgument::Unknown);
            }
            slots += 1;
        }
    }
    let site = (valid && useful && bytes <= 1024).then(|| {
        let mut at = position(node);
        at.line += offset;
        CallSiteEvidence {
            position: at,
            receiver,
            arguments,
        }
    });
    if site.is_none() {
        callee = text(function, source);
    }
    ValueExpr::Call {
        callee: callee.to_owned(),
        site,
    }
}

fn is_simple_assignment(node: Syntax<'_>, source: &str) -> bool {
    if node.kind() != "assignment_expression" {
        return false;
    }
    if let Some(operator) = node.child_by_field_name("operator") {
        return text(operator, source).trim() == "=";
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            let t = text(child, source).trim();
            if t == "=" {
                return true;
            } else if t.ends_with('=') {
                return false;
            }
        }
    }
    true
}

pub(crate) struct TypeScriptValueFlowCollector<'source, 'tree> {
    root: Syntax<'tree>,
    source: &'source str,
    core: ValueFlowCollectorCore,
    object_aliases: std::collections::BTreeSet<&'source str>,
    returned: ReturnSummary<'tree>,
    asynchronous: bool,
    generator: bool,
    definitions: hashbrown::HashMap<&'source str, Syntax<'tree>>,
    declared: hashbrown::HashSet<&'source str>,
    writes: hashbrown::HashSet<&'source str>,
    duplicate_definition: bool,
    terminal: bool,
}

impl<'source, 'tree> TypeScriptValueFlowCollector<'source, 'tree> {
    fn empty(root: Syntax<'tree>, source: &'source str, result: ValueFlowFacts) -> Self {
        let mut core = ValueFlowCollectorCore::default();
        *core.facts_mut() = result;
        Self {
            root,
            source,
            core,
            object_aliases: Default::default(),
            returned: ReturnSummary::new(root),
            asynchronous: false,
            generator: false,
            definitions: Default::default(),
            declared: Default::default(),
            writes: Default::default(),
            duplicate_definition: false,
            terminal: false,
        }
    }
    pub(crate) fn new(root: Syntax<'tree>, source: &'source str) -> Self {
        if root.kind() == "object" {
            let result = ValueFlowFacts {
                framework: Some(Box::new(crate::semantic::FrameworkScopeFacts {
                    return_value: Some(object_value(root, source, None)),
                    return_position: Some(position(root)),
                    ..Default::default()
                })),
                ..Default::default()
            };
            return Self {
                terminal: true,
                ..Self::empty(root, source, result)
            };
        }
        let documented_return = || jsdoc(root, source).1;
        if root.kind() == "pair" {
            if let Some(field_value) = root
                .child_by_field_name("value")
                .filter(|value| value.kind() == "array")
            {
                let result = ValueFlowFacts {
                    framework: Some(Box::new(crate::semantic::FrameworkScopeFacts {
                        return_value: Some(value(field_value, source)),
                        return_position: Some(completed(field_value)),
                        ..Default::default()
                    })),
                    ..Default::default()
                };
                return Self {
                    terminal: true,
                    ..Self::empty(root, source, result)
                };
            }
        }
        let mut cursor = root.walk();
        let generator = root.kind().contains("generator_function")
            || root.children(&mut cursor).any(|child| child.kind() == "*");
        let mut cursor = root.walk();
        let asynchronous = root
            .children(&mut cursor)
            .any(|child| child.kind() == "async");
        let annotation = root
            .child_by_field_name("return_type")
            .map(|ty| named_type(ty, source))
            .or_else(documented_return);
        let promise_payload = match &annotation {
            Some(TypeExpr::Applied { base, args }) if base == "Promise" && args.len() == 1 => {
                Some(args[0].clone())
            }
            _ => None,
        };
        let wrapped_return = generator || asynchronous || promise_payload.is_some();
        let sig = extract_ts_callable_signature(root, source, annotation.clone());
        let mut result = ValueFlowFacts {
            return_type: if wrapped_return {
                Some(TypeExpr::Unknown)
            } else {
                annotation.clone()
            },
            ..Default::default()
        };
        if let Some(sig) = sig {
            result.callable_signature = Some(sig);
        }
        let async_return_type = if generator {
            None
        } else if asynchronous {
            promise_payload.or(annotation)
        } else {
            promise_payload
        };
        if let Some(async_return_type) = async_return_type {
            result.async_return_type = Some(async_return_type);
        }
        let mut collector = Self {
            asynchronous,
            generator,
            ..Self::empty(root, source, result)
        };
        record_ts_callback_params(root, source, &mut collector.core);
        collector
    }

    fn record_write(&mut self, node: Syntax<'tree>) {
        let written = match node.kind() {
            "assignment_expression" | "augmented_assignment_expression" => {
                node.child_by_field_name("left")
            }
            "update_expression" => node.child_by_field_name("argument"),
            _ => None,
        };
        if let Some(name) = written {
            match name.kind() {
                "identifier" => {
                    self.writes.insert(text(name, self.source));
                }
                "member_expression" | "subscript_expression" => {}
                // Unsupported binding targets can rewrite a captured callable.
                // This guard affects configured summaries only, including while
                // the outer collector is suspended for a nested closure.
                _ => self.returned.setup_invalid = true,
            }
        }
        if node.kind() == "unary_expression"
            && node
                .child_by_field_name("operator")
                .is_some_and(|operator| text(operator, self.source) == "delete")
        {
            self.returned.setup_invalid = true;
        }
    }

    pub(crate) fn enter_suspended(&mut self, node: Syntax<'tree>) {
        if !self.terminal {
            self.record_write(node);
        }
    }

    fn record_direct_body_definition(
        &mut self,
        node: Syntax<'tree>,
        body: Option<Syntax<'tree>>,
    ) {
        let source = self.source;
        if node.kind() == "function_declaration" && node.parent() == body {
            if let Some(name) = node.child_by_field_name("name") {
                let name = text(name, source);
                self.duplicate_definition |= !self.declared.insert(name);
                self.definitions.insert(name, node);
            }
        }
        if node.kind() == "variable_declarator" && node.parent().and_then(|n| n.parent()) == body {
            if let Some(name) = node
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")
            {
                let name = text(name, source);
                self.duplicate_definition |= !self.declared.insert(name);
                if node
                    .parent()
                    .is_some_and(|n| text(n, source).trim_start().starts_with("const "))
                {
                    if let Some(value) = node
                        .child_by_field_name("value")
                        .filter(|n| matches!(n.kind(), "arrow_function" | "function_expression"))
                    {
                        self.definitions.insert(name, value);
                    }
                }
            }
        }
    }

    fn record_variable_declarator_flow(
        &mut self,
        node: Syntax<'tree>,
        branch_conditional: bool,
        try_conditional: bool,
    ) {
        let source = self.source;
        let lexical = node
            .parent()
            .is_some_and(|parent| parent.kind() == "lexical_declaration");
        let immutable = node.parent().is_some_and(|parent| {
            parent.kind() == "lexical_declaration"
                && parent
                    .child_by_field_name("kind")
                    .is_some_and(|keyword| text(keyword, source) == "const")
        });
        let binding_conditional = branch_conditional || (try_conditional && !lexical);
        let scope_end = (try_conditional && lexical && !branch_conditional)
            .then(|| {
                let mut parent = node.parent();
                while let Some(ancestor) = parent {
                    if ancestor.kind() == "statement_block" {
                        return Some(completed(ancestor));
                    }
                    parent = ancestor.parent();
                }
                None
            })
            .flatten();
        let scoped = |value| match scope_end {
            Some(body_end) => ValueExpr::Scoped {
                value: Box::new(value),
                body_end,
            },
            None => value,
        };
        if let Some(name) = node
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
        {
            let initializer = node.child_by_field_name("value");
            let expression = initializer
                .map(|initializer| {
                    node.child_by_field_name("type")
                        .map(|ty| named_type(ty, source))
                        .or_else(|| jsdoc(node, source).2)
                        .map(|type_expr| ValueExpr::Annotated { type_expr })
                        .unwrap_or_else(|| value(initializer, source))
                })
                .unwrap_or(ValueExpr::Unknown);
            let local = text(name, source);
            self.object_aliases.remove(local);
            if matches!(
                &expression,
                ValueExpr::Object { .. } | ValueExpr::Alias { .. }
            ) {
                self.object_aliases.insert(local);
            }
            self.core.push_binding(
                local.to_owned(),
                position(node),
                scoped(ValueExpr::Unknown),
                binding_conditional,
            );
            if !matches!(&expression, ValueExpr::Unknown) {
                if immutable && !binding_conditional {
                    self.core.facts_mut().immutable_initializers.push(completed(node));
                }
                self.core.push_binding(
                    local.to_owned(),
                    completed(node),
                    scoped(expression),
                    binding_conditional,
                );
            }
        } else if let (Some(pattern), Some(initializer)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) {
            if matches!(pattern.kind(), "object_pattern" | "array_pattern")
                && pattern.named_child_count() <= 32
            {
                let source_value = value(initializer, source);
                let mut bindings = Vec::new();
                ValueFlowCollectorCore::collect_destructuring_bindings(
                    pattern,
                    source_value,
                    source,
                    &mut bindings,
                );
                let projected = self.core.push_destructured_bindings(
                    bindings,
                    completed(node),
                    Some(position(node)),
                    binding_conditional,
                    scoped,
                );
                if immutable && !binding_conditional && projected {
                    self.core.facts_mut().immutable_initializers.push(completed(node));
                }
            }
        }
    }

    fn record_field_or_assignment_flow(&mut self, node: Syntax<'tree>, conditional: bool) {
        let source = self.source;
        if matches!(
            node.kind(),
            "public_field_definition" | "field_definition" | "property_signature"
        ) {
            if let Some(name) = node.child_by_field_name("name") {
                let value = node
                    .child_by_field_name("type")
                    .map(|ty| named_type(ty, source))
                    .or_else(|| jsdoc(node, source).2)
                    .map(|type_expr| ValueExpr::Annotated { type_expr })
                    .or_else(|| {
                        node.child_by_field_name("value")
                            .map(|initializer| value(initializer, source))
                    })
                    .unwrap_or(ValueExpr::Unknown);
                self.core.push_field(
                    text(name, source).to_owned(),
                    position(node),
                    value,
                    conditional,
                );
            }
            return;
        }
        if !matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression" | "update_expression"
        ) {
            return;
        }
        let left = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("argument"));
        if let Some(name) = left.filter(|left| left.kind() == "identifier") {
            let assigned = if is_simple_assignment(node, source) {
                node.child_by_field_name("right")
                    .map(|right| value(right, source))
                    .unwrap_or(ValueExpr::Unknown)
            } else {
                ValueExpr::Unknown
            };
            self.core.push_binding(
                text(name, source).to_owned(),
                completed(node),
                assigned,
                conditional,
            );
        }
        let Some(member) = left.filter(|left| left.kind() == "member_expression") else {
            return;
        };
        if let Some(receiver) = member
            .child_by_field_name("object")
            .filter(|receiver| receiver.kind() == "identifier")
        {
            let name = text(receiver, source);
            if self.object_aliases.contains(name) {
                for name in std::mem::take(&mut self.object_aliases) {
                    self.core.push_binding(
                        name.to_owned(),
                        completed(node),
                        ValueExpr::Unknown,
                        conditional,
                    );
                }
            }
        }
        if member
            .child_by_field_name("object")
            .is_some_and(|object| object.kind() == "this")
        {
            if let Some(prop) = member
                .child_by_field_name("property")
                .filter(|p| p.kind() == "property_identifier")
            {
                let name = text(prop, source).to_owned();
                if is_simple_assignment(node, source) {
                    let assigned = node
                        .child_by_field_name("right")
                        .map(|right| value(right, source))
                        .unwrap_or(ValueExpr::Unknown);
                    self.core.push_field(
                        name,
                        position(node),
                        assigned,
                        conditional,
                    );
                }
            }
        }
    }

    pub(crate) fn enter(&mut self, node: Syntax<'tree>) {
        if self.terminal {
            return;
        }
        let source = self.source;
        let body = self.root.child_by_field_name("body");
        // Returned object members require direct callable definitions and all writes,
        // including writes in nested closures. Both are recorded by shared events.
        self.record_direct_body_definition(node, body);
        self.record_write(node);
        let current = self.core.scope_state().current_or_default();
        let active = node == self.root || (current.is_active() && !nested_scope(node));
        let annotation_owner = node == self.root
            || (current.is_annotation_owner() && kind(node.kind()).is_none());
        let branch_conditional = current.is_branch_conditional()
            || matches!(
                node.kind(),
                "if_statement"
                    | "switch_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "while_statement"
                    | "do_statement"
                    | "ternary_expression"
            );
        let try_conditional = current.is_try_conditional() || node.kind() == "try_statement";
        self.core.scope_state_mut().push(
            ScopeFrameFlags::new()
                .with_active(active)
                .with_branch_conditional(branch_conditional)
                .with_try_conditional(try_conditional)
                .with_annotation_owner(annotation_owner),
        );
        self.returned.enter(node, active);
        if !active {
            return;
        }
        if annotation_owner
            && node.kind() == "type_query"
            && !node.has_error()
            && node.named_child_count() == 1
        {
            if let Some(identifier) = node.named_child(0).filter(|n| n.kind() == "identifier") {
                self.core.facts_mut().push_annotation_reference(
                    crate::semantic::AnnotationReferenceFact {
                        expression: text(identifier, source).to_owned(),
                        position: position(identifier),
                        value_query: true,
                    },
                );
            }
        }
        let conditional = branch_conditional || try_conditional;
        if node.kind() == "required_parameter" {
            if let (Some(name), Some(annotation)) = (
                node.child_by_field_name("pattern")
                    .filter(|name| name.kind() == "identifier"),
                node.child_by_field_name("type"),
            ) {
                let type_expr = named_type(annotation, source);
                if matches!(&type_expr, TypeExpr::Applied { base, .. } if base == "Promise") {
                    self.core.push_binding(
                        text(name, source).to_owned(),
                        position(node),
                        ValueExpr::Annotated { type_expr },
                        false,
                    );
                }
            }
        }
        if node.kind() == "variable_declarator" {
            self.record_variable_declarator_flow(node, branch_conditional, try_conditional);
        }
        self.record_field_or_assignment_flow(node, conditional);
    }
    pub(crate) fn exit(&mut self, _node: Syntax<'tree>) {
        self.core.scope_state_mut().pop();
    }

    pub(crate) fn finish(mut self) -> ValueFlowFacts {
        if self.terminal {
            return self.core.finish();
        }
        let (source, asynchronous, generator) = (self.source, self.asynchronous, self.generator);
        let definitions = (!self.duplicate_definition
            && !self
                .writes
                .iter()
                .any(|name| self.definitions.contains_key(name)))
        .then_some(&self.definitions);
        if !asynchronous && !generator {
            if let (Some(definitions), Some(expression)) =
                (definitions, self.returned.expression(true))
            {
                setup_summaries::complete_setup(self.root, expression, source, definitions);
            }
        }
        if asynchronous && !generator && self.core.facts().async_return_type.is_none() {
            if let Some(expression) = self.returned.expression(false) {
                if let ValueExpr::Construct { callee } = value(expression, source) {
                    self.core.facts_mut().async_return_type = Some(TypeExpr::Named { name: callee });
                }
            }
        }
        if self.core.facts().return_type.is_none() {
            if let Some(expression) = self.returned.expression(false) {
                let returned = if expression.kind() == "object" {
                    object_value(expression, source, definitions)
                } else {
                    value(expression, source)
                };
                if !matches!(returned, ValueExpr::Unknown) {
                    self.core.facts_mut().return_value = Some(returned);
                    self.core.set_return_position(position(expression));
                }
            }
        }
        self.core.facts_mut().immutable_initializers.sort_unstable();
        self.core.finish()
    }
}

fn nested_scope(node: Syntax<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "generator_function"
            | "generator_function_expression"
            | "arrow_function"
            | "method_definition"
            | "class_declaration"
            | "class"
    )
}

struct ReturnSummary<'tree> {
    root: Syntax<'tree>,
    body: Option<Syntax<'tree>>,
    last: Option<Syntax<'tree>>,
    returns: Vec<usize>,
    invalid: bool,
    setup_invalid: bool,
}
impl<'tree> ReturnSummary<'tree> {
    fn new(root: Syntax<'tree>) -> Self {
        Self {
            root,
            body: root.child_by_field_name("body"),
            last: None,
            returns: Vec::new(),
            invalid: false,
            setup_invalid: false,
        }
    }
    fn enter(&mut self, node: Syntax<'tree>, active: bool) {
        if !active {
            return;
        }
        if node.kind() != "comment" && node.parent() == self.body {
            self.last = Some(node);
        }
        if matches!(
            node.kind(),
            "if_statement"
                | "switch_statement"
                | "for_statement"
                | "for_in_statement"
                | "while_statement"
                | "do_statement"
                | "try_statement"
                | "throw_statement"
                | "ternary_expression"
        ) {
            self.invalid = true;
            if !matches!(node.kind(), "if_statement" | "try_statement") {
                self.setup_invalid = true;
            }
        }
        if matches!(node.kind(), "break_statement" | "continue_statement") {
            self.setup_invalid = true;
        }
        if node.kind() == "return_statement" {
            self.returns.push(node.id());
        }
    }
    fn expression(&self, configured_setup: bool) -> Option<Syntax<'tree>> {
        if !matches!(
            self.root.kind(),
            "function_declaration" | "function_expression" | "arrow_function" | "method_definition"
        ) {
            return None;
        }
        let body = self.body?;
        if body.kind() != "statement_block" {
            return Some(body);
        }
        let last = self.last?;
        // Initialization branches may precede a configured setup's sole final
        // return. Ordinary return inference retains its existing strict policy.
        let invalid = if configured_setup {
            self.setup_invalid
        } else {
            self.invalid
        };
        if invalid
            || last.kind() != "return_statement"
            || self.returns.iter().any(|id| *id != last.id())
        {
            return None;
        }
        last.named_child(0)
    }
}

pub(super) fn extract(root: Syntax<'_>, source: &str) -> ValueFlowFacts {
    extract_with_return(root, source, || jsdoc(root, source).1)
}
impl<'tree> crate::ast::syntax::SyntaxCollector<'tree> for TypeScriptValueFlowCollector<'_, 'tree> {
    type Output = ValueFlowFacts;
    fn enter(&mut self, node: Syntax<'tree>) {
        TypeScriptValueFlowCollector::enter(self, node);
    }
    fn exit(&mut self, node: Syntax<'tree>) {
        TypeScriptValueFlowCollector::exit(self, node);
    }
    fn finish(self) -> ValueFlowFacts {
        TypeScriptValueFlowCollector::finish(self)
    }
}

impl<'source, 'tree> crate::ast::syntax::ScopeVisitor<'source, 'tree> for TypeScriptValueFlowCollector<'source, 'tree> {
    fn enter(&mut self, node: Syntax<'tree>) {
        TypeScriptValueFlowCollector::enter(self, node);
    }

    fn exit(&mut self, node: Syntax<'tree>) {
        TypeScriptValueFlowCollector::exit(self, node);
    }

    fn enter_suspended(&mut self, node: Syntax<'tree>) {
        TypeScriptValueFlowCollector::enter_suspended(self, node);
    }

    fn finish(
        self: Box<Self>,
        _collect_module_aliases: bool,
    ) -> (ValueFlowFacts, Option<HashSet<String>>) {
        ((*self).finish(), None)
    }
}

pub(super) fn extract_with_return(
    root: Syntax<'_>,
    source: &str,
    _documented_return: impl FnOnce() -> Option<TypeExpr>,
) -> ValueFlowFacts {
    crate::ast::syntax::collect(root, TypeScriptValueFlowCollector::new(root, source))
}

fn extract_ts_callable_signature(
    root: Syntax<'_>,
    source: &str,
    return_type: Option<TypeExpr>,
) -> Option<crate::semantic::CallableSignature> {
    if !matches!(
        root.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "method_signature"
            | "generator_function_declaration"
    ) {
        return None;
    }
    let has_receiver = matches!(root.kind(), "method_definition" | "method_signature");
    let mut params = Vec::new();
    let mut has_callback_param = false;
    if let Some(parameters) = root.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if params.len() >= 128 {
                break;
            }
            let name = parameter
                .child_by_field_name("pattern")
                .or_else(|| parameter.child_by_field_name("name"))
                .or_else(|| (parameter.kind() == "identifier").then_some(parameter))
                .filter(|node| node.kind() == "identifier")
                .map(|node| text(node, source).trim().to_owned());
            let mut callback_params = Vec::new();
            let param_ty = parameter
                .child_by_field_name("type")
                .map(|annotation| {
                    callback_params = extract_ts_function_type_params(annotation, source);
                    if !callback_params.is_empty() {
                        has_callback_param = true;
                    }
                    named_type(annotation, source)
                })
                .unwrap_or(TypeExpr::Unknown);
            params.push(crate::semantic::CallableParameter {
                name,
                type_expr: param_ty,
                callback_params,
            });
        }
    }
    if !has_callback_param {
        return None;
    }
    let sig = crate::semantic::CallableSignature {
        has_receiver,
        params,
        return_type,
    };
    sig.valid().then_some(sig)
}

fn extract_ts_function_type_params(annotation: Syntax<'_>, source: &str) -> Vec<TypeExpr> {
    let mut target = annotation;
    if target.kind() == "type_annotation" {
        let mut cursor = target.walk();
        let Some(inner) = target.named_children(&mut cursor).next() else {
            return Vec::new();
        };
        target = inner;
    }
    if target.kind() != "function_type" {
        return Vec::new();
    }
    let Some(parameters) = target.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut cursor = parameters.walk();
    let mut out = Vec::new();
    for parameter in parameters.named_children(&mut cursor) {
        if out.len() >= 32 {
            break;
        }
        let ty = parameter
            .child_by_field_name("type")
            .map_or(TypeExpr::Unknown, |node| named_type(node, source));
        out.push(ty);
    }
    out
}

fn record_ts_callback_params(
    root: Syntax<'_>,
    source: &str,
    core: &mut ValueFlowCollectorCore,
) {
    if !matches!(root.kind(), "arrow_function" | "function_expression") {
        return;
    }
    let Some(arguments) = root.parent().filter(|parent| parent.kind() == "arguments") else {
        return;
    };
    let Some(call) = arguments
        .parent()
        .filter(|parent| parent.kind() == "call_expression")
    else {
        return;
    };
    let mut cursor = arguments.walk();
    let Some(arg_index) = arguments
        .named_children(&mut cursor)
        .filter(|node| node.kind() != "comment")
        .position(|node| node.id() == root.id())
    else {
        return;
    };
    let Some(callee_expr) = callback_call_expr(call, source) else {
        return;
    };
    let at = position(root);
    if let Some(parameter) = root
        .child_by_field_name("parameter")
        .filter(|node| node.kind() == "identifier")
    {
        core.push_callback_param(text(parameter, source), at, callee_expr, arg_index, 0);
        return;
    }
    let Some(parameters) = root.child_by_field_name("parameters") else {
        return;
    };
    let mut p_cursor = parameters.walk();
    for (param_index, parameter) in parameters.named_children(&mut p_cursor).enumerate() {
        if parameter.child_by_field_name("type").is_some() {
            continue;
        }
        let Some(ident) = parameter
            .child_by_field_name("pattern")
            .or_else(|| parameter.child_by_field_name("name"))
            .or_else(|| (parameter.kind() == "identifier").then_some(parameter))
            .filter(|node| node.kind() == "identifier")
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

fn callback_call_expr(call: Syntax<'_>, source: &str) -> Option<ValueExpr> {
    let function = call.child_by_field_name("function")?;
    if function.kind() == "member_expression" {
        let object = function.child_by_field_name("object")?;
        let member = function
            .child_by_field_name("property")
            .filter(|node| node.kind() == "property_identifier")?;
        let receiver = value(object, source);
        if !matches!(receiver, ValueExpr::Unknown) {
            return Some(ValueExpr::Call {
                callee: text(member, source).to_owned(),
                site: Some(crate::semantic::CallSiteEvidence {
                    position: position(call),
                    receiver: Some(Box::new(receiver)),
                    arguments: Vec::new(),
                }),
            });
        }
    }
    let expr = call_value(call, source, 0);
    (!matches!(expr, ValueExpr::Unknown)).then_some(expr)
}

// Occurrence-local lexical proof used by framework handlers, never a scope-exit collector.
pub(super) fn final_return(root: Syntax<'_>) -> Option<Syntax<'_>> {
    let mut summary = ReturnSummary::new(root);
    let mut active = Vec::new();
    crate::ast::syntax::walk(root, &mut |node, entering| {
        if entering {
            let included =
                node == root || (active.last().copied().unwrap_or(false) && !nested_scope(node));
            active.push(included);
            summary.enter(node, included);
        } else {
            active.pop();
        }
    });
    summary.expression(false)
}

fn binding(
    name: &str,
    local: Option<&str>,
    module: Option<&str>,
    type_only: bool,
    star: bool,
) -> ExportBinding {
    ExportBinding {
        name: name.to_owned(),
        local: local.map(str::to_owned),
        module: module.map(str::to_owned),
        type_only,
        star,
    }
}

fn scan_export_imports_and_mutations<'a>(
    root: Syntax<'_>,
    source: &'a str,
) -> (bool, HashSet<&'a str>) {
    let mut commonjs_invalidated = false;
    let mut type_imports = HashSet::new();
    let mut pending = vec![(root, true, true, false)];
    while let Some((node, scan_mutations, scan_imports, inherited_type_only)) = pending.pop() {
        if scan_mutations
            && node.kind() == "assignment_expression"
            && field(node, source, "left").is_some_and(|left| {
                left == "module.exports"
                    || left.starts_with("exports.")
                    || left.starts_with("module.exports.")
            })
            && !node
                .parent()
                .and_then(|parent| parent.parent())
                .is_some_and(|parent| parent.id() == root.id())
        {
            commonjs_invalidated = true;
        }
        let type_only = scan_imports
            && (inherited_type_only
                || (node.kind() == "import_statement"
                    && text(node, source).trim_start().starts_with("import type "))
                || (node.kind() == "import_specifier"
                    && text(node, source).trim_start().starts_with("type ")));
        if type_only && node.kind() == "import_specifier" {
            if let Some(name) = field(node, source, "alias").or_else(|| field(node, source, "name"))
            {
                type_imports.insert(name);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let child_mutations = scan_mutations
                && !matches!(
                    child.kind(),
                    "function_declaration"
                        | "function_expression"
                        | "arrow_function"
                        | "method_definition"
                        | "class_declaration"
                );
            let child_imports = scan_imports
                && matches!(
                    child.kind(),
                    "import_statement" | "import_clause" | "named_imports" | "import_specifier"
                );
            if child_mutations || child_imports {
                pending.push((child, child_mutations, child_imports, type_only));
            }
        }
    }
    (commonjs_invalidated, type_imports)
}

fn collect_es_export_statement(node: Syntax<'_>, source: &str, result: &mut Vec<ExportBinding>) {
    let module = field(node, source, "source").map(|module| module.trim_matches(['\'', '"']));
    let type_only = text(node, source).trim_start().starts_with("export type ");
    let default = text(node, source).trim_start().starts_with("export default ");
    if default {
        if let Some(local) = node
            .child_by_field_name("value")
            .filter(|value| value.kind() == "identifier")
        {
            result.push(binding("default", Some(text(local, source)), None, false, false));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "export_clause" => {
                let mut cursor = child.walk();
                for item in child.named_children(&mut cursor) {
                    if let Some(local) = field(item, source, "name") {
                        result.push(binding(
                            field(item, source, "alias").unwrap_or(local),
                            Some(local),
                            module,
                            type_only || text(item, source).trim_start().starts_with("type "),
                            false,
                        ));
                    }
                }
            }
            "namespace_export" => {
                if let Some(name) = child.named_child(0) {
                    result.push(binding(text(name, source), None, module, type_only, false));
                }
            }
            "function_declaration"
            | "class_declaration"
            | "enum_declaration"
            | "interface_declaration"
            | "type_alias_declaration" => {
                if let Some(name) = field(child, source, "name") {
                    result.push(binding(
                        if default { "default" } else { name },
                        Some(name),
                        None,
                        type_only
                            || matches!(
                                child.kind(),
                                "interface_declaration" | "type_alias_declaration"
                            ),
                        false,
                    ));
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut cursor = child.walk();
                for declaration in child.named_children(&mut cursor) {
                    if let Some(name) = declaration
                        .child_by_field_name("name")
                        .filter(|name| name.kind() == "identifier")
                    {
                        result.push(binding(
                            text(name, source),
                            Some(text(name, source)),
                            None,
                            type_only,
                            false,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if module.is_some() && text(node, source).trim_start().starts_with("export * from") {
        result.push(binding("*", None, module, type_only, true));
    }
}

fn collect_commonjs_assignment(
    node: Syntax<'_>,
    source: &str,
    result: &mut Vec<ExportBinding>,
    module_replacements: &mut usize,
    commonjs_invalidated: &mut bool,
) {
    let Some(assignment) = node
        .named_child(0)
        .filter(|item| item.kind() == "assignment_expression")
    else {
        return;
    };
    let (Some(left), Some(right)) = (
        assignment.child_by_field_name("left"),
        assignment.child_by_field_name("right"),
    ) else {
        return;
    };
    let left = text(left, source).trim();
    if left == "module.exports" {
        *module_replacements += 1;
    }
    let function_local = |right: Syntax<'_>| {
        field(right, source, "name")
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "anonymous@{}:{}",
                    right.start_position().row + 1,
                    right.start_position().column + 1
                )
            })
    };
    if let Some(name) = left
        .strip_prefix("exports.")
        .or_else(|| left.strip_prefix("module.exports."))
    {
        if right.kind() == "identifier" {
            result.push(binding(name, Some(text(right, source)), None, false, false));
        } else if let Some(module) = required_module(right, source) {
            result.push(binding(name, None, Some(&module), false, false));
        } else if matches!(right.kind(), "function_expression" | "arrow_function") {
            let local = function_local(right);
            result.push(binding(name, Some(&local), None, false, false));
        } else {
            result.push(binding(name, None, None, false, false));
        }
        return;
    }
    if left != "module.exports" {
        return;
    }
    if let Some(module) = required_module(right, source) {
        result.push(binding("*", None, Some(&module), false, true));
    } else if matches!(right.kind(), "function_expression" | "arrow_function") {
        let local = function_local(right);
        result.push(binding("default", Some(&local), None, false, false));
    } else if right.kind() == "object" {
        collect_commonjs_object_exports(right, source, result, commonjs_invalidated);
    } else {
        *commonjs_invalidated = true;
    }
}

fn collect_commonjs_object_exports(
    right: Syntax<'_>,
    source: &str,
    result: &mut Vec<ExportBinding>,
    commonjs_invalidated: &mut bool,
) {
    let mut cursor = right.walk();
    for item in right.named_children(&mut cursor) {
        if item.kind() == "spread_element" {
            *commonjs_invalidated = true;
        }
        if item.kind() == "shorthand_property_identifier" {
            result.push(binding(
                text(item, source),
                Some(text(item, source)),
                None,
                false,
                false,
            ));
        } else if let (Some(key), Some(value)) = (
            item.child_by_field_name("key"),
            item.child_by_field_name("value"),
        ) {
            if !matches!(
                key.kind(),
                "property_identifier" | "identifier" | "string"
            ) {
                *commonjs_invalidated = true;
            }
            if value.kind() == "identifier" {
                result.push(binding(
                    text(key, source).trim_matches(['\'', '"']),
                    Some(text(value, source)),
                    None,
                    false,
                    false,
                ));
            }
        }
    }
}

pub(super) fn exports(
    root: Syntax<'_>,
    source: &str,
    bindings: &crate::ast::syntax::ScopeBindings,
) -> Vec<ExportBinding> {
    let mut result = Vec::new();
    let commonjs_shadowed = bindings
        .all
        .iter()
        .any(|name| matches!(name.as_str(), "module" | "exports"));
    let (mut commonjs_invalidated, type_imports) = scan_export_imports_and_mutations(root, source);
    let mut module_replacements = 0;
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() == "export_statement" {
            collect_es_export_statement(node, source, &mut result);
        } else if node.kind() == "expression_statement" && !commonjs_shadowed {
            collect_commonjs_assignment(
                node,
                source,
                &mut result,
                &mut module_replacements,
                &mut commonjs_invalidated,
            );
        }
    }
    if commonjs_invalidated || module_replacements > 1 {
        result.retain(|export| export.type_only);
    }
    for export in &mut result {
        if export.module.is_none()
            && export
                .local
                .as_deref()
                .is_some_and(|name| type_imports.contains(name))
        {
            export.type_only = true;
        }
    }
    result
}
