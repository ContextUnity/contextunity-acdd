use super::flow_store::{remap_identifier, FlowStore};
use crate::models::types::Node;
use crate::semantic::relations::{SemanticFact, SemanticFactRole};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SemanticStateView<'a> {
    Raw(&'a Value),
    Facts(&'a [SemanticFact]),
}

impl<'a> From<&'a SemanticState> for SemanticStateView<'a> {
    fn from(state: &'a SemanticState) -> Self {
        match state {
            SemanticState::Valid {
                original: Some(value),
                ..
            }
            | SemanticState::Invalid {
                original: value, ..
            } => Self::Raw(value),
            SemanticState::Valid { facts, .. } => Self::Facts(facts),
        }
    }
}

impl SemanticState {
    pub fn view(&self) -> impl Serialize + '_ {
        SemanticStateView::from(self)
    }
}

#[derive(Clone, Debug)]
pub enum SemanticState {
    Valid {
        facts: Vec<SemanticFact>,
        original: Option<Value>,
    },
    Invalid {
        original: Value,
        error: String,
    },
}

#[derive(Clone, Debug)]
pub struct SemanticEntry {
    pub owner: String,
    pub line: usize,
    pub qualname: String,
    pub state: SemanticState,
}

#[derive(Clone, Debug, Default)]
#[doc(hidden)]
pub struct SemanticStore {
    entries: std::collections::BTreeMap<usize, SemanticEntry>,
    owners: HashMap<String, usize>,
    complete: bool,
    pub flows: FlowStore,
}

fn remap_semantic_fact(
    fact: &mut SemanticFact,
    ids: &HashMap<String, String>,
    original: &mut Option<Value>,
) {
    let mut changed = remap_identifier(&mut fact.occurrence.owner_id, ids);
    changed |= match &mut fact.role {
        SemanticFactRole::FileProvider { target } => remap_identifier(target, ids),
        SemanticFactRole::CallToFile { import_owner, .. } => remap_identifier(import_owner, ids),
        _ => false,
    };
    if changed {
        *original = None;
    }
}

impl SemanticStore {
    fn decode(value: Value) -> SemanticState {
        match serde_json::from_value(value.clone()) {
            Ok(facts) => SemanticState::Valid {
                facts,
                original: Some(value),
            },
            Err(error) => SemanticState::Invalid {
                original: value,
                error: error.to_string(),
            },
        }
    }
    pub fn ingest(&mut self, index: usize, node: &mut Node) {
        let Some(value) = node
            .details
            .as_object_mut()
            .and_then(|details| details.remove("semantic_facts"))
        else {
            return;
        };
        // Native entries already own this reserved slot.
        if self.entries.contains_key(&index) {
            return;
        }
        let state = Self::decode(value);
        self.owners.entry(node.id.clone()).or_insert(index);
        self.entries.insert(
            index,
            SemanticEntry {
                owner: node.id.clone(),
                line: node.line,
                qualname: node.qualname.clone(),
                state,
            },
        );
    }
    pub fn push(&mut self, nodes: &mut [Node], fact: SemanticFact) -> bool {
        let owner = &fact.occurrence.owner_id;
        let index = self
            .owners
            .get(owner)
            .copied()
            .filter(|i| nodes.get(*i).is_some_and(|n| n.id == *owner))
            .or_else(|| nodes.iter().position(|node| node.id == *owner));
        let Some(index) = index else {
            return false;
        };
        if !self.entries.contains_key(&index) {
            if let Some(details) = nodes[index].details.as_object_mut() {
                if details.get("semantic_facts").is_none_or(Value::is_null) {
                    details.remove("semantic_facts");
                }
            }
        }
        self.ingest(index, &mut nodes[index]);
        if let Some(entry) = self.entries.get_mut(&index) {
            if matches!(&entry.state,SemanticState::Invalid{original,..} if original.is_null()) {
                entry.state = SemanticState::Valid {
                    facts: Vec::new(),
                    original: None,
                };
            }
        }
        let entry = self.entries.entry(index).or_insert_with(|| {
            let node = &nodes[index];
            SemanticEntry {
                owner: node.id.clone(),
                line: node.line,
                qualname: node.qualname.clone(),
                state: SemanticState::Valid {
                    facts: Vec::new(),
                    original: None,
                },
            }
        });
        self.owners.entry(owner.clone()).or_insert(index);
        match &mut entry.state {
            SemanticState::Valid { facts, original } => {
                if let Some(original) = original {
                    let Some(values) = original.as_array_mut() else {
                        return false;
                    };
                    values.push(serde_json::to_value(&fact).expect("semantic DTO serializes"));
                }
                facts.push(fact);
                true
            }
            SemanticState::Invalid { original, error } => {
                let Some(values) = original.as_array_mut() else {
                    return false;
                };
                values.push(serde_json::to_value(fact).expect("semantic DTO serializes"));
                if let SemanticState::Invalid { error: new, .. } = Self::decode(original.clone()) {
                    *error = new;
                }
                true
            }
        }
    }
    pub fn state(&self, index: usize) -> Option<&SemanticState> {
        self.entries.get(&index).map(|entry| &entry.state)
    }
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&usize, &SemanticEntry)> {
        self.entries.iter()
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn mark_complete(&mut self) {
        self.complete = true;
    }
    pub fn restore_cache_entry(&mut self, index: usize, entry: SemanticEntry) {
        self.owners.entry(entry.owner.clone()).or_insert(index);
        self.entries.insert(index, entry);
    }
    pub fn remap_order(&mut self, map: &HashMap<usize, usize>) {
        self.entries = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|(i, e)| (map.get(&i).copied().unwrap_or(i), e))
            .collect();
        for index in self.owners.values_mut() {
            if let Some(new) = map.get(index) {
                *index = *new;
            }
        }
    }
    pub fn retain_remapped(&mut self, map: &HashMap<usize, usize>) {
        self.entries.retain(|index, _| map.contains_key(index));
        self.owners.retain(|_, index| map.contains_key(index));
        self.remap_order(map);
    }
    pub fn append_remapped(
        &mut self,
        other: &mut Self,
        map: &HashMap<usize, usize>,
        ids: &HashMap<String, String>,
    ) {
        for (index, mut entry) in std::mem::take(&mut other.entries) {
            let Some(&index) = map.get(&index) else {
                continue;
            };
            if let Some(id) = ids.get(&entry.owner) {
                entry.owner.clone_from(id);
            }
            if let SemanticState::Valid { facts, original } = &mut entry.state {
                for fact in facts.iter_mut() {
                    remap_semantic_fact(fact, ids, original);
                }
            }
            if let Some(previous) = self.entries.get_mut(&index) {
                if let (
                    SemanticState::Valid {
                        facts: previous,
                        original: None,
                    },
                    SemanticState::Valid {
                        facts: incoming,
                        original: None,
                    },
                ) = (&mut previous.state, &mut entry.state)
                {
                    previous.append(incoming);
                    continue;
                }
                let mut raw =
                    serde_json::to_value(previous.state.view()).expect("semantic facts serialize");
                let incoming =
                    serde_json::to_value(entry.state.view()).expect("semantic facts serialize");
                if let (Some(values), Value::Array(incoming)) = (raw.as_array_mut(), incoming) {
                    values.extend(incoming);
                }
                previous.state = Self::decode(raw);
            } else {
                self.owners.entry(entry.owner.clone()).or_insert(index);
                self.entries.insert(index, entry);
            }
        }
        other.owners.clear();
        other.flows.remap_nodes(ids);
        self.flows.append(&mut other.flows);
    }
}
