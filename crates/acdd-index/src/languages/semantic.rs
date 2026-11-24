//! Strict storage adapter for facts shared by existing language and linker owners.
use crate::models::{Facts, Node, Reference};

pub use super::manifests::semantic::EffectiveSemanticRule;
pub(crate) use super::manifests::semantic::{SemanticRelation, SemanticTarget};

pub(crate) use crate::semantic::{
    SemanticFact, SemanticFactRole, SemanticIdentity,
};

pub(crate) fn push(facts: &mut Facts, fact: SemanticFact) {
    if matches!(&fact.identity.target, SemanticTarget::ImportedSymbol { .. }) {
        return;
    }
    let key = dependency_key(&fact.identity);
    let occurrence = fact.occurrence.clone();
    if facts.semantic.push(&mut facts.nodes, fact) {
        facts.references.push(Reference {
            source: occurrence.owner_id,
            expression: key,
            kind: "semantic_key".into(),
            line: occurrence.line,
            column: occurrence.column,
            alias: None,
            module: None,
            dynamic: false,
            receiver_hint: None,
        });
    }
}

/// Decodes semantic facts stored in node details.
pub fn decode(node: &Node) -> anyhow::Result<Vec<SemanticFact>> {
    match node.detail_get(crate::models::details::NodeDetailKey::SEMANTIC_FACTS) {
        None => Ok(Vec::new()),
        Some(value) => Ok(serde_json::from_value(value.clone())?),
    }
}

/// Computes stable dependency key for a semantic identity.
pub fn dependency_key(identity: &SemanticIdentity) -> String {
    format!(
        "semantic:{}",
        acdd_core::format::sha256_serde_json_hex(identity).expect("identity serializes")
    )
}
