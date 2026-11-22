use super::*;
/// Represents csharp data.
pub struct CSharp;
/// Shared csharp language profile.
pub static CSHARP: CSharp = CSharp;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class_declaration", DeclarationRole::Class),
        ("record_declaration", DeclarationRole::Class),
        ("interface_declaration", DeclarationRole::Interface),
        ("struct_declaration", DeclarationRole::Struct),
        ("enum_declaration", DeclarationRole::Enum),
        ("method_declaration", DeclarationRole::Method),
        ("constructor_declaration", DeclarationRole::Method),
        ("local_function_statement", DeclarationRole::Function),
    ],
    references: &[
        "object_creation_expression",
        "implicit_object_creation_expression",
        "constructor_initializer",
        "invocation_expression",
    ],
    call_forms: &[
        ("object_creation_expression", CallForm::Constructor),
        ("implicit_object_creation_expression", CallForm::Constructor),
        ("constructor_initializer", CallForm::Constructor),
        ("invocation_expression", CallForm::Member),
    ],
    scopes: &[
        "compilation_unit",
        "namespace_declaration",
        "file_scoped_namespace_declaration",
        "class_declaration",
        "record_declaration",
        "struct_declaration",
        "method_declaration",
        "constructor_declaration",
        "local_function_statement",
    ],
    imports: &["using_directive"],
    comments: &["comment"],
    typed_bindings: &["parameter", "variable_declarator"],
    assignments: &["assignment_expression"],
    constructors: &["object_creation_expression"],
    member_calls: &["invocation_expression"],
    heritage: &["base_list"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "foreach_statement",
        "while_statement",
        "switch_statement",
        "switch_expression",
        "try_statement",
        "catch_clause",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for CSharp {
    fn id(&self) -> &'static str {
        "csharp"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["*.csproj"]
    }
    fn is_stdlib(&self, module: &str) -> bool {
        module == "System"
            || module.starts_with("System.")
            || module == "Microsoft"
            || module.starts_with("Microsoft.")
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("csharp")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_c_sharp::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class_declaration" | "record_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "struct_declaration" => Some("struct"),
            "enum_declaration" => Some("enum"),
            "method_declaration" | "constructor_declaration" => Some("method"),
            "local_function_statement" => Some("function"),
            _ => None,
        }
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        support::parse_namespace_line(source)
            .map(str::to_owned)
            .unwrap_or_else(|| self.module_name(path))
    }
    fn sibling_accessible(&self, _path_a: &str, _path_b: &str) -> bool {
        true
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "cs"
    }
    fn normalize_import(&self, _owner: &str, module: &str) -> Option<ImportPath> {
        let trimmed = module.trim();
        (!trimmed.is_empty()).then(|| ImportPath {
            namespace: trimmed.to_owned(),
            relative: false,
            symbol_path: true,
        })
    }
    fn class_scope(&self) -> bool {
        true
    }
    fn receiver(&self, name: &str, _owner: &Node) -> bool {
        name == "this"
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ast::ScopeBindings {
        syntax::bindings(
            self,
            node,
            source,
            &["parameter", "parameter_list"],
            &["variable_declarator"],
        )
    }
    support::delegate_extract_file!();
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "using_directive" {
            let mut c = ctx.node.walk();
            if let Some(target) = ctx
                .node
                .named_children(&mut c)
                .find(|n| Some(*n) != ctx.node.child_by_field_name("name"))
            {
                syntax::import(
                    ctx,
                    facts,
                    text(target, ctx.source),
                    Some(
                        field(ctx.node, ctx.source, "name")
                            .unwrap_or("*")
                            .to_owned(),
                    ),
                );
            };
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "object_creation_expression" {
            if let Some(ty) = ctx.node.child_by_field_name("type") {
                syntax::constructor_call_reference(ctx, facts, ty);
            }
        } else if ctx.node.kind() == "implicit_object_creation_expression" {
            syntax::call_reference(ctx, facts, "new", false);
        } else if ctx.node.kind() == "constructor_initializer" {
            let target = ctx.node.child(1).unwrap_or(ctx.node);
            syntax::call_reference(ctx, facts, text(target, ctx.source).trim(), false);
        }
        if ctx.node.kind() == "invocation_expression" {
            call(ctx, facts, false);
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some(bases) = syntax::child(ctx.node, &["base_list"]) {
            syntax::types(ctx, facts, bases, "bases", &[]);
        }
    }
}

/// Language profiles provided by this module.
pub static PROFILES: &[&dyn LanguageProfile] = &[&CSHARP];
