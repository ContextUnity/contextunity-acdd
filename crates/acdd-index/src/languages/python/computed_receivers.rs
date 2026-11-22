use crate::models::ReceiverHint;
use crate::ast::{bounded_expression, text};
use tree_sitter::Node;

fn static_callee(node: Node<'_>) -> bool {
    match node.kind() {
        "identifier" => true,
        "attribute" => {
            node.child_by_field_name("attribute")
                .is_some_and(|member| member.kind() == "identifier")
                && node
                    .child_by_field_name("object")
                    .is_some_and(static_callee)
        }
        _ => false,
    }
}

fn typed_call_hint(value: crate::semantic::ValueExpr, member: String) -> Option<ReceiverHint> {
    matches!(value, crate::semantic::ValueExpr::Call { .. })
        .then_some(ReceiverHint::TypedCallResult { value, member })
}

pub(super) fn receiver_hint(call: Node<'_>, source: &str) -> Option<ReceiverHint> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let member = function.child_by_field_name("attribute")?;
    if member.kind() != "identifier" {
        return None;
    }
    let receiver = function.child_by_field_name("object")?;
    match receiver.kind() {
        "string" => {
            let start = receiver.named_child(0)?;
            if start.kind() != "string_start" {
                return None;
            }
            let prefix = text(start, source).split(['\'', '"']).next()?;
            if prefix
                .bytes()
                .any(|byte| matches!(byte, b'b' | b'B' | b'f' | b'F'))
            {
                return None;
            }
            Some(ReceiverHint::StringLiteral {
                member: text(member, source).to_owned(),
            })
        }
        "call" => {
            let callee = receiver.child_by_field_name("function")?;
            if !(callee.kind() == "identifier" && text(callee, source) == "super") {
                if let Some(hint) = typed_call_hint(
                    super::value_flow::call_value(receiver, source),
                    text(member, source).to_owned(),
                ) {
                    return Some(hint);
                }
            }
            if callee.kind() == "call" {
                let inner = callee.child_by_field_name("function")?;
                let arguments = callee.child_by_field_name("arguments")?;
                if !static_callee(inner) || arguments.named_child_count() != 0 {
                    return None;
                }
                return Some(ReceiverHint::CallResult {
                    callee: bounded_expression(&format!("{}()", text(inner, source))),
                    member: text(member, source).to_owned(),
                });
            }
            if !static_callee(callee) {
                return None;
            }
            if callee.kind() == "identifier" && text(callee, source) == "super" {
                let arguments = receiver.child_by_field_name("arguments")?;
                if arguments.named_child_count() != 0 {
                    return None;
                }
                Some(ReceiverHint::Super {
                    member: text(member, source).to_owned(),
                })
            } else {
                Some(ReceiverHint::CallResult {
                    callee: bounded_expression(text(callee, source)),
                    member: text(member, source).to_owned(),
                })
            }
        }
        "attribute" => {
            let mut parts = vec![text(member, source)];
            let mut cur = receiver;
            while cur.kind() == "attribute" {
                if parts.len() >= crate::languages::MAX_VALUE_FLOW_TYPE_DEPTH {
                    return None;
                }
                let attr = cur
                    .child_by_field_name("attribute")
                    .filter(|n| n.kind() == "identifier")?;
                parts.push(text(attr, source));
                cur = cur.child_by_field_name("object")?;
            }
            let value = match cur.kind() {
                "call" => super::value_flow::call_value(cur, source),
                "binary_operator" | "subscript" | "parenthesized_expression" => {
                    super::value_flow::computed_receiver_value(cur, source)?
                }
                _ => return None,
            };
            parts.reverse();
            typed_call_hint(value, parts.join("."))
        }
        "binary_operator" | "subscript" | "parenthesized_expression" => {
            let value = super::value_flow::computed_receiver_value(receiver, source)?;
            typed_call_hint(value, text(member, source).to_owned())
        }
        _ => None,
    }
}
