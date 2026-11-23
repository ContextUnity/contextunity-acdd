//! Language-neutral evidence for statically inferred receiver types.

pub mod builtins;
pub mod relations;
pub mod types;

pub use builtins::*;
pub use relations::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_keys_borrow_only_proven_semantic_expressions() {
        let details = serde_json::json!({"value_flow": {
            "return_type": {"kind":"named", "name":"pkg.Worker"},
            "alias_type": {"kind":"named", "name":"AliasTarget"},
            "bindings": [
                {"value":{"kind":"construct", "callee":"pkg.Factory"}},
                {"value":{"kind":"field", "receiver":"client", "member":"run"}},
                {"value":{"kind":"unknown", "callee":"invented"}}
            ],
            "branch_joins": [{"value":{"kind":"branch_join", "alternatives":[
                {"kind":"construct", "callee":"JoinTarget"},
                {"kind":"unknown"}
            ]}}],
            "fields": [{"value":{"kind":"annotated", "type_expr":{"kind":"named", "name":"FieldType"}}}]
        }});
        let mut keys = std::collections::BTreeSet::new();
        ValueFlowFacts::reference_keys_from_details(&details, |key| {
            keys.insert(key);
        });
        assert_eq!(
            keys,
            [
                "AliasTarget",
                "FieldType",
                "JoinTarget",
                "client",
                "pkg.Factory",
                "pkg.Worker",
                "run"
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn semantic_facts_round_trip_and_default_optional_sections() {
        use crate::typed_facts::{decode_facts, encode_facts, TypedFacts};
        use serde_json::json;
        let hot_layout = std::mem::size_of::<ValueFlowFacts>();
        assert!(
            hot_layout <= 160,
            "ValueFlowFacts hot layout: {hot_layout} bytes"
        );
        eprintln!("ValueFlowFacts hot layout: {hot_layout} bytes");
        let origin = json!({"pack":"proof", "rule":"projection"});
        let position = json!({"line":2,"column":3});
        let rows = [
            (
                "symbol_attachments",
                json!({"origin":origin,"position":position,"source":{"kind":"node","id":"callback"},"target":{"kind":"name","expression":"Model"},"relation":"uses_model"}),
            ),
            (
                "factory_results",
                json!({"origin":origin,"position":position,"binding":"useCart","members":[{"name":"checkout","position":position}]}),
            ),
            (
                "named_bindings",
                json!({"origin":origin,"position":position,"domain":"event","key":{"kind":"literal","value":"opened"},"role":{"kind":"reference"}}),
            ),
            (
                "endpoint_requests",
                json!({"origin":origin,"position":position,"url":{"kind":"unknown"},"method":{"kind":"unknown"}}),
            ),
        ];
        for (vector, fact) in rows {
            let encoded = json!({vector:[fact.clone()]});
            let mut flow: ValueFlowFacts = serde_json::from_value(encoded.clone()).unwrap();
            assert_eq!(
                crate::typed_facts::scope_fact_count(&flow),
                1,
                "{vector}: scope accounting"
            );
            assert_eq!(
                serde_json::to_value(&flow).unwrap()[vector],
                encoded[vector]
            );
            let mut typed_keys = std::collections::BTreeSet::new();
            flow.reference_keys(|key| {
                typed_keys.insert(key.to_owned());
            });
            let mut raw_keys = std::collections::BTreeSet::new();
            ValueFlowFacts::reference_keys_from_value(&encoded, |key| {
                raw_keys.insert(key.to_owned());
            });
            assert_eq!(typed_keys, raw_keys, "{vector}: dependency projection");
            flow.offset_lines(10);
            assert_eq!(
                serde_json::to_value(&flow).unwrap()[vector][0]["position"]["line"],
                12
            );
            if vector == "factory_results" {
                assert_eq!(
                    flow.factory_results().next().unwrap().members[0]
                        .position
                        .line,
                    12
                );
            }
            let public = json!({"nodes":[{"id":"scope","kind":"template_scope","name":"scope","qualname":"scope","path":"page.html","line":1,"end_line":3,"is_test":false,"language":"javascript","generated":false,"details":{"classic_global":true,"value_flow":encoded}}],"edges":[],"references":[],"docs":[],"errors":[]});
            let mut typed: TypedFacts = serde_json::from_value(public).unwrap();
            // Generated projections can create the first flow on an otherwise
            // empty scope; canonical serialization must publish that entry too.
            typed.facts.nodes[0]
                .details
                .as_object_mut()
                .unwrap()
                .remove("value_flow");
            typed.flows.insert_classic(
                "scope".into(),
                serde_json::from_value(encoded.clone()).unwrap(),
            );
            let reopened = decode_facts(&encode_facts(&typed).unwrap()).unwrap();
            assert_eq!(
                serde_json::to_value(&typed).unwrap(),
                serde_json::to_value(&reopened).unwrap(),
                "{vector}: compressed classic round trip"
            );
            assert_eq!(
                reopened.flows.get(&reopened.nodes[0]).unwrap(),
                &serde_json::from_value::<ValueFlowFacts>(encoded.clone()).unwrap()
            );
            for count in [128, 129] {
                let bounded = json!({vector:vec![fact.clone(); count]});
                assert_eq!(
                    serde_json::from_value::<ValueFlowFacts>(bounded).is_ok(),
                    count == 128,
                    "{vector}: vector boundary"
                );
            }
            let mut unknown_field = fact.clone();
            unknown_field["unchecked"] = true.into();
            let mut missing_position = fact.clone();
            missing_position.as_object_mut().unwrap().remove("position");
            for malformed in [
                unknown_field,
                missing_position,
                json!({"kind":"unsupported"}),
            ] {
                assert!(
                    serde_json::from_value::<ValueFlowFacts>(json!({vector:[malformed]})).is_err(),
                    "{vector}: strict fact"
                );
            }
        }
        let facts = ValueFlowFacts {
            bindings: vec![BindingFact {
                name: "client".into(),
                position: SourcePosition { line: 2, column: 3 },
                value: ValueExpr::Construct {
                    callee: "Worker".into(),
                },
                conditional: false,
            }],
            ..Default::default()
        };
        let json = serde_json::to_value(&facts).unwrap();
        assert_eq!(
            serde_json::from_value::<ValueFlowFacts>(json).unwrap(),
            facts
        );
        assert!(serde_json::from_str::<ValueFlowFacts>("{}")
            .unwrap()
            .is_empty());
        let legacy = serde_json::json!({"bindings": [], "fields": []});
        assert!(serde_json::from_value::<ValueFlowFacts>(legacy)
            .unwrap()
            .branch_joins
            .is_empty());
        assert!(serde_json::from_str::<ValueFlowFacts>(r#"{"guessed_type":"Worker"}"#).is_err());
        for encoded in ["{}", r#"{"bindings":[],"fields":[],"rule_origins":[]}"#] {
            let mut empty: ValueFlowFacts = serde_json::from_str(encoded).unwrap();
            empty.offset_lines(10);
            assert!(
                empty.framework.is_none(),
                "empty scopes need no framework allocation"
            );
        }
        let ordered: ValueFlowFacts = serde_json::from_value(json!({
            "bindings": [{"name":"x","position":{"line":1,"column":0},"value":{"kind":"unknown"},"conditional":false}],
            "rule_origins": [{"pack":"proof","rule":"order"}],
            "return_type": {"kind":"named","name":"Worker"}
        })).unwrap();
        assert_eq!(
            serde_json::to_value(&ordered)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["rule_origins", "bindings", "return_type"]
        );
    }

    #[test]
    fn source_order_retains_same_line_columns_and_document_offsets() {
        let mut facts = ValueFlowFacts {
            bindings: vec![BindingFact {
                name: "x".into(),
                position: SourcePosition { line: 1, column: 4 },
                value: ValueExpr::Unknown,
                conditional: true,
            }],
            ..Default::default()
        };
        facts.offset_lines(10);
        assert_eq!(
            facts.bindings[0].position,
            SourcePosition {
                line: 11,
                column: 4
            }
        );
        assert!(
            SourcePosition {
                line: 11,
                column: 4
            } < SourcePosition {
                line: 11,
                column: 5
            }
        );
    }
}
