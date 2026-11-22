use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    models::{
        details::NodeDetailKey,
        tags::{EdgeKind, LanguageId, NodeKind},
        Facts,
    },
    typed_facts::{FlowStore, TypedFacts},
};

/// Workspace linking inputs, without storage-only syntax and documentation.
///
/// Definitions retain their namespace, container, import and export metadata.
/// References and typed value flow remain intact, including source positions
/// used by lexical visibility and assignment ordering.
#[derive(Default)]
pub struct SymbolCatalog {
    pub files: BTreeMap<String, TypedFacts>,
    pub occurrences: BTreeMap<String, Vec<u8>>,
}

impl SymbolCatalog {
    /// Consumes extracted facts and keeps their linking projection.
    pub fn from_facts(files: BTreeMap<String, Facts>) -> Self {
        let mut catalog = Self::default();
        for (path, facts) in files {
            catalog.insert_typed(path, TypedFacts::from_public(facts), false);
        }
        catalog
    }

    /// Borrows declarations and references for one workspace file.
    /// Typed value flow is retained separately and borrowed by the linker.
    pub fn file(&self, path: &str) -> Option<&Facts> {
        self.files.get(path).map(AsRef::as_ref)
    }

    /// Restores one file's reference occurrences while definitions stay borrowed.
    pub fn references(&self, path: &str) -> Option<Vec<crate::models::Reference>> {
        self.files.get(path).map(|facts| {
            self.occurrences.get(path).map_or_else(
                || facts.references.clone(),
                |bytes| decode_catalog_references(bytes, &facts.references),
            )
        })
    }

    pub fn into_files(self) -> BTreeMap<String, TypedFacts> {
        self.files
    }

    pub fn extend(&mut self, other: Self) {
        self.files.extend(other.files);
        self.occurrences.extend(other.occurrences);
    }

    pub fn insert_typed(
        &mut self,
        path: String,
        mut facts: TypedFacts,
        retain_storage: bool,
    ) -> CatalogStoragePayload {
        let mut payload = CatalogStoragePayload::default();
        if retain_storage {
            payload.flows = facts.flows.clone();
            payload.statement_fingerprints = std::mem::take(&mut facts.facts.statement_fingerprints);
        } else {
            facts.facts.statement_fingerprints.clear();
        }
        if facts.nodes.iter().any(|node| {
            node.kind == NodeKind::Module.as_str() && node.language == LanguageId::Python.as_str()
        }) {
            thread_local! {
                static COMPRESSOR: std::cell::RefCell<(zstd::bulk::Compressor<'static>, Vec<u8>)> =
                    std::cell::RefCell::new((zstd::bulk::Compressor::new(1)
                        .expect("reference compressor initialization"), Vec::new()));
            }
            let packed = COMPRESSOR
                .with(|slot| {
                    let mut scratch = slot.borrow_mut();
                    let (compressor, bytes) = &mut *scratch;
                    bytes.clear();
                    crate::typed_facts::binary::encode_references(&facts.references, bytes);
                    compressor.compress(bytes)
                })
                .expect("in-memory reference compression");
            self.occurrences.insert(path.clone(), packed);
            if retain_storage {
                payload.references = Some(facts.references.clone());
            }
            // Imports and receiver hints participate in workspace indexes. Plain
            // Python occurrences are consumed only by the per-file reference pass.
            facts.facts.references.retain(|reference| {
                !matches!(
                    reference.kind.as_str(),
                    kind if kind == EdgeKind::Calls.as_str()
                        || kind == EdgeKind::References.as_str()
                ) || reference.receiver_hint.is_some()
            });
            facts.facts.references.shrink_to_fit();
        }
        for node in &facts.facts.nodes {
            if matches!(
                facts.flows.state(node),
                Some(crate::typed_facts::FlowState::Valid {
                    original: Some(_),
                    ..
                })
            ) {
                facts.flows.get_mut(&node.id);
            }
        }
        for node in &mut facts.nodes {
            if !retain_storage {
                if let Some(details) = node.details.as_object_mut() {
                    details.retain(|key, _| !storage_only_detail(key));
                }
                continue;
            }
            let Some(details) = node.details.as_object() else {
                continue;
            };
            if !details.keys().any(|key| storage_only_detail(key)) {
                continue;
            }
            // Keep the original object, including property order, for durable
            // facts and commitment bytes. Only the smaller linking view is copied.
            let lean = details
                .iter()
                .filter(|(key, _)| !storage_only_detail(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let original = std::mem::replace(&mut node.details, serde_json::Value::Object(lean));
            if retain_storage {
                payload.node_details.push((node.id.clone(), original));
            }
        }
        for (index, doc) in facts.docs.iter_mut().enumerate() {
            let stored = (
                index,
                std::mem::take(&mut doc.content),
                std::mem::take(&mut doc.invariants),
            );
            if retain_storage {
                payload.docs.push(stored);
            }
        }
        self.files.insert(path, facts);
        payload
    }
}

fn storage_only_detail(key: &str) -> bool {
    matches!(
        key,
        NodeDetailKey::DOC | NodeDetailKey::DOCSTRING | "syntax_tree" | "occurrence_token_spans"
    )
}

/// Storage-only fields recovered from one file's durable fact blob.
#[derive(Default)]
pub struct CatalogStoragePayload {
    node_details: Vec<(String, serde_json::Value)>,
    docs: Vec<(usize, String, Vec<String>)>,
    flows: FlowStore,
    references: Option<Vec<crate::models::Reference>>,
    statement_fingerprints: Vec<crate::models::CallableFingerprints>,
}

impl CatalogStoragePayload {
    pub fn restore_composed_node(&self, node: &mut crate::models::Node) {
        let Some(source_id) = node.details["source_id"].as_str() else {
            return;
        };
        if let Some((_, original)) = self.node_details.iter().find(|(id, _)| id == source_id) {
            let mut original = original.clone();
            if let (Some(original), Some(lean)) =
                (original.as_object_mut(), node.details.as_object_mut())
            {
                original.extend(std::mem::take(lean));
            }
            node.details = original;
        }
    }

    pub fn restore(mut self, facts: &mut TypedFacts) {
        if let Some(mut references) = self.references {
            append_composed_references(&mut references, &facts.references);
            facts.facts.references = references;
        }
        let mut originals: hashbrown::HashMap<_, _> = self.node_details.into_iter().collect();
        for node in &mut facts.nodes {
            if let Some(mut original) = originals.remove(&node.id) {
                if let (Some(original), Some(lean)) =
                    (original.as_object_mut(), node.details.as_object_mut())
                {
                    // Preserve semantic updates made during workspace composition.
                    original.extend(std::mem::take(lean));
                }
                node.details = original;
            }
        }
        for (index, content, invariants) in self.docs {
            facts.docs[index].content = content;
            facts.docs[index].invariants = invariants;
        }
        facts.flows.append(&mut self.flows);
        if facts.facts.statement_fingerprints.is_empty() {
            facts.facts.statement_fingerprints = self.statement_fingerprints;
        }
    }
}

fn append_composed_references(
    references: &mut Vec<crate::models::Reference>,
    composed: &[crate::models::Reference],
) {
    let original_len = references.len();
    for reference in composed
        .iter()
        .filter(|reference| reference.source.contains(":included:"))
    {
        if !references[..original_len].iter().any(|original| {
            original.source == reference.source
                && original.kind == reference.kind
                && original.expression == reference.expression
                && original.line == reference.line
                && original.column == reference.column
        }) {
            references.push(reference.clone());
        }
    }
}

pub fn decode_catalog_references(
    bytes: &[u8],
    composed: &[crate::models::Reference],
) -> Vec<crate::models::Reference> {
    use std::io::Read;
    thread_local! {
        static DECODER: std::cell::RefCell<(zstd::zstd_safe::DCtx<'static>, Vec<u8>)> =
            std::cell::RefCell::new((zstd::zstd_safe::DCtx::create(), Vec::new()));
    }
    let mut references = DECODER.with(|slot| {
        let mut scratch = slot.borrow_mut();
        let (context, decoded) = &mut *scratch;
        context.init().expect("reference decoder initialization");
        decoded.clear();
        zstd::stream::read::Decoder::with_context(bytes, context)
            .read_to_end(decoded)
            .expect("catalog reference compression is lossless");
        crate::typed_facts::binary::decode_references(decoded).unwrap_or_else(|| {
            serde_json::from_slice(decoded)
                .expect("catalog reference JSON was serialized internally")
        })
    });
    append_composed_references(&mut references, composed);
    references
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
/// Language-neutral receiver catalog mapping type names and expressions to receiver hints and target types.
pub struct ReceiverCatalog {
    /// Statically known receiver hints indexed by canonical type or expression.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receivers: BTreeMap<String, String>,
}

impl ReceiverCatalog {
    /// Creates an empty receiver catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a receiver hint for a canonical type or expression.
    pub fn register(&mut self, type_name: impl Into<String>, receiver_hint: impl Into<String>) {
        self.receivers
            .insert(type_name.into(), receiver_hint.into());
    }

    /// Looks up a receiver hint for a type or expression.
    pub fn lookup(&self, type_name: &str) -> Option<&str> {
        self.receivers.get(type_name).map(String::as_str)
    }
}
