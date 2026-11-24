use super::template_loaders;
pub(crate) use template_loaders::{BindingWriteCollector, BindingWrites};
use crate::ast::syntax::{field, scope_bindings, text, ScopeBindings};
use crate::models::Facts;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as Syntax;

pub(crate) fn lexical_type_parameters(scope: Syntax<'_>, source: &str) -> Vec<String> {
    template_loaders::type_parameter_names(scope, source)
}

pub(crate) fn lexical_binding_owner<'tree>(
    node: Syntax<'tree>,
    source: &str,
    name: &str,
) -> Option<Syntax<'tree>> {
    lexical_binding_owner_with(node, |scope| {
        template_loaders::type_parameter_names(scope, source)
            .iter()
            .any(|bound| bound == name)
            || scope_bindings(scope, source)
                .all
                .iter()
                .any(|bound| bound == name)
            || scope
                .child_by_field_name("body")
                .is_some_and(|body| template_loaders::binding_count(body, source, name) != 0)
    })
}

/// Applies Python lexical boundaries to a caller-owned, completed binding proof.
pub(crate) fn lexical_binding_owner_with<'tree>(
    node: Syntax<'tree>,
    mut binds: impl FnMut(Syntax<'tree>) -> bool,
) -> Option<Syntax<'tree>> {
    let mut parent = node.parent();
    while let Some(scope) = parent {
        match scope.kind() {
            "function_definition" => {
                if binds(scope) {
                    return Some(scope);
                }
                scope.child_by_field_name("body")?;
            }
            "class_definition"
            | "lambda"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => return None,
            "module" => return Some(scope),
            _ => {}
        }
        parent = scope.parent();
    }
    None
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

#[derive(Default)]
pub(crate) struct MainGuardTracker {
    calls: HashMap<String, (usize, usize)>,
    declarations: HashMap<String, (usize, usize, bool)>,
    callable_assignments: HashSet<String>,
}

impl MainGuardTracker {
    pub(crate) fn observe(&mut self, node: Syntax<'_>, source: &str) {
        if node.kind() == "assignment"
            && module_scope(node)
            && node
                .child_by_field_name("right")
                .is_some_and(|right| right.kind() == "lambda")
        {
            if let Some(left) = node
                .child_by_field_name("left")
                .filter(|left| left.kind() == "identifier")
            {
                self.callable_assignments
                    .insert(text(left, source).to_owned());
            }
        }
        if node.kind() == "function_definition" && module_scope(node) {
            if let Some(name) = field(node, source, "name") {
                let direct = node.parent().is_some_and(|parent| {
                    parent.kind() == "module"
                        || (parent.kind() == "decorated_definition"
                            && parent
                                .parent()
                                .is_some_and(|outer| outer.kind() == "module"))
                });
                let entry = self.declarations.entry(name.to_owned()).or_insert((
                    0,
                    node.start_byte(),
                    direct,
                ));
                entry.0 += 1;
            }
            return;
        }
        if node.kind() != "expression_statement" {
            return;
        }
        let Some(body) = node.parent().filter(|n| n.kind() == "block") else {
            return;
        };
        let Some(guard) = body.parent().filter(|n| n.kind() == "if_statement") else {
            return;
        };
        if !guard.parent().is_some_and(|n| n.kind() == "module")
            || guard.child_by_field_name("consequence") != Some(body)
        {
            return;
        }
        let Some(condition) = guard.child_by_field_name("condition") else {
            return;
        };
        let mut cursor = condition.walk();
        let mut operands = condition.named_children(&mut cursor);
        let (Some(left), Some(right), None) = (operands.next(), operands.next(), operands.next())
        else {
            return;
        };
        let lhs = text(left, source).trim();
        let rhs = text(right, source).trim();
        let is_main = |s: &str| s == "'__main__'" || s == "\"__main__\"";
        if condition.kind() != "comparison_operator"
            || !((lhs == "__name__" && is_main(rhs)) || (rhs == "__name__" && is_main(lhs)))
            || !text(condition, source).contains("==")
        {
            return;
        }
        let Some(call) = node.named_child(0).filter(|n| n.kind() == "call") else {
            return;
        };
        let Some(function) = call
            .child_by_field_name("function")
            .filter(|n| n.kind() == "identifier")
        else {
            return;
        };
        let entry = self
            .calls
            .entry(text(function, source).to_owned())
            .or_insert((0, guard.start_byte()));
        entry.0 += 1;
    }

    pub(crate) fn finish(self, module: &str, bindings: &ScopeBindings, facts: &mut Facts) {
        let rebound: HashSet<&str> = bindings.rebindings.iter().map(String::as_str).collect();
        let imported: HashSet<&str> = facts
            .references
            .iter()
            .filter(|r| r.kind == "imports" && r.source.starts_with("module:"))
            .filter_map(|r| r.alias.as_deref())
            .collect();
        for node in &mut facts.nodes {
            if node.kind != crate::models::tags::NodeKind::Function
                || node
                    .qualname
                    .strip_prefix(module)
                    .and_then(|s| s.strip_prefix('.'))
                    != Some(node.name.as_str())
            {
                continue;
            }
            let Some(definitions) = self.declarations.get(&node.name) else {
                continue;
            };
            if definitions.0 == 1 && definitions.2 {
                node.set_detail(crate::models::details::NodeDetailKey::ENTRYPOINT_DIRECT_MODULE, serde_json::json!(true));
            }
            if node.detail_get(crate::models::details::NodeDetailKey::ENTRYPOINT_BASE).is_some()
                || !definitions.2
                || rebound.contains(node.name.as_str())
                || imported.contains(node.name.as_str())
                || self.callable_assignments.contains(node.name.as_str())
            {
                continue;
            }
            let Some(calls) = self.calls.get(&node.name) else {
                continue;
            };
            if calls.0 == 1 && definitions.0 == 1 && definitions.1 < calls.1 {
                node.set_detail(crate::models::details::NodeDetailKey::ENTRYPOINT_BASE, serde_json::json!("main"));
            }
        }
    }
}
