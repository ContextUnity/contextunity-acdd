use super::bundle::TypedFacts;
use crate::models::facts::Facts;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

impl Serialize for TypedFacts {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let field_count = 5
            + usize::from(!self.facts.activation_signals.is_empty())
            + usize::from(!self.facts.statement_fingerprints.is_empty());
        let mut state = serializer.serialize_struct("Facts", field_count)?;
        state.serialize_field("nodes", &self.nodes_view())?;
        state.serialize_field("edges", &self.facts.edges)?;
        state.serialize_field("references", &self.facts.references)?;
        state.serialize_field("docs", &self.facts.docs)?;
        state.serialize_field("errors", &self.facts.errors)?;
        if !self.facts.activation_signals.is_empty() {
            state.serialize_field("activation_signals", &self.facts.activation_signals)?;
        }
        if !self.facts.statement_fingerprints.is_empty() {
            state.serialize_field("statement_fingerprints", &self.facts.statement_fingerprints)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for TypedFacts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let facts = Self::from_public(Facts::deserialize(deserializer)?);
        if facts.flows.has_invalid_named_uncertainty_carriers() {
            return Err(serde::de::Error::custom(
                "malformed named-definition uncertainty carrier",
            ));
        }
        Ok(facts)
    }
}
