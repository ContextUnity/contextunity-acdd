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

#[cfg(feature = "lang-python")]
#[test]
fn django_call_to_file_semantic_pack_replacement_contract() {
    use acdd_index::ast::extract_typed_with_registry;
    use acdd_index::languages::dependency_registry::DependencyRegistry;
    use acdd_index::languages::LanguageFamily;

    let temporary = acdd_core::fs::ScopedTempDir::create("acdd-framework-packs-call-to-file").unwrap();
    let semantic_root = temporary.path().join("semantic-replacement");
    std::fs::create_dir_all(semantic_root.join(".acdd/frameworks")).unwrap();
    std::fs::write(
        semantic_root.join("pyproject.toml"),
        "[project]\nname='call-file-fixture'\ndependencies=['django']\n",
    )
    .unwrap();
    let replacement = semantic_root.join(".acdd/frameworks/django.toml");
    for (phase, enabled) in [
        ("bundled", true),
        ("empty replacement", false),
        ("restored", true),
    ] {
        if phase == "empty replacement" {
            std::fs::write(&replacement, "replace = true\n").unwrap();
        } else if phase == "restored" {
            std::fs::remove_file(&replacement).unwrap();
        }
        let registry = DependencyRegistry::try_collect_with_paths(&semantic_root, None, &[]).unwrap();
        let manifest = registry
            .framework_manifest_for_path(LanguageFamily("python"), "views.py", "django", "django")
            .expect("package/project pack remains available");
        let _ = manifest;
        let extracted = extract_typed_with_registry(
            "views.py",
            "python",
            "from django.shortcuts import render as show_page\n\ndef page(request):\n    return show_page(request, 'catalog/card.html')\n\ndef shadowed(request, show_page):\n    return show_page(request, 'catalog/card.html')\n\ndef starred(request, args):\n    return show_page(*args, 'catalog/card.html')\n\ndef comprehension(request, callbacks):\n    return [show_page(request, 'catalog/card.html') for show_page in callbacks]\n",
            &registry,
        )
        .unwrap();
        assert!(extracted.facts.errors.is_empty(), "{phase}: parse errors");
        let native_owner = &extracted
            .facts
            .nodes
            .iter()
            .find(|node| node.name == "page")
            .expect("native ordinary caller declaration survives replacement")
            .id;
        let native_consumers: Vec<_> = extracted
            .facts
            .semantic
            .entries()
            .flat_map(|(_, entry)| match &entry.state {
                acdd_index::typed_facts::SemanticState::Valid { facts, .. } => facts.iter(),
                acdd_index::typed_facts::SemanticState::Invalid { .. } => {
                    panic!("{phase}: canonical native semantic facts must remain valid")
                }
            })
            .filter(|fact| {
                matches!(
                    fact.role,
                    acdd_index::semantic::SemanticFactRole::CallToFile { .. }
                )
            })
            .collect();
        assert_eq!(
            native_consumers.len(),
            usize::from(enabled),
            "{phase}: native exact imported-call consumer fact"
        );
        assert!(
            native_consumers
                .iter()
                .all(|fact| &fact.occurrence.owner_id == native_owner),
            "{phase}: native actual caller declaration"
        );
        let extracted = serde_json::to_value(&extracted).unwrap();
        let nodes = extracted["nodes"].as_array().unwrap();
        let owner = nodes
            .iter()
            .find(|node| node["name"] == "page")
            .expect("ordinary caller declaration survives replacement");
        let consumers: Vec<_> = nodes
            .iter()
            .flat_map(|node| {
                node["details"]
                    .get("semantic_facts")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|fact| fact["role"]["kind"] == "call_to_file")
            .collect();
        if enabled {
            assert_eq!(
                consumers.len(),
                1,
                "{phase}: exact imported-call consumer fact"
            );
            assert_eq!(
                consumers[0]["occurrence"]["owner_id"], owner["id"],
                "{phase}: actual caller declaration"
            );
            assert_eq!(
                consumers[0]["role"],
                serde_json::json!({"kind":"call_to_file","import_owner":"module:views.py","alias":"show_page","module":"django.shortcuts","export":"render","relation":"renders_template"})
            );
            assert_eq!(
                consumers[0]["identity"]["path"], "catalog/card.html",
                "{phase}: literal identity"
            );
            assert_eq!(
                consumers[0]["identity"]["target"],
                serde_json::json!({"kind":"project_path_suffix","prefix":"templates/","language":"html"})
            );
            assert_eq!(consumers[0]["occurrence"]["line"], 4, "{phase}: occurrence");
            for field in ["", "/identity", "/occurrence", "/role"] {
                let mut malformed = consumers[0].clone();
                malformed
                    .pointer_mut(field)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("unsupported".into(), true.into());
                assert!(
                    serde_json::from_value::<acdd_index::semantic::SemanticFact>(malformed)
                        .is_err(),
                    "strict stored DTO field {field}"
                );
            }
        } else {
            assert!(
                consumers.is_empty(),
                "empty replacement must remove consumer facts: {consumers:?}"
            );
        }
    }
}
