use crate::interner::{LinkerInterner, SymbolId};
use crate::languages::{self, LanguageProfile};
#[cfg(feature = "lang-html")]
use crate::linker::html::templates as html_templates;
use crate::linker::{
    routes, semantic_context,
    semantic_facts::{
        emit_route_match, CompositionInput, PreparedSemanticFacts, ENDPOINT_ROUTE_PRIMITIVE,
        NAMED_ROUTE_PRIMITIVE,
    },
};
use crate::models::{
    compact_graph::{CompactCoverage as Coverage, CompactGraph as Graph},
    Facts, Node,
};
use crate::typed_facts::FlowState;
use hashbrown::HashMap;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn resolve_unique_sql_table<'a>(
    expression: &str,
    sql_qualified: &HashMap<String, Vec<&'a Node>>,
    sql_tables: &HashMap<String, Vec<&'a Node>>,
) -> Option<&'a Node> {
    let (full, short, qualified) = languages::sql::identifier_keys(expression);
    let (tables, key) = if qualified {
        (sql_qualified, full)
    } else {
        (sql_tables, short)
    };
    let targets = tables.get(&key)?;
    let mut physical = targets.iter().filter(|node| !node.generated);
    let first = physical.next().copied();
    if first.is_some() && physical.next().is_none() {
        first
    } else if first.is_none() && targets.len() == 1 {
        targets.first().copied()
    } else {
        None
    }
}

/// Composes relations that require facts owned by more than one language.
///
#[allow(clippy::too_many_arguments)]
fn compose_file_framework_relations<'a>(
    path: &'a str,
    facts: &'a Facts,
    flows: Option<&'a HashMap<SymbolId, &'a FlowState>>,
    dependencies: &languages::dependency_registry::DependencyRegistry,
    semantic_context: &semantic_context::Context<'a>,
    interner: &LinkerInterner<'a>,
    by_id: &HashMap<SymbolId, &'a Node>,
    prepared: &PreparedSemanticFacts<'_>,
    sql_qualified: &HashMap<String, Vec<&Node>>,
    sql_tables: &HashMap<String, Vec<&Node>>,
    route_index: &routes::RouteIndex<'_>,
    http_target_contexts: &routes::HttpTargetContexts,
    semantic_targets: &BTreeMap<languages::semantic::SemanticIdentity, BTreeSet<SymbolId>>,
    #[cfg(feature = "lang-html")] render_contexts: &html_templates::RenderContextRegistry<'a>,
    #[cfg(feature = "lang-html")] expressions: &html_templates::ExpressionRegistry<'a>,
) -> Graph {
    let path_arc = std::sync::Arc::<str>::from(path);
    let mut graph = Graph {
        edges: Vec::new(),
        coverage: Vec::new(),
        path_arc: Some(std::sync::Arc::clone(&path_arc)),
        path_pool: hashbrown::HashSet::new(),
        node_id_pool: hashbrown::HashSet::new(),
        evidence_pool: hashbrown::HashSet::new(),
    };
    let has_type_queries = facts
        .references
        .iter()
        .any(|reference| reference.kind == "sql_type_queries_table");
    let macro_rules = has_type_queries.then(|| dependencies.language_context_for_path(path));
    let mut local_types = HashMap::<&str, Option<&Node>>::new();
    let mut parents = HashMap::<&str, &str>::new();
    let mut shadows = std::collections::HashSet::<&str>::new();
    let mut imports = std::collections::HashSet::<&str>::new();
    if has_type_queries {
        shadows.extend(facts.nodes.iter().map(|node| node.name.as_str()));
        imports.extend(
            facts
                .references
                .iter()
                .filter(|reference| reference.kind == "imports")
                .filter_map(|reference| reference.alias.as_deref()),
        );
        for node in facts
            .nodes
            .iter()
            .filter(|node| node.kind == crate::models::tags::NodeKind::Struct)
        {
            local_types
                .entry(node.name.as_str())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(node));
        }
        for edge in facts.edges.iter().filter(|edge| edge.kind == "contains") {
            parents.insert(edge.dst.as_str(), edge.src.as_str());
        }
    }
    for reference in facts.references.iter().filter(|reference| {
        matches!(
            reference.kind.as_str(),
            "defines_table" | "queries_table" | "mutates_table" | "references_table"
        )
    }) {
        if let Some(target) =
            resolve_unique_sql_table(&reference.expression, sql_qualified, sql_tables)
        {
            graph.push_edge(
                path,
                &reference.source,
                &target.id,
                crate::models::tags::EdgeKind::parse(&reference.kind),
                reference.line,
                reference.expression.as_str(),
                crate::models::tags::EdgeConfidence::Exact,
            );
        }
    }
    for reference in facts
        .references
        .iter()
        .filter(|reference| reference.kind == "sql_type_queries_table")
    {
        let Some(type_name) = reference.alias.as_deref() else {
            continue;
        };
        let Some(model) = local_types.get(type_name).and_then(|node| *node) else {
            continue;
        };
        let Some(macro_path) = reference.module.as_deref() else {
            continue;
        };
        if !macro_rules
            .as_ref()
            .and_then(|rules| rules.prepared_projections("rust"))
            .is_some_and(|rules| rules.macro_tables.contains_key(macro_path))
        {
            continue;
        }
        let Some(macro_head) = macro_path.split("::").next() else {
            continue;
        };
        if parents.get(reference.source.as_str()) != parents.get(model.id.as_str())
            || !parents.contains_key(reference.source.as_str())
            || shadows.contains(macro_head)
            || imports.contains(macro_head)
            || imports.contains(type_name)
        {
            continue;
        }
        if let Some(target) =
            resolve_unique_sql_table(&reference.expression, sql_qualified, sql_tables)
        {
            graph.push_edge(
                path,
                &model.id,
                &target.id,
                crate::models::tags::EdgeKind::QueriesTable,
                reference.line,
                reference.expression.as_str(),
                crate::models::tags::EdgeConfidence::Exact,
            );
        }
    }
    for owner in &facts.nodes {
        if let Some(FlowState::Valid { facts: flow, .. }) = interner
            .lookup_symbol(&owner.id)
            .and_then(|id| flows.and_then(|flows| flows.get(&id).copied()))
        {
            super::projections::compose(
                &mut graph,
                owner,
                flow.as_ref(),
                semantic_context,
                by_id,
                dependencies,
            );
        }
    }
    for (line, expression, error) in interner
        .lookup_path(path)
        .and_then(|path| prepared.errors.get(&path))
        .into_iter()
        .flatten()
    {
        graph.coverage.push(Coverage {
            path: std::sync::Arc::clone(&path_arc),
            line: *line,
            expression: expression.clone(),
            status: "unresolved".into(),
            evidence: format!("invalid persisted semantic fact: {error}").into(),
        });
    }
    super::python::semantic_relations::compose_file_relations(
        path,
        by_id,
        dependencies,
        semantic_context,
        prepared,
        semantic_targets,
        &mut graph,
    );
    #[cfg(feature = "lang-html")]
    super::html::relations::emit_render_targets(path, render_contexts, &mut graph);
    emit_file_endpoint_and_route_relations(
        path,
        facts,
        dependencies,
        route_index,
        http_target_contexts,
        &mut graph,
    );
    #[cfg(feature = "lang-html")]
    super::html::relations::emit_request_routes(
        path,
        facts,
        flows,
        interner,
        route_index,
        dependencies,
        &mut graph,
    );
    #[cfg(feature = "lang-html")]
    super::html::relations::emit_template_references(
        path,
        facts,
        expressions,
        render_contexts,
        dependencies,
        &mut graph,
    );
    graph
}

fn emit_file_endpoint_and_route_relations(
    path: &str,
    facts: &Facts,
    dependencies: &languages::dependency_registry::DependencyRegistry,
    route_index: &routes::RouteIndex<'_>,
    http_target_contexts: &routes::HttpTargetContexts,
    graph: &mut Graph,
) {
    let mut file_rules = None;
    let mut unscoped_endpoint_rule = None;
    for reference in facts
        .references
        .iter()
        .filter(|reference| reference.kind == "calls_endpoint")
    {
        let rules = file_rules.get_or_insert_with(|| dependencies.language_context_for_path(path));
        let has_unscoped_endpoint_rule = *unscoped_endpoint_rule.get_or_insert_with(|| {
            rules.syntax_rules.iter().any(|rule| {
                matches!(
                    rule.operation,
                    languages::manifests::syntax::SyntaxOperation::HttpClientCall {
                        target_context: None,
                        ..
                    } | languages::manifests::syntax::SyntaxOperation::HtmlRequestAttributes { .. }
                )
            }) || rules.semantic_rules.iter().any(|rule| {
                rule.active
                    && matches!(
                        rule.rule.operation,
                        languages::manifests::semantic::SemanticOperation::EndpointProjection { .. }
                    )
            })
        });
        let origin = match &reference.receiver_hint {
            Some(crate::models::ReceiverHint::ConfiguredReference { origin }) => {
                let origin_is_configured = rules.syntax_rules.iter().any(|rule| {
                    rule.pack == origin.pack
                        && rule.id == origin.rule
                        && matches!(
                            rule.operation,
                            languages::manifests::syntax::SyntaxOperation::HttpClientCall { .. }
                                | languages::manifests::syntax::SyntaxOperation::HtmlRequestAttributes { .. }
                        )
                }) || rules.semantic_rules.iter().any(|rule| {
                    rule.active
                        && rule.pack == origin.pack
                        && rule.rule.id == origin.rule
                        && matches!(
                            rule.rule.operation,
                            languages::manifests::semantic::SemanticOperation::EndpointProjection { .. }
                        )
                });
                if !origin_is_configured {
                    continue;
                }
                Some(origin)
            }
            _ if has_unscoped_endpoint_rule => None,
            _ => continue,
        };
        let (method, url) = reference
            .expression
            .split_once(' ')
            .unwrap_or(("ANY", &reference.expression));
        if routes::is_remote_client_url(url) {
            graph.reference_coverage(path, reference, "external", "remote HTTP endpoint".into());
            continue;
        }
        let target_context = origin.and_then(|origin| http_target_contexts.get(path, origin));
        emit_route_match(
            graph,
            path,
            &reference.source,
            reference.line,
            ENDPOINT_ROUTE_PRIMITIVE,
            &reference.expression,
            route_index.match_route_in_context(method, url, target_context, path),
            None,
            &reference.expression,
        );
    }
    for reference in facts
        .references
        .iter()
        .filter(|reference| reference.kind == "references_route")
    {
        let Some(crate::models::ReceiverHint::NamedRoute { origin, key: name }) =
            &reference.receiver_hint
        else {
            continue;
        };
        let rules = file_rules.get_or_insert_with(|| dependencies.language_context_for_path(path));
        if !rules.syntax_rules.iter().any(|rule| {
            rule.pack == origin.pack
                && rule.id == origin.rule
                && matches!(
                    rule.operation,
                    languages::manifests::syntax::SyntaxOperation::NamedRouteCall { .. }
                        | languages::manifests::syntax::SyntaxOperation::TemplateNamedRoute { .. }
                )
        }) {
            continue;
        }
        emit_route_match(
            graph,
            path,
            &reference.source,
            reference.line,
            NAMED_ROUTE_PRIMITIVE,
            &reference.expression,
            route_index.match_route_name(name),
            Some(name.as_str()),
            name.as_str(),
        );
    }
}

/// Composes cross-language framework, route, template, and SQL graph relations from indexed facts.
///
/// Route handlers, HTTP clients, HTML requests, and template render contexts
/// are indexed independently by their source profiles. This pass joins those
/// explicit facts after language resolution and returns only graph output.
pub(super) fn compose<'a>(
    all: &'a (dyn crate::FactsLookup + Sync),
    owners: Option<&std::collections::BTreeSet<String>>,
    flows: Option<&'a HashMap<SymbolId, &'a FlowState>>,
    dependencies: &languages::dependency_registry::DependencyRegistry,
    _profiles: &HashMap<&'a str, &'static dyn LanguageProfile>,
    semantic_context: &semantic_context::Context<'a>,
    input: CompositionInput<'a, '_>,
) -> Vec<Graph> {
    let CompositionInput {
        nodes,
        by_id,
        semantic: prepared,
    } = input;
    let interner = prepared.interner;
    let mut sql_tables = HashMap::<String, Vec<&Node>>::new();
    let mut sql_qualified = HashMap::<String, Vec<&Node>>::new();
    for node in nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "table" | "view"))
    {
        let (full, short, _) = languages::sql::identifier_keys(&node.qualname);
        sql_tables.entry(short).or_default().push(node);
        sql_qualified.entry(full).or_default().push(node);
    }
    let route_index = routes::RouteIndex::build(nodes);
    let http_target_contexts = routes::HttpTargetContexts::build(
        all.iter_facts()
            .filter(|(path, _)| owners.is_none_or(|owners| owners.contains(*path)))
            .filter(|(_, source)| {
                source.references.iter().any(|reference| {
                    reference.kind == "calls_endpoint"
                        && matches!(
                            &reference.receiver_hint,
                            Some(crate::models::ReceiverHint::ConfiguredReference { .. })
                        )
                })
            })
            .map(|(path, _)| path),
        dependencies,
    );
    use languages::semantic::{SemanticFactRole, SemanticIdentity};
    let semantic_facts = &prepared.facts;
    let mut semantic_targets = BTreeMap::<SemanticIdentity, BTreeSet<SymbolId>>::new();
    for (&path, decoded) in semantic_facts {
        let path = interner.resolve_path(path);
        for fact in decoded {
            if let SemanticFactRole::FileProvider { target } = &fact.role {
                if let Some(node) = interner.lookup_node(by_id, target.as_str()).filter(|node| {
                    node.kind == crate::models::tags::NodeKind::Module
                        && node.path == path
                        && fact.occurrence.owner_id == node.id
                        && fact
                            .identity
                            .target
                            .paths(path, node.language.as_str())
                            .contains(&fact.identity.path)
                }) {
                    semantic_targets
                        .entry(fact.identity.clone())
                        .or_default()
                        .insert(interner.lookup_symbol(&node.id).expect("catalog target"));
                }
            }
        }
    }
    #[cfg(feature = "lang-html")]
    let render_contexts =
        html_templates::RenderContextRegistry::build(all, flows, semantic_context, dependencies);
    #[cfg(feature = "lang-html")]
    let expressions = html_templates::ExpressionRegistry::build(all);

    let owned_files: Vec<_> = all
        .iter_facts()
        .filter(|(path, _)| owners.is_none_or(|owners| owners.contains(*path)))
        .collect();
    owned_files
        .par_iter()
        .map(|&(path, facts)| {
            compose_file_framework_relations(
                path,
                facts,
                flows,
                dependencies,
                semantic_context,
                interner,
                by_id,
                prepared,
                &sql_qualified,
                &sql_tables,
                &route_index,
                &http_target_contexts,
                &semantic_targets,
                #[cfg(feature = "lang-html")]
                &render_contexts,
                #[cfg(feature = "lang-html")]
                &expressions,
            )
        })
        .collect()
}
