use super::support::TestProvider;
use acdd_core::errors::ErrorCode;
use serde_json::json;

#[test]
fn acdd_lint_gate_and_docs_profile_seam_bootstrap() {
    let provider = TestProvider::full_composite();
    provider
        .validate_check_ref("code.lint", &json!({"categories": ["arch", "docs"]}))
        .expect("valid code.lint params pass");

    let err = provider
        .validate_check_ref("code.lint", &json!({"categories": ["invalid"]}))
        .expect_err("invalid category must fail closed");
    assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid);
}
