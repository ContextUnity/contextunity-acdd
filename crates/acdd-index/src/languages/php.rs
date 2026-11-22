use super::*;
/// Language profile for php source files.
pub struct Php;
/// Shared php language profile.
pub static PHP: Php = Php;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class_declaration", DeclarationRole::Class),
        ("interface_declaration", DeclarationRole::Interface),
        ("trait_declaration", DeclarationRole::Trait),
        ("enum_declaration", DeclarationRole::Enum),
        ("function_definition", DeclarationRole::Function),
        ("method_declaration", DeclarationRole::Method),
    ],
    references: &[
        "function_call_expression",
        "member_call_expression",
        "nullsafe_member_call_expression",
        "scoped_call_expression",
        "object_creation_expression",
    ],
    call_forms: &[
        ("function_call_expression", CallForm::Direct),
        ("member_call_expression", CallForm::Member),
        ("nullsafe_member_call_expression", CallForm::Member),
        ("scoped_call_expression", CallForm::Scoped),
        ("object_creation_expression", CallForm::Constructor),
    ],
    scopes: &[
        "program",
        "class_declaration",
        "interface_declaration",
        "trait_declaration",
        "enum_declaration",
        "function_definition",
        "method_declaration",
    ],
    imports: &[
        "namespace_use_clause",
        "namespace_use_group_clause",
        "include_expression",
        "include_once_expression",
        "require_expression",
        "require_once_expression",
    ],
    comments: &["comment"],
    typed_bindings: &[
        "simple_parameter",
        "variadic_parameter",
        "property_promotion_parameter",
    ],
    assignments: &["assignment_expression"],
    constructors: &["object_creation_expression"],
    member_calls: &[
        "member_call_expression",
        "nullsafe_member_call_expression",
    ],
    heritage: &["base_clause", "class_interface_clause"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "foreach_statement",
        "while_statement",
        "switch_statement",
        "match_expression",
        "try_statement",
        "catch_clause",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Php {
    fn id(&self) -> &'static str {
        "php"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["composer.json"]
    }
    fn is_stdlib(&self, module: &str) -> bool {
        let root = module.trim_start_matches('\\');
        matches!(
            root,
            "DateTime"
                | "DateTimeImmutable"
                | "DateTimeInterface"
                | "DateInterval"
                | "Exception"
                | "RuntimeException"
                | "InvalidArgumentException"
                | "LogicException"
                | "Throwable"
                | "Error"
                | "PDO"
                | "PDOStatement"
                | "stdClass"
                | "Closure"
                | "Generator"
                | "Iterator"
                | "Countable"
                | "ArrayAccess"
                | "JsonSerializable"
                | "Stringable"
                | "SplFileInfo"
                | "SplFileObject"
                | "DirectoryIterator"
                | "RecursiveDirectoryIterator"
        )
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("php")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["php"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_php::language_php()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "trait_declaration" => Some("trait"),
            "enum_declaration" => Some("enum"),
            "function_definition" => Some("function"),
            "method_declaration" => Some("method"),
            _ => None,
        }
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "php"
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        if let Some(raw) = support::parse_namespace_line(source) {
            let ns = raw.trim_start_matches('\\').replace('\\', ".");
            if !ns.is_empty() {
                return ns;
            }
        }
        self.module_name(path)
    }
    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let trimmed = module.trim().trim_matches(['"', '\'']);
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.ends_with(".php") || trimmed.starts_with("./") || trimmed.starts_with("../") {
            let namespace = relative_namespace(owner, module_stem(trimmed))?;
            return Some(ImportPath {
                namespace,
                relative: true,
                symbol_path: false,
            });
        }
        let namespace = trimmed.trim_start_matches('\\').replace('\\', ".");
        (!namespace.is_empty()).then_some(ImportPath {
            namespace,
            relative: false,
            symbol_path: true,
        })
    }
    fn receiver(&self, name: &str, owner: &Node) -> bool {
        name == "this" && owner.details["receiver_name"] == "this"
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        _source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        SymbolMetadata {
            receiver_name: (node.kind() == "method_declaration"
                && syntax::child(node, &["static_modifier"]).is_none())
            .then(|| "this".into()),
            ..Default::default()
        }
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ast::ScopeBindings {
        syntax::bindings(
            self,
            node,
            source,
            &[
                "simple_parameter",
                "variadic_parameter",
                "property_promotion_parameter",
            ],
            &["assignment_expression"],
        )
    }
    support::delegate_extract_file!();
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if matches!(
            ctx.node.kind(),
            "namespace_use_clause" | "namespace_use_group_clause"
        ) {
            let target = syntax::named(
                ctx.node,
                ctx.source,
                &["qualified_name", "namespace_name", "name"],
            );
            if let Some(target) = target {
                let full_target = if ctx.node.kind() == "namespace_use_group_clause" {
                    ctx.node
                        .parent()
                        .and_then(|group| group.parent())
                        .and_then(|declaration| syntax::named(declaration, ctx.source, &["namespace_name"]))
                        .map(|prefix| format!("{}\\{target}", prefix.trim_end_matches('\\')))
                } else {
                    None
                };
                let target = full_target.as_deref().unwrap_or(target);
                let alias = syntax::child(ctx.node, &["namespace_aliasing_clause"])
                    .and_then(|n| syntax::named(n, ctx.source, &["name"]))
                    .or_else(|| target.rsplit('\\').next());
                syntax::import(ctx, facts, target, alias.map(str::to_owned));
            }
        }
        if matches!(
            ctx.node.kind(),
            "include_expression"
                | "include_once_expression"
                | "require_expression"
                | "require_once_expression"
        ) {
            let mut cursor = ctx.node.walk();
            let target = ctx
                .node
                .named_children(&mut cursor)
                .find_map(|mut child| {
                    while child.kind() == "parenthesized_expression" {
                        child = child.named_child(0)?;
                    }
                    matches!(child.kind(), "string" | "encapsed_string")
                        .then(|| text(child, ctx.source).trim().trim_matches(['"', '\'']))
                })
                .unwrap_or_else(|| text(ctx.node, ctx.source));
            syntax::import(ctx, facts, target, Some("*".to_owned()));
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        match ctx.node.kind() {
            "function_call_expression" => {
                if let Some(callee) = ctx.node.child_by_field_name("function") {
                    syntax::call_reference(
                        ctx,
                        facts,
                        text(callee, ctx.source),
                        callee.kind() == "variable_name",
                    );
                }
            }
            "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => {
                if let (Some(name), Some(receiver)) = (
                    ctx.node.child_by_field_name("name"),
                    ctx.node
                        .child_by_field_name("object")
                        .or_else(|| ctx.node.child_by_field_name("scope")),
                ) {
                    let receiver_text = text(receiver, ctx.source);
                    let proven_this = receiver_text == "$this"
                        && facts
                            .nodes
                            .iter()
                            .find(|n| n.id == ctx.owner)
                            .is_some_and(|n| {
                                n.kind == "method" && n.details["receiver_name"] == "this"
                            });
                    let expression = format!(
                        "{}.{}",
                        if proven_this { "this" } else { receiver_text },
                        text(name, ctx.source)
                    );
                    syntax::call_reference(
                        ctx,
                        facts,
                        &expression,
                        name.kind() == "variable_name",
                    );
                }
            }
            "object_creation_expression" => {
                if let Some(name) = ctx.node.named_child(0) {
                    syntax::call_reference(
                        ctx,
                        facts,
                        text(name, ctx.source),
                        name.kind() == "variable_name",
                    );
                }
            }
            _ => {}
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some(base) = syntax::child(ctx.node, &["base_clause"]) {
            syntax::types(ctx, facts, base, "inherits", &[]);
        }
        if let Some(base) = syntax::child(ctx.node, &["class_interface_clause"]) {
            syntax::types(ctx, facts, base, "implements", &[]);
        }
    }
}

/// Language profiles provided by this module.
pub static PROFILES: &[&dyn LanguageProfile] = &[&PHP];
