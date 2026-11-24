//! Single-pass AST statement fingerprinting and structural cognitive complexity metrics.

use crate::models::{stable_hash64, StatementFingerprint};
use tree_sitter::Node as Syntax;

const NESTED_DECLS: &[&str] = &["function_item", "function_definition", "function_declaration", "generator_function_declaration", "method_definition", "method_declaration", "class_definition", "class_declaration", "struct_item", "enum_item", "impl_item", "trait_item", "mod_item"];
const STATEMENT_CONTAINERS: &[&str] = &["block", "statement_block", "compound_statement", "function_body", "suite", "declaration_list", "match_block", "switch_body", "case_block", "case_statement", "switch_case", "default_clause"];
const CONTROL_CLAUSES: &[&str] = &["else_clause", "elif_clause", "catch_clause", "except_clause", "finally_clause"];
const IGNORED_STMTS: &[&str] = &["empty_statement", "pass_statement", "match_arm", "switch_case", "default_clause", "case_statement", "case_clause", "when_entry", "visibility_modifier", "attribute_item", "inner_attribute_item", "decorator"];
const BRANCH_AND_NEST_KINDS: &[&str] = &["for_expression", "for_statement", "for_in_statement", "for_of_statement", "for_range_statement", "while_expression", "while_statement", "loop_expression", "do_statement", "do_while_statement", "foreach_statement", "enhanced_for_statement", "match_expression", "match_statement", "switch_statement", "switch_expression", "catch_clause", "except_clause", "rescue", "conditional_expression", "ternary_expression"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFlowEffect { BranchAndNest, FlatIncrement, NestOnly, None }

/// Per-callable structural complexity and normalized statement fingerprint tracker.
#[derive(Debug, Default)]
pub(crate) struct CallableMetricsTracker {
    pub(crate) cognitive_complexity: usize,
    pub(crate) max_nesting_depth: usize,
    current_depth: usize,
    pub(crate) statements: Vec<StatementFingerprint>,
    scratch: String,
}

impl CallableMetricsTracker {
    /// Observes entering an AST node inside a callable's scope in the single-pass walk.
    pub(crate) fn enter(&mut self, node: Syntax<'_>, root_id: usize, source: &str, offset: usize) {
        if node.id() == root_id || is_nested_or_comment(node.kind()) {
            return;
        }
        match control_flow_effect(node) {
            ControlFlowEffect::BranchAndNest => {
                self.cognitive_complexity = self.cognitive_complexity.saturating_add(1 + self.current_depth);
                CallableMetricsTracker::push_nest(self);
            }
            ControlFlowEffect::FlatIncrement => self.cognitive_complexity = self.cognitive_complexity.saturating_add(1),
            ControlFlowEffect::NestOnly => CallableMetricsTracker::push_nest(self),
            ControlFlowEffect::None => {}
        }
        if is_statement_unit(node) {
            let token_count = normalize_statement_into(node, source, &mut self.scratch);
            if token_count > 0 {
                let line = node.start_position().row + offset + 1;
                let end_line = (node.end_position().row + offset + 1).max(line);
                let hash = stable_hash64(self.scratch.as_str()) as u64;
                self.statements.push(StatementFingerprint { hash, line, end_line, token_count });
            }
        }
    }

    /// Observes exiting an AST node inside a callable's scope.
    pub(crate) fn exit(&mut self, node: Syntax<'_>, root_id: usize) {
        if node.id() != root_id && !is_nested_or_comment(node.kind())
            && matches!(control_flow_effect(node), ControlFlowEffect::BranchAndNest | ControlFlowEffect::NestOnly)
        {
            self.current_depth = self.current_depth.saturating_sub(1);
        }
    }

    fn push_nest(&mut self) {
        self.current_depth = self.current_depth.saturating_add(1);
        self.max_nesting_depth = self.max_nesting_depth.max(self.current_depth);
    }
}

fn is_nested_or_comment(kind: &str) -> bool { NESTED_DECLS.contains(&kind) || kind.contains("comment") }

fn is_statement_unit(node: Syntax<'_>) -> bool {
    let kind = node.kind();
    if is_nested_or_comment(kind) || IGNORED_STMTS.contains(&kind) || CONTROL_CLAUSES.contains(&kind) || STATEMENT_CONTAINERS.contains(&kind) {
        return false;
    }
    node.parent().is_some_and(|parent| {
        let pkind = parent.kind();
        STATEMENT_CONTAINERS.contains(&pkind)
            && !(matches!(pkind, "case_statement" | "switch_case" | "default_clause")
                && Some(node.id()) == parent.child_by_field_name("value").map(|n| n.id()))
    })
}

fn logical_operator_token(node: Syntax<'_>) -> Option<&'static str> {
    if !matches!(node.kind(), "binary_expression" | "boolean_operator") { return None; }
    let as_logic = |k: &str| match k { "&&" => Some("&&"), "||" => Some("||"), "and" => Some("and"), "or" => Some("or"), _ => None };
    if let Some(op) = node.child_by_field_name("operator") { return as_logic(op.kind()); }
    let mut cursor = node.walk();
    let op = node.children(&mut cursor).filter(|c| !c.is_named()).find_map(|c| as_logic(c.kind()));
    op
}

fn control_flow_effect(node: Syntax<'_>) -> ControlFlowEffect {
    let kind = node.kind();
    match kind {
        "if_expression" | "if_statement" => if node.parent().is_some_and(|p| p.kind() == "else_clause") { ControlFlowEffect::FlatIncrement } else { ControlFlowEffect::BranchAndNest },
        "else_clause" => {
            let mut c = node.walk();
            if node.named_children(&mut c).any(|n| matches!(n.kind(), "if_expression" | "if_statement")) { ControlFlowEffect::None } else { ControlFlowEffect::FlatIncrement }
        }
        "elif_clause" => ControlFlowEffect::FlatIncrement,
        _ if BRANCH_AND_NEST_KINDS.contains(&kind) => ControlFlowEffect::BranchAndNest,
        "closure_expression" | "lambda" | "arrow_function" | "function_expression" | "generator_function_expression" | "generator_function" => ControlFlowEffect::NestOnly,
        "binary_expression" | "boolean_operator" => match logical_operator_token(node) {
            Some(op) if !node.parent().and_then(logical_operator_token).is_some_and(|p| p == op) => ControlFlowEffect::FlatIncrement,
            _ => ControlFlowEffect::None,
        },
        "break_expression" | "continue_expression" | "break_statement" | "continue_statement" => {
            let mut c = node.walk();
            let labeled = node.child_by_field_name("label").is_some() || node.named_children(&mut c).any(|n| matches!(n.kind(), "label" | "loop_label"));
            if labeled { ControlFlowEffect::FlatIncrement } else { ControlFlowEffect::None }
        }
        _ => ControlFlowEffect::None,
    }
}

fn is_normalizable_local_ident(node: Syntax<'_>, raw: &str) -> bool {
    if node.kind() != "identifier"
        || matches!(
            raw,
            "self" | "Self" | "super" | "crate" | "this" | "None" | "Some" | "Ok" | "Err"
                | "true" | "false" | "True" | "False" | "null" | "undefined" | "nil" | "_"
        )
    {
        return false;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    let pkind = parent.kind();
    if matches!(
        pkind,
        "scoped_identifier" | "scoped_type_identifier" | "qualified_name" | "use_declaration"
            | "use_as_clause" | "use_list" | "import_statement" | "import_from_statement"
    ) {
        return false;
    }
    if matches!(
        pkind,
        "call_expression" | "call" | "invocation_expression" | "macro_invocation"
    ) && parent
        .child_by_field_name("function")
        .or_else(|| parent.child_by_field_name("macro"))
        .is_some_and(|f| f.id() == node.id())
    {
        return false;
    }
    if matches!(pkind, "attribute" | "member_expression" | "field_expression")
        && parent
            .child_by_field_name("attribute")
            .or_else(|| parent.child_by_field_name("property"))
            .or_else(|| parent.child_by_field_name("field"))
            .is_some_and(|f| f.id() == node.id())
    {
        return false;
    }
    true
}

fn push_normalized_token<'a>(
    out: &mut String,
    node: Syntax<'_>,
    part: &'a str,
    ident_slots: &mut Vec<&'a str>,
) {
    if !out.is_empty() {
        out.push(' ');
    }
    if is_normalizable_local_ident(node, part) {
        let slot = match ident_slots.iter().position(|&existing| existing == part) {
            Some(idx) => idx,
            None => {
                ident_slots.push(part);
                ident_slots.len() - 1
            }
        };
        use std::fmt::Write;
        let _ = write!(out, "$v{slot}");
    } else {
        out.push_str(part);
    }
}

fn normalize_statement_into(stmt: Syntax<'_>, source: &str, out: &mut String) -> usize {
    out.clear();
    let mut token_count = 0usize;
    let mut ident_slots: Vec<&str> = Vec::new();
    let mut cursor = stmt.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let skip = kind.contains("comment")
            || (node.id() != stmt.id()
                && (STATEMENT_CONTAINERS.contains(&kind) || CONTROL_CLAUSES.contains(&kind) || NESTED_DECLS.contains(&kind)));
        if !skip && node.child_count() == 0 {
            let raw = source[node.byte_range()].trim();
            if !raw.is_empty() {
                if matches!(raw, ")" | "]" | "}") && out.ends_with(" ,") { out.truncate(out.len() - 2); }
                for part in raw.split_whitespace() {
                    push_normalized_token(out, node, part, &mut ident_slots);
                }
                if !matches!(raw, "{" | "}" | "(" | ")" | "[" | "]" | ";" | ",") { token_count += 1; }
            }
        }
        if !skip && cursor.goto_first_child() { continue; }
        loop {
            if cursor.node().id() == stmt.id() { return token_count; }
            if cursor.goto_next_sibling() { break; }
            if !cursor.goto_parent() { return token_count; }
        }
    }
}
