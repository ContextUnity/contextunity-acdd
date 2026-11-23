use super::profile::LanguageProfile;
use super::support;
use super::types::{DeclarationRole, ImportPath, SyntaxContext, SyntaxGrammarSpec};
use crate::ast::syntax::{symbol_name, text};
use crate::models::Facts;
use tree_sitter::Node as Syntax;

/// Language profile for JSON documents and JSON Schema files.
struct Json;
/// Shared JSON language profile.
static JSON: Json = Json;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[("pair", DeclarationRole::Field)],
    references: &["pair"],
    scopes: &["document"],
    imports: &["pair"],
    comments: &["comment"],
    ..SyntaxGrammarSpec::DEFAULT
};

fn pair_key<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    let key_node = node.child_by_field_name("key")?;
    let raw = text(key_node, source).trim();
    Some(raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(raw))
}

fn json_ref_target<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() != "pair" || pair_key(node, source) != Some("$ref") {
        return None;
    }
    let value = node.child_by_field_name("value")?;
    if value.kind() != "string" {
        return None;
    }
    let raw = text(value, source).trim();
    let trimmed = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    (!trimmed.is_empty()).then_some(trimmed)
}

impl LanguageProfile for Json {
    fn id(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_json::language()
    }

    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        (kind == "pair").then_some("field")
    }

    fn symbol_with_source(&self, node: Syntax<'_>, source: &str) -> Option<&'static str> {
        if node.kind() != "pair" {
            return None;
        }
        let key = pair_key(node, source)?;
        if key == "$ref" || key.is_empty() {
            return None;
        }
        let object = node.parent().filter(|p| p.kind() == "object")?;
        let container = object.parent()?;
        let admitted = container.kind() == "document"
            || (container.kind() == "pair"
                && matches!(pair_key(container, source), Some("$defs" | "definitions")));
        admitted.then_some("field")
    }

    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        if node.kind() == "pair" {
            return pair_key(node, source);
        }
        symbol_name(node, source)
    }

    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }

    fn node_prefix(&self, _kind: &str) -> &'static str {
        "json"
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let (file_part, _) = module.split_once('#').unwrap_or((module, ""));
        if file_part.is_empty() || file_part.contains("://") {
            return None;
        }
        support::relative_file_import(owner, file_part)
    }

    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let Some(target) = json_ref_target(ctx.node, ctx.source) else {
            return;
        };
        let (file_part, _) = target.split_once('#').unwrap_or((target, ""));
        if file_part.is_empty() || file_part.contains("://") {
            return;
        }
        ctx.import(
            facts,
            target.to_owned(),
            Some("*".to_owned()),
            Some(target.to_owned()),
        );
    }

    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let Some(target) = json_ref_target(ctx.node, ctx.source) else {
            return;
        };
        let (file_part, fragment) = target.split_once('#').unwrap_or((target, ""));
        let frag_tail = fragment
            .trim_start_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        if !frag_tail.is_empty() && !file_part.contains("://") {
            support::push_static_reference(ctx, facts, frag_tail);
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&JSON];
