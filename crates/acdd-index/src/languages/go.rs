use super::context::FileContext;
use super::profile::LanguageProfile;
use super::types::{
    call, module_stem, parse_package_declaration, CallForm, DeclarationRole, ImportPath,
    LanguageFamily, SymbolMetadata, SyntaxContext, SyntaxGrammarSpec,
};
use crate::ast::syntax::{field, text};
use crate::models::Facts;
use tree_sitter::Node as Syntax;
/// Performs language.
fn language() -> tree_sitter::Language {
    tree_sitter_go::language()
}
/// Performs kind.
fn kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" => Some("function"),
        "method_declaration" => Some("method"),
        "type_spec" => Some("type"),
        _ => None,
    }
}

fn parse_package_name(source: &str) -> Option<&str> {
    parse_package_declaration(source, false)
}

/// Language profile for go source files.
struct Go;
/// Shared go language profile.
static GO: Go = Go;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("function_declaration", DeclarationRole::Function),
        ("method_declaration", DeclarationRole::Method),
        ("type_spec", DeclarationRole::Type),
    ],
    references: &["call_expression"],
    call_forms: &[("call_expression", CallForm::Direct)],
    scopes: &[
        "source_file",
        "function_declaration",
        "method_declaration",
        "func_literal",
    ],
    imports: &["import_spec"],
    comments: &["comment"],
    typed_bindings: &[
        "parameter_declaration",
        "variadic_parameter_declaration",
        "var_spec",
        "const_spec",
    ],
    assignments: &["short_var_declaration", "assignment_statement"],
    constructors: &["composite_literal"],
    member_calls: &["call_expression"],
    conditionals: &[
        "if_statement",
        "for_statement",
        "expression_switch_statement",
        "type_switch_statement",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Go {
    fn id(&self) -> &'static str {
        "go"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["go.mod"]
    }
    fn extract_manifest_dependencies(&self, filename: &str, content: &str) -> Vec<String> {
        use super::dependency_registry::parser::ManifestParser;
        if filename != "go.mod" {
            return Vec::new();
        }
        super::dependency_registry::parsers::go::GO_PARSER
            .parse(filename, content)
            .dependencies
    }
    fn is_stdlib(&self, module: &str) -> bool {
        const STANDARD_PACKAGES: &[&str] = &[
            "archive", "bufio", "bytes", "cmp", "compress", "container", "context",
            "crypto", "database", "debug", "embed", "encoding", "errors", "expvar",
            "flag", "fmt", "go", "hash", "html", "image", "index", "io", "iter",
            "log", "maps", "math", "mime", "net", "os", "path", "plugin", "reflect",
            "regexp", "runtime", "slices", "sort", "strconv", "strings", "sync",
            "syscall", "testing", "text", "time", "unicode", "unique", "unsafe",
        ];
        let root = module.split('/').next().unwrap_or(module);
        STANDARD_PACKAGES.contains(&root)
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        language()
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("go")
    }
    fn symbol_kind(&self, k: &str) -> Option<&'static str> {
        kind(k)
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        if node.kind() == "type_spec" {
            return match node.child_by_field_name("type").map(|t| t.kind()) {
                Some("struct_type") => Some("struct"),
                Some("interface_type") => Some("interface"),
                _ => Some("type"),
            };
        }
        self.symbol_kind(node.kind())
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "go"
    }
    fn module_name_for_source(&self, path: &str, source: &str) -> String {
        let pkg = parse_package_name(source);
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        match (dir, pkg) {
            ("", Some(pkg)) => pkg.to_string(),
            ("", None) => module_stem(path).to_string(),
            (dir, Some(pkg)) => {
                let dir_mod = dir.replace('/', ".");
                if dir.rsplit('/').next() == Some(pkg) {
                    dir_mod
                } else {
                    format!("{dir_mod}.{pkg}")
                }
            }
            (dir, None) => dir.replace('/', "."),
        }
    }
    fn module_name(&self, path: &str) -> String {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        if dir.is_empty() {
            module_stem(path).to_string()
        } else {
            dir.replace('/', ".")
        }
    }
    fn sibling_accessible(&self, path_a: &str, path_b: &str) -> bool {
        let dir_a = path_a.rsplit_once('/').map_or("", |(d, _)| d);
        let dir_b = path_b.rsplit_once('/').map_or("", |(d, _)| d);
        dir_a == dir_b
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let (node, source) = (ctx.node, ctx.source);
        let mut add = |expression, alias, module| ctx.import(facts, expression, alias, module);
        if node.kind() == "import_spec" {
            if let Some(path) = field(node, source, "path") {
                let path = path.trim_matches(['"', '`']);
                add(
                    path.into(),
                    Some(
                        field(node, source, "name")
                            .unwrap_or(path.rsplit('/').next().unwrap_or(path))
                            .into(),
                    ),
                    Some(path.into()),
                );
            }
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if matches!(ctx.node.kind(), "call_expression") {
            call(ctx, facts, false);
        }
    }
    fn builtin(&self, symbol: &str) -> bool {
        matches!(
            symbol,
            "make"
                | "new"
                | "len"
                | "cap"
                | "append"
                | "copy"
                | "delete"
                | "recover"
                | "close"
                | "panic"
                | "print"
                | "println"
                | "clear"
                | "min"
                | "max"
                | "complex"
                | "real"
                | "imag"
        )
    }

    fn normalize_import(&self, _owner: &str, module: &str) -> Option<ImportPath> {
        Some(ImportPath::absolute(module.replace('/', ".")))
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        if node.kind() == "method_declaration" {
            let receiver = field(node, source, "receiver").map(str::to_owned);
            let param = node
                .child_by_field_name("receiver")
                .and_then(|recv| recv.named_child(0));
            let receiver_type = param
                .and_then(|p| p.child_by_field_name("type"))
                .map(|ty| text(ty, source).trim().trim_start_matches('*').to_owned());
            let receiver_name = param
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| text(n, source).trim().to_owned());
            return SymbolMetadata {
                receiver,
                receiver_name,
                receiver_type,
                is_method: Some(true),
                is_static: Some(false),
                ..Default::default()
            };
        }
        SymbolMetadata {
            receiver: field(node, source, "receiver").map(str::to_owned),
            ..Default::default()
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&GO];
