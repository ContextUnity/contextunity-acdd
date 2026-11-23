use super::profile::LanguageProfile;
use super::support;
use super::types::ImportPath;
use crate::ast::syntax::{text, ScopeBindings};
use tree_sitter::Node as Syntax;

/// Represents toml data.
struct Toml;
/// Shared toml language profile.
static TOML: Toml = Toml;
/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&TOML];

impl LanguageProfile for Toml {
    fn id(&self) -> &'static str {
        "toml"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_toml_ng::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "table" | "table_array_element" => Some("namespace"),
            "pair" => Some("variable"),
            _ => None,
        }
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        let mut cursor = node.walk();
        let key = node
            .named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "bare_key" | "quoted_key" | "dotted_key"))
            .map(|key| text(key, source));
        key
    }
    fn node_prefix(&self, kind: &str) -> &'static str {
        if kind == "namespace" {
            "namespace"
        } else {
            "variable"
        }
    }
    fn normalize_import(&self, _owner: &str, _module: &str) -> Option<ImportPath> {
        None
    }
    fn pattern_wrapper(&self, kind: &str) -> bool {
        kind == "document"
    }
    fn bindings(&self, _node: Syntax<'_>, _source: &str) -> ScopeBindings {
        support::empty_bindings()
    }
}
