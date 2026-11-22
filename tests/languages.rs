#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "languages/mod.rs"]
mod languages;
pub(crate) use languages::support;

#[path = "framework_manifests/mod.rs"]
mod manifests;

#[test]
fn helper_modules_are_not_advertised_as_language_profiles() {
    use acdd_index::languages;

    for (id, feature) in [
        ("support", "lang-support"),
        ("registries", "lang-registries"),
    ] {
        assert!(
            !languages::AVAILABLE_LANGUAGE_FEATURES.contains(&feature),
            "helper module {id} was advertised as an available language feature"
        );
        let error = match languages::require(id) {
            Ok(profile) => panic!(
                "helper module {id} unexpectedly resolved to profile {}",
                profile.id()
            ),
            Err(error) => error.to_string(),
        };
        assert!(
            error.starts_with("unknown AST language"),
            "helper module {id} should be reported as unknown, got: {error}"
        );
    }
}

#[cfg(all(feature = "lang-python", feature = "lang-typescript"))]
mod cognitive_and_statement_contracts {
    use acdd_index::ast;
    use acdd_index::models::details::NodeDetailKey;

    #[test]
    fn python_and_typescript_emit_single_pass_complexity_and_statement_fingerprints() {
        let py_src = "def check(items):\n    for x in items:\n        if x > 0:\n            return x\n    return 0\n";
        let py_facts = ast::extract("app.py", "python", py_src).unwrap();
        let py_fn = py_facts.nodes.iter().find(|n| n.name == "check").unwrap();
        assert_eq!(py_fn.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(), Some(3));
        assert_eq!(py_fn.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(), Some(2));
        assert!(!py_facts.statement_fingerprints.is_empty());

        let ts_src = "export function route(kind: string): number {\n  switch (kind) {\n    case 'a': return 1;\n    case 'b': return 2;\n    default: return 0;\n  }\n}\n\nexport function withCallback(items: number[], ready: boolean): number {\n  const mapped = items.map((x: number): number => {\n    if (x > 0) {\n      return x;\n    }\n    return 0;\n  });\n  if (ready) {\n    return mapped.length;\n  }\n  return 0;\n}\n";
        let ts_facts = ast::extract("app.ts", "typescript", ts_src).unwrap();
        let ts_fn = ts_facts.nodes.iter().find(|n| n.name == "route").unwrap();
        assert_eq!(ts_fn.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(), Some(1));
        assert_eq!(ts_fn.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(), Some(1));
        let outer_cb = ts_facts.nodes.iter().find(|n| n.name == "withCallback").unwrap();
        assert_eq!(outer_cb.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(), Some(1));
        assert_eq!(outer_cb.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(), Some(1));
        let inner_cb = ts_facts.nodes.iter().find(|n| n.name.starts_with("anonymous@")).unwrap();
        assert_eq!(inner_cb.details[NodeDetailKey::COGNITIVE_COMPLEXITY].as_u64(), Some(1));
        assert_eq!(inner_cb.details[NodeDetailKey::MAX_NESTING_DEPTH].as_u64(), Some(1));
    }
}
