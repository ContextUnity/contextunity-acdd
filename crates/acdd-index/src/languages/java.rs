use super::*;
/// Language profile for java source files.
pub struct Java;
/// Shared java language profile.
pub static JAVA: Java = Java;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class_declaration", DeclarationRole::Class),
        ("record_declaration", DeclarationRole::Class),
        ("interface_declaration", DeclarationRole::Interface),
        ("enum_declaration", DeclarationRole::Enum),
        ("method_declaration", DeclarationRole::Method),
        ("constructor_declaration", DeclarationRole::Method),
    ],
    references: &[
        "explicit_constructor_invocation",
        "method_invocation",
        "object_creation_expression",
    ],
    call_forms: &[
        ("explicit_constructor_invocation", CallForm::Constructor),
        ("method_invocation", CallForm::Member),
        ("object_creation_expression", CallForm::Constructor),
    ],
    scopes: &[
        "program",
        "class_declaration",
        "record_declaration",
        "interface_declaration",
        "enum_declaration",
        "method_declaration",
        "constructor_declaration",
    ],
    imports: &["import_declaration"],
    comments: &["line_comment", "block_comment"],
    typed_bindings: &[
        "formal_parameter",
        "spread_parameter",
        "catch_formal_parameter",
        "variable_declarator",
    ],
    assignments: &["assignment_expression"],
    constructors: &["object_creation_expression"],
    member_calls: &["method_invocation"],
    heritage: &["superclass", "super_interfaces", "extends_interfaces"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "enhanced_for_statement",
        "while_statement",
        "switch_expression",
        "try_statement",
        "catch_clause",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Java {
    fn id(&self) -> &'static str {
        "java"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["pom.xml", "build.gradle", "build.gradle.kts"]
    }
    fn extract_manifest_dependencies(&self, filename: &str, content: &str) -> Vec<String> {
        super::build_manifest::dependencies(filename, content)
    }
    fn is_stdlib(&self, module: &str) -> bool {
        ["java.", "javax.", "org.w3c.", "org.xml.sax.", "org.ietf.jgss."]
            .iter()
            .any(|prefix| module.starts_with(prefix) || module == prefix.trim_end_matches('.'))
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("java")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_java::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class_declaration" | "record_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "method_declaration" | "constructor_declaration" => Some("method"),
            _ => None,
        }
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn sibling_accessible(&self, _path_a: &str, _path_b: &str) -> bool {
        true
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "java"
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        source
            .lines()
            .map(str::trim)
            .find_map(|line| {
                line.strip_prefix("package ")
                    .map(|package| package.trim_end_matches(';').trim().to_owned())
            })
            .filter(|package| !package.is_empty())
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
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ast::ScopeBindings {
        syntax::bindings(
            self,
            node,
            source,
            &[
                "formal_parameter",
                "spread_parameter",
                "catch_formal_parameter",
            ],
            &["variable_declarator"],
        )
    }
    fn value_binding_applies(&self, reference: &Reference) -> bool {
        reference.kind != "calls" || reference.expression.contains('.')
    }
    support::delegate_extract_file!();
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "import_declaration" {
            if let Some(module) =
                syntax::named(ctx.node, ctx.source, &["scoped_identifier", "identifier"])
            {
                let has_wildcard = syntax::child(ctx.node, &["asterisk"]).is_some();
                if has_wildcard {
                    let target = format!("{module}.*");
                    syntax::import(ctx, facts, &target, Some("*".to_owned()));
                } else {
                    syntax::import(
                        ctx,
                        facts,
                        module,
                        module.rsplit('.').next().map(str::to_owned),
                    );
                }
            }
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "explicit_constructor_invocation" {
            let target = ctx
                .node
                .child_by_field_name("constructor")
                .map(|n| text(n, ctx.source))
                .unwrap_or_else(|| syntax::base_constructor_target(text(ctx.node, ctx.source)));
            syntax::call_reference(ctx, facts, target, false);
        }
        if ctx.node.kind() == "method_invocation" {
            if let Some(name) = ctx.node.child_by_field_name("name") {
                syntax::member_call(ctx, facts, name, ctx.node.child_by_field_name("object"));
            }
        }
        if ctx.node.kind() == "object_creation_expression" {
            if let Some(name) = ctx.node.child_by_field_name("type") {
                syntax::constructor_call_reference(ctx, facts, name);
            }
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some(base) = ctx.node.child_by_field_name("superclass") {
            syntax::types(ctx, facts, base, "inherits", &[]);
        }
        if let Some(interfaces) = ctx
            .node
            .child_by_field_name("interfaces")
            .and_then(|n| syntax::child(n, &["type_list"]))
        {
            syntax::types(ctx, facts, interfaces, "implements", &[]);
        }
        if let Some(interfaces) = syntax::child(ctx.node, &["extends_interfaces"])
            .and_then(|n| syntax::child(n, &["type_list"]))
        {
            syntax::types(ctx, facts, interfaces, "inherits", &[]);
        }
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        if matches!(node.kind(), "method_declaration" | "constructor_declaration") {
            let owner = node.parent().and_then(|body| body.parent());
            let receiver_type = owner
                .and_then(|o| o.child_by_field_name("name"))
                .map(|n| text(n, source).trim().to_owned());
            let is_static = (0..node.child_count()).any(|i| {
                node.child(i).is_some_and(|c| {
                    c.kind() == "modifiers" && text(c, source).split_whitespace().any(|w| w == "static")
                })
            });
            return SymbolMetadata {
                receiver_name: (!is_static).then(|| "this".into()),
                receiver_type,
                is_method: Some(true),
                is_static: Some(is_static),
                ..Default::default()
            };
        }
        SymbolMetadata::default()
    }
}

/// Language profiles provided by this module.
pub static PROFILES: &[&dyn LanguageProfile] = &[&JAVA];
