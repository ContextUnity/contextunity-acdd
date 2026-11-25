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

#[derive(Debug, Clone, Copy)]
struct MetricsNodeFrame {
    node_id: usize,
    pushed_nest: bool,
    is_else_clause: bool,
    logical_op: Option<&'static str>,
    is_stmt_container: bool,
    case_value_id: Option<usize>,
}

impl MetricsNodeFrame {
    const EMPTY: Self = Self {
        node_id: 0,
        pushed_nest: false,
        is_else_clause: false,
        logical_op: None,
        is_stmt_container: false,
        case_value_id: None,
    };
}

/// Per-callable structural complexity and normalized statement fingerprint tracker.
#[derive(Debug, Default)]
pub(crate) struct CallableMetricsTracker {
    pub(crate) cognitive_complexity: usize,
    pub(crate) max_nesting_depth: usize,
    current_depth: usize,
    pub(crate) statements: Vec<StatementFingerprint>,
    scratch: String,
    stack: Vec<MetricsNodeFrame>,
    norm_parents: Vec<(bool, Option<usize>)>,
}

impl CallableMetricsTracker {
    /// Observes entering an AST node inside a callable's scope in the single-pass walk.
    pub(crate) fn enter(
        &mut self,
        node: Syntax<'_>,
        kind: &str,
        root_id: usize,
        source: &str,
        offset: usize,
    ) {
        let node_id = node.id();
        if node_id == root_id {
            return;
        }
        if is_nested_or_comment(kind) {
            self.stack.push(MetricsNodeFrame {
                node_id,
                ..MetricsNodeFrame::EMPTY
            });
            return;
        }
        let parent = self.stack.last().copied().unwrap_or(MetricsNodeFrame::EMPTY);
        let (effect, logical_op) =
            control_flow_effect(node, kind, parent.is_else_clause, parent.logical_op);
        let pushed_nest = matches!(
            effect,
            ControlFlowEffect::BranchAndNest | ControlFlowEffect::NestOnly
        );
        match effect {
            ControlFlowEffect::BranchAndNest => {
                self.cognitive_complexity = self
                    .cognitive_complexity
                    .saturating_add(1 + self.current_depth);
                CallableMetricsTracker::push_nest(self);
            }
            ControlFlowEffect::FlatIncrement => {
                self.cognitive_complexity = self.cognitive_complexity.saturating_add(1);
            }
            ControlFlowEffect::NestOnly => CallableMetricsTracker::push_nest(self),
            ControlFlowEffect::None => {}
        }
        let is_stmt_container = STATEMENT_CONTAINERS.contains(&kind);
        let case_value_id = if matches!(kind, "case_statement" | "switch_case" | "default_clause")
        {
            node.child_by_field_name("value").map(|n| n.id())
        } else {
            None
        };
        self.stack.push(MetricsNodeFrame {
            node_id,
            pushed_nest,
            is_else_clause: kind == "else_clause",
            logical_op,
            is_stmt_container,
            case_value_id,
        });
        if parent.is_stmt_container
            && parent.case_value_id != Some(node_id)
            && !is_stmt_container
            && !IGNORED_STMTS.contains(&kind)
            && !CONTROL_CLAUSES.contains(&kind)
        {
            let token_count =
                normalize_statement_into(node, source, &mut self.scratch, &mut self.norm_parents);
            if token_count > 0 {
                let line = node.start_position().row + offset + 1;
                let end_line = (node.end_position().row + offset + 1).max(line);
                let hash = stable_hash64(self.scratch.as_str()) as u64;
                self.statements.push(StatementFingerprint {
                    hash,
                    line,
                    end_line,
                    token_count,
                });
            }
        }
    }

    /// Observes exiting an AST node inside a callable's scope.
    pub(crate) fn exit(&mut self, node_id: usize) {
        if self
            .stack
            .last()
            .is_some_and(|frame| frame.node_id == node_id)
            && self.stack.pop().is_some_and(|frame| frame.pushed_nest)
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

fn logical_operator_token(node: Syntax<'_>, kind: &str) -> Option<&'static str> {
    if !matches!(kind, "binary_expression" | "boolean_operator") { return None; }
    let as_logic = |k: &str| match k { "&&" => Some("&&"), "||" => Some("||"), "and" => Some("and"), "or" => Some("or"), _ => None };
    if let Some(op) = node.child_by_field_name("operator") { return as_logic(op.kind()); }
    let mut cursor = node.walk();
    let op = node.children(&mut cursor).filter(|c| !c.is_named()).find_map(|c| as_logic(c.kind()));
    op
}

fn control_flow_effect(
    node: Syntax<'_>,
    kind: &str,
    parent_is_else_clause: bool,
    parent_logical_op: Option<&'static str>,
) -> (ControlFlowEffect, Option<&'static str>) {
    match kind {
        "if_expression" | "if_statement" => (
            if parent_is_else_clause {
                ControlFlowEffect::FlatIncrement
            } else {
                ControlFlowEffect::BranchAndNest
            },
            None,
        ),
        "else_clause" => {
            let mut c = node.walk();
            let effect = if node
                .named_children(&mut c)
                .any(|n| matches!(n.kind(), "if_expression" | "if_statement"))
            {
                ControlFlowEffect::None
            } else {
                ControlFlowEffect::FlatIncrement
            };
            (effect, None)
        }
        "elif_clause" => (ControlFlowEffect::FlatIncrement, None),
        _ if BRANCH_AND_NEST_KINDS.contains(&kind) => (ControlFlowEffect::BranchAndNest, None),
        "closure_expression" | "lambda" | "arrow_function" | "function_expression"
        | "generator_function_expression" | "generator_function" => {
            (ControlFlowEffect::NestOnly, None)
        }
        "binary_expression" | "boolean_operator" => match logical_operator_token(node, kind) {
            op @ Some(_) if parent_logical_op != op => (ControlFlowEffect::FlatIncrement, op),
            op => (ControlFlowEffect::None, op),
        },
        "break_expression" | "continue_expression" | "break_statement" | "continue_statement" => {
            let mut c = node.walk();
            let labeled = node.child_by_field_name("label").is_some()
                || node
                    .named_children(&mut c)
                    .any(|n| matches!(n.kind(), "label" | "loop_label"));
            let effect = if labeled {
                ControlFlowEffect::FlatIncrement
            } else {
                ControlFlowEffect::None
            };
            (effect, None)
        }
        _ => (ControlFlowEffect::None, None),
    }
}

fn normalization_parent_rule(node: Syntax<'_>, kind: &str) -> (bool, Option<usize>) {
    if matches!(
        kind,
        "scoped_identifier"
            | "scoped_type_identifier"
            | "qualified_name"
            | "use_declaration"
            | "use_as_clause"
            | "use_list"
            | "import_statement"
            | "import_from_statement"
    ) {
        return (true, None);
    }
    if matches!(
        kind,
        "call_expression" | "call" | "invocation_expression" | "macro_invocation"
    ) {
        let excluded = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("macro"))
            .map(|f| f.id());
        return (false, excluded);
    }
    if matches!(kind, "attribute" | "member_expression" | "field_expression") {
        let excluded = node
            .child_by_field_name("attribute")
            .or_else(|| node.child_by_field_name("property"))
            .or_else(|| node.child_by_field_name("field"))
            .map(|f| f.id());
        return (false, excluded);
    }
    (false, None)
}

fn is_normalizable_local_ident(
    kind: &str,
    node_id: usize,
    raw: &str,
    parent_rule: (bool, Option<usize>),
) -> bool {
    kind == "identifier"
        && !parent_rule.0
        && parent_rule.1 != Some(node_id)
        && !matches!(
            raw,
            "self" | "Self" | "super" | "crate" | "this" | "None" | "Some" | "Ok" | "Err"
                | "true" | "false" | "True" | "False" | "null" | "undefined" | "nil" | "_"
        )
}

struct IdentSlots<'a> {
    inline: [&'a str; 8],
    inline_len: usize,
    overflow: Vec<&'a str>,
}

impl<'a> IdentSlots<'a> {
    fn new() -> Self {
        Self {
            inline: [""; 8],
            inline_len: 0,
            overflow: Vec::new(),
        }
    }

    fn slot_for(&mut self, part: &'a str) -> usize {
        for (idx, existing) in self.inline[..self.inline_len].iter().enumerate() {
            if *existing == part {
                return idx;
            }
        }
        if let Some(pos) = self.overflow.iter().position(|&existing| existing == part) {
            return 8 + pos;
        }
        if self.inline_len < self.inline.len() {
            let slot = self.inline_len;
            self.inline[slot] = part;
            self.inline_len += 1;
            slot
        } else {
            let slot = 8 + self.overflow.len();
            self.overflow.push(part);
            slot
        }
    }
}

fn push_normalized_token<'a>(
    out: &mut String,
    kind: &str,
    node_id: usize,
    part: &'a str,
    parent_rule: (bool, Option<usize>),
    ident_slots: &mut IdentSlots<'a>,
) {
    if !out.is_empty() {
        out.push(' ');
    }
    if is_normalizable_local_ident(kind, node_id, part, parent_rule) {
        let slot = ident_slots.slot_for(part);
        use std::fmt::Write;
        let _ = write!(out, "$v{slot}");
    } else {
        out.push_str(part);
    }
}

fn normalize_statement_into(
    stmt: Syntax<'_>,
    source: &str,
    out: &mut String,
    parents: &mut Vec<(bool, Option<usize>)>,
) -> usize {
    out.clear();
    parents.clear();
    let mut token_count = 0usize;
    let mut ident_slots = IdentSlots::new();
    let mut cursor = stmt.walk();
    let stmt_id = stmt.id();
    loop {
        let node = cursor.node();
        let node_id = node.id();
        let kind = node.kind();
        let skip = kind.contains("comment")
            || (node_id != stmt_id
                && (STATEMENT_CONTAINERS.contains(&kind)
                    || CONTROL_CLAUSES.contains(&kind)
                    || NESTED_DECLS.contains(&kind)));
        if !skip && node.child_count() == 0 {
            let raw = source[node.byte_range()].trim();
            if !raw.is_empty() {
                if matches!(raw, ")" | "]" | "}") && out.ends_with(" ,") {
                    out.truncate(out.len() - 2);
                }
                let parent_rule = parents.last().copied().unwrap_or((false, None));
                for part in raw.split_whitespace() {
                    push_normalized_token(
                        out,
                        kind,
                        node_id,
                        part,
                        parent_rule,
                        &mut ident_slots,
                    );
                }
                if !matches!(raw, "{" | "}" | "(" | ")" | "[" | "]" | ";" | ",") {
                    token_count += 1;
                }
            }
        }
        if !skip && cursor.goto_first_child() {
            parents.push(normalization_parent_rule(node, kind));
            continue;
        }
        loop {
            if cursor.node().id() == stmt_id {
                return token_count;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return token_count;
            }
            parents.pop();
        }
    }
}
