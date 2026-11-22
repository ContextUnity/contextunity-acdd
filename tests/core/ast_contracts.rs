use acdd_core::errors::ErrorCode;
use acdd_core::lint::{ArchRuleKind, ArchitectureConfig, ComplexityLimits, LintSeverity};
use acdd_index::ast;
use acdd_index::models::details::NodeDetailKey;
use acdd_index::typed_facts::{decode_facts, encode_facts, TypedFacts};

#[test]
fn architecture_complexity_limits_contract_and_locked_rule_kinds() {
    assert_eq!(ArchRuleKind::ALL.len(), 8);

    let default_cfg = ArchitectureConfig::default();
    assert!(default_cfg.is_empty());
    default_cfg.validate().unwrap();
    assert!(default_cfg.complexity.max_cognitive_complexity > 0);
    assert!(default_cfg.complexity.max_nesting_depth > 0);
    assert!(default_cfg.complexity.max_fanout > 0);

    let custom_yaml = r#"
complexity:
  max_cognitive_complexity: 42
  max_nesting_depth: 5
  max_fanout: 30
  severity: error
  invariant: INV-BOUNDED-STRUCTURAL-COMPLEXITY
"#;
    let custom: ArchitectureConfig = serde_yaml::from_str(custom_yaml).unwrap();
    assert!(!custom.is_empty());
    custom.validate().unwrap();
    assert_eq!(custom.complexity.max_cognitive_complexity, 42);
    assert_eq!(custom.complexity.max_nesting_depth, 5);
    assert_eq!(custom.complexity.max_fanout, 30);
    assert_eq!(custom.complexity.severity, LintSeverity::Error);
    assert_eq!(
        custom.complexity.invariant.as_deref(),
        Some("INV-BOUNDED-STRUCTURAL-COMPLEXITY")
    );

    for (label, bad_yaml) in [
        ("zero_cognitive", "complexity:\n  max_cognitive_complexity: 0\n"),
        ("zero_nesting", "complexity:\n  max_nesting_depth: 0\n"),
        ("zero_fanout", "complexity:\n  max_fanout: 0\n"),
        ("empty_invariant", "complexity:\n  invariant: \"   \"\n"),
    ] {
        let cfg: ArchitectureConfig = serde_yaml::from_str(bad_yaml).unwrap();
        let err = cfg
            .validate()
            .expect_err(&format!("{label}: expected validation failure"));
        assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{label}: {err}");
    }

    assert!(
        serde_yaml::from_str::<ComplexityLimits>("unknown_limit: 1\n").is_err(),
        "ComplexityLimits must deny unknown fields"
    );

    assert_eq!(NodeDetailKey::COGNITIVE_COMPLEXITY, "cognitive_complexity");
    assert_eq!(NodeDetailKey::MAX_NESTING_DEPTH, "max_nesting_depth");
    assert_eq!(NodeDetailKey::FACT_LIMIT_EXCEEDED, "fact_limit_exceeded");
}

#[cfg(feature = "lang-rust")]
#[test]
fn single_pass_ast_cognitive_metrics_and_statement_fingerprints_contract() {
    let rust_src = r#"
pub enum Code { A, B, C, D, E, F }

pub fn flat_router(c: Code) -> i32 {
    match c {
        Code::A => 1,
        Code::B => 2,
        Code::C => 3,
        Code::D => 4,
        Code::E => 5,
        Code::F => 6,
    }
}

pub fn deeply_nested(items: &[i32]) -> i32 {
    let mut sum = 0;
    for item in items {
        if *item > 0 {
            if *item % 2 == 0 {
                sum += *item;
            }
        }
    }
    sum
}

pub fn single_line_call(a: i32, b: i32) -> i32 {
    let res = helper(a, b);
    res
}

pub fn wrapped_call_with_comments(a: i32, b: i32) -> i32 {
    let res = helper(
        a, // first arg
        b,
    );
    res
}
"#;
    let typed = ast::extract_typed("src/sample.rs", "rust", rust_src).unwrap();

    let flat_node = typed.nodes.iter().find(|n| n.name == "flat_router").unwrap();
    assert_eq!(
        flat_node.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(),
        Some(1),
        "flat match table must score +1 regardless of arm count: {}",
        flat_node.details
    );
    assert_eq!(
        flat_node.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(),
        Some(1)
    );

    let nested_node = typed.nodes.iter().find(|n| n.name == "deeply_nested").unwrap();
    assert_eq!(
        nested_node.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(),
        Some(6),
        "for(+1+0) + if(+1+1) + if(+1+2) = 6: {}",
        nested_node.details
    );
    assert_eq!(
        nested_node.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(),
        Some(3)
    );

    let single_node = typed.nodes.iter().find(|n| n.name == "single_line_call").unwrap();
    let wrapped_node = typed
        .nodes
        .iter()
        .find(|n| n.name == "wrapped_call_with_comments")
        .unwrap();

    let single_fp = typed
        .statement_fingerprints
        .iter()
        .find(|fp| fp.node_id == single_node.id)
        .expect("single_line_call fingerprints");
    let wrapped_fp = typed
        .statement_fingerprints
        .iter()
        .find(|fp| fp.node_id == wrapped_node.id)
        .expect("wrapped_call_with_comments fingerprints");
    assert_eq!(single_fp.statements.len(), 2);
    assert_eq!(wrapped_fp.statements.len(), 2);
    assert_eq!(
        single_fp.statements[0].hash, wrapped_fp.statements[0].hash,
        "multi-line wrapped call with trailing comma and inline comment must normalize to single-line hash"
    );
    assert!(
        wrapped_fp.statements[0].end_line > wrapped_fp.statements[0].line,
        "wrapped call statement must collapse into one span"
    );

    let encoded = encode_facts(&typed).unwrap();
    let decoded: TypedFacts = decode_facts(&encoded).unwrap();
    assert_eq!(decoded.facts.statement_fingerprints, typed.facts.statement_fingerprints);
}
