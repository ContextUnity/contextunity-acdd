use super::profile::LanguageProfile;
use super::support::c_family;
use super::types::{
    call, CallForm, DeclarationRole, ImportPath, LanguageFamily, SyntaxContext, SyntaxGrammarSpec,
};
use crate::ast::syntax::ScopeBindings;
use crate::models::Facts;
use tree_sitter::Node as Syntax;
/// Language profile for c source files.
struct CProfile;
/// Shared c language profile.
static C: CProfile = CProfile;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("function_definition", DeclarationRole::Function),
        ("struct_specifier", DeclarationRole::Struct),
        ("union_specifier", DeclarationRole::Struct),
        ("enum_specifier", DeclarationRole::Enum),
        ("type_definition", DeclarationRole::Type),
    ],
    references: &["call_expression"],
    call_forms: &[("call_expression", CallForm::Direct)],
    scopes: &["translation_unit", "function_definition"],
    imports: &["preproc_include"],
    comments: &["comment"],
    typed_bindings: &[
        "parameter_declaration",
        "init_declarator",
        "declaration",
    ],
    assignments: &["assignment_expression"],
    member_calls: &["call_expression"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "while_statement",
        "do_statement",
        "switch_statement",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for CProfile {
    fn id(&self) -> &'static str {
        "c"
    }
    fn is_stdlib(&self, module: &str) -> bool {
        c_family::is_c_stdlib(module)
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("c")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["c", "h"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_c::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        c_family::symbol_kind(kind)
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        c_family::symbol(node)
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        c_family::symbol_name(node, source)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "c"
    }
    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        c_family::include_path(self, owner, module)
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        c_family::bindings(self, node, source)
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        c_family::include(ctx, facts);
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "call_expression" {
            call(ctx, facts, false);
        }
    }
}
/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&C];
