pub mod compact_graph;
pub mod details;
pub mod facts;
pub mod semantic;
pub mod tags;
pub mod types;

pub use details::{NodeDetailFlags, NodeDetailKey};
pub use facts::{Facts, FactsLookup};
pub use types::{
    is_test, stable_hash64, CallableFingerprints, Diagnostic, DocSection, Edge, FileEntry, Graph,
    Node, ReceiverHint, Reference, StatementFingerprint, INDEX_SEMANTICS_VERSION,
};

#[cfg(test)]
mod tests {
    use super::tags::{EdgeKind, LanguageId};
    use super::NodeDetailKey;

    #[test]
    fn wire_tags_and_detail_keys_roundtrip_expected_constants() {
        for (variant, wire) in [
            (LanguageId::Bash, "bash"),
            (LanguageId::Css, "css"),
            (LanguageId::Json, "json"),
        ] {
            assert_eq!(
                (variant.as_str(), LanguageId::parse(wire)),
                (wire, Some(variant))
            );
            assert!(LanguageId::ALL.contains(&variant) && LanguageId::WIRE_NAMES.contains(&wire));
        }
        for (variant, wire) in [
            (EdgeKind::Governs, "governs"),
            (EdgeKind::Documents, "documents"),
            (EdgeKind::ReferencesDoc, "references_doc"),
            (EdgeKind::Overrides, "overrides"),
            (EdgeKind::Extends, "extends"),
        ] {
            assert_eq!(
                (variant.as_str(), EdgeKind::parse(wire)),
                (wire, Some(variant))
            );
            assert!(EdgeKind::ALL.contains(&variant) && EdgeKind::WIRE_NAMES.contains(&wire));
        }
        assert_eq!(EdgeKind::ALL.len(), EdgeKind::WIRE_NAMES.len());
        assert_eq!(EdgeKind::parse("nonexistent_edge_kind"), None);
        for (actual, expected) in [
            (NodeDetailKey::SIGNATURE, "signature"),
            (NodeDetailKey::DOC, "doc"),
            (NodeDetailKey::VISIBILITY, "visibility"),
            (NodeDetailKey::PARAMS, "params"),
            (NodeDetailKey::RETURNS, "returns"),
            (NodeDetailKey::ASYNC, "async"),
            (NodeDetailKey::EXPORT, "export"),
            (NodeDetailKey::STATIC, "static"),
            (NodeDetailKey::CATEGORY, "category"),
            (NodeDetailKey::DEFAULT_EXPORT, "default_export"),
            (NodeDetailKey::RETURN_TYPE, "return_type"),
            (NodeDetailKey::RULE, "rule"),
        ] {
            assert_eq!(actual, expected);
        }
    }
}
