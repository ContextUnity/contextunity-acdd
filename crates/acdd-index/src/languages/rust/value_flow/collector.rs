use super::expr::{
    callee_name, sanitize_binding_value, strip_pattern_wrappers, unwrap_option_result_member,
    value_depth, PatternProjection,
};
use super::macros::{
    collect_ancestor_shadows, file_macro_facts, parse_thread_local_statics, FileMacroFacts,
};
use super::types::{
    extract_callable_signature, generic_names, generic_param_serializer_bound, is_async,
    type_expr_has_unknown, type_expr_inner,
};
use crate::ast::syntax::{collect, text, ScopeVisitor, SyntaxCollector};
use crate::languages::support::value_flow_core::{ScopeFrameFlags, ValueFlowCollectorCore};
use crate::semantic::{
    BindingFact, CallSiteEvidence, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use std::collections::HashSet;
use tree_sitter::Node;

pub(crate) struct RustValueFlowCollector<'source, 'tree> {
    root: Node<'tree>,
    body: Option<Node<'tree>>,
    source: &'source str,
    file_facts: std::sync::Arc<FileMacroFacts>,
    core: ValueFlowCollectorCore,
    generic_names: Option<HashSet<&'source str>>,
    block_shadows: Vec<HashSet<&'static str>>,
    closure_depth: usize,
}

#[derive(Clone, Copy)]
struct PatternBindOptions {
    position: SourcePosition,
    body_end: Option<SourcePosition>,
    conditional: bool,
    skip_uppercase_ident: bool,
    wrap_unknown: bool,
}

impl<'source, 'tree> RustValueFlowCollector<'source, 'tree> {
    pub(crate) fn new(node: Node<'tree>, source: &'source str) -> Self {
        let file_facts = file_macro_facts(node, source);
        let root_shadows = collect_ancestor_shadows(node, source, &file_facts);
        let block_shadows = vec![root_shadows];
        let mut core = ValueFlowCollectorCore::with_scope_capacity(16);
        collect_enclosing_thread_locals(node, &file_facts, &mut core);

        let mut generics = None;
        let mut collector = Self {
            root: node,
            body: node.child_by_field_name("body"),
            source,
            file_facts,
            core,
            generic_names: None,
            block_shadows,
            closure_depth: 0,
        };
        if matches!(node.kind(), "type_item" | "const_item" | "static_item") {
            let names = generics.get_or_insert_with(|| generic_names(node, source));
            let alias = node
                .child_by_field_name("type")
                .map(|ty| type_expr_inner(ty, source, names, 0));
            collector.core.set_alias_type_if_none(alias);
        }
        if matches!(node.kind(), "function_item" | "function_signature_item") {
            let names = generics.get_or_insert_with(|| generic_names(node, source));
            collector.init_function_facts(node, names);
        }
        if node.kind() == "struct_item" {
            let names = generics.get_or_insert_with(|| generic_names(node, source));
            collector.init_struct_fields(names);
        }
        collector.generic_names = generics;
        collector
    }

    fn init_function_facts(&mut self, fn_node: Node<'tree>, names: &HashSet<&'source str>) {
        let source = self.source;
        let ret_ty = fn_node.child_by_field_name("return_type").map(|ty| {
            if is_async(fn_node, source) {
                TypeExpr::Unknown
            } else {
                type_expr_inner(ty, source, names, 0)
            }
        });
        if let Some(sig) = extract_callable_signature(fn_node, source, names, ret_ty.clone()) {
            self.core.facts_mut().callable_signature = Some(sig);
        }
        self.core.set_return_type_if_none(ret_ty);
        let Some(parameters) = fn_node.child_by_field_name("parameters") else {
            return;
        };
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            self.init_function_parameter(fn_node, parameter, names);
        }
    }

    fn init_function_parameter(
        &mut self,
        fn_node: Node<'tree>,
        parameter: Node<'tree>,
        names: &HashSet<&'source str>,
    ) {
        let source = self.source;
        if parameter.kind() == "self_parameter" {
            let syntax = text(parameter, source).trim();
            let mut type_expr = TypeExpr::Named {
                name: "Self".to_owned(),
            };
            if syntax.starts_with('&') {
                type_expr = TypeExpr::Applied {
                    base: if syntax
                        .split_whitespace()
                        .any(|part| matches!(part, "mut" | "&mut"))
                    {
                        "&mut"
                    } else {
                        "&"
                    }
                    .to_owned(),
                    args: vec![type_expr],
                };
            }
            let position = parameter.end_position();
            self.core.push_binding(
                "self".to_owned(),
                SourcePosition {
                    line: position.row + 1,
                    column: position.column,
                },
                ValueExpr::Annotated { type_expr },
                false,
            );
            return;
        }
        let (Some(pattern), Some(annotation)) = (
            parameter.child_by_field_name("pattern"),
            parameter.child_by_field_name("type"),
        ) else {
            return;
        };
        let position = parameter.end_position();
        let bind_pos = SourcePosition {
            line: position.row + 1,
            column: position.column,
        };
        let resolved = match type_expr_inner(annotation, source, names, 0) {
            TypeExpr::Unknown
                if annotation.kind() == "type_identifier"
                    && generic_param_serializer_bound(
                        fn_node,
                        text(annotation, source).trim(),
                        source,
                    ) =>
            {
                TypeExpr::Named {
                    name: "serde::Serializer".to_owned(),
                }
            }
            other => other,
        };
        let nominal = super::types::unwrap_reference_type(annotation);
        if (nominal.kind() == "primitive_type" && text(nominal, source) != "str")
            || matches!(resolved, TypeExpr::Unknown)
        {
            return;
        }
        self.bind_rust_pattern(
            pattern,
            ValueExpr::Annotated {
                type_expr: resolved,
            },
            PatternBindOptions {
                position: bind_pos,
                body_end: None,
                conditional: false,
                skip_uppercase_ident: false,
                wrap_unknown: false,
            },
        );
    }

    fn init_struct_fields(&mut self, names: &HashSet<&'source str>) {
        let Some(body) = self.body else {
            return;
        };
        let source = self.source;
        let mut cursor = body.walk();
        if body.kind() == "ordered_field_declaration_list" {
            for (ordinal, ty) in body.children_by_field_name("type", &mut cursor).enumerate() {
                let position = ty.start_position();
                self.core.push_field(
                    ordinal.to_string(),
                    SourcePosition {
                        line: position.row + 1,
                        column: position.column,
                    },
                    ValueExpr::Annotated {
                        type_expr: type_expr_inner(ty, source, names, 0),
                    },
                    false,
                );
            }
        } else {
            for field in body.named_children(&mut cursor) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                if let (Some(name), Some(ty)) = (
                    field.child_by_field_name("name"),
                    field.child_by_field_name("type"),
                ) {
                    let position = field.start_position();
                    self.core.push_field(
                        text(name, source).to_owned(),
                        SourcePosition {
                            line: position.row + 1,
                            column: position.column,
                        },
                        ValueExpr::Annotated {
                            type_expr: type_expr_inner(ty, source, names, 0),
                        },
                        false,
                    );
                }
            }
        }
        self.body = None;
    }

    fn current_shadows(&self) -> HashSet<&'static str> {
        let mut shadows = HashSet::new();
        for scope in &self.block_shadows {
            shadows.extend(scope);
        }
        shadows
    }

    fn push_binding_checked(
        &mut self,
        name: String,
        position: SourcePosition,
        value: ValueExpr,
        conditional: bool,
    ) {
        if name.is_empty() || name.len() > 4_096 || position.line == 0 {
            return;
        }
        self.core
            .push_binding(name, position, sanitize_binding_value(value), conditional);
    }

    fn wrap_scoped(
        value: ValueExpr,
        body_end: Option<SourcePosition>,
        wrap_unknown: bool,
    ) -> ValueExpr {
        match body_end {
            Some(end) if wrap_unknown || !matches!(value, ValueExpr::Unknown) => ValueExpr::Scoped {
                value: Box::new(value),
                body_end: end,
            },
            _ => value,
        }
    }

    /// Recursively binds identifiers in any Rust pattern (`identifier`, `tuple_pattern`,
    /// `slice_pattern`, `tuple_struct_pattern`, `struct_pattern`, `or_pattern`, and wrappers)
    /// against `base_val`.
    fn bind_rust_pattern(
        &mut self,
        pattern: Node<'_>,
        base_val: ValueExpr,
        opts: PatternBindOptions,
    ) {
        let pat = strip_pattern_wrappers(pattern);
        match pat.kind() {
            "identifier" => {
                let name = text(pat, self.source).trim();
                let valid = name != "_"
                    && (!opts.skip_uppercase_ident
                        || name.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_'));
                if valid {
                    self.push_binding_checked(
                        name.to_owned(),
                        opts.position,
                        Self::wrap_scoped(base_val, opts.body_end, opts.wrap_unknown),
                        opts.conditional,
                    );
                }
            }
            "tuple_pattern" | "slice_pattern" => {
                let is_tuple = pat.kind() == "tuple_pattern";
                let mut cursor = pat.walk();
                let elems: Vec<Node<'_>> = pat.named_children(&mut cursor).collect();
                let rest_idx = elems.iter().position(|p| {
                    p.kind() == "rest_pattern" || text(*p, self.source).trim() == ".."
                });
                for (idx, elem) in elems.into_iter().enumerate() {
                    if elem.kind() == "rest_pattern" || text(elem, self.source).trim() == ".." {
                        continue;
                    }
                    let elem_val = if matches!(base_val, ValueExpr::Unknown)
                        || (is_tuple && rest_idx.is_some_and(|r| idx > r))
                    {
                        ValueExpr::Unknown
                    } else if is_tuple {
                        ValueExpr::RustField {
                            value: Box::new(base_val.clone()),
                            member: idx.to_string(),
                        }
                    } else {
                        ValueExpr::RustProject {
                            value: Box::new(base_val.clone()),
                            operation: crate::semantic::types::RustProjection::ElementIndex,
                        }
                    };
                    self.bind_rust_pattern(elem, elem_val, opts);
                }
            }
            "tuple_struct_pattern" => self.bind_tuple_struct_pattern(pat, base_val, opts),
            "struct_pattern" => self.bind_struct_pattern(pat, base_val, opts),
            "or_pattern" if pat.named_child_count() == 1 => {
                if let Some(inner) = pat.named_child(0) {
                    self.bind_rust_pattern(inner, base_val, opts);
                }
            }
            _ => {
                let mut bound_names = Vec::new();
                super::super::patterns::bindings(pat, |id_node| {
                    bound_names.push(text(id_node, self.source).to_owned());
                });
                for name in bound_names {
                    self.push_binding_checked(
                        name,
                        opts.position,
                        Self::wrap_scoped(ValueExpr::Unknown, opts.body_end, opts.wrap_unknown),
                        opts.conditional,
                    );
                }
            }
        }
    }

    fn bind_tuple_struct_pattern(
        &mut self,
        pat: Node<'_>,
        base_val: ValueExpr,
        opts: PatternBindOptions,
    ) {
        let type_child = pat.child_by_field_name("type");
        let unwrap_member = unwrap_option_result_member(pat, self.source);
        let mut cursor = pat.walk();
        let payloads: Vec<Node<'_>> = pat
            .named_children(&mut cursor)
            .filter(|c| Some(*c) != type_child)
            .collect();
        for (idx, child) in payloads.into_iter().enumerate() {
            let child_val = if matches!(base_val, ValueExpr::Unknown) {
                ValueExpr::Unknown
            } else if let Some(projection) = unwrap_member {
                match projection {
                    PatternProjection::Identity => base_val.clone(),
                    PatternProjection::Error => ValueExpr::RustProject {
                        value: Box::new(base_val.clone()),
                        operation: crate::semantic::types::RustProjection::ErrorPayload,
                    },
                    PatternProjection::Payload => ValueExpr::Project {
                        value: Box::new(base_val.clone()),
                        member: "unwrap".to_owned(),
                    },
                }
            } else {
                ValueExpr::RustField {
                    value: Box::new(base_val.clone()),
                    member: idx.to_string(),
                }
            };
            self.bind_rust_pattern(child, child_val, opts);
        }
    }

    fn bind_struct_pattern(
        &mut self,
        pat: Node<'_>,
        base_val: ValueExpr,
        opts: PatternBindOptions,
    ) {
        let source = self.source;
        let mut cursor = pat.walk();
        for field_node in pat.named_children(&mut cursor) {
            match field_node.kind() {
                "shorthand_field_identifier" => {
                    let field_name = text(field_node, source).trim().to_owned();
                    let proj = Self::project_struct_field(&base_val, &field_name);
                    self.push_binding_checked(
                        field_name,
                        opts.position,
                        Self::wrap_scoped(proj, opts.body_end, opts.wrap_unknown),
                        opts.conditional,
                    );
                }
                "field_pattern" => {
                    let f = field_node
                        .child_by_field_name("name")
                        .map(|n| text(n, source).trim().to_owned());
                    let sub_pat = field_node.child_by_field_name("pattern");
                    if let (Some(field_name), Some(sub)) = (f, sub_pat) {
                        let proj = Self::project_struct_field(&base_val, &field_name);
                        self.bind_rust_pattern(sub, proj, opts);
                    } else if let Some(shorthand) = {
                        let mut fc = field_node.walk();
                        let found = field_node
                            .named_children(&mut fc)
                            .find(|c| c.kind() == "shorthand_field_identifier");
                        found
                    } {
                        let field_name = text(shorthand, source).trim().to_owned();
                        let proj = Self::project_struct_field(&base_val, &field_name);
                        self.push_binding_checked(
                            field_name,
                            opts.position,
                            Self::wrap_scoped(proj, opts.body_end, opts.wrap_unknown),
                            opts.conditional,
                        );
                    } else {
                        self.bind_rust_pattern(field_node, ValueExpr::Unknown, opts);
                    }
                }
                _ => {}
            }
        }
    }

    fn project_struct_field(base_val: &ValueExpr, field_name: &str) -> ValueExpr {
        if matches!(base_val, ValueExpr::Unknown) {
            ValueExpr::Unknown
        } else {
            ValueExpr::RustField {
                value: Box::new(base_val.clone()),
                member: field_name.to_string(),
            }
        }
    }

    pub(crate) fn enter(&mut self, current: Node<'tree>) {
        let parent = self.core.scope_state().current_or_default();
        let active = self.body == Some(current)
            || (parent.is_active()
                && !matches!(
                    current.kind(),
                    "function_item" | "impl_item" | "trait_item" | "mod_item"
                ));
        let branch_conditional = parent.is_branch_conditional()
            || (self.body != Some(current)
                && matches!(
                    current.kind(),
                    "if_expression"
                        | "match_expression"
                        | "while_expression"
                        | "for_expression"
                        | "loop_expression"
                ));
        self.core.scope_state_mut().push(
            ScopeFrameFlags::new()
                .with_active(active)
                .with_branch_conditional(branch_conditional)
                .with_collect_facts(active),
        );
        if !active {
            return;
        }
        match current.kind() {
            "macro_invocation" => self.handle_thread_local_macro(current),
            "block" => self.push_block_macro_shadows(current),
            "macro_definition" => self.record_block_macro_shadow(current),
            "for_expression" => self.handle_for_expression(current),
            "closure_expression" => self.handle_closure_expression(current),
            "let_declaration" if self.closure_depth == 0 => {
                self.handle_let_or_assignment(current);
            }
            "assignment_expression" | "compound_assignment_expr" => {
                self.handle_let_or_assignment(current);
            }
            "let_condition" => self.handle_let_condition(current),
            "match_arm" => self.handle_match_arm(current),
            _ => {}
        }
    }

    fn handle_thread_local_macro(&mut self, current: Node<'tree>) {
        let Some(m_name) = current.child_by_field_name("macro") else {
            return;
        };
        if !matches!(
            text(m_name, self.source).trim(),
            "thread_local" | "std::thread_local"
        ) {
            return;
        }
        let Some(tree) = current
            .named_children(&mut current.walk())
            .find(|c| c.kind() == "token_tree")
        else {
            return;
        };
        let cond = self.core.scope_state().is_conditional();
        for (name, pos, ty_expr) in parse_thread_local_statics(tree, self.source) {
            self.push_binding_checked(
                name,
                pos,
                ValueExpr::Annotated {
                    type_expr: TypeExpr::Applied {
                        base: "std::thread::LocalKey".to_owned(),
                        args: vec![ty_expr],
                    },
                },
                cond,
            );
        }
    }

    fn push_block_macro_shadows(&mut self, current: Node<'tree>) {
        let block_set = self
            .file_facts
            .blocks
            .get(&current.id())
            .map(|b| b.imports.clone())
            .unwrap_or_default();
        self.block_shadows.push(block_set);
    }

    fn record_block_macro_shadow(&mut self, current: Node<'tree>) {
        if let Some(m_name) = current.child_by_field_name("name") {
            let name = text(m_name, self.source).trim();
            if let Some(last) = self.block_shadows.last_mut() {
                if name == "vec" {
                    last.insert("vec");
                } else if name == "format" {
                    last.insert("format");
                }
            }
        }
    }

    fn handle_for_expression(&mut self, current: Node<'tree>) {
        let (Some(pattern), Some(iterable), Some(body)) = (
            current.child_by_field_name("pattern"),
            current.child_by_field_name("value"),
            current.child_by_field_name("body"),
        ) else {
            return;
        };
        let start = body.start_position();
        let end = body.end_position();
        let body_pos = SourcePosition {
            line: start.row + 1,
            column: start.column,
        };
        let body_end = SourcePosition {
            line: end.row + 1,
            column: end.column,
        };
        let iter_expr = value_depth(iterable, self.source, 0, &self.current_shadows());
        let iter_item = match iter_expr {
            ValueExpr::RustProject { value, operation: crate::semantic::types::RustProjection::ClosureFlatMap } => {
                ValueExpr::RustProject {
                    value,
                    operation: crate::semantic::types::RustProjection::FlatMapItem,
                }
            }
            ValueExpr::RustProject { operation: crate::semantic::types::RustProjection::ClosureFilterMap, .. } => {
                ValueExpr::RustProject {
                    value: Box::new(iter_expr),
                    operation: crate::semantic::types::RustProjection::IteratorItem,
                }
            }
            other => ValueExpr::RustProject {
                value: Box::new(ValueExpr::Project {
                    value: Box::new(other),
                    member: "into_iter".to_owned(),
                }),
                operation: crate::semantic::types::RustProjection::IteratorItem,
            },
        };
        self.bind_rust_pattern(
            pattern,
            iter_item,
            PatternBindOptions {
                position: body_pos,
                body_end: Some(body_end),
                conditional: false,
                skip_uppercase_ident: false,
                wrap_unknown: true,
            },
        );
    }

    fn handle_closure_expression(&mut self, current: Node<'tree>) {
        self.closure_depth += 1;
        let body_end_pos = current.end_position();
        let body_end = SourcePosition {
            line: body_end_pos.row + 1,
            column: body_end_pos.column,
        };
        let inferred_param_values = self.infer_closure_param_values(current);
        let Some(params) = current.child_by_field_name("parameters") else {
            return;
        };
        let mut cursor = params.walk();
        for (idx, param) in params.named_children(&mut cursor).enumerate() {
            let (param_node, explicit_type) = if param.kind() == "parameter" {
                (
                    param.child_by_field_name("pattern"),
                    param.child_by_field_name("type").map(|ty| {
                        let names = self
                            .generic_names
                            .get_or_insert_with(|| generic_names(self.root, self.source));
                        type_expr_inner(ty, self.source, names, 0)
                    }),
                )
            } else {
                (Some(param), None)
            };
            let Some(param_node) = param_node else {
                continue;
            };
            let val_expr = if let Some(ty) = explicit_type {
                ValueExpr::Annotated { type_expr: ty }
            } else if let Some(Some(inferred)) = inferred_param_values.get(idx) {
                inferred.clone()
            } else {
                ValueExpr::Unknown
            };
            let pos = param.end_position();
            let position = SourcePosition {
                line: pos.row + 1,
                column: pos.column,
            };
            self.bind_rust_pattern(
                param_node,
                val_expr,
                PatternBindOptions {
                    position,
                    body_end: Some(body_end),
                    conditional: false,
                    skip_uppercase_ident: false,
                    wrap_unknown: true,
                },
            );
        }
    }

    fn infer_closure_param_values(&self, closure: Node<'_>) -> Vec<Option<ValueExpr>> {
        let mut inferred = Vec::new();
        let Some((parent, arg_node, arg_idx, call)) = enclosing_call_argument(closure) else {
            return inferred;
        };
        let Some(func) = call.child_by_field_name("function").map(|f| {
            if f.kind() == "generic_function" {
                f.child_by_field_name("function").unwrap_or(f)
            } else {
                f
            }
        }) else {
            return inferred;
        };
        let param_count = closure
            .child_by_field_name("parameters")
            .map_or(0, |p| p.named_child_count());
        if func.kind() == "field_expression" {
            let Some(field) = func.child_by_field_name("field") else {
                return inferred;
            };
            let recv_opt = func.child_by_field_name("value");
            let method_name = text(field, self.source);
            let recv_val_opt = recv_opt.and_then(|recv| {
                let shadows = self.current_shadows();
                let val = value_depth(recv, self.source, 0, &shadows);
                (!matches!(val, ValueExpr::Unknown)).then_some(val)
            });
            if push_builtin_closure_projections(
                method_name,
                parent,
                arg_node,
                param_count,
                recv_val_opt.as_ref(),
                &mut inferred,
            ) {
                return inferred;
            }
            if let Some(recv_val) = recv_val_opt {
                let pos = call.start_position();
                let call_expr = ValueExpr::Call {
                    callee: method_name.to_owned(),
                    site: Some(CallSiteEvidence {
                        position: SourcePosition {
                            line: pos.row + 1,
                            column: pos.column,
                        },
                        receiver: Some(Box::new(recv_val)),
                        arguments: Vec::new(),
                    }),
                };
                for param_idx in 0..param_count {
                    inferred.push(Some(ValueExpr::CallbackParam {
                        callee: Box::new(call_expr.clone()),
                        arg_index: arg_idx,
                        param_index: param_idx,
                    }));
                }
            }
        } else if let Some(callee) = callee_name(func, self.source) {
            let call_expr = ValueExpr::Call {
                callee: callee.into_owned(),
                site: None,
            };
            for param_idx in 0..param_count {
                inferred.push(Some(ValueExpr::CallbackParam {
                    callee: Box::new(call_expr.clone()),
                    arg_index: arg_idx,
                    param_index: param_idx,
                }));
            }
        }
        inferred
    }

    fn handle_let_or_assignment(&mut self, current: Node<'tree>) {
        let is_conditional = self.core.scope_state().is_conditional();
        let declaration = current.kind() == "let_declaration";
        let lexical_end = declaration.then(|| find_enclosing_block_end(current, self.body)).flatten();
        let Some(pattern) = current
            .child_by_field_name("pattern")
            .or_else(|| current.child_by_field_name("left"))
        else {
            return;
        };
        if !declaration
            && !matches!(
                pattern.kind(),
                "identifier" | "tuple_pattern" | "mut_pattern"
            )
        {
            return;
        }
        let pos = current.end_position();
        let bind_pos = SourcePosition {
            line: pos.row + 1,
            column: pos.column,
        };
        if declaration && pattern.kind() == "tuple_pattern" {
            self.handle_let_tuple_pattern(current, pattern, bind_pos, lexical_end, is_conditional);
            return;
        }
        let val_expr = if declaration {
            self.let_declaration_value(current, pattern)
        } else {
            self.assignment_value(current)
        };
        self.bind_rust_pattern(
            pattern,
            val_expr,
            PatternBindOptions {
                position: bind_pos,
                body_end: lexical_end,
                conditional: is_conditional,
                skip_uppercase_ident: false,
                wrap_unknown: true,
            },
        );
    }

    fn handle_let_tuple_pattern(
        &mut self,
        current: Node<'tree>,
        pattern: Node<'_>,
        bind_pos: SourcePosition,
        lexical_end: Option<SourcePosition>,
        is_conditional: bool,
    ) {
        let mut pat_cursor = pattern.walk();
        let pat_elements: Vec<Node<'_>> = pattern.named_children(&mut pat_cursor).collect();
        let val_opt = current.child_by_field_name("value");
        let val_elements = tuple_children(val_opt, "tuple_expression");
        let mut type_elements = tuple_children(current.child_by_field_name("type"), "tuple_type");
        if val_elements.is_none() && type_elements.is_none() {
            if let Some(val_node) = val_opt.filter(|v| v.kind() == "call_expression") {
                if let Some(func_node) = val_node.child_by_field_name("function") {
                    let fn_name = text(func_node, self.source).trim();
                    type_elements = resolve_function_return_tuple(
                        current,
                        fn_name,
                        self.source,
                        &self.core.facts().bindings,
                    );
                }
            }
        }
        let rest_idx = pat_elements
            .iter()
            .position(|p| p.kind() == "rest_pattern" || text(*p, self.source).trim() == "..");
        let shadows = self.current_shadows();
        let resolve_tuple_idx = |i: usize, len: usize| -> Option<usize> {
            if let Some(r) = rest_idx {
                if i < r {
                    Some(i)
                } else {
                    len.checked_sub(pat_elements.len() - i)
                }
            } else {
                Some(i)
            }
        };
        let base_tuple_val = if type_elements.is_none() && val_elements.is_none() && rest_idx.is_none()
        {
            val_opt.map(|val_node| value_depth(val_node, self.source, 0, &shadows))
        } else {
            None
        };
        for (i, pat_node) in pat_elements.iter().enumerate() {
            if pat_node.kind() == "rest_pattern" || text(*pat_node, self.source).trim() == ".." {
                continue;
            }
            let slot_val = if let Some(ref types) = type_elements {
                resolve_tuple_idx(i, types.len())
                    .and_then(|idx| types.get(idx).copied())
                    .map_or(ValueExpr::Unknown, |ty_node| {
                        let names = self
                            .generic_names
                            .get_or_insert_with(|| generic_names(self.root, self.source));
                        ValueExpr::Annotated {
                            type_expr: type_expr_inner(ty_node, self.source, names, 0),
                        }
                    })
            } else if let Some(ref vals) = val_elements {
                resolve_tuple_idx(i, vals.len())
                    .and_then(|idx| vals.get(idx).copied())
                    .map_or(ValueExpr::Unknown, |val_node| {
                        value_depth(val_node, self.source, 0, &shadows)
                    })
            } else if let Some(ref base) = base_tuple_val {
                if matches!(base, ValueExpr::Unknown) {
                    ValueExpr::Unknown
                } else {
                    ValueExpr::RustField {
                        value: Box::new(base.clone()),
                        member: i.to_string(),
                    }
                }
            } else {
                ValueExpr::Unknown
            };
            self.bind_rust_pattern(
                *pat_node,
                slot_val,
                PatternBindOptions {
                    position: bind_pos,
                    body_end: lexical_end,
                    conditional: is_conditional,
                    skip_uppercase_ident: false,
                    wrap_unknown: true,
                },
            );
        }
    }

    fn let_declaration_value(&mut self, current: Node<'tree>, pattern: Node<'_>) -> ValueExpr {
        let Some(val_node) = current.child_by_field_name("value") else {
            return ValueExpr::Unknown;
        };
        if matches!(strip_pattern_wrappers(pattern).kind(), "identifier") {
            let complete_type = current.child_by_field_name("type").and_then(|ty| {
                let names = self
                    .generic_names
                    .get_or_insert_with(|| generic_names(self.root, self.source));
                let expr = type_expr_inner(ty, self.source, names, 0);
                (!type_expr_has_unknown(&expr)).then_some(ValueExpr::Annotated { type_expr: expr })
            });
            if let Some(expr) = complete_type {
                return expr;
            }
        }
        let shadows = self.current_shadows();
        value_depth(val_node, self.source, 0, &shadows)
    }

    fn assignment_value(&self, current: Node<'tree>) -> ValueExpr {
        let Some(val) = current
            .child_by_field_name("right")
            .or_else(|| current.child_by_field_name("value"))
        else {
            return ValueExpr::Unknown;
        };
        let shadows = self.current_shadows();
        let expr = value_depth(val, self.source, 0, &shadows);
        if matches!(expr, ValueExpr::Project { .. } | ValueExpr::RustField { .. } | ValueExpr::RustProject { .. } | ValueExpr::Alias { .. })
            || matches!(
                &expr,
                ValueExpr::Call { callee, .. }
                    if matches!(callee.as_str(), "Some" | "Ok" | "Err" | "None")
            )
        {
            expr
        } else {
            ValueExpr::Unknown
        }
    }

    fn handle_let_condition(&mut self, current: Node<'tree>) {
        let (Some(pattern), Some(val_node)) = (
            current.child_by_field_name("pattern"),
            current.child_by_field_name("value"),
        ) else {
            return;
        };
        let body_end = find_let_condition_body_end(current);
        let is_conditional = body_end.is_none() || self.core.scope_state().is_conditional();
        let pos = current.end_position();
        let bind_pos = SourcePosition {
            line: pos.row + 1,
            column: pos.column,
        };
        let shadows = self.current_shadows();
        let cond_opts = PatternBindOptions {
            position: bind_pos,
            body_end,
            conditional: is_conditional,
            skip_uppercase_ident: true,
            wrap_unknown: body_end.is_some(),
        };
        if pattern.kind() == "tuple_pattern" && val_node.kind() == "tuple_expression" {
            let mut p_cursor = pattern.walk();
            let mut v_cursor = val_node.walk();
            let pat_els: Vec<_> = pattern.named_children(&mut p_cursor).collect();
            let val_els: Vec<_> = val_node.named_children(&mut v_cursor).collect();
            for (idx, pat_el) in pat_els.into_iter().enumerate() {
                let slot_val = val_els
                    .get(idx)
                    .map_or(ValueExpr::Unknown, |v| value_depth(*v, self.source, 0, &shadows));
                self.bind_rust_pattern(pat_el, slot_val, cond_opts);
            }
            return;
        }
        let base_val = value_depth(val_node, self.source, 0, &shadows);
        self.bind_rust_pattern(pattern, base_val, cond_opts);
    }

    fn handle_match_arm(&mut self, current: Node<'tree>) {
        let scrutinee = current
            .parent()
            .and_then(|block| block.parent())
            .filter(|m| m.kind() == "match_expression")
            .and_then(|m| m.child_by_field_name("value"));
        let mut pattern_opt = current.child_by_field_name("pattern");
        if let Some(pat) = pattern_opt.filter(|p| p.kind() == "match_pattern") {
            pattern_opt = pat.named_child(0);
        }
        let (Some(pat), Some(val_node)) = (pattern_opt, scrutinee) else {
            return;
        };
        let end = current.end_position();
        let body_end = SourcePosition {
            line: end.row + 1,
            column: end.column,
        };
        let pos = pat.end_position();
        let bind_pos = SourcePosition {
            line: pos.row + 1,
            column: pos.column,
        };
        let shadows = self.current_shadows();
        let base_val = value_depth(val_node, self.source, 0, &shadows);
        self.bind_rust_pattern(
            pat,
            base_val,
            PatternBindOptions {
                position: bind_pos,
                body_end: Some(body_end),
                conditional: true,
                skip_uppercase_ident: true,
                wrap_unknown: true,
            },
        );
    }

    pub(crate) fn exit(&mut self, node: Node<'tree>) {
        if node.kind() == "closure_expression"
            && self.core.scope_state().current_or_default().is_active()
        {
            self.closure_depth = self.closure_depth.saturating_sub(1);
        }
        if node.kind() == "block" && self.block_shadows.len() > 1 {
            self.block_shadows.pop();
        }
        self.core.scope_state_mut().pop();
    }

    pub(crate) fn finish(self) -> ValueFlowFacts {
        self.core.finish()
    }
}

impl<'tree> SyntaxCollector<'tree> for RustValueFlowCollector<'_, 'tree> {
    type Output = ValueFlowFacts;
    fn enter(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::enter(self, node);
    }
    fn exit(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::exit(self, node);
    }
    fn finish(self) -> ValueFlowFacts {
        RustValueFlowCollector::finish(self)
    }
}

impl<'source, 'tree> ScopeVisitor<'source, 'tree> for RustValueFlowCollector<'source, 'tree> {
    fn enter(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::enter(self, node);
    }

    fn exit(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::exit(self, node);
    }

    fn enter_suspended(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::enter(self, node);
    }

    fn exit_suspended(&mut self, node: Node<'tree>) {
        RustValueFlowCollector::exit(self, node);
    }

    fn finish(
        self: Box<Self>,
        _collect_module_aliases: bool,
    ) -> (ValueFlowFacts, Option<HashSet<String>>) {
        ((*self).finish(), None)
    }
}

pub(in super::super) fn extract(node: Node<'_>, source: &str) -> ValueFlowFacts {
    collect(node, RustValueFlowCollector::new(node, source))
}

fn collect_enclosing_thread_locals(
    node: Node<'_>,
    file_facts: &FileMacroFacts,
    core: &mut ValueFlowCollectorCore,
) {
    let mut scope_ids = Vec::new();
    let mut anc = node.parent();
    while let Some(curr) = anc {
        if curr.kind() == "declaration_list" || curr.kind() == "source_file" {
            scope_ids.push(curr.id());
        }
        anc = curr.parent();
    }
    for (scope_id, name, _pos, ty_expr) in &file_facts.thread_locals {
        if scope_ids.contains(scope_id) {
            core.push_binding(
                name.clone(),
                SourcePosition { line: 1, column: 0 },
                ValueExpr::Annotated {
                    type_expr: TypeExpr::Applied {
                        base: "std::thread::LocalKey".to_owned(),
                        args: vec![ty_expr.clone()],
                    },
                },
                false,
            );
        }
    }
}

fn push_builtin_closure_projections(
    method_name: &str,
    parent: Node<'_>,
    arg_node: Node<'_>,
    param_count: usize,
    recv_val: Option<&ValueExpr>,
    inferred: &mut Vec<Option<ValueExpr>>,
) -> bool {
    use crate::semantic::types::RustProjection;
    let special = match method_name {
        "query_map" | "query_row" | "query_and_then" | "query_row_and_then" => Some(RustProjection::ClosureRow),
        "filter_entry" => Some(RustProjection::FilterEntryItem),
        "with" | "try_with" => Some(RustProjection::LocalKey),
        "map_err" | "inspect_err" | "unwrap_or_else" | "ok_or_else" | "or_else" => Some(RustProjection::ErrorPayload),
        "map" | "filter" | "find" | "position" | "rposition" | "any" | "all" | "for_each"
        | "flat_map" | "filter_map" | "inspect" | "is_some_and" | "is_none_or" | "is_ok_and"
        | "is_err_and" | "and_then" | "find_map" | "try_for_each" | "skip_while" | "take_while"
        | "partition" | "max_by_key" | "min_by_key" | "sort_by_key" | "sort_unstable_by_key" => Some(RustProjection::IteratorItem),
        "map_or" | "map_or_else" if parent.named_child(1) == Some(arg_node) => Some(RustProjection::IteratorItem),
        _ => None,
    };
    if let Some(operation) = special {
        if let Some(recv_val) = recv_val {
            inferred.push(Some(ValueExpr::RustProject {
                value: Box::new(recv_val.clone()),
                operation,
            }));
        }
        return true;
    }
    let Some(recv_val) = recv_val else {
        return matches!(method_name, "retain" | "sort_by" | "max_by" | "min_by" | "reduce" | "fold" | "try_fold");
    };
    let item = ValueExpr::RustProject {
        value: Box::new(recv_val.clone()),
        operation: RustProjection::IteratorItem,
    };
    match method_name {
        "retain" => {
            if param_count.max(1) >= 2 {
                inferred.push(Some(ValueExpr::RustField {
                    value: Box::new(item.clone()),
                    member: "0".to_owned(),
                }));
                inferred.push(Some(ValueExpr::RustField {
                    value: Box::new(item),
                    member: "1".to_owned(),
                }));
            } else {
                inferred.push(Some(item));
            }
            true
        }
        "sort_by" | "max_by" | "min_by" | "reduce" => {
            inferred.push(Some(item.clone()));
            inferred.push(Some(item));
            true
        }
        "fold" | "try_fold" => {
            inferred.push(None);
            inferred.push(Some(item));
            true
        }
        _ => false,
    }
}

fn find_enclosing_block_end(current: Node<'_>, body: Option<Node<'_>>) -> Option<SourcePosition> {
    let mut parent = current.parent();
    while let Some(node) = parent {
        if node.kind() == "block" {
            if Some(node) != body {
                let position = node.end_position();
                return Some(SourcePosition {
                    line: position.row + 1,
                    column: position.column,
                });
            }
            return None;
        }
        parent = node.parent();
    }
    None
}

fn find_let_condition_body_end(current: Node<'_>) -> Option<SourcePosition> {
    let mut p = current.parent();
    while let Some(parent) = p {
        if parent.kind() == "if_expression" || parent.kind() == "while_expression" {
            let block = if parent.kind() == "if_expression" {
                parent.child_by_field_name("consequence")
            } else {
                parent.child_by_field_name("body")
            }?;
            let pos = block.end_position();
            return Some(SourcePosition {
                line: pos.row + 1,
                column: pos.column,
            });
        }
        if parent.kind() == "block" || parent.kind() == "function_item" {
            return None;
        }
        p = parent.parent();
    }
    None
}

fn tuple_children<'a>(node_opt: Option<Node<'a>>, kind: &str) -> Option<Vec<Node<'a>>> {
    let node = node_opt.filter(|n| n.kind() == kind)?;
    let mut cursor = node.walk();
    Some(node.named_children(&mut cursor).collect())
}

fn extract_function_return_tuple_elements<'a>(func_item: Node<'a>) -> Option<Vec<Node<'a>>> {
    let ret_type = func_item.child_by_field_name("return_type")?;
    let target_tuple = if ret_type.kind() == "tuple_type" {
        Some(ret_type)
    } else {
        (0..ret_type.named_child_count()).find_map(|i| {
            let c = ret_type.named_child(i)?;
            (c.kind() == "tuple_type").then_some(c)
        })
    };
    let tup = target_tuple?;
    Some(
        (0..tup.named_child_count())
            .filter_map(|i| tup.named_child(i))
            .collect(),
    )
}

fn resolve_function_return_tuple<'a>(
    mut curr: Node<'a>,
    fn_name: &str,
    source: &str,
    bindings: &[BindingFact],
) -> Option<Vec<Node<'a>>> {
    if bindings.iter().any(|b| b.name == fn_name) {
        return None;
    }
    while let Some(parent) = curr.parent() {
        let count = parent.named_child_count();
        for i in 0..count {
            if let Some(child) = parent.named_child(i) {
                if child.kind() == "function_item"
                    && child
                        .child_by_field_name("name")
                        .is_some_and(|n| text(n, source).trim() == fn_name)
                {
                    return extract_function_return_tuple_elements(child);
                }
            }
        }
        curr = parent;
    }
    None
}

fn enclosing_call_argument(closure: Node<'_>) -> Option<(Node<'_>, Node<'_>, usize, Node<'_>)> {
    let mut arg = closure;
    if let Some(parent) = arg.parent().filter(|p| p.kind() == "reference_expression") {
        arg = parent;
    }
    let args = arg.parent().filter(|p| p.kind() == "arguments")?;
    let call = args.parent().filter(|p| p.kind() == "call_expression")?;
    let mut cursor = args.walk();
    let idx = args
        .named_children(&mut cursor)
        .position(|child| child.id() == arg.id())?;
    Some((args, arg, idx, call))
}
