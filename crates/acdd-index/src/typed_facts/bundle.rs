use super::flow_store::FlowStore;
use crate::models::facts::{Facts, PublicNodesView};
use crate::models::semantic::NodeSemanticEntry;
use crate::models::types::{Node, ReceiverHint, Reference};
use crate::semantic::types::{NamedBindingRole, SourcePosition, StaticString, ValueExpr, ValueFlowFacts};
use rayon::prelude::*;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct TypedFacts {
    pub facts: Facts,
    pub flows: FlowStore,
}

fn named_selector_reference(
    node: &Node,
    key: &StaticString,
    kind: &str,
    position: SourcePosition,
    domain: &str,
) -> Reference {
    Reference {
        source: node.id.clone(),
        line: position.line,
        column: position.column,
        expression: match key {
            StaticString::Literal { value } => value.clone(),
            StaticString::Unknown => String::new(),
        },
        kind: kind.to_owned(),
        alias: None,
        module: Some(domain.to_owned()),
        dynamic: false,
        receiver_hint: None,
    }
}

fn normalize_empty_details(node: &mut Node) {
    if node
        .details
        .as_object()
        .is_some_and(serde_json::Map::is_empty)
    {
        node.details = Value::Null;
    }
}

impl TypedFacts {
    /// Project compact selectors into existing indexed reference storage.
    pub fn project_named_selectors(&mut self) {
        for node in &self.facts.nodes {
            for marker in self.flows.named_uncertainties(&node.id) {
                self.facts.references.push(named_selector_reference(
                    node,
                    &marker.key,
                    "semantic_named_definition",
                    SourcePosition {
                        line: node.line,
                        column: 0,
                    },
                    &marker.domain,
                ));
            }
            let Some(flow) = self.flows.get(node) else {
                continue;
            };
            for fact in flow.named_bindings() {
                let kind = if matches!(
                    fact.role,
                    NamedBindingRole::Definition { .. }
                        | NamedBindingRole::RegistryDefinition { .. }
                ) {
                    "semantic_named_definition"
                } else {
                    "semantic_named_reference"
                };
                self.facts.references.push(named_selector_reference(
                    node,
                    &fact.key,
                    kind,
                    fact.position,
                    &fact.domain,
                ));
            }
            for fact in flow.endpoint_requests() {
                let (StaticString::Literal { value: url }, StaticString::Literal { value: method }) =
                    (&fact.url, &fact.method)
                else {
                    continue;
                };
                self.facts.references.push(Reference {
                    source: node.id.clone(),
                    expression: format!("{method} {url}"),
                    kind: "calls_endpoint".into(),
                    line: fact.position.line,
                    column: fact.position.column,
                    alias: None,
                    module: None,
                    dynamic: false,
                    receiver_hint: Some(ReceiverHint::ConfiguredReference {
                        origin: fact.origin.clone(),
                    }),
                });
            }
        }
    }

    pub fn nodes_view(&self) -> PublicNodesView<'_, '_> {
        PublicNodesView::new(&self.facts, &self.flows)
    }

    pub fn push_node(&mut self, mut node: Node) {
        NodeSemanticEntry::ingest(&mut node);
        self.flows.ingest(&mut node);
        self.facts
            .semantic
            .ingest(self.facts.nodes.len(), &mut node);
        normalize_empty_details(&mut node);
        self.facts.nodes.push(node);
    }

    pub fn from_public(mut facts: Facts) -> Self {
        let mut flows = std::mem::take(&mut facts.semantic.flows);
        for (index, node) in facts.nodes.iter_mut().enumerate() {
            NodeSemanticEntry::ingest(node);
            flows.ingest(node);
            facts.semantic.ingest(index, node);
            normalize_empty_details(node);
        }
        facts.semantic.mark_complete();
        facts.pending_flow_nodes.clear();
        Self { facts, flows }
    }

    pub fn into_public(mut self) -> Facts {
        self.facts.semantic.flows = self.flows;
        self.facts.pending_flow_nodes.clear();
        self.facts
    }

    /// Extracts canonical semantic reference keys from typed facts.
    pub fn reference_keys(&self) -> Vec<&str> {
        self.collect_reference_keys(false)
    }

    /// Extracts semantic reference keys from typed facts, optionally including component declarations.
    pub fn collect_reference_keys<'a>(&'a self, include_components: bool) -> Vec<&'a str> {
        let mut keys = Vec::new();
        let mut insert = |expression: &'a str| {
            keys.push(expression);
            for token in expression.split(['.', ':', '/']) {
                if !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    keys.push(token);
                }
            }
        };
        for expression in self
            .facts
            .references
            .iter()
            .filter(|r| !r.dynamic)
            .flat_map(|r| std::iter::once(r.expression.as_str()).chain(r.module.as_deref()))
            .chain(
                self.facts
                    .nodes
                    .iter()
                    .filter(|n| n.kind == crate::models::tags::NodeKind::Impl)
                    .map(|n| n.name.split('<').next().unwrap_or(&n.name).trim()),
            )
            .chain(
                self.facts
                    .nodes
                    .iter()
                    .filter(|n| include_components && n.kind == crate::models::tags::NodeKind::Component)
                    .map(|n| n.name.as_str()),
            )
            .chain(
                self.facts
                    .docs
                    .iter()
                    .flat_map(|d| d.referenced_symbols.iter().map(String::as_str)),
            )
        {
            insert(expression);
        }
        for reference in &self.facts.references {
            if let Some(ReceiverHint::TypedCallResult { value, .. }) = &reference.receiver_hint {
                let mut current = value;
                for _ in 0..=8 {
                    let ValueExpr::Call { callee, site } = current else {
                        break;
                    };
                    insert(callee);
                    let Some(receiver) = site.as_ref().and_then(|site| site.receiver.as_deref())
                    else {
                        break;
                    };
                    current = receiver;
                }
            }
            if let Some(
                ReceiverHint::CallResult { callee, .. }
                | ReceiverHint::ConstructorResult { callee, .. },
            ) = &reference.receiver_hint
            {
                insert(callee);
            }
        }
        for node in &self.facts.nodes {
            if let Some(flow) = self.flows.get(node) {
                flow.reference_keys(&mut insert);
            } else if let Some(raw) = self.flows.raw(node) {
                ValueFlowFacts::reference_keys_from_value(raw, &mut insert);
            } else {
                ValueFlowFacts::reference_keys_from_details(&node.details, &mut insert);
            }
            if let Some(exports) = node.detail_entry(crate::models::details::NodeDetailKey::EXPORTS).as_array() {
                for export in exports {
                    for key in ["local", "module"] {
                        if let Some(name) = export.get(key).and_then(Value::as_str) {
                            insert(name);
                        }
                    }
                }
            }
            for (_, ty) in node.parameter_types() {
                if let Some(ty) = ty {
                    insert(ty);
                }
            }
        }
        if keys.len() >= 8192 {
            keys.par_sort_unstable();
        } else {
            keys.sort_unstable();
        }
        keys.dedup();
        keys
    }
}

impl std::ops::Deref for TypedFacts {
    type Target = Facts;
    fn deref(&self) -> &Facts {
        &self.facts
    }
}

impl std::ops::DerefMut for TypedFacts {
    fn deref_mut(&mut self) -> &mut Facts {
        &mut self.facts
    }
}

impl AsRef<Facts> for TypedFacts {
    fn as_ref(&self) -> &Facts {
        &self.facts
    }
}
