use super::profile::LanguageProfile;
use super::types::{relative_namespace, ImportPath, LanguageFamily, SyntaxContext};
use crate::ast::syntax::{field, text};
use crate::models::{Facts, Reference};
use tree_sitter::{Language, Node as Syntax};

extern "C" {
    fn tree_sitter_proto() -> Language;
}

/// Performs language.
fn language() -> Language {
    unsafe { tree_sitter_proto() }
}

/// Performs kind.
fn kind(kind: &str) -> Option<&'static str> {
    match kind {
        "message" => Some("class"),
        "service" => Some("service"),
        "rpc" => Some("function"),
        "enum" => Some("enum"),
        _ => None,
    }
}

fn parse_proto_package(source: &str) -> Option<&str> {
    super::types::parse_package_declaration(source, false)
}

fn is_proto_primitive(name: &str) -> bool {
    matches!(
        name,
        "double"
            | "float"
            | "int32"
            | "int64"
            | "uint32"
            | "uint64"
            | "sint32"
            | "sint64"
            | "fixed32"
            | "fixed64"
            | "sfixed32"
            | "sfixed64"
            | "bool"
            | "string"
            | "bytes"
    )
}

/// Represents proto data.
struct Proto;
/// Shared proto language profile.
static PROTO: Proto = Proto;
impl LanguageProfile for Proto {
    fn id(&self) -> &'static str {
        "proto"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["proto"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        language()
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("proto")
    }
    fn symbol_kind(&self, k: &str) -> Option<&'static str> {
        kind(k)
    }
    fn node_prefix(&self, kind: &str) -> &'static str {
        match kind {
            "class" => "message",
            "service" => "service",
            "function" => "rpc",
            "enum" => "enum",
            _ => "proto",
        }
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        parse_proto_package(source)
            .map(str::to_owned)
            .unwrap_or_else(|| self.module_name(path))
    }
    fn sibling_accessible(&self, path_a: &str, path_b: &str) -> bool {
        let dir_a = path_a.rsplit_once('/').map_or("", |(d, _)| d);
        let dir_b = path_b.rsplit_once('/').map_or("", |(d, _)| d);
        dir_a == dir_b
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let (node, source) = (ctx.node, ctx.source);
        let mut add = |expression, alias, module| ctx.import(facts, expression, alias, module);
        if node.kind() == "import" {
            let mut p = field(node, source, "path");
            if p.is_none() {
                let mut c = node.walk();
                p = node
                    .named_children(&mut c)
                    .find(|n| n.kind() == "string")
                    .map(|n| text(n, source));
            }
            if let Some(path) = p {
                let path = path.trim_matches(['"', '\'', '`']);
                add(
                    path.into(),
                    Some(path.rsplit('/').next().unwrap_or(path).into()),
                    Some(path.into()),
                );
            }
        }
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let stripped = module.trim_end_matches(".proto");
        let relative = stripped.starts_with("./")
            || stripped.starts_with("../")
            || !stripped.contains('/');
        let namespace = if relative {
            relative_namespace(owner, stripped)?
        } else {
            stripped.trim_start_matches('/').replace('/', ".")
        };
        Some(ImportPath {
            namespace,
            relative,
            symbol_path: false,
        })
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        let mut c = node.walk();
        let name = node
            .named_children(&mut c)
            .find(|n| {
                matches!(
                    n.kind(),
                    "message_name" | "service_name" | "rpc_name" | "enum_name"
                )
            })
            .map(|n| text(n, source));
        name
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let push_type_ref = |node: Syntax<'_>, facts: &mut Facts| {
            let expression = text(node, ctx.source).trim();
            if !expression.is_empty() && !is_proto_primitive(expression) {
                facts.references.push(Reference {
                    source: ctx.owner.into(),
                    dynamic: false,
                    expression: expression.into(),
                    kind: "references".into(),
                    line: node.start_position().row + 1,
                    column: node.start_position().column,
                    alias: None,
                    module: expression.rsplit_once('.').map(|(m, _)| m.into()),
                    receiver_hint: None,
                });
            }
        };
        if ctx.node.kind() == "rpc" {
            let mut c = ctx.node.walk();
            for ch in ctx
                .node
                .named_children(&mut c)
                .filter(|ch| ch.kind() == "message_or_enum_type")
            {
                push_type_ref(ch, facts);
            }
        } else if ctx.node.kind() == "message" {
            let mut stack = Vec::new();
            let mut body_cursor = ctx.node.walk();
            for ch in ctx
                .node
                .named_children(&mut body_cursor)
                .filter(|ch| ch.kind() == "message_body")
            {
                stack.push(ch);
            }
            while let Some(curr) = stack.pop() {
                if matches!(curr.kind(), "message" | "enum") {
                    continue;
                }
                if curr.kind() == "type" {
                    push_type_ref(curr, facts);
                    continue;
                }
                let mut walk = curr.walk();
                for child in curr.named_children(&mut walk) {
                    stack.push(child);
                }
            }
        }
    }

    fn is_stdlib(&self, module: &str) -> bool {
        (module.starts_with("google/protobuf/") && module.ends_with(".proto"))
            || module.trim_start_matches('.').starts_with("google.protobuf.")
    }

    fn external_import(&self, module: &str) -> Option<&'static str> {
        self.is_stdlib(module)
            .then_some("Protobuf standard library")
    }

    fn builtin(&self, symbol: &str) -> bool {
        let Some(name) = symbol.trim_start_matches('.').strip_prefix("google.protobuf.") else {
            return false;
        };
        matches!(
            name,
            "Any"
                | "Api"
                | "BoolValue"
                | "BytesValue"
                | "DescriptorProto"
                | "DoubleValue"
                | "Duration"
                | "Empty"
                | "Enum"
                | "EnumOptions"
                | "EnumValue"
                | "EnumValueOptions"
                | "Field"
                | "FieldMask"
                | "FieldOptions"
                | "FileDescriptorProto"
                | "FileDescriptorSet"
                | "FileOptions"
                | "FloatValue"
                | "Int32Value"
                | "Int64Value"
                | "ListValue"
                | "MessageOptions"
                | "Method"
                | "MethodOptions"
                | "Mixin"
                | "NullValue"
                | "OneofOptions"
                | "Option"
                | "ServiceOptions"
                | "SourceContext"
                | "StringValue"
                | "Struct"
                | "Syntax"
                | "Timestamp"
                | "Type"
                | "UInt32Value"
                | "UInt64Value"
                | "Value"
        )
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&PROTO];
