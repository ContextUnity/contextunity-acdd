//! Single-pass AST parsing and tree extraction helpers.

use super::syntax::ScopeBindings;
use super::visitor;
use crate::languages::{
    context::LanguageContext,
    dependency_registry::DependencyRegistry,
    profile::{LanguageProfile, TreeExtractor},
    routes, types,
};
use crate::models::{details::NodeDetailKey, tags::NodeKind, Facts};
use crate::typed_facts::{FlowStore, TypedFacts};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tree_sitter::{Node as Syntax, Tree};

pub(crate) use crate::languages::profile::TreeExtractArgs as TreeExtractContext;

pub(crate) fn compact_node_details(facts: &mut Facts, path: &str) {
    let mut declarations_per_line = HashMap::new();
    for node in facts
        .nodes
        .iter()
        .filter(|node| node.path == path && node.kind != NodeKind::Module)
    {
        *declarations_per_line.entry(node.line).or_insert(0usize) += 1;
    }
    for node in facts.nodes.iter_mut().filter(|node| node.path == path) {
        let empty = if let Some(details) = node.details.as_object_mut() {
            details.retain(|key, value| match key.as_str() {
                "value_flow" => !value.is_null(),
                "bindings" | "rebindings" => {
                    !value.as_array().is_some_and(|values| values.is_empty())
                }
                k if k == NodeDetailKey::ASYNC
                    || k == NodeDetailKey::DEFAULT_EXPORT
                    || k == "is_method"
                    || k == "is_public" =>
                {
                    value != &serde_json::Value::Bool(false)
                }
                "is_static" => value != &serde_json::Value::Bool(false),
                _ => true,
            });
            let has_other_details = details.keys().any(|key| key != "column");
            let keep_column = !matches!(
                node.kind.as_str(),
                k if k == NodeKind::Function.as_str() || k == NodeKind::Method.as_str()
            ) || has_other_details
                || details
                    .get("column")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|column| column > 0)
                || declarations_per_line
                    .get(&node.line)
                    .is_some_and(|count| *count > 1);
            if !keep_column {
                details.remove("column");
            }
            details.is_empty()
        } else {
            false
        };
        if empty && node.native_semantic.is_none() {
            node.details = serde_json::Value::Null;
        }
    }
}

pub(crate) fn clear_direct_module_markers(facts: &mut Facts) {
    for node in &mut facts.nodes {
        if let Some(details) = node.details.as_object_mut() {
            details.remove("entrypoint_direct_module");
        }
    }
}

pub(crate) fn extract_tree(
    profile: &dyn LanguageProfile,
    root: Syntax<'_>,
    path: &str,
    source: &str,
    module: &str,
    facts: &mut Facts,
) {
    let registry = DependencyRegistry::bundled_framework_rules();
    let language_context = registry.language_context_for_path(path);
    extract_tree_with_language_context(
        TreeExtractContext {
            profile,
            root,
            path,
            source,
            module,
            owner: &format!("module:{path}"),
            language_context: &language_context,
        },
        facts,
        None,
    );
    if profile.id() == "python" {
        clear_direct_module_markers(facts);
    }
    facts.semantic.mark_complete();
    compact_node_details(facts, path);
}

pub(crate) fn extract_tree_with_language_context(
    ctx: TreeExtractContext<'_, '_>,
    facts: &mut Facts,
    flows: Option<&mut FlowStore>,
) -> ScopeBindings {
    let rule_ir = ctx.language_context.bind_rule_ir(&ctx.root.language());
    let route_context = if ctx.profile.route_extraction_enabled() {
        routes::RouteContext::prepare(ctx.root, ctx.source, ctx.language_context)
    } else {
        routes::RouteContext::default()
    };
    visitor::SinglePassExtractor {
        source: ctx.source,
        path: ctx.path,
        profile: ctx.profile,
        language_context: ctx.language_context,
        rule_ir,
        route_context,
        file: ctx
            .profile
            .prepare_with_context(ctx.root, ctx.source, ctx.language_context),
        module: ctx.module,
        offset: 0,
        symbols: HashMap::new(),
        ids: facts.nodes.iter().map(|n| n.id.clone()).collect(),
        flows,
    }
    .visit(ctx.root, &mut Vec::new(), ctx.owner, facts)
}

fn parser_slot(profile_id: &'static str, path: &str) -> &'static str {
    match profile_id {
        "python" => "python",
        "rust" => "rust",
        "html" => "html",
        "javascript" => "javascript",
        "vue" | "typescript" if path.ends_with(".tsx") => "typescript-tsx",
        "vue" | "typescript"
            if path.ends_with(".js")
                || path.ends_with(".jsx")
                || path.ends_with(".mjs")
                || path.ends_with(".cjs") =>
        {
            "javascript"
        }
        "vue" | "typescript" => "typescript",
        active_profile => active_profile,
    }
}

fn validate_profile(profile: &dyn LanguageProfile) -> Result<()> {
    anyhow::ensure!(
        crate::models::tags::LanguageId::parse(profile.id()).is_some(),
        "unsupported language profile: {}",
        profile.id()
    );
    Ok(())
}

pub(super) fn parse_tree(profile: &dyn LanguageProfile, path: &str, source: &str) -> Result<Tree> {
    validate_profile(profile)?;
    let grammar = profile.grammar(path);
    if let Some(spec) = profile.syntax_grammar_spec() {
        spec.compile_cached(&grammar)?;
    }
    types::with_warm_parser(parser_slot(profile.id(), path), grammar, |parser| {
        parser.parse(source, None)
    })?
    .context("Tree-sitter parse cancelled")
}

/// Parses one source file into indexed facts.
pub fn parse_file(
    profile: &dyn LanguageProfile,
    path: &str,
    source: &str,
    module: &str,
    facts: &mut Facts,
) -> Result<()> {
    validate_profile(profile)?;
    match profile.id() {
        #[cfg(feature = "lang-html")]
        "html" => parse_embedded_file(
            path,
            source,
            module,
            facts,
            crate::languages::html::extract_typed_with_context,
        ),
        #[cfg(feature = "lang-vue")]
        "vue" if source.contains("<script") || source.contains("<template") => parse_embedded_file(
            path,
            source,
            module,
            facts,
            crate::languages::vue::extract_typed,
        ),
        _ if !profile.has_grammar() => profile.extract_file(path, source, module, facts),
        _ => {
            let tree = parse_tree(profile, path, source)?;
            extract_tree(profile, tree.root_node(), path, source, module, facts);
            Ok(())
        }
    }
}

#[cfg(any(feature = "lang-html", feature = "lang-vue"))]
type EmbeddedExtractor =
    fn(&str, &str, &str, &mut TypedFacts, &Arc<LanguageContext>, TreeExtractor) -> Result<()>;

#[cfg(any(feature = "lang-html", feature = "lang-vue"))]
fn parse_embedded_file(
    path: &str,
    source: &str,
    module: &str,
    facts: &mut Facts,
    extract_fn: EmbeddedExtractor,
) -> Result<()> {
    let registry = DependencyRegistry::bundled_framework_rules();
    let rules = registry.language_context_for_path(path);
    let mut typed = TypedFacts {
        facts: std::mem::take(facts),
        flows: FlowStore::default(),
    };
    extract_fn(
        path,
        source,
        module,
        &mut typed,
        &rules,
        extract_tree_with_language_context,
    )?;
    typed.facts.semantic.mark_complete();
    compact_node_details(&mut typed.facts, path);
    *facts = typed.into_public();
    Ok(())
}

pub(crate) fn parse_file_typed(
    profile: &dyn LanguageProfile,
    path: &str,
    source: &str,
    module: &str,
    typed: &mut TypedFacts,
    language_context: &Arc<LanguageContext>,
) -> Result<()> {
    let tree = parse_tree(profile, path, source)?;
    extract_tree_with_language_context(
        TreeExtractContext {
            profile,
            root: tree.root_node(),
            path,
            source,
            module,
            owner: &format!("module:{path}"),
            language_context,
        },
        &mut typed.facts,
        Some(&mut typed.flows),
    );
    typed.project_named_selectors();
    Ok(())
}
