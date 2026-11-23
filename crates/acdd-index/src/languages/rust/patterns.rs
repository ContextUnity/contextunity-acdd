use crate::ast::syntax::{text, Syntax};
use tree_sitter::Node;

pub(crate) fn bindings<'tree>(pattern: Node<'tree>, emit: impl FnMut(Node<'tree>)) {
    crate::ast::syntax::rust_pattern_bindings(pattern, emit);
}

pub(crate) fn collect_use(
    node: Syntax<'_>,
    source: &str,
    prefix: &str,
    out: &mut Vec<(String, String)>,
) {
    fn joined(prefix: &str, name: &str) -> String {
        if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}::{name}")
        }
    }
    match node.kind() {
        "scoped_use_list" => {
            if let (Some(path), Some(list)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("list"),
            ) {
                collect_use(list, source, &joined(prefix, text(path, source)), out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_use(child, source, prefix, out);
            }
        }
        "use_as_clause" => {
            if let (Some(path), Some(alias)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("alias"),
            ) {
                let path = text(path, source);
                let name = if path == "self" {
                    prefix.to_owned()
                } else {
                    joined(prefix, path)
                };
                if !name.is_empty() {
                    out.push((name, text(alias, source).to_owned()));
                }
            }
        }
        "use_wildcard" => {
            let path = node.named_child(0).map(|child| text(child, source));
            let name = path.map_or_else(|| prefix.to_owned(), |path| joined(prefix, path));
            out.push((format!("{name}::*"), "*".into()));
        }
        "self" if !prefix.is_empty() => {
            out.push((
                prefix.to_owned(),
                prefix.rsplit("::").next().unwrap_or(prefix).into(),
            ));
        }
        "identifier" | "scoped_identifier" | "crate" | "super" | "self" => {
            let name = joined(prefix, text(node, source));
            out.push((
                name.clone(),
                name.rsplit("::").next().unwrap_or(&name).into(),
            ));
        }
        _ => {}
    }
}
