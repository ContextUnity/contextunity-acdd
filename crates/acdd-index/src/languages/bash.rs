use super::profile::LanguageProfile;
use super::support;
use super::types::{CallForm, DeclarationRole, ImportPath, SyntaxContext, SyntaxGrammarSpec};
use crate::ast::syntax::{field, text};
use crate::models::Facts;
use tree_sitter::Node as Syntax;

struct Bash;
static BASH: Bash = Bash;

static SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declaration_roles: &[
        ("function_definition", DeclarationRole::Function),
        ("variable_assignment", DeclarationRole::Variable),
    ],
    references: &["command"],
    call_forms: &[("command", CallForm::Direct)],
    scopes: &["program", "function_definition"],
    imports: &["command"],
    comments: &["comment"],
    assignments: &["variable_assignment"],
    conditionals: &[
        "if_statement",
        "case_statement",
        "for_statement",
        "while_statement",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

const STDLIB_BUILTINS: &[&str] = &[
    ".", ":", "[", "[[", "alias", "bg", "break", "builtin", "caller", "cd", "command", "compgen",
    "complete", "compopt", "continue", "declare", "dirs", "echo", "enable", "eval", "exec",
    "exit", "export", "false", "fg", "getopts", "hash", "help", "jobs", "kill", "let", "local",
    "logout", "mapfile", "popd", "printf", "pushd", "pwd", "read", "readarray", "readonly",
    "return", "set", "shift", "shopt", "source", "suspend", "test", "times", "trap", "true",
    "type", "typeset", "ulimit", "umask", "unalias", "unset", "wait",
];

fn is_top_level_or_exported(node: Syntax<'_>, source: &str) -> bool {
    let mut current = node.parent();
    let mut exported = false;
    while let Some(parent) = current {
        match parent.kind() {
            "function_definition" => return false,
            "declaration_command" if text(parent, source).trim_start().starts_with("export") => {
                exported = true;
            }
            _ => {}
        }
        current = parent.parent();
    }
    exported || node.parent().is_some_and(|p| p.kind() == "program")
}

impl LanguageProfile for Bash {
    fn id(&self) -> &'static str {
        "bash"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["sh", "bash", "zsh"]
    }

    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        tree_sitter_bash::language()
    }

    fn symbol_kind(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "function_definition" => Some("function"),
            "variable_assignment" => Some("variable"),
            _ => None,
        }
    }

    fn symbol_with_source(&self, node: Syntax<'_>, source: &str) -> Option<&'static str> {
        if node.kind() == "variable_assignment" && !is_top_level_or_exported(node, source) {
            return None;
        }
        self.symbol_kind(node.kind())
    }

    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&SPEC)
    }

    fn node_prefix(&self, _kind: &str) -> &'static str {
        "sh"
    }

    fn is_stdlib(&self, module: &str) -> bool {
        STDLIB_BUILTINS.contains(&module)
    }

    fn builtin(&self, name: &str) -> bool {
        self.is_stdlib(name)
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        let trimmed = module.trim().trim_matches(['"', '\'']);
        if trimmed.is_empty()
            || trimmed.starts_with('/')
            || trimmed.contains("://")
            || trimmed.contains('$')
        {
            return None;
        }
        support::relative_file_import(owner, trimmed)
    }

    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if !command_name(ctx).is_some_and(|cmd| matches!(cmd, "source" | ".")) {
            return;
        }
        let Some(arg) = ctx.node.child_by_field_name("argument") else {
            return;
        };
        let raw_path = text(arg, ctx.source).trim().trim_matches(['"', '\'']);
        if raw_path.is_empty() || raw_path.contains('$') || raw_path.contains('`') {
            return;
        }
        ctx.import(
            facts,
            raw_path.to_owned(),
            Some("*".to_owned()),
            Some(raw_path.to_owned()),
        );
    }

    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let Some(cmd) = command_name(ctx).filter(|cmd| !cmd.is_empty() && !matches!(*cmd, "source" | ".")) else {
            return;
        };
        support::push_call_reference(
            ctx,
            facts,
            cmd,
            !cmd.chars().all(|c| c.is_alphanumeric() || "_-.:/".contains(c)),
        );
    }
}

fn command_name<'a>(ctx: &SyntaxContext<'a, '_>) -> Option<&'a str> {
    (ctx.node.kind() == "command")
        .then(|| field(ctx.node, ctx.source, "name").map(str::trim))
        .flatten()
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&BASH];
