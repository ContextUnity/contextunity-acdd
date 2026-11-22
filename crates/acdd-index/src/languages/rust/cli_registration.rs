use std::collections::HashSet;

use crate::ast::{field, text};
use crate::languages::{manifests::syntax::SyntaxOperation, LanguageContext};
use tree_sitter::Node;

thread_local! {
    static HANDLERS: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
}

pub(super) fn prepare(root: Node<'_>, source: &str, rules: &LanguageContext) {
    HANDLERS.with(|current| *current.borrow_mut() = handlers(root, source, rules));
}

pub(super) fn contains(name: &str) -> bool {
    HANDLERS.with(|current| current.borrow().contains(name))
}

pub(super) fn clear() {
    HANDLERS.with(|current| current.borrow_mut().clear());
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn derived(node: Node<'_>, source: &str, name: &str) -> bool {
    let mut previous = node.prev_named_sibling();
    while let Some(attribute) = previous.filter(|node| node.kind() == "attribute_item") {
        let raw = compact(text(attribute, source));
        if let Some(values) = raw
            .strip_prefix("#[derive(")
            .and_then(|s| s.strip_suffix(")]"))
        {
            if values.split(',').any(|value| value == name) {
                return true;
            }
        }
        previous = attribute.prev_named_sibling();
    }
    false
}

struct CommandDispatchSpec<'a> {
    parse_method: &'a str,
    parser: &'a str,
    command: &'a str,
    member: &'a str,
    variants: HashSet<String>,
}

fn resolve_command_dispatch_spec<'a>(
    top: &[Node<'a>],
    source: &'a str,
    operation: &'a SyntaxOperation,
) -> Option<CommandDispatchSpec<'a>> {
    let SyntaxOperation::CommandDispatch {
        module,
        parser_derive,
        subcommand_derive,
        parse_method,
        field_attribute,
        field_marker,
    } = operation
    else {
        return None;
    };
    if top
        .iter()
        .any(|node| node.kind() == "mod_item" && field(*node, source, "name") == Some(module))
    {
        return None;
    }
    let imports = top
        .iter()
        .filter(|node| node.kind() == "use_declaration")
        .map(|node| compact(text(*node, source)))
        .collect::<Vec<_>>();
    let grouped = format!("use{module}::{{{parser_derive},{subcommand_derive}}};");
    let grouped_reverse = format!("use{module}::{{{subcommand_derive},{parser_derive}}};");
    if imports
        .iter()
        .filter(|value| value.contains(parser_derive))
        .count()
        != 1
        || imports
            .iter()
            .filter(|value| value.contains(subcommand_derive))
            .count()
            != 1
    {
        return None;
    }
    if !imports
        .iter()
        .any(|value| value == &grouped || value == &grouped_reverse)
        && !(imports.contains(&format!("use{module}::{parser_derive};"))
            && imports.contains(&format!("use{module}::{subcommand_derive};")))
    {
        return None;
    }
    if top.iter().any(|node| {
        matches!(
            node.kind(),
            "struct_item" | "enum_item" | "type_item" | "trait_item"
        ) && field(*node, source, "name")
            .is_some_and(|name| name == parser_derive || name == subcommand_derive)
    }) {
        return None;
    }
    let parsers: Vec<_> = top
        .iter()
        .copied()
        .filter(|node| node.kind() == "struct_item" && derived(*node, source, parser_derive))
        .collect();
    let enums: Vec<_> = top
        .iter()
        .copied()
        .filter(|node| node.kind() == "enum_item" && derived(*node, source, subcommand_derive))
        .collect();
    if parsers.len() != 1 || enums.len() != 1 {
        return None;
    }
    let parser = field(parsers[0], source, "name")?;
    if top
        .iter()
        .any(|node| node.kind() == "impl_item" && text(*node, source).contains(parser))
    {
        return None;
    }
    let command = field(enums[0], source, "name")?;
    let mut variants = HashSet::new();
    if let Some(body) = enums[0].child_by_field_name("body") {
        let mut cursor = body.walk();
        for variant in body
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "enum_variant")
        {
            if let Some(name) = field(variant, source, "name") {
                variants.insert(name.to_owned());
            }
        }
    }
    let parser_text = compact(text(parsers[0], source));
    let marker = format!("#[{field_attribute}({field_marker})]");
    let after_marker = parser_text.split_once(&marker).map(|(_, after)| after)?;
    let (member_raw, _) = after_marker.split_once(&format!(":{command}"))?;
    if member_raw.is_empty() || !member_raw.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let member = parsers[0]
        .child_by_field_name("body")
        .and_then(|body| {
            let mut cursor = body.walk();
            let found = body
                .named_children(&mut cursor)
                .filter_map(|node| field(node, source, "name"))
                .find(|name| *name == member_raw);
            found
        })?;
    Some(CommandDispatchSpec {
        parse_method,
        parser,
        command,
        member,
        variants,
    })
}

fn has_nested_type_conflict(body: Node<'_>, source: &str, parser: &str, command: &str) -> bool {
    let mut inspect = vec![body];
    while let Some(node) = inspect.pop() {
        if matches!(
            node.kind(),
            "struct_item" | "enum_item" | "type_item" | "use_declaration"
        ) && (text(node, source).contains(parser) || text(node, source).contains(command))
        {
            return true;
        }
        let mut cursor = node.walk();
        inspect.extend(node.named_children(&mut cursor));
    }
    false
}

fn collect_match_handlers(
    match_node: Node<'_>,
    function: Node<'_>,
    body: Node<'_>,
    top: &[Node<'_>],
    source: &str,
    spec: &CommandDispatchSpec<'_>,
    result: &mut HashSet<String>,
) {
    let signature = &source[function.start_byte()..body.start_byte()];
    let mut stack = vec![match_node];
    while let Some(child) = stack.pop() {
        if child.kind() != "match_arm" {
            let mut cursor = child.walk();
            stack.extend(child.named_children(&mut cursor));
            continue;
        }
        let (Some(pattern), Some(value)) = (
            child.child_by_field_name("pattern"),
            child.child_by_field_name("value"),
        ) else {
            continue;
        };
        let pattern = compact(text(pattern, source));
        let Some(variant) = pattern.strip_prefix(&format!("{}::", spec.command)) else {
            continue;
        };
        if !spec.variants.contains(variant) || value.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = value
            .child_by_field_name("function")
            .filter(|value| value.kind() == "identifier")
        else {
            continue;
        };
        let name = text(callee, source);
        let unique_top = top
            .iter()
            .filter(|node| node.kind() == "function_item" && field(**node, source, "name") == Some(name))
            .count()
            == 1;
        if unique_top && !signature.contains(name) && text(body, source).matches(name).count() == 1 {
            result.insert(name.to_owned());
        }
    }
}

fn collect_function_handlers(
    function: Node<'_>,
    top: &[Node<'_>],
    source: &str,
    spec: &CommandDispatchSpec<'_>,
    result: &mut HashSet<String>,
) {
    let Some(body) = function.child_by_field_name("body") else {
        return;
    };
    let mut body_cursor = body.walk();
    let bindings: Vec<_> = body
        .named_children(&mut body_cursor)
        .filter(|node| node.kind() == "let_declaration")
        .filter_map(|node| {
            let pattern = node.child_by_field_name("pattern")?;
            let value = node.child_by_field_name("value")?;
            (pattern.kind() == "identifier"
                && compact(text(value, source)) == format!("{}::{}()", spec.parser, spec.parse_method))
            .then(|| text(pattern, source).to_owned())
        })
        .collect();
    if bindings.len() != 1 {
        return;
    }
    let binding = &bindings[0];
    let body_text = text(body, source);
    let sig = &source[function.start_byte()..body.start_byte()];
    if body_text.matches(binding.as_str()).count() != 2
        || body_text.matches(spec.parser).count() != 1
        || sig.contains(spec.parser)
        || sig.contains(spec.command)
        || has_nested_type_conflict(body, source, spec.parser, spec.command)
    {
        return;
    }
    let target = format!("{binding}.{}", spec.member);
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if node.kind() == "match_expression"
            && node
                .child_by_field_name("value")
                .is_some_and(|value| compact(text(value, source)) == target)
        {
            collect_match_handlers(node, function, body, top, source, spec, result);
            continue;
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
}

pub(super) fn handlers(root: Node<'_>, source: &str, rules: &LanguageContext) -> HashSet<String> {
    let mut result = HashSet::new();
    let mut cursor = root.walk();
    let top: Vec<_> = root.named_children(&mut cursor).collect();
    for rule in &rules.syntax_rules {
        let Some(spec) = resolve_command_dispatch_spec(&top, source, &rule.operation) else {
            continue;
        };
        for function in top.iter().copied().filter(|node| node.kind() == "function_item") {
            collect_function_handlers(function, &top, source, &spec, &mut result);
        }
    }
    result
}
