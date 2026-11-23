use super::prototypes::lexical_this_method;
use super::scope_facts::{
    self, collect_array_shadow_fact, collect_dom_global_shadow_fact, collect_pattern_binding_names,
    collect_rebound_parameter_fact, nearest_binding_scope, pattern_binds_require,
};
use crate::ast::syntax::{field, text, Syntax};
use crate::languages::context::FileContext;
use crate::languages::manifests;
use crate::languages::types::{DomListenerLocalDeclaration, DomListenerReceiverExpression};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Resolves a DOM receiver type name to a canonical receiver classification.
pub fn typed_dom_receiver(name: &str) -> Option<&'static str> {
    match name {
        "Document" | "document" => Some("Document"),
        "Window" | "window" => Some("Window"),
        "EventTarget" => Some("EventTarget"),
        "Event" | "CustomEvent" | "MouseEvent" | "KeyboardEvent" | "DragEvent"
        | "ClipboardEvent" | "FocusEvent" => Some("Event"),
        "URL" => Some("URL"),
        "URLSearchParams" => Some("URLSearchParams"),
        "Headers" => Some("Headers"),
        "DOMTokenList" => Some("DOMTokenList"),
        "Storage" => Some("Storage"),
        "Element" | "HTMLElement" | "Node" => Some("Element"),
        "ParentNode" => Some("ParentNode"),
        "ChildNode" => Some("ChildNode"),
        "DocumentFragment" => Some("DocumentFragment"),
        "Location" | "location" => Some("Location"),
        "History" | "history" => Some("History"),
        "Navigator" | "navigator" => Some("Navigator"),
        "Clipboard" => Some("Clipboard"),
        "DOMRect" => Some("DOMRect"),
        "CSSStyleDeclaration" => Some("CSSStyleDeclaration"),
        "DOMStringMap" => Some("DOMStringMap"),
        "FormData" => Some("FormData"),
        "Request" => Some("Request"),
        "Response" => Some("Response"),
        "AbortController" => Some("AbortController"),
        "AbortSignal" => Some("AbortSignal"),
        "WebSocket" => Some("WebSocket"),
        "EventSource" => Some("EventSource"),
        "XMLHttpRequest" => Some("XMLHttpRequest"),
        "IntersectionObserver" => Some("IntersectionObserver"),
        "MutationObserver" => Some("MutationObserver"),
        "ResizeObserver" => Some("ResizeObserver"),
        "Performance" | "performance" => Some("Performance"),
        "Crypto" | "crypto" => Some("Crypto"),
        "SubtleCrypto" => Some("SubtleCrypto"),
        "TextEncoder" => Some("TextEncoder"),
        "TextDecoder" => Some("TextDecoder"),
        "Blob" => Some("Blob"),
        "File" => Some("File"),
        "FileReader" => Some("FileReader"),
        "NodeList" | "NodeListOf" => Some("NodeList"),
        "HTMLCollection" | "HTMLCollectionOf" => Some("HTMLCollection"),
        "ReadableStream" => Some("ReadableStream"),
        "ReadableStreamDefaultReader" => Some("ReadableStreamDefaultReader"),
        _ if (name.starts_with("HTML") || name.starts_with("SVG")) && name.ends_with("Element") => {
            Some("Element")
        }
        _ => None,
    }
}

/// Resolves a DOM property on a receiver to its return type.
pub fn typed_dom_property(receiver: &str, property: &str) -> Option<&'static str> {
    match (typed_dom_receiver(receiver)?, property) {
        (
            "Element",
            "parentElement"
            | "firstElementChild"
            | "lastElementChild"
            | "nextElementSibling"
            | "previousElementSibling",
        ) => Some("Element"),
        ("Element", "ownerDocument") => Some("Document"),
        ("Element", "classList") => Some("DOMTokenList"),
        ("Document", "documentElement" | "body" | "head" | "activeElement") => Some("Element"),
        ("Document", "defaultView") => Some("Window"),
        ("Document", "location") => Some("Location"),
        ("Window", "document") => Some("Document"),
        ("Window", "location") => Some("Location"),
        ("Window", "history") => Some("History"),
        ("Window", "navigator") => Some("Navigator"),
        ("Window", "performance") => Some("Performance"),
        ("Window", "crypto") => Some("Crypto"),
        ("Navigator", "clipboard") => Some("Clipboard"),
        ("Crypto", "subtle") => Some("SubtleCrypto"),
        ("Element", "style") => Some("CSSStyleDeclaration"),
        ("Event", "target" | "currentTarget") => Some("Element"),
        ("URL", "searchParams") => Some("URLSearchParams"),
        _ => None,
    }
}

pub(super) fn browser_global_type(name: &str) -> bool {
    if typed_dom_receiver(name).is_some() {
        return true;
    }
    matches!(name, "Response" | "Request" | "FormData" | "Blob" | "File")
}

fn record_dom_local_writes(node: Syntax<'_>, source: &str, file: &mut FileContext) {
    let written = match node.kind() {
        "assignment_expression" | "augmented_assignment_expression" => {
            node.child_by_field_name("left")
        }
        "update_expression" => node.named_child(0),
        _ => None,
    };
    let Some(name) = written.filter(|name| name.kind() == "identifier") else {
        return;
    };
    let name = text(name, source).to_owned();
    let local_writes =
        std::sync::Arc::make_mut(&mut file.state.typescript_mut().dom_listener_local_writes);
    let mut parent = node.parent();
    while let Some(scope) = parent {
        if matches!(scope.kind(), "statement_block" | "program" | "source_file") {
            local_writes
                .entry(scope.id())
                .or_default()
                .entry(name.clone())
                .or_default()
                .push(node.start_byte());
        }
        if matches!(
            scope.kind(),
            "function_declaration" | "function_expression" | "arrow_function" | "method_definition"
        ) {
            break;
        }
        parent = scope.parent();
    }
}

fn collect_dom_class_field_fact(node: Syntax<'_>, source: &str, file: &mut FileContext) {
    if !matches!(node.kind(), "field_definition" | "public_field_definition") {
        return;
    }
    let Some(class) = node
        .parent()
        .filter(|body| body.kind() == "class_body")
        .and_then(|body| body.parent())
        .filter(|class| matches!(class.kind(), "class_declaration" | "class"))
    else {
        return;
    };
    let Some(name) = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
        .filter(|name| matches!(name.kind(), "property_identifier" | "identifier"))
    else {
        return;
    };
    let static_field = (0..node.child_count()).any(|index| {
        node.child(index)
            .is_some_and(|child| child.kind() == "static")
    });
    let ty = (!static_field)
        .then(|| {
            node.child_by_field_name("type")
                .and_then(|annotation| annotation.named_child(0))
                .filter(|ty| ty.kind() == "type_identifier")
                .map(|ty| text(ty, source).to_owned())
        })
        .flatten();
    let fields =
        std::sync::Arc::make_mut(&mut file.state.typescript_mut().dom_listener_class_fields)
            .entry(class.id())
            .or_default();
    let name = text(name, source).to_owned();
    match fields.entry(name) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            entry.insert(None);
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(ty);
        }
    }
}

fn collect_dom_function_receivers(node: Syntax<'_>, source: &str, file: &mut FileContext) {
    if !matches!(
        node.kind(),
        "function_declaration" | "method_definition" | "function_expression" | "arrow_function"
    ) {
        return;
    }
    let mut receivers = HashMap::new();
    let mut duplicates = HashSet::new();
    if let Some(parameters) = node.child_by_field_name("parameters") {
        for parameter in parameters.named_children(&mut parameters.walk()) {
            let Some(name) = parameter
                .child_by_field_name("name")
                .or_else(|| parameter.child_by_field_name("pattern"))
                .filter(|name| name.kind() == "identifier")
            else {
                continue;
            };
            let Some(annotation) = parameter
                .child_by_field_name("type")
                .and_then(|ty| ty.named_child(0))
                .filter(|ty| ty.kind() == "type_identifier")
            else {
                continue;
            };
            let ty = text(annotation, source);
            if !matches!(
                typed_dom_receiver(ty),
                Some("Element" | "Document" | "Window" | "EventTarget")
            ) {
                continue;
            }
            let name = text(name, source).to_owned();
            if receivers.insert(name.clone(), ty.to_owned()).is_some() {
                duplicates.insert(name);
            }
        }
    }
    if node.kind() == "arrow_function" {
        let parameter = first_callback_parameter(node);
        if let (Some(parameter), Some(element)) =
            (parameter, array_callback_element(node, source, file))
        {
            if matches!(
                typed_dom_receiver(element),
                Some("Element" | "Document" | "Window" | "EventTarget")
            ) {
                receivers.insert(text(parameter, source).to_owned(), element.to_owned());
            }
        }
    }
    for name in duplicates {
        receivers.remove(&name);
    }
    if !receivers.is_empty() {
        std::sync::Arc::make_mut(&mut file.state.typescript_mut().dom_listener_receivers)
            .insert(node.id(), receivers);
    }
}

pub(super) fn collect_scope_facts(
    root: Syntax<'_>,
    source: &str,
    file: &mut FileContext,
    typescript: bool,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let (binding, var_scoped) = match node.kind() {
            "variable_declarator" => (
                node.child_by_field_name("name")
                    .is_some_and(|name| pattern_binds_require(name, source)),
                node.parent()
                    .is_some_and(|parent| parent.kind() == "variable_declaration"),
            ),
            "formal_parameters" | "required_parameter" | "optional_parameter"
            | "formal_parameter" => (pattern_binds_require(node, source), true),
            "arrow_function" => (
                node.child_by_field_name("parameter")
                    .is_some_and(|parameter| pattern_binds_require(parameter, source)),
                true,
            ),
            "function_expression" | "generator_function" | "generator_function_expression" => (
                field(node, source, "name").is_some_and(|name| name.trim() == "require"),
                true,
            ),
            "function_declaration" | "generator_function_declaration" | "class_declaration" => (
                field(node, source, "name").is_some_and(|name| name.trim() == "require"),
                false,
            ),
            "import_statement" => (import_binds_require(node, source), false),
            "catch_clause" => (
                node.child_by_field_name("parameter")
                    .is_some_and(|pattern| pattern_binds_require(pattern, source)),
                false,
            ),
            _ => (false, false),
        };
        if binding {
            if let Some(scope) = nearest_binding_scope(node, var_scoped) {
                std::sync::Arc::make_mut(
                    &mut file.state.typescript_mut().shadowed_require_scopes,
                )
                .insert(scope.id());
            }
        }
        if !typescript {
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
            continue;
        }
        if node.kind() == "variable_declarator" {
            scope_facts::index_dom_listener_declarator(node, source, file);
        }
        if let Some(params_node) = match node.kind() {
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "generator_function_expression"
            | "arrow_function"
            | "method_definition" => node
                .child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter")),
            "catch_clause" => node.child_by_field_name("parameter"),
            _ => None,
        } {
            let mut names = Vec::new();
            collect_pattern_binding_names(params_node, source, &mut names);
            if !names.is_empty() {
                std::sync::Arc::make_mut(
                    &mut file.state.typescript_mut().dom_listener_parameter_bindings,
                )
                .entry(node.id())
                .or_default()
                .extend(names);
            }
        }
        record_dom_local_writes(node, source, file);
        collect_dom_global_shadow_fact(node, source, file);
        collect_dom_class_field_fact(node, source, file);
        collect_dom_function_receivers(node, source, file);
        collect_array_shadow_fact(node, source, file);
        collect_rebound_parameter_fact(node, source, file);
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

pub(super) fn import_binds_name(node: Syntax<'_>, source: &str, name: &str) -> bool {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "import_clause" | "namespace_import" => {
                let mut cursor = current.walk();
                if current
                    .named_children(&mut cursor)
                    .any(|child| child.kind() == "identifier" && text(child, source).trim() == name)
                {
                    return true;
                }
            }
            "import_specifier" => {
                let local =
                    field(current, source, "alias").or_else(|| field(current, source, "name"));
                if local.is_some_and(|local| local.trim() == name) {
                    return true;
                }
            }
            _ => {}
        }
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    false
}

pub(super) fn import_binds_require(node: Syntax<'_>, source: &str) -> bool {
    import_binds_name(node, source, "require")
}

pub(super) fn has_type_import_modifier(node: Syntax<'_>) -> bool {
    (0..node.child_count()).any(|index| {
        node.child(index)
            .is_some_and(|child| child.kind() == "type")
    })
}

pub(super) fn array_type_shadowed(node: Syntax<'_>, file: &FileContext) -> bool {
    let Some(ts_state) = file.state.typescript() else {
        return false;
    };
    let mut current = Some(node);
    while let Some(scope) = current {
        if ts_state.shadowed_array_type_scopes.contains(&scope.id()) {
            return true;
        }
        current = scope.parent();
    }
    false
}

fn element_type_from_annotation<'a>(
    annotation: Syntax<'_>,
    arrow: Syntax<'_>,
    file: &FileContext,
    source: &'a str,
) -> Option<&'a str> {
    let element = match annotation.kind() {
        "array_type" => annotation.named_child(0)?,
        "generic_type" => {
            let name = annotation.child_by_field_name("name")?;
            let name_str = text(name, source);
            if name.kind() != "type_identifier"
                || !matches!(
                    name_str,
                    "Array" | "NodeListOf" | "HTMLCollectionOf" | "Set"
                )
            {
                return None;
            }
            let arguments = annotation.child_by_field_name("type_arguments")?;
            if arguments.named_child_count() != 1 || array_type_shadowed(arrow, file) {
                return None;
            }
            arguments.named_child(0)?
        }
        _ => return None,
    };
    if !matches!(element.kind(), "type_identifier" | "identifier") {
        return None;
    }
    Some(text(element, source))
}

fn enclosing_function_node<'a>(mut current: Option<Syntax<'a>>) -> Option<Syntax<'a>> {
    while let Some(node) = current {
        if matches!(
            node.kind(),
            "function_declaration"
                | "method_definition"
                | "function_expression"
                | "arrow_function"
        ) {
            return Some(node);
        }
        current = node.parent();
    }
    None
}

fn dom_query_all_element<'a>(
    call_expr: Syntax<'_>,
    source: &'a str,
    file: &FileContext,
) -> Option<&'a str> {
    let callee = call_expr
        .child_by_field_name("function")
        .filter(|c| c.kind() == "member_expression")?;
    let method = callee.child_by_field_name("property")?;
    if !matches!(
        text(method, source),
        "querySelectorAll" | "getElementsByTagName" | "getElementsByClassName"
    ) {
        return None;
    }
    let object = callee.child_by_field_name("object")?;
    dom_receiver_from_ast(object, source, file, 0).map(|_| "Element")
}

fn element_from_parameter_annotation<'a>(
    call: Syntax<'_>,
    receiver_name: &str,
    arrow: Syntax<'_>,
    file: &FileContext,
    source: &'a str,
) -> Option<&'a str> {
    let enclosing = enclosing_function_node(call.parent())?;
    let parameters = enclosing.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let mut matches = parameters.named_children(&mut cursor).filter(|parameter| {
        parameter
            .child_by_field_name("name")
            .or_else(|| parameter.child_by_field_name("pattern"))
            .is_some_and(|name| name.kind() == "identifier" && text(name, source) == receiver_name)
    });
    let parameter = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let annotation = parameter.child_by_field_name("type")?.named_child(0)?;
    let rebound = file.state.typescript().is_some_and(|state| {
        state
            .rebound_function_parameters
            .get(&enclosing.id())
            .is_some_and(|names| names.contains(receiver_name))
    });
    (!rebound).then(|| element_type_from_annotation(annotation, arrow, file, source))?
}

fn element_from_declarator<'a>(
    declarator: Syntax<'_>,
    receiver_name: &str,
    arrow: Syntax<'_>,
    file: &FileContext,
    source: &'a str,
) -> Option<&'a str> {
    if declarator.kind() != "variable_declarator" {
        return None;
    }
    let var_name = declarator.child_by_field_name("name")?;
    if var_name.kind() != "identifier" || text(var_name, source) != receiver_name {
        return None;
    }
    if let Some(ty_node) = declarator.child_by_field_name("type") {
        let annotation = ty_node.named_child(0).unwrap_or(ty_node);
        if let Some(el) = element_type_from_annotation(annotation, arrow, file, source) {
            return Some(el);
        }
    }
    let init = declarator
        .child_by_field_name("value")
        .filter(|init| init.kind() == "call_expression")?;
    dom_query_all_element(init, source, file)
}

fn element_from_local_declaration<'a>(
    call: Syntax<'_>,
    receiver_name: &str,
    arrow: Syntax<'_>,
    file: &FileContext,
    source: &'a str,
) -> Option<&'a str> {
    let mut current_scope = call.parent();
    while let Some(node) = current_scope {
        if matches!(node.kind(), "statement_block" | "program" | "source_file") {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.start_byte() >= call.start_byte() {
                    break;
                }
                if !matches!(child.kind(), "lexical_declaration" | "variable_declaration") {
                    continue;
                }
                let mut decl_cursor = child.walk();
                for declarator in child.named_children(&mut decl_cursor) {
                    if let Some(el) =
                        element_from_declarator(declarator, receiver_name, arrow, file, source)
                    {
                        return Some(el);
                    }
                }
            }
        }
        if matches!(
            node.kind(),
            "function_declaration"
                | "method_definition"
                | "function_expression"
                | "arrow_function"
        ) {
            break;
        }
        current_scope = node.parent();
    }
    None
}

fn element_from_this_field<'a>(
    receiver: Syntax<'_>,
    arrow: Syntax<'_>,
    file: &FileContext,
    source: &'a str,
) -> Option<&'a str> {
    let property = receiver.child_by_field_name("property")?;
    let field_name = text(property, source);
    let (_, class) = lexical_this_method(arrow)?;
    let body = class.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if !matches!(child.kind(), "field_definition" | "public_field_definition") {
            continue;
        }
        let Some(prop) = child
            .child_by_field_name("property")
            .or_else(|| child.child_by_field_name("name"))
        else {
            continue;
        };
        if text(prop, source) == field_name {
            let ty_node = child.child_by_field_name("type")?;
            let annotation = ty_node.named_child(0).unwrap_or(ty_node);
            return element_type_from_annotation(annotation, arrow, file, source);
        }
    }
    None
}

pub(super) fn array_callback_element<'a>(
    arrow: Syntax<'_>,
    source: &'a str,
    file: &FileContext,
) -> Option<&'a str> {
    let call = arrow
        .parent()
        .filter(|parent| parent.kind() == "arguments")?
        .parent()?;
    let function = call
        .child_by_field_name("function")
        .filter(|function| function.kind() == "member_expression")?;
    let method = function.child_by_field_name("property")?;
    if !matches!(
        text(method, source),
        "forEach" | "map" | "filter" | "find" | "some" | "every" | "flatMap"
    ) {
        return None;
    }
    let receiver = function.child_by_field_name("object")?;
    match receiver.kind() {
        "identifier" => {
            let receiver_name = text(receiver, source);
            element_from_parameter_annotation(call, receiver_name, arrow, file, source).or_else(
                || element_from_local_declaration(call, receiver_name, arrow, file, source),
            )
        }
        "member_expression"
            if receiver
                .child_by_field_name("object")
                .is_some_and(|obj| obj.kind() == "this") =>
        {
            element_from_this_field(receiver, arrow, file, source)
        }
        "call_expression" => dom_query_all_element(receiver, source, file),
        _ => None,
    }
}

pub(super) fn dom_global_shadowed(node: Syntax<'_>, file: &FileContext, name: &str) -> bool {
    let Some(ts_state) = file.state.typescript() else {
        return false;
    };
    let mut current = Some(node);
    while let Some(scope) = current {
        if ts_state
            .shadowed_dom_global_scopes
            .get(&scope.id())
            .is_some_and(|names| names.contains(name))
        {
            return true;
        }
        current = scope.parent();
    }
    false
}

pub(super) fn dom_global_shadowed_in_scopes(
    scopes: &[usize],
    file: &FileContext,
    name: &str,
) -> bool {
    let Some(ts_state) = file.state.typescript() else {
        return false;
    };
    scopes.iter().any(|scope| {
        ts_state
            .shadowed_dom_global_scopes
            .get(scope)
            .is_some_and(|names| names.contains(name))
    })
}

pub(super) fn dom_indexed_const<'a>(
    file: &'a FileContext,
    name: &str,
    at: usize,
    scopes: &[usize],
) -> Option<Result<&'a DomListenerLocalDeclaration, usize>> {
    let ts_state = file.state.typescript()?;
    for scope in scopes {
        let declaration = ts_state
            .dom_listener_local_declarations
            .get(scope)
            .and_then(|bindings| bindings.get(name));
        let rebound = ts_state
            .dom_listener_local_writes
            .get(scope)
            .and_then(|names| names.get(name))
            .is_some_and(|writes| writes.iter().any(|position| *position < at));
        if declaration.is_some() || rebound {
            let valid = declaration.filter(|declaration| {
                declaration.declaration_count == 1
                    && declaration.is_const
                    && declaration.statement_end <= at
                    && declaration.initializer.is_some()
                    && !rebound
            });
            return Some(valid.ok_or(*scope));
        }
        if ts_state
            .dom_listener_parameter_bindings
            .get(scope)
            .is_some_and(|names| names.contains(name))
        {
            return Some(Err(*scope));
        }
    }
    None
}

pub(super) fn dom_receiver_from_indexed_expression(
    expression: &DomListenerReceiverExpression,
    file: &FileContext,
    scopes: &[usize],
    depth: usize,
) -> Option<&'static str> {
    if depth > 4 {
        return None;
    }
    let ts_state = file.state.typescript()?;
    match expression {
        DomListenerReceiverExpression::Identifier { name, position } => {
            if matches!(name.as_str(), "document" | "window")
                && !dom_global_shadowed_in_scopes(scopes, file, name)
            {
                return Some(if name == "document" {
                    "Document"
                } else {
                    "Window"
                });
            }
            match dom_indexed_const(file, name, *position, scopes)? {
                Ok(declaration) => dom_receiver_from_indexed_expression(
                    declaration.initializer.as_ref()?,
                    file,
                    &declaration.scope_chain,
                    depth + 1,
                ),
                Err(scope) => {
                    let rebound = ts_state
                        .rebound_function_parameters
                        .get(&scope)
                        .is_some_and(|names| names.contains(name));
                    if rebound {
                        return None;
                    }
                    let ty = ts_state.dom_listener_receivers.get(&scope)?.get(name)?;
                    typed_dom_receiver(ty)
                        .filter(|_| !dom_global_shadowed_in_scopes(scopes, file, ty))
                }
            }
        }
        DomListenerReceiverExpression::ThisField {
            name,
            class_scope,
            static_method,
        } => {
            if *static_method {
                return None;
            }
            let ty = ts_state
                .dom_listener_class_fields
                .get(class_scope)?
                .get(name)?
                .as_deref()?;
            typed_dom_receiver(ty).filter(|_| !dom_global_shadowed_in_scopes(scopes, file, ty))
        }
        DomListenerReceiverExpression::MemberCall {
            receiver,
            method,
            type_arguments,
            has_arguments,
        } => {
            if !has_arguments {
                return None;
            }
            if !type_arguments.is_empty()
                && (type_arguments.len() != 1
                    || typed_dom_receiver(&type_arguments[0]) != Some("Element")
                    || dom_global_shadowed_in_scopes(scopes, file, &type_arguments[0]))
            {
                return None;
            }
            let owner = dom_receiver_from_indexed_expression(receiver, file, scopes, depth + 1)?;
            let produces_element = match owner {
                "Document" => {
                    matches!(
                        method.as_str(),
                        "getElementById" | "createElement" | "querySelector"
                    )
                }
                "Element" => matches!(method.as_str(), "querySelector" | "closest"),
                _ => false,
            };
            produces_element.then_some("Element")
        }
        DomListenerReceiverExpression::Parenthesized(inner) => {
            dom_receiver_from_indexed_expression(inner, file, scopes, depth + 1)
        }
        DomListenerReceiverExpression::Unsupported => None,
    }
}

pub(super) fn dom_const_initializer<'tree>(
    node: Syntax<'tree>,
    file: &FileContext,
    name: &str,
) -> Option<Result<Syntax<'tree>, Syntax<'tree>>> {
    let ts_state = file.state.typescript()?;
    let at = node.start_byte();
    let mut current = node.parent();
    while let Some(scope) = current {
        if matches!(scope.kind(), "statement_block" | "program" | "source_file") {
            let declaration = ts_state
                .dom_listener_local_declarations
                .get(&scope.id())
                .and_then(|bindings| bindings.get(name));
            let rebound = ts_state
                .dom_listener_local_writes
                .get(&scope.id())
                .and_then(|names| names.get(name))
                .is_some_and(|writes| writes.iter().any(|position| *position < at));
            if declaration.is_some() || rebound {
                return Some(
                    if declaration.is_some_and(|declaration| {
                        declaration.declaration_count == 1
                            && declaration.is_const
                            && declaration.statement_end <= at
                            && declaration.initializer.is_some()
                            && !rebound
                    }) {
                        Ok(scope)
                    } else {
                        Err(scope)
                    },
                );
            }
        }
        if ts_state
            .dom_listener_parameter_bindings
            .get(&scope.id())
            .is_some_and(|names| names.contains(name))
        {
            return Some(Err(scope));
        }
        current = scope.parent();
    }
    None
}

pub(super) fn dom_receiver_from_ast(
    receiver: Syntax<'_>,
    source: &str,
    file: &FileContext,
    depth: usize,
) -> Option<&'static str> {
    if depth > 4 {
        return None;
    }
    match receiver.kind() {
        "identifier" => {
            let name = text(receiver, source);
            if matches!(name, "document" | "window") {
                return (!dom_global_shadowed(receiver, file, name)).then_some(
                    if name == "document" {
                        "Document"
                    } else {
                        "Window"
                    },
                );
            }
            match dom_const_initializer(receiver, file, name)? {
                Ok(scope) => {
                    let ts_state = file.state.typescript()?;
                    let declaration = ts_state
                        .dom_listener_local_declarations
                        .get(&scope.id())?
                        .get(name)?;
                    dom_receiver_from_indexed_expression(
                        declaration.initializer.as_ref()?,
                        file,
                        &declaration.scope_chain,
                        depth + 1,
                    )
                }
                Err(scope)
                    if matches!(
                        scope.kind(),
                        "function_declaration"
                            | "function_expression"
                            | "method_definition"
                            | "arrow_function"
                    ) =>
                {
                    let ts_state = file.state.typescript()?;
                    if ts_state
                        .rebound_function_parameters
                        .get(&scope.id())
                        .is_some_and(|names| names.contains(name))
                    {
                        return None;
                    }
                    let ty = ts_state.dom_listener_receivers.get(&scope.id())?.get(name)?;
                    typed_dom_receiver(ty).filter(|_| !dom_global_shadowed(receiver, file, ty))
                }
                Err(_) => None,
            }
        }
        "member_expression" => {
            let object = receiver.child_by_field_name("object")?;
            let property = receiver
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")?;
            if object.kind() == "this" {
                let (method, class) = lexical_this_method(receiver)?;
                if (0..method.child_count()).any(|index| {
                    method
                        .child(index)
                        .is_some_and(|child| child.kind() == "static")
                }) {
                    return None;
                }
                let ty = file
                    .state
                    .typescript()?
                    .dom_listener_class_fields
                    .get(&class.id())?
                    .get(text(property, source))?
                    .as_deref()?;
                return typed_dom_receiver(ty).filter(|_| !dom_global_shadowed(receiver, file, ty));
            }
            None
        }
        "call_expression" => {
            let callee = receiver
                .child_by_field_name("function")
                .filter(|callee| callee.kind() == "member_expression")?;
            let object = callee.child_by_field_name("object")?;
            let method = callee
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")?;
            let arguments = receiver.child_by_field_name("arguments")?;
            if arguments.named_child_count() == 0 {
                return None;
            }
            if let Some(type_arguments) = receiver.child_by_field_name("type_arguments") {
                let ty = type_arguments
                    .named_child(0)
                    .filter(|_| type_arguments.named_child_count() == 1)?;
                let name = text(ty, source);
                if ty.kind() != "type_identifier"
                    || typed_dom_receiver(name) != Some("Element")
                    || dom_global_shadowed(receiver, file, name)
                {
                    return None;
                }
            }
            let owner = dom_receiver_from_ast(object, source, file, depth + 1)?;
            let method = text(method, source);
            let produces_element = match owner {
                "Document" => {
                    matches!(method, "getElementById" | "createElement" | "querySelector")
                }
                "Element" => matches!(method, "querySelector" | "closest"),
                _ => false,
            };
            produces_element.then_some("Element")
        }
        "parenthesized_expression" => receiver
            .named_child(0)
            .and_then(|inner| dom_receiver_from_ast(inner, source, file, depth + 1)),
        _ => None,
    }
}

pub(super) fn dom_event_callback_parameter<'a>(
    callback: Syntax<'_>,
    source: &'a str,
    file: &FileContext,
) -> Option<&'a str> {
    if let Some(args) = callback
        .parent()
        .filter(|parent| parent.kind() == "arguments")
    {
        if args.named_child(1) != Some(callback) || args.named_child_count() > 3 {
            return None;
        }
        let event = args
            .named_child(0)
            .filter(|event| event.kind() == "string")?;
        let literal = text(event, source);
        if literal.len() < 2
            || literal.contains('\\')
            || !matches!(literal.as_bytes()[0], b'\'' | b'"')
            || literal.as_bytes()[0] != *literal.as_bytes().last()?
        {
            return None;
        }
        let call = args
            .parent()
            .filter(|parent| parent.kind() == "call_expression")?;
        let callee = call
            .child_by_field_name("function")
            .filter(|callee| callee.kind() == "member_expression")?;
        let member = callee.child_by_field_name("property")?;
        if member.kind() != "property_identifier" || text(member, source) != "addEventListener" {
            return None;
        }
        let receiver = callee.child_by_field_name("object")?;
        let verified_receiver = matches!(
            dom_receiver_from_ast(receiver, source, file, 0),
            Some("Element" | "Document" | "Window" | "EventTarget")
        );
        if !verified_receiver
            || dom_global_shadowed(callback, file, "Event")
            || dom_global_shadowed(callback, file, "EventTarget")
        {
            return None;
        }
    } else {
        let assignment = callback
            .parent()
            .filter(|p| p.kind() == "assignment_expression")?;
        if assignment.child_by_field_name("right") != Some(callback) {
            return None;
        }
        let left = assignment
            .child_by_field_name("left")
            .filter(|left| left.kind() == "member_expression")?;
        let property = left.child_by_field_name("property")?;
        let prop_name = text(property, source);
        if !prop_name.starts_with("on") || prop_name.len() <= 2 {
            return None;
        }
        let receiver = left.child_by_field_name("object")?;
        let verified_receiver = matches!(
            dom_receiver_from_ast(receiver, source, file, 0),
            Some("Element" | "Document" | "Window" | "EventTarget")
        );
        if !verified_receiver || dom_global_shadowed(callback, file, "Event") {
            return None;
        }
    }
    first_callback_parameter(callback).map(|parameter| text(parameter, source))
}

fn matches_callback_callee(
    mut callee: Syntax<'_>,
    source: &str,
    configured: &[String],
) -> bool {
    if configured.is_empty() {
        return false;
    }
    if callee.kind() == "identifier" {
        let name = text(callee, source).trim();
        return configured.iter().any(|candidate| candidate == name);
    }
    let mut segments = [""; 8];
    let mut count = 0usize;
    while callee.kind() == "member_expression" {
        if count == segments.len() {
            return false;
        }
        let Some(property) = callee
            .child_by_field_name("property")
            .filter(|node| node.kind() == "property_identifier")
        else {
            return false;
        };
        segments[count] = text(property, source).trim();
        count += 1;
        let Some(object) = callee.child_by_field_name("object") else {
            return false;
        };
        callee = object;
    }
    if callee.kind() != "identifier" || count == 0 || count == segments.len() {
        return false;
    }
    segments[count] = text(callee, source).trim();
    count += 1;
    configured.iter().any(|candidate| {
        let mut parts = candidate.split('.');
        for segment in segments[..count].iter().rev() {
            if parts.next() != Some(*segment) {
                return false;
            }
        }
        parts.next().is_none()
    })
}

fn unwrap_parameter_pattern<'tree>(mut parameter: Syntax<'tree>) -> Syntax<'tree> {
    for _ in 0..4 {
        if matches!(
            parameter.kind(),
            "required_parameter" | "optional_parameter" | "formal_parameter"
        ) {
            if let Some(inner) = parameter
                .child_by_field_name("pattern")
                .or_else(|| parameter.child_by_field_name("name"))
            {
                parameter = inner;
                continue;
            }
        } else if parameter.kind() == "assignment_pattern" {
            if let Some(left) = parameter.child_by_field_name("left") {
                parameter = left;
                continue;
            }
        }
        break;
    }
    parameter
}

pub(super) fn collect_configured_callback_parameters(
    callback: Syntax<'_>,
    source: &str,
    receiver: &manifests::syntax::ReceiverTypeRule,
    param_types: &mut BTreeMap<String, String>,
) {
    let Some(call) = callback
        .parent()
        .filter(|p| p.kind() == "arguments")
        .and_then(|arguments| arguments.parent())
        .filter(|call| call.kind() == "call_expression")
    else {
        return;
    };
    let Some(callee) = call.child_by_field_name("function") else {
        return;
    };
    if !matches_callback_callee(callee, source, &receiver.callback_callees) {
        return;
    }
    let Some(raw_parameter) = callback
        .child_by_field_name("parameter")
        .or_else(|| callback.child_by_field_name("parameters")?.named_child(0))
    else {
        return;
    };
    let parameter = unwrap_parameter_pattern(raw_parameter);
    if parameter.kind() == "identifier" {
        let local = text(parameter, source).trim();
        if !local.is_empty() {
            param_types
                .entry(local.to_owned())
                .or_insert_with(|| receiver.name.clone());
        }
        return;
    }
    if parameter.kind() != "object_pattern"
        || receiver.properties.is_empty()
        || parameter.named_child_count() > 32
    {
        return;
    }
    let mut cursor = parameter.walk();
    for item in parameter.named_children(&mut cursor) {
        let pair = match item.kind() {
            "shorthand_property_identifier_pattern" => {
                let key = text(item, source).trim();
                Some((key, key))
            }
            "object_assignment_pattern" => item
                .child_by_field_name("left")
                .filter(|left| {
                    matches!(
                        left.kind(),
                        "shorthand_property_identifier_pattern" | "identifier"
                    )
                })
                .map(|left| {
                    let key = text(left, source).trim();
                    (key, key)
                }),
            "pair_pattern" => {
                let key = item
                    .child_by_field_name("key")
                    .filter(|key| matches!(key.kind(), "property_identifier" | "identifier"));
                let value = item
                    .child_by_field_name("value")
                    .map(unwrap_parameter_pattern)
                    .filter(|value| value.kind() == "identifier");
                key.zip(value)
                    .map(|(k, v)| (text(k, source).trim(), text(v, source).trim()))
            }
            _ => None,
        };
        if let Some((key, local)) = pair {
            if !local.is_empty() {
                if let Some(ty) = receiver.properties.get(key) {
                    param_types
                        .entry(local.to_owned())
                        .or_insert_with(|| ty.clone());
                }
            }
        }
    }
}

pub(in crate::languages) fn first_callback_parameter<'tree>(
    callback: Syntax<'tree>,
) -> Option<Syntax<'tree>> {
    let parameter = callback
        .child_by_field_name("parameter")
        .or_else(|| callback.child_by_field_name("parameters")?.named_child(0))?;
    let parameter = if parameter.kind() == "identifier" {
        parameter
    } else {
        parameter
            .child_by_field_name("name")
            .or_else(|| parameter.child_by_field_name("pattern"))?
    };
    (parameter.kind() == "identifier").then_some(parameter)
}
