use super::types::SyntaxContext;
#[cfg(any(feature = "lang-bash", feature = "lang-css", feature = "lang-json"))]
use super::types::{module_stem, relative_namespace, ImportPath};
use crate::ast::syntax as ast;
use crate::models::{Facts, Reference};

pub mod value_flow_core;

#[cfg(any(feature = "lang-java", feature = "lang-kotlin"))]
pub(super) fn symbol_import_path(module: &str) -> Option<ImportPath> {
    Some(ImportPath {
        namespace: module.to_owned(),
        relative: false,
        symbol_path: true,
    })
}

#[cfg(any(feature = "lang-bash", feature = "lang-css", feature = "lang-json"))]
pub(super) fn relative_file_import(owner: &str, target: &str) -> Option<ImportPath> {
    let namespace = relative_namespace(owner, module_stem(target))?;
    Some(ImportPath {
        namespace,
        relative: true,
        symbol_path: false,
    })
}

#[cfg(any(
    feature = "lang-bash",
    feature = "lang-cpp",
    feature = "lang-csharp",
    feature = "lang-css",
    feature = "lang-java",
    feature = "lang-json",
    feature = "lang-kotlin",
    feature = "lang-php",
    feature = "lang-ruby"
))]
fn push_reference(
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    kind: &str,
    expression: String,
    dynamic: bool,
) {
    facts.references.push(Reference {
        source: ctx.owner.into(),
        expression,
        kind: kind.into(),
        line: ctx.line(),
        column: ctx.node.start_position().column,
        alias: None,
        module: None,
        dynamic,
        receiver_hint: None,
    });
}

#[cfg(any(feature = "lang-css", feature = "lang-json"))]
pub(super) fn push_static_reference(
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    expression: &str,
) {
    push_reference(ctx, facts, "references", expression.to_owned(), false);
}

#[cfg(any(feature = "lang-csharp", feature = "lang-php"))]
pub(in crate::languages) fn parse_namespace_line(source: &str) -> Option<&str> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("namespace")
            .filter(|rest| rest.chars().next().is_some_and(char::is_whitespace))
        {
            let ns = rest.trim().trim_end_matches([';', '{']).trim();
            if !ns.is_empty() && !ns.chars().any(char::is_whitespace) {
                return Some(ns);
            }
        }
    }
    None
}

#[cfg(any(
    feature = "lang-bash",
    feature = "lang-cpp",
    feature = "lang-csharp",
    feature = "lang-java",
    feature = "lang-kotlin",
    feature = "lang-php",
    feature = "lang-ruby"
))]
pub(in crate::languages) fn push_call_reference(
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    expression: &str,
    dynamic: bool,
) {
    push_reference(
        ctx,
        facts,
        "calls",
        ast::bounded_expression(expression),
        dynamic,
    );
}

#[cfg(any(feature = "lang-toml", feature = "lang-yaml"))]
pub(super) fn empty_bindings() -> ast::ScopeBindings {
    ast::ScopeBindings {
        all: Vec::new(),
        rebindings: Vec::new(),
    }
}

#[cfg(any(feature = "lang-c", feature = "lang-cpp"))]
pub(crate) mod c_family;

#[cfg(any(
    feature = "lang-c",
    feature = "lang-cpp",
    feature = "lang-csharp",
    feature = "lang-java",
    feature = "lang-kotlin",
    feature = "lang-php",
    feature = "lang-ruby"
))]
mod shared {
    use crate::ast::relations;
    use crate::ast::syntax::{text, ScopeBindings};
    use crate::languages::profile::LanguageProfile;
    use crate::languages::types::SyntaxContext;
    use crate::models::Facts;
    use std::collections::BTreeSet;
    use tree_sitter::Node as Syntax;

    pub(in crate::languages) fn child<'tree>(
        node: Syntax<'tree>,
        kinds: &[&str],
    ) -> Option<Syntax<'tree>> {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|n| kinds.contains(&n.kind()));
        found
    }

    #[cfg(any(feature = "lang-java", feature = "lang-kotlin", feature = "lang-php"))]
    pub(in crate::languages) fn named<'a>(
        node: Syntax<'_>,
        source: &'a str,
        kinds: &[&str],
    ) -> Option<&'a str> {
        child(node, kinds).map(|n| text(n, source))
    }

    #[cfg(any(
        feature = "lang-cpp",
        feature = "lang-csharp",
        feature = "lang-java",
        feature = "lang-kotlin",
        feature = "lang-php",
        feature = "lang-ruby"
    ))]
    pub(in crate::languages) fn call_reference(
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        expression: &str,
        dynamic: bool,
    ) {
        let is_dynamic = dynamic
            || !expression
                .chars()
                .all(|c| c.is_alphanumeric() || "_.:$".contains(c));
        super::push_call_reference(ctx, facts, expression, is_dynamic);
    }

    #[cfg(any(
        feature = "lang-cpp",
        feature = "lang-csharp",
        feature = "lang-java",
        feature = "lang-kotlin"
    ))]
    pub(in crate::languages) fn base_constructor_target(raw: &str) -> &str {
        let trimmed = raw.trim();
        trimmed
            .split(['<', '(', ';'])
            .next()
            .map(str::trim)
            .filter(|head| !head.is_empty())
            .unwrap_or(trimmed)
    }

    #[cfg(any(
        feature = "lang-cpp",
        feature = "lang-csharp",
        feature = "lang-java",
        feature = "lang-kotlin"
    ))]
    pub(in crate::languages) fn constructor_call_reference(
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        ty: Syntax<'_>,
    ) {
        call_reference(
            ctx,
            facts,
            base_constructor_target(text(ty, ctx.source)),
            false,
        );
    }

    #[cfg(any(feature = "lang-java", feature = "lang-ruby"))]
    pub(in crate::languages) fn member_call(
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        name: Syntax<'_>,
        receiver: Option<Syntax<'_>>,
    ) {
        let expression = match receiver {
            Some(receiver) => format!("{}.{}", text(receiver, ctx.source), text(name, ctx.source)),
            None => text(name, ctx.source).into(),
        };
        call_reference(ctx, facts, &expression, false);
    }

    pub(in crate::languages) fn import(
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        module: &str,
        alias: Option<String>,
    ) {
        ctx.import(facts, module.into(), alias, Some(module.into()));
    }

    #[cfg(any(
        feature = "lang-cpp",
        feature = "lang-csharp",
        feature = "lang-java",
        feature = "lang-php",
        feature = "lang-ruby"
    ))]
    pub(in crate::languages) fn types(
        ctx: &SyntaxContext<'_, '_>,
        facts: &mut Facts,
        container: Syntax<'_>,
        kind: &str,
        ignored: &[&str],
    ) {
        let mut cursor = container.walk();
        for node in container
            .named_children(&mut cursor)
            .filter(|n| !ignored.contains(&n.kind()))
        {
            relations::reference(
                facts,
                ctx.owner,
                relations::type_name(node, ctx.source),
                kind,
                ctx.line(),
            );
        }
    }

    pub(in crate::languages) fn declarator(node: Syntax<'_>) -> Syntax<'_> {
        let mut current = node;
        while let Some(inner) = current.child_by_field_name("declarator").or_else(|| {
            (current.kind() == "parenthesized_declarator")
                .then(|| current.named_child(0))
                .flatten()
        }) {
            current = inner;
        }
        current
    }

    pub(in crate::languages) fn bindings(
        profile: &dyn LanguageProfile,
        root: Syntax<'_>,
        source: &str,
        parameters: &[&str],
        locals: &[&str],
    ) -> ScopeBindings {
        let mut bound = BTreeSet::new();
        let mut rebindings = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node != root && profile.symbol(node).is_some() {
                continue;
            }
            let parameter = parameters.contains(&node.kind());
            if parameter || locals.contains(&node.kind()) {
                let mut cursor = node.walk();
                let mut names: Vec<_> = node
                    .children_by_field_name("declarator", &mut cursor)
                    .map(declarator)
                    .collect();
                if names.is_empty() {
                    names.extend(
                        node.child_by_field_name("name")
                            .or_else(|| node.child_by_field_name("left"))
                            .or_else(|| {
                                child(node, &["simple_identifier", "identifier", "variable_name"])
                            }),
                    );
                }
                for name in names {
                    let name = text(name, source).to_owned();
                    bound.insert(name.clone());
                    if !parameter {
                        rebindings.insert(name);
                    }
                }
            }
            let mut c = node.walk();
            stack.extend(node.named_children(&mut c));
        }
        ScopeBindings {
            all: bound.into_iter().collect(),
            rebindings: rebindings.into_iter().collect(),
        }
    }
}

#[cfg(any(
    feature = "lang-c",
    feature = "lang-cpp",
    feature = "lang-csharp",
    feature = "lang-java",
    feature = "lang-kotlin",
    feature = "lang-php",
    feature = "lang-ruby"
))]
pub(super) use shared::*;
