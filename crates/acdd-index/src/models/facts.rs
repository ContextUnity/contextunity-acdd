use super::types::{CallableFingerprints, Diagnostic, DocSection, Edge, Node, Reference};
use crate::typed_facts::flow_store::FlowStore;
use crate::typed_facts::node_view::NodeView;
use crate::typed_facts::semantic_store::SemanticStore;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(transparent)]
pub struct PublicNodesView<'node, 'flow>(Vec<NodeView<'node, 'flow>>);

impl<'node, 'flow> PublicNodesView<'node, 'flow> {
    pub fn new(facts: &'node Facts, flows: &'flow FlowStore) -> Self {
        Self(
            facts
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| NodeView::new(node, flows, facts.semantic.state(index)))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
/// Extracted per-file facts before cross-file linking and persistence.
pub struct Facts {
    #[serde(skip)]
    #[doc(hidden)]
    pub semantic: SemanticStore,
    #[serde(skip)]
    #[doc(hidden)]
    pub pending_flow_nodes: Vec<usize>,
    /// Declarations found in the file.
    pub nodes: Vec<Node>,
    /// Relationships resolved within the file.
    pub edges: Vec<Edge>,
    /// References requiring linker resolution.
    pub references: Vec<Reference>,
    /// Documentation sections extracted from the file.
    pub docs: Vec<DocSection>,
    /// Extraction diagnostics.
    pub errors: Vec<Diagnostic>,
    /// Per-file syntax activation signals observed during extraction.
    #[serde(
        default,
        skip_serializing_if = "crate::languages::manifests::semantic::FileActivationSignals::is_empty"
    )]
    pub activation_signals: crate::languages::manifests::semantic::FileActivationSignals,
    /// Per-callable normalized AST statement fingerprints for structural clone detection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statement_fingerprints: Vec<CallableFingerprints>,
}

impl Serialize for Facts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 5
            + usize::from(!self.activation_signals.is_empty())
            + usize::from(!self.statement_fingerprints.is_empty());
        let mut state = serializer.serialize_struct("Facts", field_count)?;
        state.serialize_field("nodes", &PublicNodesView::new(self, &self.semantic.flows))?;
        state.serialize_field("edges", &self.edges)?;
        state.serialize_field("references", &self.references)?;
        state.serialize_field("docs", &self.docs)?;
        state.serialize_field("errors", &self.errors)?;
        if !self.activation_signals.is_empty() {
            state.serialize_field("activation_signals", &self.activation_signals)?;
        }
        if !self.statement_fingerprints.is_empty() {
            state.serialize_field("statement_fingerprints", &self.statement_fingerprints)?;
        }
        state.end()
    }
}

impl Facts {
    pub fn retain_nodes(&mut self, keep: impl FnMut(&Node) -> bool) {
        let tracked: std::collections::HashSet<_> = self
            .pending_flow_nodes
            .iter()
            .copied()
            .chain(self.semantic.entries().map(|(i, _)| *i))
            .collect();
        if tracked.is_empty() {
            self.nodes.retain(keep);
            let ids: std::collections::HashSet<String> =
                self.nodes.iter().map(|node| node.id.clone()).collect();
            self.statement_fingerprints
                .retain(|entry| ids.contains(&entry.node_id));
            self.semantic.flows.retain_nodes(&ids);
            return;
        }
        let mut mapping = std::collections::HashMap::new();
        let (mut old, mut new) = (0, 0);
        let mut keep = keep;
        self.nodes.retain(|node| {
            let retained = keep(node);
            if retained {
                if tracked.contains(&old) {
                    mapping.insert(old, new);
                }
                new += 1;
            }
            old += 1;
            retained
        });
        self.pending_flow_nodes.retain_mut(|index| {
            if let Some(new) = mapping.get(index) {
                *index = *new;
                true
            } else {
                false
            }
        });
        self.semantic.retain_remapped(&mapping);
        let ids: std::collections::HashSet<String> =
            self.nodes.iter().map(|node| node.id.clone()).collect();
        self.statement_fingerprints
            .retain(|entry| ids.contains(&entry.node_id));
        self.semantic.flows.retain_nodes(&ids);
    }

    pub fn remap_deferred_nodes(&mut self, base: usize, batches: &[(usize, usize)]) {
        let mut mapping = std::collections::HashMap::new();
        for old in self
            .pending_flow_nodes
            .iter()
            .copied()
            .chain(self.semantic.entries().map(|(i, _)| *i))
        {
            let mut appended = 0;
            let mut mapped = None;
            for &(frontier, count) in batches {
                if old < base {
                    if frontier <= old {
                        appended += count;
                    }
                } else if old - base >= appended && old - base < appended + count {
                    mapped = Some(frontier + appended + (old - base - appended));
                    break;
                } else {
                    appended += count;
                }
            }
            mapping.insert(
                old,
                mapped.unwrap_or(old + if old < base { appended } else { 0 }),
            );
        }
        self.remap_node_order(&mapping);
    }

    pub fn remap_node_order(&mut self, old_to_new: &std::collections::HashMap<usize, usize>) {
        for index in &mut self.pending_flow_nodes {
            if let Some(new) = old_to_new.get(index) {
                *index = *new;
            }
        }
        self.semantic.remap_order(old_to_new);
    }
}

impl AsRef<Facts> for Facts {
    fn as_ref(&self) -> &Facts {
        self
    }
}

/// Object-safe read view over a path-keyed catalog of file [`Facts`], allowing
/// internal linker passes to compile once rather than monomorphizing per `AsRef<Facts>` type.
pub trait FactsLookup {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn get_facts(&self, path: &str) -> Option<&Facts>;
    fn get_entry<'a>(&'a self, path: &str) -> Option<(&'a str, &'a Facts)>;
    fn contains_facts(&self, path: &str) -> bool {
        self.get_facts(path).is_some()
    }
    fn iter_facts<'a>(&'a self) -> Box<dyn Iterator<Item = (&'a str, &'a Facts)> + 'a>;
    fn values_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Facts> + 'a>;
    fn keys_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a>;
}

impl<F: AsRef<Facts>> FactsLookup for std::collections::BTreeMap<String, F> {
    fn len(&self) -> usize {
        std::collections::BTreeMap::len(self)
    }

    fn get_facts(&self, path: &str) -> Option<&Facts> {
        self.get(path).map(AsRef::as_ref)
    }

    fn get_entry<'a>(&'a self, path: &str) -> Option<(&'a str, &'a Facts)> {
        self.get_key_value(path)
            .map(|(key, facts)| (key.as_str(), facts.as_ref()))
    }

    fn contains_facts(&self, path: &str) -> bool {
        self.contains_key(path)
    }

    fn iter_facts<'a>(&'a self) -> Box<dyn Iterator<Item = (&'a str, &'a Facts)> + 'a> {
        Box::new(
            self.iter()
                .map(|(path, facts)| (path.as_str(), facts.as_ref())),
        )
    }

    fn values_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Facts> + 'a> {
        Box::new(self.values().map(AsRef::as_ref))
    }

    fn keys_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        Box::new(self.keys().map(String::as_str))
    }
}

impl FactsLookup for std::collections::BTreeMap<&str, &Facts> {
    fn len(&self) -> usize {
        std::collections::BTreeMap::len(self)
    }

    fn get_facts(&self, path: &str) -> Option<&Facts> {
        self.get(path).copied()
    }

    fn get_entry<'a>(&'a self, path: &str) -> Option<(&'a str, &'a Facts)> {
        self.get_key_value(path).map(|(&key, &facts)| (key, facts))
    }

    fn contains_facts(&self, path: &str) -> bool {
        self.contains_key(path)
    }

    fn iter_facts<'a>(&'a self) -> Box<dyn Iterator<Item = (&'a str, &'a Facts)> + 'a> {
        Box::new(self.iter().map(|(&path, &facts)| (path, facts)))
    }

    fn values_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Facts> + 'a> {
        Box::new(self.values().copied())
    }

    fn keys_facts<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        Box::new(self.keys().copied())
    }
}
