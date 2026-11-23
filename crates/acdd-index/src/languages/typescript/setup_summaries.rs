use crate::ast::syntax::{text, Syntax};
use crate::semantic::{ObjectMember, ObjectMemberTarget, SourcePosition};
use std::collections::{HashMap, HashSet};

thread_local! {
    static SETUP_SUMMARIES: std::cell::RefCell<HashMap<usize, Vec<ObjectMember>>> = std::cell::RefCell::new(HashMap::new());
}

/// Transfer only the completed direct callback summary, before its enclosing call exits.
pub(super) fn complete_setup(
    function: Syntax<'_>,
    returned: Syntax<'_>,
    source: &str,
    definitions: &hashbrown::HashMap<&str, Syntax<'_>>,
) {
    if function
        .parent()
        .is_none_or(|parent| parent.kind() != "arguments")
        || returned.kind() != "object"
        || returned.named_child_count() > 128
    {
        return;
    }
    let mut members = Vec::new();
    let mut names = HashSet::new();
    for item in returned.named_children(&mut returned.walk()) {
        if item.kind() == "comment" {
            continue;
        }
        let (name, value) = match item.kind() {
            "shorthand_property_identifier" => (text(item, source), item),
            "pair" => {
                let (Some(key), Some(value)) = (
                    item.child_by_field_name("key"),
                    item.child_by_field_name("value"),
                ) else {
                    return;
                };
                if !matches!(key.kind(), "identifier" | "property_identifier") {
                    return;
                }
                (text(key, source), value)
            }
            _ => return,
        };
        if !names.insert(name) {
            return;
        }
        let declaration = if matches!(value.kind(), "identifier" | "shorthand_property_identifier")
        {
            definitions.get(text(value, source)).copied()
        } else if matches!(value.kind(), "arrow_function" | "function_expression") {
            Some(value)
        } else {
            None
        };
        let Some(declaration) = declaration.filter(|node| {
            matches!(
                node.kind(),
                "function_declaration" | "arrow_function" | "function_expression"
            )
        }) else {
            continue;
        };
        let declared = declaration
            .child_by_field_name("name")
            .map(|node| text(node, source))
            .or_else(|| {
                declaration
                    .parent()
                    .filter(|node| node.kind() == "variable_declarator")
                    .and_then(|node| node.child_by_field_name("name"))
                    .map(|node| text(node, source))
            })
            .unwrap_or(name);
        members.push(ObjectMember {
            name: name.to_owned(),
            position: SourcePosition {
                line: declaration.start_position().row + 1,
                column: declaration.start_position().column,
            },
            target: if declared == name {
                ObjectMemberTarget::SameName
            } else {
                ObjectMemberTarget::Alias {
                    name: declared.to_owned(),
                }
            },
        });
    }
    if !members.is_empty() {
        SETUP_SUMMARIES.with(|summaries| {
            summaries.borrow_mut().insert(function.id(), members);
        });
    }
}

pub(super) fn has_setup_summaries() -> bool {
    SETUP_SUMMARIES.with(|summaries| !summaries.borrow().is_empty())
}

pub(super) fn take_setup_summary(node_id: usize) -> Option<Vec<ObjectMember>> {
    SETUP_SUMMARIES.with(|summaries| summaries.borrow_mut().remove(&node_id))
}

pub(super) fn clear_setup_summaries() {
    SETUP_SUMMARIES.with(|summaries| summaries.borrow_mut().clear());
}
