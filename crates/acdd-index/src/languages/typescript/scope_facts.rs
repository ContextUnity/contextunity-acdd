use super::prototypes::{has_static_child, lexical_this_method};
use crate::ast::syntax::{field, text, Syntax};
use crate::languages::context::FileContext;
use crate::languages::types::DomListenerReceiverExpression;

fn dom_global_name(name: &str) -> Option<&'static str> {
    match name {
        "document" => Some("document"),
        "window" => Some("window"),
        "Event" => Some("Event"),
        "CustomEvent" => Some("CustomEvent"),
        "MouseEvent" => Some("MouseEvent"),
        "KeyboardEvent" => Some("KeyboardEvent"),
        "EventTarget" => Some("EventTarget"),
        "Element" => Some("Element"),
        "HTMLElement" => Some("HTMLElement"),
        "HTMLTableElement" => Some("HTMLTableElement"),
        "HTMLDivElement" => Some("HTMLDivElement"),
        "HTMLInputElement" => Some("HTMLInputElement"),
        "HTMLButtonElement" => Some("HTMLButtonElement"),
        "HTMLAnchorElement" => Some("HTMLAnchorElement"),
        "Document" => Some("Document"),
        "Window" => Some("Window"),
        _ => None,
    }
}

fn remember_dom_global(name: &str, names: &mut [Option<&'static str>; 16], count: &mut usize) {
    let Some(global) = dom_global_name(name.trim()) else {
        return;
    };
    if *count < names.len() && !names[..*count].contains(&Some(global)) {
        names[*count] = Some(global);
        *count += 1;
    }
}

fn collect_dom_pattern_globals(
    pattern: Syntax<'_>,
    source: &str,
    names: &mut [Option<&'static str>; 16],
    count: &mut usize,
) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            remember_dom_global(text(pattern, source), names, count);
        }
        "pair_pattern" => {
            if let Some(value) = pattern.child_by_field_name("value") {
                collect_dom_pattern_globals(value, source, names, count);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = pattern.child_by_field_name("left") {
                collect_dom_pattern_globals(left, source, names, count);
            }
        }
        "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern"
        | "required_parameter" | "optional_parameter" | "formal_parameter" => {
            if let Some(candidate) = pattern
                .child_by_field_name("name")
                .or_else(|| pattern.child_by_field_name("pattern"))
            {
                collect_dom_pattern_globals(candidate, source, names, count);
                return;
            }
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor) {
                collect_dom_pattern_globals(child, source, names, count);
            }
        }
        _ => {}
    }
}

fn collect_dom_import_globals(
    node: Syntax<'_>,
    source: &str,
    names: &mut [Option<&'static str>; 16],
    count: &mut usize,
) {
    match node.kind() {
        "import_clause" | "namespace_import" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    remember_dom_global(text(child, source), names, count);
                }
            }
        }
        "import_specifier" => {
            if let Some(local) = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
            {
                remember_dom_global(text(local, source), names, count);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_dom_import_globals(child, source, names, count);
    }
}

pub(super) fn nearest_binding_scope(node: Syntax<'_>, var_scoped: bool) -> Option<Syntax<'_>> {
    let mut current = Some(node);
    while let Some(scope) = current {
        if var_scoped {
            if matches!(
                scope.kind(),
                "program"
                    | "source_file"
                    | "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "generator_function"
                    | "generator_function_expression"
                    | "arrow_function"
                    | "method_definition"
            ) {
                return Some(scope);
            }
        } else if matches!(
            scope.kind(),
            "statement_block"
                | "catch_clause"
                | "for_statement"
                | "for_in_statement"
                | "program"
                | "source_file"
        ) {
            return Some(scope);
        }
        current = scope.parent();
    }
    None
}

pub(super) fn collect_array_shadow_fact(node: Syntax<'_>, source: &str, file: &mut FileContext) {
    let (array_binding, array_var_scoped) = match node.kind() {
        "variable_declarator" => (
            node.child_by_field_name("name")
                .is_some_and(|name| name.kind() == "identifier" && text(name, source) == "Array"),
            node.parent()
                .is_some_and(|parent| parent.kind() == "variable_declaration"),
        ),
        "class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration"
        | "function_declaration" => (field(node, source, "name") == Some("Array"), false),
        "type_parameter" => (field(node, source, "name") == Some("Array"), false),
        "required_parameter" | "optional_parameter" => {
            (field(node, source, "name") == Some("Array"), true)
        }
        "import_specifier" => (
            node.child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
                .is_some_and(|name| text(name, source) == "Array"),
            false,
        ),
        "import_clause" => (
            node.named_child(0)
                .is_some_and(|name| name.kind() == "identifier" && text(name, source) == "Array"),
            false,
        ),
        "namespace_import" => (
            node.named_child(0)
                .is_some_and(|name| name.kind() == "identifier" && text(name, source) == "Array"),
            false,
        ),
        _ => (false, false),
    };
    if array_binding {
        let scope = if node.kind() == "type_parameter" {
            let mut current = node.parent();
            let mut owner = None;
            while let Some(candidate) = current {
                if matches!(
                    candidate.kind(),
                    "class_declaration"
                        | "class"
                        | "function_declaration"
                        | "function_expression"
                        | "arrow_function"
                        | "method_definition"
                ) {
                    owner = Some(candidate);
                    break;
                }
                current = candidate.parent();
            }
            owner
        } else {
            nearest_binding_scope(node, array_var_scoped)
        };
        if let Some(scope) = scope {
            std::sync::Arc::make_mut(
                &mut file.state.typescript_mut().shadowed_array_type_scopes,
            )
            .insert(scope.id());
        }
    }
}

pub(super) fn collect_rebound_parameter_fact(
    node: Syntax<'_>,
    source: &str,
    file: &mut FileContext,
) {
    let rebound = match node.kind() {
        "assignment_expression" | "augmented_assignment_expression" => {
            node.child_by_field_name("left")
        }
        "variable_declarator" => node.child_by_field_name("name"),
        "update_expression" => node.named_child(0),
        _ => None,
    };
    let Some(name) = rebound.filter(|name| name.kind() == "identifier") else {
        return;
    };
    let name = text(name, source);
    let mut current = node.parent();
    while let Some(scope) = current {
        if matches!(
            scope.kind(),
            "function_declaration" | "method_definition" | "function_expression" | "arrow_function"
        ) {
            std::sync::Arc::make_mut(
                &mut file.state.typescript_mut().rebound_function_parameters,
            )
            .entry(scope.id())
            .or_default()
            .insert(name.to_owned());
        }
        current = scope.parent();
    }
}

pub(super) fn collect_dom_global_shadow_fact(
    node: Syntax<'_>,
    source: &str,
    file: &mut FileContext,
) {
    let mut shadowed_globals = [None; 16];
    let mut shadowed_count = 0;
    let mut var_scoped = false;
    match node.kind() {
        "variable_declarator" => {
            var_scoped = node
                .parent()
                .is_some_and(|parent| parent.kind() == "variable_declaration");
            if let Some(name) = node.child_by_field_name("name") {
                collect_dom_pattern_globals(
                    name,
                    source,
                    &mut shadowed_globals,
                    &mut shadowed_count,
                );
            }
        }
        "formal_parameters" | "required_parameter" | "optional_parameter" | "formal_parameter" => {
            var_scoped = true;
            collect_dom_pattern_globals(node, source, &mut shadowed_globals, &mut shadowed_count);
        }
        "arrow_function" => {
            var_scoped = true;
            if let Some(parameter) = node.child_by_field_name("parameter") {
                collect_dom_pattern_globals(
                    parameter,
                    source,
                    &mut shadowed_globals,
                    &mut shadowed_count,
                );
            }
        }
        "function_declaration"
        | "class_declaration"
        | "enum_declaration"
        | "interface_declaration"
        | "type_alias_declaration" => {
            if let Some(name) = field(node, source, "name") {
                remember_dom_global(name, &mut shadowed_globals, &mut shadowed_count);
            }
        }
        "function_expression" => {
            var_scoped = true;
            if let Some(name) = field(node, source, "name") {
                remember_dom_global(name, &mut shadowed_globals, &mut shadowed_count);
            }
        }
        "import_statement" => {
            collect_dom_import_globals(node, source, &mut shadowed_globals, &mut shadowed_count)
        }
        "catch_clause" => {
            if let Some(parameter) = node.child_by_field_name("parameter") {
                collect_dom_pattern_globals(
                    parameter,
                    source,
                    &mut shadowed_globals,
                    &mut shadowed_count,
                );
            }
        }
        "assignment_expression" | "augmented_assignment_expression" => {
            var_scoped = true;
            if let Some(left) = node
                .child_by_field_name("left")
                .filter(|left| left.kind() == "identifier")
            {
                remember_dom_global(
                    text(left, source),
                    &mut shadowed_globals,
                    &mut shadowed_count,
                );
            }
        }
        _ => {}
    }
    if shadowed_count > 0 {
        if let Some(scope) = nearest_binding_scope(node, var_scoped) {
            let by_scope = std::sync::Arc::make_mut(
                &mut file.state.typescript_mut().shadowed_dom_global_scopes,
            );
            let globals = by_scope.entry(scope.id()).or_default();
            for global in shadowed_globals[..shadowed_count].iter().flatten() {
                globals.insert((*global).to_owned());
            }
        }
    }
}

fn pattern_binds_name(pattern: Syntax<'_>, source: &str, name: &str) -> bool {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            text(pattern, source).trim() == name
        }
        "pair_pattern" => pattern
            .child_by_field_name("value")
            .is_some_and(|value| pattern_binds_name(value, source, name)),
        "assignment_pattern" => pattern
            .child_by_field_name("left")
            .is_some_and(|left| pattern_binds_name(left, source, name)),
        "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern"
        | "required_parameter" | "optional_parameter" | "formal_parameter" => {
            let candidate = pattern
                .child_by_field_name("name")
                .or_else(|| pattern.child_by_field_name("pattern"));
            if let Some(candidate) = candidate {
                return pattern_binds_name(candidate, source, name);
            }
            let mut cursor = pattern.walk();
            let binds = pattern
                .named_children(&mut cursor)
                .any(|child| pattern_binds_name(child, source, name));
            binds
        }
        _ => false,
    }
}

pub(super) fn pattern_binds_require(pattern: Syntax<'_>, source: &str) -> bool {
    pattern_binds_name(pattern, source, "require")
}

pub(super) fn collect_pattern_binding_names(
    pattern: Syntax<'_>,
    source: &str,
    names: &mut Vec<String>,
) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            names.push(text(pattern, source).to_owned());
        }
        "pair_pattern" => {
            if let Some(value) = pattern.child_by_field_name("value") {
                collect_pattern_binding_names(value, source, names);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = pattern.child_by_field_name("left") {
                collect_pattern_binding_names(left, source, names);
            }
        }
        "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern"
        | "required_parameter" | "optional_parameter" | "formal_parameter" => {
            if let Some(candidate) = pattern
                .child_by_field_name("name")
                .or_else(|| pattern.child_by_field_name("pattern"))
            {
                collect_pattern_binding_names(candidate, source, names);
                return;
            }
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor) {
                collect_pattern_binding_names(child, source, names);
            }
        }
        _ => {}
    }
}

#[derive(Default)]
pub(super) struct ScopeFactNeeds {
    pub dom_scope_facts: bool,
    pub array_callback_scope_facts: bool,
    pub dom_array_receiver_facts: bool,
    pub commonjs_scope_facts: bool,
}

const ARRAY_CALLBACK_METHODS: &[&str] = &[
    "forEach", "map", "filter", "find", "some", "every", "flatMap",
];
const DOM_ARRAY_RECEIVER_METHODS: &[&str] = &[
    "querySelectorAll",
    "getElementsByTagName",
    "getElementsByClassName",
];

#[derive(Default)]
struct MemberCandidates {
    dom_event_property: bool,
    add_event_listener: bool,
    array_callback: bool,
    dom_array_receiver: bool,
}

fn is_array_callback_property(property: &[u8]) -> bool {
    ARRAY_CALLBACK_METHODS.iter().any(|method| {
        property.starts_with(method.as_bytes())
            && property.get(method.len()).is_none_or(|byte| {
                !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'$') && *byte < 0x80
            })
    })
}

fn is_dom_event_property(property: &[u8]) -> bool {
    property.starts_with(b"on") && property.len() > 2
}

fn is_dom_array_receiver_property(property: &[u8]) -> bool {
    DOM_ARRAY_RECEIVER_METHODS
        .iter()
        .any(|method| property == method.as_bytes())
}

fn is_scope_trivia(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{200b}' | '\u{2060}' | '\u{feff}')
}

fn is_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn member_candidates(source: &str) -> MemberCandidates {
    let bytes = source.as_bytes();
    let mut candidates = MemberCandidates::default();
    let mut dot = 0;
    while dot < bytes.len() {
        if bytes[dot] != b'.' {
            dot += 1;
            continue;
        }
        let mut property = dot + 1;
        loop {
            while let Some(character) = source.get(property..).and_then(|rest| rest.chars().next())
            {
                if !is_scope_trivia(character) {
                    break;
                }
                property += character.len_utf8();
            }
            if bytes.get(property..property + 2) == Some(b"//") {
                property += 2;
                while let Some(character) =
                    source.get(property..).and_then(|rest| rest.chars().next())
                {
                    if is_line_terminator(character) {
                        break;
                    }
                    property += character.len_utf8();
                }
                continue;
            }
            if bytes.get(property..property + 2) == Some(b"/*") {
                property += 2;
                while property + 1 < bytes.len() && bytes.get(property..property + 2) != Some(b"*/")
                {
                    property += 1;
                }
                property = (property + 2).min(bytes.len());
                continue;
            }
            break;
        }
        let start = property;
        while let Some(character) = source.get(property..).and_then(|rest| rest.chars().next()) {
            let identifier_continuation = if character.is_ascii() {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
            } else {
                !is_scope_trivia(character)
            };
            if !identifier_continuation {
                break;
            }
            property += character.len_utf8();
        }
        let name = &bytes[start..property];
        candidates.dom_event_property |=
            is_dom_event_property(name) || (name == b"on" && bytes.get(property) == Some(&b'\\'));
        candidates.add_event_listener |= name == b"addEventListener";
        candidates.array_callback |= is_array_callback_property(name);
        candidates.dom_array_receiver |= is_dom_array_receiver_property(name);
        dot += 1;
    }
    candidates
}

pub(super) fn classify_scope_fact_needs(
    root: Syntax<'_>,
    source: &str,
    javascript: bool,
) -> ScopeFactNeeds {
    let mut candidates = member_candidates(source);
    candidates.add_event_listener |= source.contains("addEventListener");
    candidates.dom_array_receiver |= candidates.array_callback
        && DOM_ARRAY_RECEIVER_METHODS
            .iter()
            .any(|method| source.contains(method));
    let commonjs_candidate = source.contains("require");
    if !commonjs_candidate
        && !candidates.dom_event_property
        && !candidates.add_event_listener
        && !candidates.array_callback
        && !candidates.dom_array_receiver
    {
        return ScopeFactNeeds::default();
    }

    let mut needs = ScopeFactNeeds::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if commonjs_candidate && node.kind() == "identifier" && text(node, source) == "require" {
            needs.commonjs_scope_facts = true;
        }
        if node.kind() == "member_expression" {
            if let Some(property) = node
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")
            {
                let name = text(property, source).as_bytes();
                needs.dom_scope_facts |= (candidates.dom_event_property
                    && is_dom_event_property(name))
                    || (candidates.add_event_listener && name == b"addEventListener");
                needs.array_callback_scope_facts |=
                    candidates.array_callback && is_array_callback_property(name);
                needs.dom_array_receiver_facts |=
                    candidates.dom_array_receiver && is_dom_array_receiver_property(name);
            }
        }

        // TypeScript's full collector subsumes all partial facts. JavaScript also
        // needs the partial collector for DOM callbacks, so require DOM evidence
        // (or its absence from the candidate set) before stopping on CommonJS.
        let dispatch_complete = if javascript {
            needs.dom_scope_facts
                || (needs.commonjs_scope_facts
                    && !candidates.dom_event_property
                    && !candidates.add_event_listener)
        } else {
            needs.dom_scope_facts
                || needs.commonjs_scope_facts
                || (needs.array_callback_scope_facts && needs.dom_array_receiver_facts)
        };
        if dispatch_complete {
            break;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    needs
}

fn dom_scope_chain(node: Syntax<'_>) -> Vec<usize> {
    let mut scopes = Vec::new();
    let mut current = Some(node);
    while let Some(scope) = current {
        scopes.push(scope.id());
        current = scope.parent();
    }
    scopes
}

fn dom_listener_receiver_expression(
    node: Syntax<'_>,
    source: &str,
    depth: usize,
) -> DomListenerReceiverExpression {
    if depth > 4 {
        return DomListenerReceiverExpression::Unsupported;
    }
    match node.kind() {
        "identifier" => DomListenerReceiverExpression::Identifier {
            name: text(node, source).to_owned(),
            position: node.start_byte(),
        },
        "member_expression" => {
            let Some(object) = node.child_by_field_name("object") else {
                return DomListenerReceiverExpression::Unsupported;
            };
            let Some(name) = node
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")
            else {
                return DomListenerReceiverExpression::Unsupported;
            };
            if object.kind() != "this" {
                return DomListenerReceiverExpression::Unsupported;
            }
            let Some((method, class)) = lexical_this_method(node) else {
                return DomListenerReceiverExpression::Unsupported;
            };
            let static_method = has_static_child(method);
            DomListenerReceiverExpression::ThisField {
                name: text(name, source).to_owned(),
                class_scope: class.id(),
                static_method,
            }
        }
        "call_expression" => {
            let Some(callee) = node
                .child_by_field_name("function")
                .filter(|callee| callee.kind() == "member_expression")
            else {
                return DomListenerReceiverExpression::Unsupported;
            };
            let Some(receiver) = callee.child_by_field_name("object") else {
                return DomListenerReceiverExpression::Unsupported;
            };
            let Some(method) = callee
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")
            else {
                return DomListenerReceiverExpression::Unsupported;
            };
            let has_arguments = node
                .child_by_field_name("arguments")
                .is_some_and(|arguments| arguments.named_child_count() > 0);
            let type_arguments = node
                .child_by_field_name("type_arguments")
                .map(|arguments| {
                    arguments
                        .named_children(&mut arguments.walk())
                        .map(|argument| text(argument, source).to_owned())
                        .collect()
                })
                .unwrap_or_default();
            DomListenerReceiverExpression::MemberCall {
                receiver: Box::new(dom_listener_receiver_expression(
                    receiver,
                    source,
                    depth + 1,
                )),
                method: text(method, source).to_owned(),
                type_arguments,
                has_arguments,
            }
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|inner| {
                DomListenerReceiverExpression::Parenthesized(Box::new(
                    dom_listener_receiver_expression(inner, source, depth + 1),
                ))
            })
            .unwrap_or_default(),
        _ => DomListenerReceiverExpression::Unsupported,
    }
}

pub(super) fn index_dom_listener_declarator(
    node: Syntax<'_>,
    source: &str,
    file: &mut FileContext,
) {
    let Some(binding) = node.child_by_field_name("name") else {
        return;
    };
    let Some(statement) = node.parent().filter(|statement| {
        matches!(
            statement.kind(),
            "lexical_declaration" | "variable_declaration"
        )
    }) else {
        return;
    };
    let Some(scope) = statement
        .parent()
        .filter(|scope| matches!(scope.kind(), "statement_block" | "program" | "source_file"))
    else {
        return;
    };
    let mut names = Vec::new();
    collect_pattern_binding_names(binding, source, &mut names);
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return;
    }
    let is_const = statement.kind() == "lexical_declaration"
        && statement
            .child_by_field_name("kind")
            .is_some_and(|keyword| text(keyword, source) == "const");
    let initializer = if is_const {
        node.child_by_field_name("value")
            .map(|value| dom_listener_receiver_expression(value, source, 0))
    } else {
        None
    };
    let scope_chain = std::sync::Arc::new(dom_scope_chain(scope));
    let declarations =
        std::sync::Arc::make_mut(&mut file.state.typescript_mut().dom_listener_local_declarations);
    let block = declarations.entry(scope.id()).or_default();
    for name in names {
        let declaration = block.entry(name).or_default();
        declaration.declaration_count += 1;
        if declaration.declaration_count == 1 {
            declaration.is_const = is_const;
            declaration.statement_end = statement.end_byte();
            declaration.initializer = initializer.clone();
            declaration.scope_chain = std::sync::Arc::clone(&scope_chain);
        } else {
            declaration.initializer = None;
        }
    }
}

pub(super) fn collect_array_callback_scope_facts(
    root: Syntax<'_>,
    source: &str,
    file: &mut FileContext,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_declarator" {
            index_dom_listener_declarator(node, source, file);
        }
        collect_dom_global_shadow_fact(node, source, file);
        collect_array_shadow_fact(node, source, file);
        collect_rebound_parameter_fact(node, source, file);
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}
