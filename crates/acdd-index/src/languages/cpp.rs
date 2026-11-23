use super::context::FileContext;
use super::profile::LanguageProfile;
use super::support as syntax;
use super::support::c_family;
use super::types::{
    call, CallForm, DeclarationRole, ImportPath, LanguageFamily, SymbolMetadata, SyntaxContext,
    SyntaxGrammarSpec,
};
use crate::ast::relations;
use crate::ast::syntax::{text, ScopeBindings};
use crate::models::{Facts, Node};
use tree_sitter::Node as Syntax;
/// Language profile for cpp source files.
struct Cpp;
/// Shared cpp language profile.
static CPP: Cpp = Cpp;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class_specifier", DeclarationRole::Class),
        ("struct_specifier", DeclarationRole::Struct),
        ("union_specifier", DeclarationRole::Struct),
        ("enum_specifier", DeclarationRole::Enum),
        ("namespace_definition", DeclarationRole::Namespace),
        ("type_definition", DeclarationRole::Type),
        ("function_definition", DeclarationRole::Function),
        ("field_declaration", DeclarationRole::Method),
    ],
    references: &["call_expression", "new_expression"],
    call_forms: &[
        ("call_expression", CallForm::Direct),
        ("new_expression", CallForm::Constructor),
    ],
    scopes: &[
        "translation_unit",
        "namespace_definition",
        "class_specifier",
        "struct_specifier",
        "function_definition",
    ],
    imports: &["preproc_include"],
    comments: &["comment"],
    typed_bindings: &[
        "parameter_declaration",
        "optional_parameter_declaration",
        "init_declarator",
        "declaration",
    ],
    assignments: &["assignment_expression"],
    constructors: &["new_expression"],
    member_calls: &["call_expression"],
    heritage: &["base_class_clause"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "for_range_loop",
        "while_statement",
        "do_statement",
        "switch_statement",
        "try_statement",
        "catch_clause",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

fn has_function_declarator(mut node: Syntax<'_>) -> bool {
    while let Some(inner) = node.child_by_field_name("declarator") {
        if inner.kind() == "function_declarator" {
            return true;
        }
        node = inner;
    }
    false
}

fn is_out_of_line_method(node: Syntax<'_>) -> bool {
    node.kind() == "function_definition"
        && !node
            .parent()
            .is_some_and(|p| p.kind() == "field_declaration_list")
        && node
            .child_by_field_name("declarator")
            .is_some_and(|declarator| syntax::declarator(declarator).kind() == "qualified_identifier")
}

fn out_of_line_method_parts<'a>(node: Syntax<'_>, source: &'a str) -> Option<(&'a str, &'a str)> {
    if !is_out_of_line_method(node) {
        return None;
    }
    let inner = syntax::declarator(node.child_by_field_name("declarator")?);
    let scope = inner.child_by_field_name("scope")?;
    let name = inner.child_by_field_name("name")?;
    let class_name = text(scope, source).trim().rsplit("::").next()?.trim();
    let method_name = text(name, source).trim();
    (!class_name.is_empty() && !method_name.is_empty()).then_some((class_name, method_name))
}

fn enclosing_class_name<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    let class_node = node
        .parent()
        .filter(|p| p.kind() == "field_declaration_list")
        .and_then(|list| list.parent())
        .filter(|owner| matches!(owner.kind(), "class_specifier" | "struct_specifier"))?;
    class_node
        .child_by_field_name("name")
        .map(|n| text(n, source).trim())
        .filter(|name| !name.is_empty())
}

impl LanguageProfile for Cpp {
    fn id(&self) -> &'static str {
        "cpp"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["CMakeLists.txt", "meson.build"]
    }
    fn is_stdlib(&self, module: &str) -> bool {
        c_family::is_cpp_stdlib(module)
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("cpp")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_cpp::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class_specifier" => Some("class"),
            "namespace_definition" => Some("namespace"),
            "field_declaration" => Some("method"),
            _ => c_family::symbol_kind(kind),
        }
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        out_of_line_method_parts(node, source)
            .map(|(_, method)| method)
            .or_else(|| c_family::symbol_name(node, source))
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        match node.kind() {
            "class_specifier" => {
                if node.child_by_field_name("body").is_none()
                    && !node.parent().is_some_and(|p| {
                        p.kind() == "declaration" && p.child_by_field_name("declarator").is_none()
                    })
                {
                    None
                } else {
                    Some("class")
                }
            }
            "namespace_definition" => Some("namespace"),
            "function_definition" => {
                if node
                    .parent()
                    .is_some_and(|p| p.kind() == "field_declaration_list")
                    || is_out_of_line_method(node)
                {
                    Some("method")
                } else {
                    Some("function")
                }
            }
            "field_declaration" => (node
                .parent()
                .is_some_and(|p| p.kind() == "field_declaration_list")
                && has_function_declarator(node))
            .then_some("method"),
            _ => c_family::symbol(node),
        }
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "cpp"
    }
    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        c_family::include_path(self, owner, module)
    }
    fn class_scope(&self) -> bool {
        true
    }
    fn receiver(&self, name: &str, _owner: &Node) -> bool {
        name == "this"
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        let mut bindings = c_family::bindings(self, node, source);
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if current != node && self.symbol(current).is_some() {
                continue;
            }
            if current.kind() == "expression_statement" {
                if let Some(names) = expression_declaration(current) {
                    for name in names {
                        let name = text(name, source).to_owned();
                        bindings.all.push(name.clone());
                        bindings.rebindings.push(name);
                    }
                }
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
        bindings.all.sort();
        bindings.all.dedup();
        bindings.rebindings.sort();
        bindings.rebindings.dedup();
        bindings
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        c_family::include(ctx, facts);
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "new_expression" {
            if let Some(ty) = ctx.node.child_by_field_name("type") {
                syntax::constructor_call_reference(ctx, facts, ty);
            }
        }
        if ctx.node.kind() == "call_expression" {
            call(ctx, facts, false);
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some((class_name, _)) = out_of_line_method_parts(ctx.node, ctx.source) {
            relations::reference(facts, ctx.owner, class_name, "references", ctx.line());
        }
        if let Some(bases) = syntax::child(ctx.node, &["base_class_clause"]) {
            syntax::types(
                ctx,
                facts,
                bases,
                "inherits",
                &["access_specifier", "virtual"],
            );
        }
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        if let Some((class_name, _)) = out_of_line_method_parts(node, source) {
            return SymbolMetadata {
                receiver_name: Some("this".into()),
                receiver_type: Some(class_name.to_owned()),
                is_method: Some(true),
                ..Default::default()
            };
        }
        if node.kind() == "function_definition"
            || (node.kind() == "field_declaration" && has_function_declarator(node))
        {
            if let Some(class_name) = enclosing_class_name(node, source) {
                return SymbolMetadata {
                    receiver_name: Some("this".into()),
                    receiver_type: Some(class_name.to_owned()),
                    is_method: Some(true),
                    is_stub: node.kind() == "field_declaration",
                    ..Default::default()
                };
            }
        }
        SymbolMetadata::default()
    }
}

fn expression_declaration(statement: Syntax<'_>) -> Option<Vec<Syntax<'_>>> {
    let mut expressions = Vec::new();
    let mut stack = vec![statement.named_child(0)?];
    while let Some(node) = stack.pop() {
        if node.kind() == "comma_expression" {
            stack.push(node.child_by_field_name("right")?);
            stack.push(node.child_by_field_name("left")?);
        } else {
            expressions.push(node);
        }
    }
    expressions
        .into_iter()
        .enumerate()
        .map(|(index, node)| expression_pointer(node, index == 0))
        .collect()
}

fn expression_pointer(mut node: Syntax<'_>, first: bool) -> Option<Syntax<'_>> {
    if node.kind() == "assignment_expression" {
        if node.child_by_field_name("operator")?.kind() != "=" {
            return None;
        }
        node = node.child_by_field_name("left")?;
    }
    if node.kind() != "call_expression"
        || node.child_by_field_name("arguments")?.named_child_count() != 0
    {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    let pointer = if first {
        if function.kind() != "call_expression"
            || function.child_by_field_name("function")?.kind() != "primitive_type"
        {
            return None;
        }
        let arguments = function.child_by_field_name("arguments")?;
        if arguments.named_child_count() != 1 {
            return None;
        }
        arguments.named_child(0)?
    } else {
        function
    };
    let pointer = unparenthesized(pointer)?;
    if pointer.kind() != "pointer_expression"
        || pointer.child_by_field_name("operator")?.kind() != "*"
    {
        return None;
    }
    let name = unparenthesized(pointer.child_by_field_name("argument")?)?;
    (name.kind() == "identifier").then_some(name)
}

fn unparenthesized(mut node: Syntax<'_>) -> Option<Syntax<'_>> {
    while node.kind() == "parenthesized_expression" {
        node = node.named_child(0)?;
    }
    Some(node)
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&CPP];
