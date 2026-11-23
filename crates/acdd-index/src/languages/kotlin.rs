use super::profile::LanguageProfile;
use super::support as syntax;
use super::types::{
    CallForm, DeclarationRole, ImportPath, LanguageFamily, SyntaxContext, SyntaxGrammarSpec,
};
use crate::ast::relations;
use crate::ast::syntax::{text, ScopeBindings};
use crate::models::{Facts, Node};
use tree_sitter::Node as Syntax;
/// Language profile for kotlin source files.
struct Kotlin;
/// Shared kotlin language profile.
static KOTLIN: Kotlin = Kotlin;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class_declaration", DeclarationRole::Class),
        ("object_declaration", DeclarationRole::Class),
        ("function_declaration", DeclarationRole::Function),
        ("type_alias", DeclarationRole::Type),
    ],
    references: &[
        "constructor_invocation",
        "constructor_delegation_call",
        "call_expression",
    ],
    call_forms: &[
        ("constructor_invocation", CallForm::Constructor),
        ("constructor_delegation_call", CallForm::Constructor),
        ("call_expression", CallForm::Direct),
    ],
    scopes: &[
        "source_file",
        "class_declaration",
        "object_declaration",
        "function_declaration",
    ],
    imports: &["import_header"],
    comments: &["line_comment", "multiline_comment"],
    typed_bindings: &[
        "parameter",
        "class_parameter",
        "variable_declaration",
        "property_declaration",
    ],
    assignments: &["assignment"],
    member_calls: &["call_expression"],
    heritage: &["delegation_specifier"],
    conditionals: &[
        "if_expression",
        "when_expression",
        "for_statement",
        "while_statement",
        "try_expression",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Kotlin {
    fn id(&self) -> &'static str {
        "kotlin"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["pom.xml", "build.gradle", "build.gradle.kts"]
    }
    fn extract_manifest_dependencies(&self, filename: &str, content: &str) -> Vec<String> {
        super::build_manifest::dependencies(filename, content)
    }
    fn is_stdlib(&self, module: &str) -> bool {
        [
            "java.",
            "javax.",
            "kotlin.",
            "org.w3c.",
            "org.xml.sax.",
            "org.ietf.jgss.",
        ]
        .iter()
        .any(|prefix| module.starts_with(prefix) || module == prefix.trim_end_matches('.'))
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("kotlin")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_kotlin::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class_declaration" | "object_declaration" => Some("class"),
            "function_declaration" => Some("function"),
            "type_alias" => Some("type"),
            _ => None,
        }
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        if node.kind() == "class_declaration" {
            let mut c = node.walk();
            if node.children(&mut c).any(|n| n.kind() == "interface") {
                return Some("interface");
            }
        }
        if node.kind() == "function_declaration"
            && node
                .parent()
                .is_some_and(|p| matches!(p.kind(), "class_body" | "enum_class_body"))
        {
            return Some("method");
        }
        self.symbol_kind(node.kind())
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn sibling_accessible(&self, _path_a: &str, _path_b: &str) -> bool {
        true
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        syntax::named(node, source, &["simple_identifier", "type_identifier"])
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "kt"
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        super::types::parse_package_declaration(source, true)
            .map(str::to_owned)
            .unwrap_or_else(|| self.module_name(path))
    }
    fn normalize_import(&self, _owner: &str, module: &str) -> Option<ImportPath> {
        syntax::symbol_import_path(module)
    }
    fn class_scope(&self) -> bool {
        true
    }
    fn receiver(&self, name: &str, _owner: &Node) -> bool {
        name == "this"
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        syntax::bindings(
            self,
            node,
            source,
            &["parameter", "class_parameter"],
            &["variable_declaration"],
        )
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "import_header" {
            if let Some(module) = syntax::named(ctx.node, ctx.source, &["identifier"]) {
                if syntax::child(ctx.node, &["wildcard_import"]).is_some() {
                    let target = format!("{module}.*");
                    syntax::import(ctx, facts, &target, Some("*".to_owned()));
                } else {
                    let alias = syntax::child(ctx.node, &["import_alias"])
                        .and_then(|n| syntax::named(n, ctx.source, &["type_identifier"]))
                        .or_else(|| module.rsplit('.').next());
                    syntax::import(ctx, facts, module, alias.map(str::to_owned));
                }
            }
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "constructor_invocation" {
            if let Some(ty) = syntax::child(ctx.node, &["user_type"]) {
                syntax::constructor_call_reference(ctx, facts, ty);
            }
        } else if ctx.node.kind() == "constructor_delegation_call" {
            let target = ctx.node.named_child(0).unwrap_or(ctx.node);
            syntax::call_reference(ctx, facts, text(target, ctx.source), false);
        }
        if ctx.node.kind() == "call_expression" {
            if let Some(callee) = ctx.node.named_child(0) {
                syntax::call_reference(
                    ctx,
                    facts,
                    text(callee, ctx.source),
                    !matches!(callee.kind(), "simple_identifier" | "navigation_expression"),
                );
            }
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        {
            let mut c = ctx.node.walk();
            for base in ctx
                .node
                .named_children(&mut c)
                .filter(|n| n.kind() == "delegation_specifier")
            {
                let mut node = base;
                while matches!(
                    node.kind(),
                    "delegation_specifier" | "constructor_invocation" | "explicit_delegation"
                ) {
                    let Some(child) = node.named_child(0) else {
                        break;
                    };
                    node = child;
                }
                relations::reference(
                    facts,
                    ctx.owner,
                    text(node, ctx.source),
                    "bases",
                    ctx.line(),
                );
            }
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&KOTLIN];
