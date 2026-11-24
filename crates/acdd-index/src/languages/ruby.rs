use super::profile::LanguageProfile;
use super::support as syntax;
use super::types::{
    module_stem, relative_namespace, CallForm, DeclarationRole, ImportPath, LanguageFamily,
    SyntaxContext, SyntaxGrammarSpec,
};
use crate::ast::syntax::{field, text, ScopeBindings};
use crate::models::{Facts, Node};
use tree_sitter::Node as Syntax;
struct Ruby;
static RUBY: Ruby = Ruby;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("class", DeclarationRole::Class),
        ("module", DeclarationRole::Namespace),
        ("method", DeclarationRole::Method),
        ("singleton_method", DeclarationRole::Method),
        ("lambda", DeclarationRole::Function),
    ],
    references: &["call"],
    call_forms: &[("call", CallForm::Member)],
    scopes: &[
        "program",
        "class",
        "module",
        "method",
        "singleton_method",
        "lambda",
        "block",
        "do_block",
    ],
    imports: &["call"],
    comments: &["comment"],
    typed_bindings: &[
        "optional_parameter",
        "keyword_parameter",
        "splat_parameter",
        "hash_splat_parameter",
        "block_parameter",
    ],
    assignments: &["assignment"],
    member_calls: &["call"],
    heritage: &["superclass"],
    conditionals: &["if", "unless", "while", "until", "for", "case", "when", "rescue"],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Ruby {
    fn id(&self) -> &'static str {
        "ruby"
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["Gemfile", "*.gemspec"]
    }
    fn is_stdlib(&self, module: &str) -> bool {
        let root = module
            .trim()
            .trim_matches(['\'', '"'])
            .split('/')
            .next()
            .unwrap_or("");
        matches!(
            root,
            "abbrev"
                | "base64"
                | "benchmark"
                | "bigdecimal"
                | "cgi"
                | "csv"
                | "date"
                | "digest"
                | "drb"
                | "English"
                | "erb"
                | "etc"
                | "expect"
                | "fcntl"
                | "fiddle"
                | "fileutils"
                | "find"
                | "forwardable"
                | "getoptlong"
                | "io"
                | "ipaddr"
                | "irb"
                | "json"
                | "logger"
                | "matrix"
                | "minitest"
                | "monitor"
                | "mutex_m"
                | "net"
                | "nkf"
                | "objspace"
                | "observer"
                | "open-uri"
                | "open3"
                | "openssl"
                | "optparse"
                | "ostruct"
                | "pathname"
                | "pp"
                | "prettyprint"
                | "prime"
                | "pstore"
                | "psych"
                | "racc"
                | "rdoc"
                | "readline"
                | "reline"
                | "resolv"
                | "rexml"
                | "rinda"
                | "ripper"
                | "rss"
                | "rubygems"
                | "securerandom"
                | "set"
                | "shellwords"
                | "singleton"
                | "socket"
                | "stringio"
                | "strscan"
                | "syslog"
                | "tempfile"
                | "time"
                | "timeout"
                | "tmpdir"
                | "tsort"
                | "unicode_normalize"
                | "uri"
                | "weakref"
                | "webrick"
                | "yaml"
                | "zlib"
        )
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("ruby")
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["rb"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_ruby::language()
    }
    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "class" => Some("class"),
            "module" => Some("namespace"),
            "method" | "singleton_method" => Some("method"),
            "lambda" => Some("function"),
            _ => None,
        }
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "rb"
    }
    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let trimmed = module.trim().trim_matches(['\'', '"']);
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with("./") || trimmed.starts_with("../") {
            let stripped = module_stem(trimmed.trim_end_matches(".rb"));
            return relative_namespace(owner, stripped).map(|namespace| ImportPath {
                namespace,
                relative: true,
                symbol_path: false,
            });
        }
        let normalized = trimmed
            .trim_end_matches(".rb")
            .replace(['/', '\\'], ".")
            .replace("::", ".");
        (!normalized.is_empty()).then(|| ImportPath::absolute(normalized))
    }
    fn class_scope(&self) -> bool {
        true
    }
    fn receiver(&self, name: &str, _owner: &Node) -> bool {
        name == "self"
    }
    fn bindings(&self, node: Syntax<'_>, source: &str) -> ScopeBindings {
        let mut bindings = syntax::bindings(
            self,
            node,
            source,
            &[
                "optional_parameter",
                "keyword_parameter",
                "splat_parameter",
                "hash_splat_parameter",
                "block_parameter",
            ],
            &["assignment"],
        );
        if let Some(params) = node.child_by_field_name("parameters") {
            let mut c = params.walk();
            bindings.all.extend(
                params
                    .named_children(&mut c)
                    .filter(|n| n.kind() == "identifier")
                    .map(|n| text(n, source).into()),
            );
        }
        bindings
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "call" && ctx.node.child_by_field_name("receiver").is_none() {
            let method = field(ctx.node, ctx.source, "method");
            if matches!(method, Some("require" | "require_relative" | "load")) {
                let value = ctx
                    .node
                    .child_by_field_name("arguments")
                    .and_then(|n| n.named_child(0));
                let raw = value.map_or_else(|| text(ctx.node, ctx.source), |n| text(n, ctx.source));
                let literal = value
                    .is_some_and(|n| n.kind() == "string" && !text(n, ctx.source).contains("#{"));
                let path = raw.trim_matches(['\'', '"']);
                let module = if literal {
                    if method == Some("require_relative") {
                        if path.starts_with("./") || path.starts_with("../") {
                            path.to_owned()
                        } else {
                            format!("./{path}")
                        }
                    } else {
                        path.to_owned()
                    }
                } else {
                    raw.into()
                };
                syntax::import(ctx, facts, &module, Some("*".to_owned()));
            }
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() == "call" {
            if let Some(name) = ctx.node.child_by_field_name("method") {
                let receiver = ctx.node.child_by_field_name("receiver");
                let method = text(name, ctx.source);
                if receiver.is_none() && matches!(method, "require" | "require_relative" | "load") {
                    return;
                }
                if let Some(recv) = receiver.filter(|r| r.kind() == "constant" && method == "new") {
                    syntax::call_reference(ctx, facts, text(recv, ctx.source), false);
                    return;
                }
                syntax::member_call(ctx, facts, name, receiver);
            }
        }
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if let Some(base) = ctx.node.child_by_field_name("superclass") {
            syntax::types(ctx, facts, base, "inherits", &[]);
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&RUBY];
