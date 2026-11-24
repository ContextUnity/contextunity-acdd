#![cfg(feature = "lang-proto")]

use crate::languages::harness::ExtractorHarness;
use std::fs;

#[cfg(feature = "lang-proto")]
#[test]
fn proto_package_qualified_types_and_fields() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write(
        "protos/auth.proto",
        r#"syntax = "proto3";

package mycompany.auth;

message Credentials {
    string username = 1;
    string token = 2;
}

message Session {
    mycompany.auth.Credentials creds = 1;
}

service AuthService {
    rpc Login(mycompany.auth.Credentials) returns (Session);
}
"#,
    );

    let facts = ExtractorHarness::for_lang("proto")
        .at_path("protos/auth.proto")
        .extract(&fs::read_to_string(w.root().join("protos/auth.proto")).unwrap());

    // Verify module qualname is package
    let module_node = facts.nodes.iter().find(|n| n.kind == acdd_index::models::tags::NodeKind::Module).unwrap();
    assert_eq!(module_node.qualname, "mycompany.auth");

    // Verify field references was extracted
    let field_refs: Vec<&str> = facts
        .references
        .iter()
        .filter(|r| r.kind == "references")
        .map(|r| r.expression.as_str())
        .collect();
    assert!(
        field_refs.contains(&"mycompany.auth.Credentials"),
        "expected field reference to mycompany.auth.Credentials: {:?}",
        field_refs
    );

    // Verify build & link
    w.build();
    let conn = w.open();
    let ref_edges: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT (SELECT id FROM nodes WHERE node_hash=src_hash), (SELECT id FROM nodes WHERE node_hash=dst_hash) FROM edges WHERE kind='references'")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap())))
            .unwrap();
        rows.map(Result::unwrap).collect()
    };
    assert!(
        ref_edges.iter().any(|(_, dst)| dst.contains("Credentials")),
        "expected reference edge to Credentials: {:?}",
        ref_edges
    );
}

#[test]
fn test_proto_ast_extractor() {
    let source = r#"
syntax = "proto3";

package contextunity.core;

import "google/protobuf/struct.proto";

enum Modality {
    TEXT = 0;
    AUDIO = 1;
}

message ContextUnit {
    string unit_id = 1;
    Modality modality = 4;
}

service RouterService {
    rpc ExecuteAgent(ContextUnit) returns (ContextUnit);
}
"#;

    let facts = ExtractorHarness::for_lang("proto")
        .at_path("core/protos/test.proto")
        .extract(source);

    // Check nodes extracted
    let node_names: Vec<&str> = facts.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        node_names.contains(&"Modality"),
        "missing Modality enum: {:?}",
        node_names
    );
    assert!(
        node_names.contains(&"ContextUnit"),
        "missing ContextUnit message: {:?}",
        node_names
    );
    assert!(
        node_names.contains(&"RouterService"),
        "missing RouterService: {:?}",
        node_names
    );
    assert!(
        node_names.contains(&"ExecuteAgent"),
        "missing ExecuteAgent rpc: {:?}",
        node_names
    );

    // Check kinds
    let cu = facts
        .nodes
        .iter()
        .find(|n| n.name == "ContextUnit")
        .unwrap();
    assert_eq!(cu.kind, acdd_index::models::tags::NodeKind::Class);

    let svc = facts
        .nodes
        .iter()
        .find(|n| n.name == "RouterService")
        .unwrap();
    assert_eq!(svc.kind, acdd_index::models::tags::NodeKind::Service);

    let rpc = facts
        .nodes
        .iter()
        .find(|n| n.name == "ExecuteAgent")
        .unwrap();
    assert_eq!(rpc.kind, acdd_index::models::tags::NodeKind::Function);

    // Check imports
    let imports: Vec<&str> = facts
        .references
        .iter()
        .filter(|r| r.kind == "imports")
        .map(|r| r.expression.as_str())
        .collect();
    assert!(
        imports.contains(&"google/protobuf/struct.proto"),
        "missing import reference: {:?}",
        imports
    );

    // Check rpc references to ContextUnit
    let rpc_refs: Vec<&str> = facts
        .references
        .iter()
        .filter(|r| r.kind == "references")
        .map(|r| r.expression.as_str())
        .collect();
    assert!(
        rpc_refs.contains(&"ContextUnit"),
        "missing rpc type references: {:?}",
        rpc_refs
    );

    // Check 0 errors
    assert!(
        facts.errors.is_empty(),
        "unexpected parse errors: {:?}",
        facts.errors
    );
}

#[test]
fn proto_well_known_stdlib_imports_and_types_are_external_and_resolved() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    w.write(
        "protos/events.proto",
        r#"syntax = "proto3";

package mycompany.events;

import "google/protobuf/timestamp.proto";
import "google/protobuf/empty.proto";
import "custom/missing.proto";

message Event {
    google.protobuf.Timestamp occurred_at = 1;
    custom.Missing extra = 2;
}

service EventService {
    rpc Record(Event) returns (google.protobuf.Empty);
}
"#,
    );
    w.build();
    let conn = w.open();
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT x.expression, c.status, COALESCE(v.evidence, '') FROM resolution_coverage c \
                 JOIN coverage_expressions x ON x.expression_id = c.expression_id \
                 LEFT JOIN coverage_evidence v ON v.evidence_id = c.evidence_id \
                 JOIN path_dictionary p ON p.path_id = c.path_id \
                 WHERE p.path = 'protos/events.proto'",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    for (expr, expected_status, evidence_needle) in [
        ("google/protobuf/timestamp.proto", "external", "standard library"),
        ("google/protobuf/empty.proto", "external", "standard library"),
        ("custom/missing.proto", "external", "external dependency"),
        ("google.protobuf.Timestamp", "resolved", "standard library or built-in"),
        ("google.protobuf.Empty", "resolved", "standard library or built-in"),
        ("custom.Missing", "unresolved", ""),
    ] {
        let found = rows
            .iter()
            .find(|(e, _, _)| e == expr)
            .unwrap_or_else(|| panic!("missing coverage row for {expr}: {rows:?}"));
        assert_eq!(found.1, expected_status, "{expr}: {found:?}");
        assert!(
            found.2.contains(evidence_needle),
            "{expr}: expected evidence containing {evidence_needle:?}, got {:?}",
            found.2
        );
    }
    let proto = acdd_index::languages::require("proto").unwrap();
    assert!(proto.is_stdlib("google/protobuf/timestamp.proto"));
    assert!(proto.is_stdlib("google.protobuf.Timestamp"));
    assert!(!proto.is_stdlib("custom/missing.proto"));
    assert!(!proto.is_stdlib("custom.Missing"));
}
