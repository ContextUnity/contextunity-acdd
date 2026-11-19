use crate::machine::submit::parse_findings_artifact;
use crate::types::{FindingItem, FindingSeverity, RejectionKind};

#[test]
fn parse_findings_artifact_handles_reset_fields_and_malformed_json() {
    let payload = serde_json::json!({
        "kind": RejectionKind::ArchitecturalDrift,
        "summary": "drift",
        "blackboard_message_id": null,
        "items": vec![FindingItem {
            severity: FindingSeverity::High,
            path: Some("src/lib.rs".to_owned()),
            message: "bypassed owner".to_owned(),
            remediation: "reset".to_owned(),
        }],
        "reset_worker_context": true,
        "reset_gate_id": "build",
    })
    .to_string();

    let parsed = parse_findings_artifact(&payload).expect("valid findings artifact");
    assert!(parsed.reset_worker_context);
    assert_eq!(parsed.reset_gate_id.as_deref(), Some("build"));
    assert_eq!(parsed.findings.kind, RejectionKind::ArchitecturalDrift);
    assert!(parse_findings_artifact("not json").is_none());
}
