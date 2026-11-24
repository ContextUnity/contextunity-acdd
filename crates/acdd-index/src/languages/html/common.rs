use crate::ast::syntax::{text, Syntax};
use crate::models::Node;
use anyhow::Result;
pub(super) use serde_json::{Map, Value};

pub(super) struct NodeFields {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) qualname: String,
    pub(super) line: usize,
    pub(super) end_line: usize,
    pub(super) details: Value,
}

pub(super) fn source_node(path: &str, language: &'static str, fields: NodeFields) -> Node {
    Node {
        native_semantic: None,
        id: fields.id,
        kind: crate::models::tags::NodeKind::parse(&fields.kind).expect("recognized HTML declaration kind"),
        name: fields.name,
        qualname: fields.qualname,
        path: path.into(),
        line: fields.line,
        end_line: fields.end_line,
        is_test: crate::models::is_test(path),
        language: crate::models::tags::LanguageId::parse(language).expect("recognized language profile"),
        generated: false,
        details: fields.details,
    }
}

pub(super) fn is_html_entity(slice: &str) -> bool {
    if !slice.starts_with('&') {
        return false;
    }
    if let Some(semi) = slice.find(';') {
        if semi > 1 && semi <= 12 {
            let inner = &slice[1..semi];
            if let Some(num) = inner.strip_prefix('#') {
                if let Some(hex) = num.strip_prefix(['x', 'X']) {
                    !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())
                } else {
                    !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
                }
            } else {
                inner.chars().all(|c| c.is_ascii_alphanumeric())
            }
        } else {
            false
        }
    } else {
        false
    }
}

pub(super) fn for_each_tag_attribute<'tree>(
    tag: Syntax<'tree>,
    mut visit: impl FnMut(Syntax<'tree>) -> Result<std::ops::ControlFlow<()>>,
) -> Result<()> {
    let mut cursor = tag.walk();
    for attribute in tag
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "attribute")
    {
        if visit(attribute)?.is_break() {
            break;
        }
    }
    Ok(())
}

pub(super) fn attribute_value<'a>(
    tag: Syntax<'_>,
    source: &'a str,
    name: &str,
) -> Option<&'a str> {
    let mut found = None;
    let _ = for_each_tag_attribute(tag, |attribute| {
        if attribute
            .named_child(0)
            .is_some_and(|node| text(node, source).eq_ignore_ascii_case(name))
        {
            found = attribute
                .named_child(1)
                .map(|node| text(node, source).trim_matches(['\'', '"']).trim());
            return Ok(std::ops::ControlFlow::Break(()));
        }
        Ok(std::ops::ControlFlow::Continue(()))
    });
    found
}
