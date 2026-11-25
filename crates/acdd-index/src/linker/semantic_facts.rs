use crate::interner::{LinkerInterner, PathId, SymbolId};
use crate::languages::{
    dependency_registry::DependencyRegistry,
    semantic::{self, SemanticFact, SemanticRelation},
    types::{workspace_path, LanguageFamily},
};
use crate::models::{compact_graph::CompactGraph as Graph, Node};
use hashbrown::HashMap;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct PreparedSemanticFacts<'a> {
    pub(crate) interner: &'a LinkerInterner<'a>,
    pub(crate) facts: BTreeMap<PathId, Vec<std::borrow::Cow<'a, SemanticFact>>>,
    pub(crate) errors: BTreeMap<PathId, Vec<(usize, String, String)>>,
}

/// Canonical declarative framework rule action model.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DeclarativeRulePrimitive {
    /// Emits a route or endpoint match edge and coverage entry (`EmitRoute`).
    EmitRoute {
        kind: crate::models::tags::EdgeKind,
        resolved_prefix: &'static str,
        unmatched_prefix: &'static str,
        ambiguous_prefix: &'static str,
    },
    /// Emits a declarative semantic occurrence edge (`EmitEdge`).
    EmitEdge { evidence: &'static str },
}

type OccurrenceKey = (SymbolId, usize, usize, SemanticRelation);
pub(in crate::linker) type OccurrenceValue = (SymbolId, BTreeSet<SymbolId>, bool, bool);

#[derive(Default)]
pub(in crate::linker) struct OccurrenceTable {
    entries: BTreeMap<OccurrenceKey, OccurrenceValue>,
}

impl OccurrenceTable {
    pub(in crate::linker) fn entry(
        &mut self,
        owner: SymbolId,
        line: usize,
        column: usize,
        relation: SemanticRelation,
        expression: &str,
        interner: &LinkerInterner<'_>,
    ) -> &mut OccurrenceValue {
        let expression = interner
            .lookup_symbol(expression)
            .expect("semantic occurrence expression");
        self.entries
            .entry((owner, line, column, relation))
            .or_insert_with(|| (expression, BTreeSet::new(), false, false))
    }

    pub(in crate::linker) fn emit_all(
        self,
        graph: &mut Graph,
        path: &str,
        primitive: DeclarativeRulePrimitive,
        interner: &LinkerInterner<'_>,
    ) {
        let DeclarativeRulePrimitive::EmitEdge { evidence } = primitive else {
            return;
        };
        for ((owner, line, _, relation), (expression, targets, ambiguous, unresolved)) in
            self.entries
        {
            emit_occurrence(
                graph,
                path,
                owner,
                line,
                relation.as_str(),
                interner.resolve_symbol(expression).to_owned(),
                targets,
                ambiguous,
                unresolved,
                evidence,
                interner,
            );
        }
    }
}

pub(super) struct CompositionInput<'a, 'lookup> {
    pub(super) nodes: &'lookup [&'a Node],
    pub(super) by_id: &'lookup HashMap<SymbolId, &'a Node>,
    pub(super) semantic: &'lookup PreparedSemanticFacts<'a>,
}

fn extend_semantic_state<'a>(
    state: &'a crate::typed_facts::SemanticState,
    line: usize,
    qualname: &str,
    facts: &mut Vec<std::borrow::Cow<'a, SemanticFact>>,
    errors: &mut Vec<(usize, String, String)>,
) {
    match state {
        crate::typed_facts::SemanticState::Valid { facts: values, .. } => {
            facts.extend(values.iter().map(std::borrow::Cow::Borrowed));
        }
        crate::typed_facts::SemanticState::Invalid { error, .. } => {
            errors.push((line, qualname.to_owned(), error.clone()));
        }
    }
}

impl<'a> PreparedSemanticFacts<'a> {
    pub(super) fn build(
        all: &'a (dyn crate::FactsLookup + Sync),
        interner: &'a LinkerInterner<'a>,
    ) -> Self {
        let entries: Vec<_> = all.iter_facts().collect();
        let files: Vec<_> = entries
            .par_iter()
            .filter_map(|&(path, source)| {
                let mut facts = Vec::new();
                let mut errors = Vec::new();
                if source.semantic.is_complete() {
                    for (_, entry) in source.semantic.entries() {
                        extend_semantic_state(
                            &entry.state,
                            entry.line,
                            &entry.qualname,
                            &mut facts,
                            &mut errors,
                        );
                    }
                } else {
                    for (index, node) in source.nodes.iter().enumerate() {
                        if let Some(state) = source.semantic.state(index) {
                            extend_semantic_state(
                                state,
                                node.line,
                                &node.qualname,
                                &mut facts,
                                &mut errors,
                            );
                            continue;
                        }
                        match semantic::decode(node) {
                            Ok(values) => {
                                facts.extend(values.into_iter().map(std::borrow::Cow::Owned))
                            }
                            Err(error) => {
                                errors.push((node.line, node.qualname.clone(), error.to_string()))
                            }
                        }
                    }
                }
                (!facts.is_empty() || !errors.is_empty()).then(|| {
                    (
                        interner.lookup_path(path).expect("catalog path"),
                        facts,
                        errors,
                    )
                })
            })
            .collect();
        let mut prepared = Self {
            interner,
            facts: BTreeMap::new(),
            errors: BTreeMap::new(),
        };
        for (path, facts, errors) in files {
            if !facts.is_empty() {
                prepared.facts.insert(path, facts);
            }
            if !errors.is_empty() {
                prepared.errors.insert(path, errors);
            }
        }
        prepared
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_occurrence(
    graph: &mut Graph,
    path: &str,
    owner_id: SymbolId,
    line: usize,
    kind: &str,
    expression: String,
    targets: BTreeSet<SymbolId>,
    ambiguous: bool,
    unresolved: bool,
    evidence: &'static str,
    interner: &LinkerInterner<'_>,
) {
    let status = if ambiguous || targets.len() > 1 {
        "ambiguous"
    } else if unresolved || targets.is_empty() {
        "unresolved"
    } else {
        "resolved"
    };
    if status == "resolved" {
        graph.push_edge(
            path,
            interner.resolve_symbol(owner_id),
            interner.resolve_symbol(targets.into_iter().next().expect("resolved target")),
            crate::models::tags::EdgeKind::parse(kind),
            line,
            expression.as_str(),
            crate::models::tags::EdgeConfidence::Exact,
        );
    }
    graph.push_coverage(path, line, expression, status, evidence);
}

pub(in crate::linker) const ENDPOINT_ROUTE_PRIMITIVE: DeclarativeRulePrimitive =
    DeclarativeRulePrimitive::EmitRoute {
        kind: crate::models::tags::EdgeKind::CallsEndpoint,
        resolved_prefix: "endpoint route",
        unmatched_prefix: "unmatched endpoint route",
        ambiguous_prefix: "ambiguous endpoint route",
    };

pub(super) const NAMED_ROUTE_PRIMITIVE: DeclarativeRulePrimitive =
    DeclarativeRulePrimitive::EmitRoute {
        kind: crate::models::tags::EdgeKind::ReferencesRoute,
        resolved_prefix: "unique route name",
        unmatched_prefix: "unmatched route name",
        ambiguous_prefix: "ambiguous route name",
    };

#[allow(clippy::too_many_arguments)]
pub(in crate::linker) fn emit_route_match<E>(
    graph: &mut Graph,
    path: &str,
    src: &str,
    line: usize,
    primitive: DeclarativeRulePrimitive,
    expression: &str,
    matched: Result<Option<&Node>, E>,
    resolved_value: Option<&str>,
    error_value: &str,
) {
    let DeclarativeRulePrimitive::EmitRoute {
        kind,
        resolved_prefix,
        unmatched_prefix,
        ambiguous_prefix,
    } = primitive
    else {
        return;
    };
    let (status, evidence) = match matched {
        Ok(Some(route)) => {
            graph.push_edge(
                path,
                src,
                route.id.clone(),
                kind,
                line,
                expression,
                crate::models::tags::EdgeConfidence::Exact,
            );
            (
                "resolved",
                format!(
                    "{resolved_prefix}: {}",
                    resolved_value.unwrap_or(&route.name)
                ),
            )
        }
        Ok(None) => ("unresolved", format!("{unmatched_prefix}: {error_value}")),
        Err(_) => ("ambiguous", format!("{ambiguous_prefix}: {error_value}")),
    };
    graph.push_coverage(path, line, expression, status, evidence);
}

pub(in crate::linker) fn project_scope(
    path: &str,
    family: LanguageFamily,
    dependencies: &DependencyRegistry,
) -> String {
    dependencies
        .nearest_manifest_scope_for_path(family, path)
        .unwrap_or_else(|| workspace_path(path).0)
        .to_owned()
}
