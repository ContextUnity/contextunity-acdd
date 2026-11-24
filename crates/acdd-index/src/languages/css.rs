use super::profile::LanguageProfile;
use super::support;
use super::types::{CallForm, DeclarationRole, ImportPath, SyntaxContext, SyntaxGrammarSpec};
use crate::ast::syntax::{symbol_name, text};
use crate::models::Facts;
use tree_sitter::Node as Syntax;

struct Css;
static CSS: Css = Css;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("rule_set", DeclarationRole::Class),
        ("keyframes_statement", DeclarationRole::Type),
        ("declaration", DeclarationRole::Field),
    ],
    references: &["call_expression"],
    call_forms: &[("call_expression", CallForm::Direct)],
    scopes: &["stylesheet", "rule_set"],
    imports: &["import_statement"],
    comments: &["comment"],
    ..SyntaxGrammarSpec::DEFAULT
};

fn named_child<'tree>(node: Syntax<'tree>, kind: &str) -> Option<Syntax<'tree>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == kind);
    found
}

fn import_target<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "string_value" => {
                let raw = text(child, source).trim().trim_matches(['"', '\'']);
                if !raw.is_empty() {
                    return Some(raw);
                }
            }
            "call_expression"
                if named_child(child, "function_name")
                    .is_some_and(|fn_node| text(fn_node, source).trim() == "url") =>
            {
                if let Some(args) = named_child(child, "arguments") {
                    let mut arg_cursor = args.walk();
                    for arg in args.named_children(&mut arg_cursor) {
                        if matches!(arg.kind(), "string_value" | "plain_value") {
                            let raw = text(arg, source).trim().trim_matches(['"', '\'']);
                            if !raw.is_empty() {
                                return Some(raw);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

impl LanguageProfile for Css {
    fn id(&self) -> &'static str {
        "css"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["css"]
    }

    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_css::language()
    }

    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "rule_set" => Some("class"),
            "keyframes_statement" => Some("type"),
            "declaration" => Some("field"),
            _ => None,
        }
    }

    fn symbol_with_source(&self, node: Syntax<'_>, source: &str) -> Option<&'static str> {
        if node.kind() == "declaration" {
            let prop = named_child(node, "property_name")?;
            return text(prop, source)
                .trim()
                .starts_with("--")
                .then_some("field");
        }
        self.symbol_kind(node.kind())
    }

    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        match node.kind() {
            "rule_set" => named_child(node, "selectors").map(|n| text(n, source).trim()),
            "keyframes_statement" => {
                named_child(node, "keyframes_name").map(|n| text(n, source).trim())
            }
            "declaration" => named_child(node, "property_name").map(|n| text(n, source).trim()),
            _ => symbol_name(node, source),
        }
    }

    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }

    fn node_prefix(&self, _kind: &str) -> &'static str {
        "css"
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let trimmed = module.trim().trim_matches(['"', '\'']);
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("//")
        {
            return Some(ImportPath::absolute(trimmed.to_owned()));
        }
        support::relative_file_import(owner, trimmed)
    }

    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() != "import_statement" {
            return;
        }
        if let Some(target) = import_target(ctx.node, ctx.source) {
            ctx.import(
                facts,
                target.to_owned(),
                Some("*".to_owned()),
                Some(target.to_owned()),
            );
        }
    }

    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() != "call_expression" {
            return;
        }
        let Some(fn_name) = named_child(ctx.node, "function_name") else {
            return;
        };
        if text(fn_name, ctx.source).trim() != "var" {
            return;
        }
        let Some(args) = named_child(ctx.node, "arguments") else {
            return;
        };
        let Some(prop) = named_child(args, "plain_value") else {
            return;
        };
        let prop_name = text(prop, ctx.source).trim();
        if prop_name.starts_with("--") {
            support::push_static_reference(ctx, facts, prop_name);
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&CSS];
