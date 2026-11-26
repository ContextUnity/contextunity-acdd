use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tree_sitter::Node as Syntax;

use crate::ast::syntax::{field, text};
use crate::languages::context::LanguageContext;
use crate::languages::manifests::syntax::SyntaxOperation;
use crate::languages::types::SyntaxContext;
use crate::models::{
    details::NodeDetailKey,
    tags::{LanguageId, NodeKind},
    Edge, Facts, Node,
};
use crate::semantic::{ExtractionRuleOrigin, FrameworkFact, FrameworkScopeFacts, ValueFlowFacts};

use super::registrations::{self, RegistrationScopeFrame, NS_MACRO, NS_TYPE, NS_VALUE};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CliParserMetadata {
    pub(crate) pack: String,
    pub(crate) rule: String,
    pub(crate) module: String,
    pub(crate) parse_method: String,
    pub(crate) subcommand_field: String,
    pub(crate) subcommand_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CliSubcommandMetadata {
    pub(crate) pack: String,
    pub(crate) rule: String,
    pub(crate) module: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CliDispatchMetadata {
    pub(crate) pack: String,
    pub(crate) rule: String,
    pub(crate) owner_id: String,
    pub(crate) parser_type: String,
    pub(crate) parse_method: String,
    pub(crate) subcommand_field: String,
    pub(crate) enum_type: String,
    pub(crate) variant: String,
    pub(crate) handler: String,
    pub(crate) handler_line: usize,
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn path_head(path: &str) -> &str {
    path.trim_start_matches("::")
        .split("::")
        .next()
        .unwrap_or(path)
}

fn build_scope_frames(node: Syntax<'_>, source: &str) -> Vec<RegistrationScopeFrame> {
    let mut containers = Vec::new();
    let mut cur = node.parent();
    while let Some(parent) = cur {
        match parent.kind() {
            "source_file" => {
                containers.push((parent, true));
                break;
            }
            "declaration_list" if parent.parent().is_some_and(|p| p.kind() == "mod_item") => {
                containers.push((parent, true));
                break;
            }
            "block" => containers.push((parent, false)),
            _ => {}
        }
        cur = parent.parent();
    }
    containers.reverse();
    containers
        .into_iter()
        .map(|(container, is_mod)| {
            let mut frame = RegistrationScopeFrame {
                is_mod,
                ..Default::default()
            };
            registrations::populate_scope_items(&mut frame, container, source);
            frame
        })
        .collect()
}

fn extract_derive_paths(node: Syntax<'_>, source: &str) -> Vec<String> {
    let mut derives = Vec::new();
    super::macros::for_each_preceding_attribute(node, |attr| {
        let raw = compact(text(attr, source));
        if let Some(inner) = raw
            .strip_prefix("#[derive(")
            .and_then(|s| s.strip_suffix(")]"))
        {
            derives.extend(
                inner
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
    });
    derives
}

fn unwrap_wrapper_type(mut ty: Syntax<'_>, source: &str) -> Option<String> {
    loop {
        if ty.kind() != "generic_type" {
            return matches!(ty.kind(), "type_identifier" | "scoped_type_identifier")
                .then(|| compact(text(ty, source)));
        }
        let base = ty.child_by_field_name("type")?;
        let base_text = compact(text(base, source));
        let tail = base_text.rsplit("::").next().unwrap_or("");
        if matches!(tail, "Option" | "Box") {
            if let Some(args) = ty.child_by_field_name("type_arguments") {
                let mut c = args.walk();
                let mut inner = args
                    .named_children(&mut c)
                    .filter(|n| n.kind() != "lifetime");
                if let (Some(first), None) = (inner.next(), inner.next()) {
                    ty = first;
                    continue;
                }
            }
        }
        return Some(base_text);
    }
}

fn has_field_marker(fd: Syntax<'_>, source: &str, attr_name: &str, marker: &str) -> bool {
    let mut matched = false;
    super::macros::for_each_preceding_attribute(fd, |attr| {
        if matched {
            return;
        }
        let raw = compact(text(attr, source));
        let Some((head, args)) = raw
            .strip_prefix("#[")
            .and_then(|s| s.strip_suffix(")]"))
            .and_then(|s| s.split_once('('))
        else {
            return;
        };
        if head.rsplit("::").next() == Some(attr_name)
            && args
                .split(',')
                .any(|arg| arg.split(['=', '(']).next() == Some(marker))
        {
            matched = true;
        }
    });
    matched
}

fn extract_subcommand_field(
    struct_node: Syntax<'_>,
    source: &str,
    field_attribute: &str,
    field_marker: &str,
) -> Option<(String, String)> {
    let body = struct_node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let found = body
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "field_declaration")
        .find_map(|fd| {
            has_field_marker(fd, source, field_attribute, field_marker).then(|| {
                let name = field(fd, source, "name")?;
                let ty = unwrap_wrapper_type(fd.child_by_field_name("type")?, source)?;
                Some((name.to_owned(), ty))
            })?
        });
    found
}

fn has_command_dispatch_rules(rules: &LanguageContext) -> bool {
    rules.syntax_rules.iter().any(|rule| {
        rule.language == "rust" && matches!(rule.operation, SyntaxOperation::CommandDispatch { .. })
    })
}

pub(super) fn extract_nominal_metadata(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    registrations::with_rules(|rules| {
        if !has_command_dispatch_rules(rules) {
            return;
        }
        let derives = extract_derive_paths(ctx.node, ctx.source);
        if derives.is_empty() {
            return;
        }
        let Some(path) = facts.nodes.first().map(|n| n.path.clone()) else {
            return;
        };
        let frames = build_scope_frames(ctx.node, ctx.source);
        let mut parsers = Vec::new();
        let mut subcommands = Vec::new();
        for rule in &rules.syntax_rules {
            let SyntaxOperation::CommandDispatch {
                module,
                field_attribute,
                field_marker,
                parser_derive,
                subcommand_derive,
                parse_method,
            } = &rule.operation
            else {
                continue;
            };
            if rule.language != "rust"
                || !rules.is_syntax_rule_active(rule, &path, &facts.activation_signals)
            {
                continue;
            }
            let mods = [module.clone()];
            let has_derive = |target: &str| {
                derives.iter().any(|d| {
                    registrations::resolve_scope_member(
                        &frames,
                        d,
                        &mods,
                        NS_TYPE | NS_MACRO,
                        |m| (m == target).then_some(target),
                    )
                    .is_some()
                })
            };
            if ctx.node.kind() == "struct_item" && has_derive(parser_derive) {
                if let Some((subcommand_field, subcommand_type)) =
                    extract_subcommand_field(ctx.node, ctx.source, field_attribute, field_marker)
                {
                    parsers.push(CliParserMetadata {
                        pack: rule.pack.clone(),
                        rule: rule.id.clone(),
                        module: module.clone(),
                        parse_method: parse_method.clone(),
                        subcommand_field,
                        subcommand_type,
                    });
                }
            } else if ctx.node.kind() == "enum_item" && has_derive(subcommand_derive) {
                subcommands.push(CliSubcommandMetadata {
                    pack: rule.pack.clone(),
                    rule: rule.id.clone(),
                    module: module.clone(),
                });
            }
        }
        if parsers.is_empty() && subcommands.is_empty() {
            return;
        }
        if let Some(owner) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
            if !parsers.is_empty() {
                owner.set_detail(NodeDetailKey::CLI_COMMAND_PARSER, json!(parsers));
            }
            if !subcommands.is_empty() {
                owner.set_detail(NodeDetailKey::CLI_COMMAND_SUBCOMMAND, json!(subcommands));
            }
        }
    });
}

fn dispatch_scope_shadowed(
    frames: &[RegistrationScopeFrame],
    module: &str,
    parser_derive: &str,
) -> bool {
    if registrations::scope_crate_shadowed(frames, module) {
        return true;
    }
    let mods = [module.to_owned()];
    for f in registrations::scope_active(frames).iter().rev() {
        if registrations::frame_has_local(f, parser_derive, NS_TYPE | NS_MACRO) {
            return true;
        }
        if f.imports.contains_key(parser_derive) {
            return registrations::resolve_scope_member(
                frames,
                parser_derive,
                &mods,
                NS_TYPE | NS_MACRO,
                |m| (m == parser_derive).then_some(parser_derive),
            )
            .is_none();
        }
    }
    false
}

fn collect_pattern_idents(node: Syntax<'_>, source: &str, out: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            out.insert(text(node, source).to_owned());
        }
        "field_pattern" => {
            if let Some(pat) = node.child_by_field_name("pattern") {
                collect_pattern_idents(pat, source, out);
            } else if let Some(name) = node.child_by_field_name("name") {
                out.insert(text(name, source).to_owned());
            }
        }
        "scoped_identifier" | "scoped_type_identifier" | "type_identifier" => {}
        _ => {
            let skip_type = matches!(node.kind(), "tuple_struct_pattern" | "struct_pattern")
                .then(|| node.child_by_field_name("type").map(|t| t.id()))
                .flatten();
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                if Some(child.id()) != skip_type {
                    collect_pattern_idents(child, source, out);
                }
            }
        }
    }
}

#[derive(Default)]
struct BodyScan {
    let_counts: HashMap<String, usize>,
    mutated_bindings: HashSet<String>,
    local_values: HashSet<String>,
    local_types: HashSet<String>,
}

fn scan_function_body(function: Syntax<'_>, body: Syntax<'_>, source: &str) -> BodyScan {
    let mut scan = BodyScan::default();
    if let Some(params) = function.child_by_field_name("parameters") {
        let mut c = params.walk();
        for param in params.named_children(&mut c) {
            if let Some(pat) = param.child_by_field_name("pattern") {
                collect_pattern_idents(pat, source, &mut scan.local_values);
            }
        }
    }
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "let_declaration" => {
                if let Some(pat) = node.child_by_field_name("pattern") {
                    let mut idents = HashSet::new();
                    collect_pattern_idents(pat, source, &mut idents);
                    for name in idents {
                        *scan.let_counts.entry(name.clone()).or_default() += 1;
                        scan.local_values.insert(name);
                    }
                }
            }
            "assignment_expression" | "compound_assignment_expr" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let raw = compact(text(left, source));
                    let root = raw.split('.').next().unwrap_or(&raw);
                    scan.mutated_bindings.insert(root.to_owned());
                }
            }
            "reference_expression" => {
                let mut c = node.walk();
                if node
                    .named_children(&mut c)
                    .any(|ch| ch.kind() == "mutable_specifier")
                {
                    if let Some(v) = node.child_by_field_name("value") {
                        scan.mutated_bindings.insert(compact(text(v, source)));
                    }
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = field(node, source, "name") {
                    scan.local_values.insert(name.to_owned());
                }
            }
            "struct_item" | "enum_item" | "type_item" | "trait_item" | "mod_item" => {
                if let Some(name) = field(node, source, "name") {
                    scan.local_types.insert(name.to_owned());
                }
            }
            _ => {}
        }
        let mut c = node.walk();
        stack.extend(node.named_children(&mut c));
    }
    scan
}

fn extract_parse_call(value: Syntax<'_>, source: &str, parse_method: &str) -> Option<String> {
    if value.kind() != "call_expression"
        || value
            .child_by_field_name("arguments")
            .is_some_and(|args| args.named_child_count() > 0)
    {
        return None;
    }
    let func = value
        .child_by_field_name("function")
        .filter(|n| n.kind() == "scoped_identifier")?;
    (text(func.child_by_field_name("name")?, source) == parse_method)
        .then(|| {
            func.child_by_field_name("path")
                .map(|p| compact(text(p, source)))
        })
        .flatten()
}

fn extract_variant_patterns(pattern: Syntax<'_>, source: &str, out: &mut Vec<(String, String)>) {
    match pattern.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            if let (Some(path), Some(name)) = (
                pattern.child_by_field_name("path"),
                pattern.child_by_field_name("name"),
            ) {
                out.push((compact(text(path, source)), text(name, source).to_owned()));
            }
        }
        "tuple_struct_pattern" | "struct_pattern" => {
            if let Some(ty) = pattern.child_by_field_name("type") {
                extract_variant_patterns(ty, source, out);
            }
        }
        "match_pattern" => {
            if let Some(inner) = pattern.named_child(0) {
                extract_variant_patterns(inner, source, out);
            }
        }
        "reference_pattern"
        | "mut_pattern"
        | "captured_pattern"
        | "or_pattern"
        | "parenthesized_pattern" => {
            let mut c = pattern.walk();
            for child in pattern.named_children(&mut c) {
                extract_variant_patterns(child, source, out);
            }
        }
        _ => {}
    }
}

fn extract_handler_call(mut expr: Syntax<'_>, source: &str) -> Option<(String, usize)> {
    loop {
        match expr.kind() {
            "block" => {
                let mut c = expr.walk();
                let mut stmts = expr
                    .named_children(&mut c)
                    .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"));
                let first = stmts.next()?;
                if stmts.next().is_some() {
                    return None;
                }
                expr = if first.kind() == "expression_statement" {
                    first.named_child(0)?
                } else {
                    first
                };
            }
            "await_expression" | "try_expression" | "parenthesized_expression" => {
                expr = expr.named_child(0)?;
            }
            "call_expression" => {
                let func = expr.child_by_field_name("function")?;
                return matches!(func.kind(), "identifier" | "scoped_identifier")
                    .then(|| (compact(text(func, source)), func.start_position().row));
            }
            _ => return None,
        }
    }
}

fn match_field_scrutinee<'a>(value: Syntax<'a>, source: &'a str, binding: &str) -> Option<&'a str> {
    let inner = if value.kind() == "reference_expression" {
        value.child_by_field_name("value")?
    } else {
        value
    };
    if inner.kind() != "field_expression" {
        return None;
    }
    let (recv, fld) = (
        inner.child_by_field_name("value")?,
        inner.child_by_field_name("field")?,
    );
    (recv.kind() == "identifier" && text(recv, source) == binding).then(|| text(fld, source))
}

pub(super) fn extract_dispatch_registrations(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    registrations::with_rules(|rules| {
        if !has_command_dispatch_rules(rules) {
            return;
        }
        let Some(body) = ctx.node.child_by_field_name("body") else {
            return;
        };
        let Some(module_node) = facts.nodes.iter().find(|n| n.kind == NodeKind::Module) else {
            return;
        };
        let (mod_id, mod_qual, path) = (
            module_node.id.clone(),
            module_node.qualname.clone(),
            module_node.path.clone(),
        );
        let is_test = facts
            .nodes
            .iter()
            .find(|n| n.id == ctx.owner)
            .is_some_and(|n| n.is_test);
        let body_text = text(body, ctx.source);
        let active_rules: Vec<_> = rules
            .syntax_rules
            .iter()
            .filter_map(|rule| {
                let SyntaxOperation::CommandDispatch {
                    module,
                    parser_derive,
                    parse_method,
                    ..
                } = &rule.operation
                else {
                    return None;
                };
                (rule.language == "rust"
                    && body_text.contains(parse_method.as_str())
                    && rules.is_syntax_rule_active(rule, &path, &facts.activation_signals))
                .then_some((
                    rule,
                    module.as_str(),
                    parser_derive.as_str(),
                    parse_method.as_str(),
                ))
            })
            .collect();
        if active_rules.is_empty() {
            return;
        }
        let frames = build_scope_frames(ctx.node, ctx.source);
        let scan = scan_function_body(ctx.node, body, ctx.source);
        let mut body_cursor = body.walk();
        let let_nodes: Vec<_> = body
            .named_children(&mut body_cursor)
            .filter(|n| n.kind() == "let_declaration")
            .collect();

        for (rule, module, parser_derive, parse_method) in active_rules {
            if dispatch_scope_shadowed(&frames, module, parser_derive) {
                continue;
            }
            for decl in &let_nodes {
                let Some(pat) = decl
                    .child_by_field_name("pattern")
                    .filter(|p| p.kind() == "identifier")
                else {
                    continue;
                };
                let Some(parser_type) = decl
                    .child_by_field_name("value")
                    .and_then(|v| extract_parse_call(v, ctx.source, parse_method))
                else {
                    continue;
                };
                let binding = text(pat, ctx.source);
                if scan.let_counts.get(binding).copied().unwrap_or(0) > 1
                    || scan.mutated_bindings.contains(binding)
                    || scan.local_types.contains(path_head(&parser_type))
                {
                    continue;
                }
                let mut pending = vec![body];
                while let Some(node) = pending.pop() {
                    if node.kind() == "match_expression" {
                        if let Some(subcommand_field) = node
                            .child_by_field_name("value")
                            .and_then(|v| match_field_scrutinee(v, ctx.source, binding))
                        {
                            let mut arm_stack = vec![node];
                            while let Some(child) = arm_stack.pop() {
                                if child.kind() != "match_arm" {
                                    let mut c = child.walk();
                                    arm_stack.extend(child.named_children(&mut c));
                                    continue;
                                }
                                let (Some(pat), Some(val)) = (
                                    child.child_by_field_name("pattern"),
                                    child.child_by_field_name("value"),
                                ) else {
                                    continue;
                                };
                                let Some((handler, handler_row)) =
                                    extract_handler_call(val, ctx.source)
                                else {
                                    continue;
                                };
                                let mut arm_locals = HashSet::new();
                                collect_pattern_idents(pat, ctx.source, &mut arm_locals);
                                if !handler.contains("::")
                                    && (scan.local_values.contains(&handler)
                                        || arm_locals.contains(&handler)
                                        || registrations::scope_active(&frames).iter().skip(1).any(
                                            |f| {
                                                registrations::frame_has_local(
                                                    f, &handler, NS_VALUE,
                                                )
                                            },
                                        ))
                                {
                                    continue;
                                }
                                let mut variants = Vec::new();
                                extract_variant_patterns(pat, ctx.source, &mut variants);
                                let handler_line = handler_row + ctx.offset + 1;
                                let col = child.start_position().column;
                                for (enum_type, variant) in variants {
                                    if scan.local_types.contains(path_head(&enum_type)) {
                                        continue;
                                    }
                                    let id = format!(
                                        "cli_command_registration:{path}:{handler_line}:{col}:{variant}"
                                    );
                                    if facts.nodes.iter().any(|n| n.id == id) {
                                        continue;
                                    }
                                    let dispatch = CliDispatchMetadata {
                                        pack: rule.pack.clone(),
                                        rule: rule.id.clone(),
                                        owner_id: ctx.owner.to_owned(),
                                        parser_type: parser_type.clone(),
                                        parse_method: parse_method.to_owned(),
                                        subcommand_field: subcommand_field.to_owned(),
                                        enum_type,
                                        variant: variant.clone(),
                                        handler: handler.clone(),
                                        handler_line,
                                    };
                                    let origin = ValueFlowFacts {
                                        framework: Some(Box::new(FrameworkScopeFacts {
                                            facts: vec![FrameworkFact::RuleOrigin(
                                                ExtractionRuleOrigin {
                                                    pack: rule.pack.clone(),
                                                    rule: rule.id.clone(),
                                                },
                                            )],
                                            ..Default::default()
                                        })),
                                        ..Default::default()
                                    };
                                    facts.pending_flow_nodes.push(facts.nodes.len());
                                    facts.nodes.push(Node {
                                        native_semantic: None,
                                        id: id.clone(),
                                        kind: NodeKind::CliCommandRegistration,
                                        name: variant.clone(),
                                        qualname: format!("{mod_qual}.cli.{variant}"),
                                        path: path.clone(),
                                        line: handler_line,
                                        end_line: handler_line,
                                        is_test,
                                        language: LanguageId::Rust,
                                        generated: false,
                                        details: json!({
                                            "framework": module,
                                            "pack": rule.pack,
                                            "rule": rule.id,
                                            NodeDetailKey::CLI_COMMAND_DISPATCH: dispatch,
                                            "value_flow": origin,
                                        }),
                                    });
                                    facts.edges.push(Edge {
                                        src: mod_id.clone(),
                                        dst: id,
                                        kind: "contains".into(),
                                        path: path.clone(),
                                        line: handler_line,
                                        evidence: "configured command dispatch".into(),
                                        confidence: "exact".into(),
                                    });
                                }
                            }
                            continue;
                        }
                    }
                    let mut c = node.walk();
                    pending.extend(node.named_children(&mut c));
                }
            }
        }
    });
}
