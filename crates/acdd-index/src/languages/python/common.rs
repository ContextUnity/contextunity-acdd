use crate::ast::syntax::{field, symbol_name as syntax_symbol_name, text, Syntax};

/// Performs language.
pub fn language() -> tree_sitter::Language {
    tree_sitter_python::language()
}

/// Performs kind.
pub fn kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" | "lambda" => Some("function"),
        "class_definition" => Some("class"),
        "type_alias_statement" => Some("type"),
        _ => None,
    }
}

pub(super) fn symbol_name<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() == "assignment" {
        return field(node, source, "left");
    }
    if node.kind() == "type_alias_statement" {
        let mut name = node.named_child(0)?;
        while matches!(name.kind(), "type" | "generic_type") {
            name = name.named_child(0)?;
        }
        return (name.kind() == "identifier").then(|| text(name, source));
    }
    syntax_symbol_name(node, source)
}

pub(super) fn module_scope(node: Syntax<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        match ancestor.kind() {
            "function_definition" | "lambda" | "class_definition" => return false,
            "module" => return true,
            _ => parent = ancestor.parent(),
        }
    }
    false
}

pub(super) fn direct_module_assignment(node: Syntax<'_>) -> bool {
    node.kind() == "assignment"
        && node
            .child_by_field_name("left")
            .is_some_and(|left| left.kind() == "identifier")
        && node
            .parent()
            .filter(|parent| parent.kind() == "expression_statement")
            .and_then(|parent| parent.parent())
            .is_some_and(|parent| parent.kind() == "module")
}

pub(super) fn typing_name(value: &str) -> &str {
    value
        .strip_prefix("typing.")
        .or_else(|| value.strip_prefix("typing_extensions."))
        .unwrap_or(value)
}

pub(crate) fn split_python_pipe_types(annotation: &str) -> Option<Vec<&str>> {
    let mut depth = 0_usize;
    let mut start = 0_usize;
    let mut parts = Vec::new();
    for (index, character) in annotation.char_indices() {
        match character {
            '[' | '(' => depth = depth.checked_add(1)?,
            ']' | ')' => depth = depth.checked_sub(1)?,
            '|' if depth == 0 => {
                let part = annotation[start..index].trim();
                if part.is_empty() {
                    return None;
                }
                parts.push(part);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 || parts.is_empty() {
        return None;
    }
    let last = annotation[start..].trim();
    if last.is_empty() {
        return None;
    }
    parts.push(last);
    Some(parts)
}

fn is_type_alias_rhs(node: Syntax<'_>, source: &str) -> bool {
    match node.kind() {
        "subscript" => true,
        "binary_operator" => field(node, source, "operator").is_some_and(|op| op == "|"),
        "identifier" => {
            let name = text(node, source);
            matches!(
                name,
                "int"
                    | "str"
                    | "float"
                    | "bool"
                    | "bytes"
                    | "dict"
                    | "list"
                    | "set"
                    | "tuple"
                    | "object"
                    | "None"
                    | "Any"
            ) || name.ends_with("Type")
                || name.ends_with("Dict")
                || name.ends_with("Value")
                || name.ends_with("Primitive")
                || name.ends_with("Mapping")
                || name.ends_with("Payload")
        }
        "attribute" => {
            let name = field(node, source, "attribute").unwrap_or_default();
            matches!(name, "Any" | "Type" | "None")
                || name.ends_with("Type")
                || name.ends_with("Dict")
                || name.ends_with("Value")
                || name.ends_with("Primitive")
                || name.ends_with("Mapping")
                || name.ends_with("Payload")
        }
        _ => false,
    }
}

pub(super) fn type_alias(node: Syntax<'_>, source: &str) -> bool {
    if !matches!(node.kind(), "assignment" | "type_alias_statement") {
        return false;
    }
    if node.kind() == "type_alias_statement" {
        return module_scope(node);
    }
    if !node
        .child_by_field_name("left")
        .is_some_and(|left| left.kind() == "identifier")
    {
        return false;
    }
    if field(node, source, "type")
        .is_some_and(|annotation| matches!(typing_name(annotation), "TypeAlias" | "TypeAliasType"))
    {
        return module_scope(node);
    }
    if node
        .child_by_field_name("right")
        .filter(|right| right.kind() == "call")
        .and_then(|call| field(call, source, "function"))
        .is_some_and(|callee| {
            matches!(typing_name(callee), "TypeVar" | "NewType" | "TypeAliasType")
        })
        && module_scope(node)
    {
        return true;
    }
    if !module_scope(node) {
        return false;
    }
    let Some(left) = node.child_by_field_name("left") else {
        return false;
    };
    let left_name = text(left, source);
    if !left_name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return false;
    }
    let Some(right) = node.child_by_field_name("right") else {
        return false;
    };
    is_type_alias_rhs(right, source)
}

pub(crate) fn normalize_python_import(
    owner: &str,
    module: &str,
) -> Option<crate::languages::types::ImportPath> {
    if module.starts_with('.') {
        let count = module.bytes().take_while(|b| *b == b'.').count();
        let path = format!(
            "{}{}",
            "../".repeat(count - 1),
            module[count..].replace('.', "/")
        );
        Some(crate::languages::types::ImportPath {
            namespace: crate::languages::types::relative_namespace(owner, &path)?,
            relative: true,
            symbol_path: false,
        })
    } else {
        Some(crate::languages::types::ImportPath::absolute(module.into()))
    }
}
