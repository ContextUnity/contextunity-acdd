#![cfg(all(feature = "lang-python", feature = "lang-rust"))]

use acdd_index::{
    ast,
    interner::LinkerInterner,
    languages::dependency_registry::DependencyRegistry,
    linker,
    models::{
        compact_graph::{CompactTag, Graph as CompactGraph},
        Facts,
    },
    typed_facts::{decode_facts, encode_facts, TypedFacts},
};
use std::collections::BTreeMap;

#[test]
fn interned_extraction_and_linker_preserve_exact_graph_parity() {
    // 1. CompactTag static/dynamic roundtrips across AST node kinds, reference kinds,
    //    languages, and edge/coverage tags.
    for tag in [
        "function",
        "method",
        "class",
        "struct",
        "enum",
        "trait",
        "impl",
        "module",
        "field",
        "constant",
        "variable",
        "type",
        "route",
        "document",
        "call",
        "calls",
        "calls_endpoint",
        "import",
        "imports",
        "reads",
        "writes",
        "member",
        "extends",
        "implements",
        "resolved",
        "unresolved",
        "ambiguous",
        "rust",
        "python",
        "typescript",
        "javascript",
        "html",
        "vue",
        "markdown",
    ] {
        assert_eq!(
            CompactTag::known(tag),
            Some(tag),
            "missing static tag {tag}"
        );
        let compact = CompactTag::from(tag);
        assert!(matches!(compact, CompactTag::Static(_)));
        assert_eq!(compact.as_str(), tag);
        assert_eq!(String::from(compact), tag);
    }
    use acdd_index::models::{CoverageStatus, EdgeConfidence, EdgeKind, LanguageId, NodeKind, ReferenceKind};
    macro_rules! check_wire {
        ($ty:ty, $($wire:literal),+ $(,)?) => {
            for wire in [$($wire),+] {
                let value: $ty = wire.parse().unwrap();
                assert_eq!(value.to_string(), wire);
                assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{wire}\""));
                assert_eq!(serde_json::from_str::<$ty>(&format!("\"{wire}\"")).unwrap(), value);
                assert_eq!(CompactTag::from(value).as_str(), wire);
            }
        };
    }
    check_wire!(EdgeKind, "calls", "imports", "possible_calls");
    check_wire!(EdgeConfidence, "exact", "inferred", "possible", "heuristic");
    check_wire!(CoverageStatus, "resolved", "external", "ambiguous", "unresolved");
    check_wire!(NodeKind, "function", "class", "interface");
    check_wire!(ReferenceKind, "call", "import", "semantic_key");
    check_wire!(LanguageId, "rust", "python", "typescript", "javascript");
    let mut typed_graph = CompactGraph::default();
    typed_graph.push_edge("a.rs", "a", "b", EdgeKind::Calls, 1, "call", EdgeConfidence::Exact);
    typed_graph.push_coverage("a.rs", 1, "b", CoverageStatus::Resolved, "call");
    let mut wire_graph = CompactGraph::default();
    wire_graph.push_edge("a.rs", "a", "b", "calls", 1, "call", "exact");
    wire_graph.push_coverage("a.rs", 1, "b", "resolved", "call");
    assert_eq!(serde_json::to_value(&typed_graph.edges).unwrap(), serde_json::to_value(&wire_graph.edges).unwrap());
    assert_eq!(serde_json::to_value(&typed_graph.coverage).unwrap(), serde_json::to_value(&wire_graph.coverage).unwrap());
    let dynamic = CompactTag::from("custom_unknown_tag");
    assert!(matches!(dynamic, CompactTag::Owned(_)));
    assert_eq!(dynamic.as_str(), "custom_unknown_tag");

    // 2. TypedFacts binary string-table encode/decode & LinkerInterner pre-hashing parity.
    let registry = DependencyRegistry::collect(None);
    let files = [
        (
            "pkg/defs.py",
            "python",
            "class Service:\n    def run(self, item):\n        return item\n",
        ),
        (
            "pkg/app.py",
            "python",
            "from pkg.defs import Service\n\ndef entry():\n    svc = Service()\n    return svc.run(1)\n",
        ),
        (
            "pkg/missing.py",
            "python",
            "def broken():\n    return nonexistent_fn(1)\n",
        ),
        (
            "src/lib.rs",
            "rust",
            "pub struct Engine;\nimpl Engine {\n    pub fn start(&self) {}\n}\npub fn boot(e: &Engine) { e.start(); }\n",
        ),
    ];

    let mut cold_catalog: BTreeMap<String, TypedFacts> = BTreeMap::new();
    let mut decoded_catalog: BTreeMap<String, TypedFacts> = BTreeMap::new();
    for &(path, language, source) in &files {
        let extracted =
            ast::extract_typed_with_registry(path, language, source, &registry).unwrap();
        let encoded = encode_facts(&extracted).unwrap();
        assert!(encoded.starts_with(b"ACDD-LOCAL-FACTS\0\x03"));
        let decoded = decode_facts(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(&extracted.facts).unwrap(),
            serde_json::to_value(&decoded.facts).unwrap(),
            "string-table binary roundtrip must preserve facts for {path}"
        );

        // String-table raw encode/decode roundtrip & corruption sensitivity.
        let mut raw = Vec::new();
        extracted.encode_with_string_table(&mut raw);
        let mut cursor = 0;
        let roundtrip = TypedFacts::decode_with_string_table(&raw, &mut cursor).unwrap();
        assert_eq!(cursor, raw.len());
        assert_eq!(
            serde_json::to_value(&roundtrip.facts).unwrap(),
            serde_json::to_value(&extracted.facts).unwrap()
        );
        let mut corrupt = raw.clone();
        *corrupt.last_mut().unwrap() ^= 0xff;
        let mut bad_cursor = 0;
        assert!(
            TypedFacts::decode_with_string_table(&corrupt, &mut bad_cursor)
                .filter(|_| bad_cursor == corrupt.len())
                .is_none()
        );

        cold_catalog.insert(path.to_owned(), extracted);
        decoded_catalog.insert(path.to_owned(), decoded);
    }

    let owned: BTreeMap<String, Facts> = cold_catalog.iter().map(|(path, facts)| (path.clone(), facts.facts.clone())).collect();
    let borrowed: BTreeMap<&str, &Facts> = owned.iter().map(|(path, facts)| (path.as_str(), facts)).collect();
    for catalog in [&owned as &dyn acdd_index::models::FactsLookup, &borrowed as &dyn acdd_index::models::FactsLookup] {
        assert_eq!(catalog.len(), files.len());
        for (path, _, _) in files {
            assert!(catalog.get_facts(path).is_some());
        }
    }

    // 3. LinkerInterner pre-hashing & ID resolution parity.
    let mut interner = LinkerInterner::from_catalog(&cold_catalog);
    for &(path, language, _) in &files {
        let path_id = interner.lookup_path(path).expect("path interned");
        assert_eq!(interner.resolve_path(path_id), path);
        let lang_hash = interner.prehash_symbol(language);
        let lang_id = interner
            .lookup_symbol_prehashed(lang_hash, language)
            .expect("language pre-interned");
        assert_eq!(interner.resolve_symbol(lang_id), language);
    }
    let sym_a = interner.intern_owned_symbol_str("distinct_alpha");
    let sym_b = interner.intern_owned_symbol_str("distinct_beta");
    assert_ne!(sym_a, sym_b);
    assert_eq!(interner.intern_owned_symbol_str("distinct_alpha"), sym_a);

    // 4. Full linker graph edge & coverage parity (cold vs decoded string-table facts).
    let cold_graph: CompactGraph =
        linker::link_compact_with_typed_registry(&cold_catalog, None, None, &registry);
    let decoded_graph: CompactGraph =
        linker::link_compact_with_typed_registry(&decoded_catalog, None, None, &registry);

    let cold_public: acdd_index::models::Graph = cold_graph.into();
    let decoded_public: acdd_index::models::Graph = decoded_graph.into();
    assert_eq!(
        serde_json::to_value(&cold_public.edges).unwrap(),
        serde_json::to_value(&decoded_public.edges).unwrap(),
        "linked graph edges must be identical"
    );
    assert_eq!(
        serde_json::to_value(&cold_public.coverage).unwrap(),
        serde_json::to_value(&decoded_public.coverage).unwrap(),
        "linked graph coverage must be identical"
    );
    assert!(
        cold_public.coverage.iter().any(|c| c.status == "resolved")
            && cold_public
                .coverage
                .iter()
                .any(|c| c.status == "unresolved"),
        "corpus must exercise both resolved and unresolved coverage"
    );

    // Exercise both preparation thresholds with identical catalogs on different pools.
    let mut large = BTreeMap::new();
    let workspace = crate::common::Workspace::new();
    for index in 0..32 {
        let (path, language, source) = if index % 2 == 0 {
            (
                format!("pkg/file{index}.py"),
                "python",
                (0..70)
                    .map(|n| format!("def function_{index}_{n}():\n    return {n}\n"))
                    .collect::<String>(),
            )
        } else {
            (
                format!("src/file{index}.rs"),
                "rust",
                (0..70)
                    .map(|n| format!("pub fn function_{index}_{n}() -> usize {{ {n} }}\n"))
                    .collect::<String>(),
            )
        };
        workspace.write(format!("source/{path}"), &source);
        large.insert(
            path.clone(),
            ast::extract_typed_with_registry(&path, language, &source, &registry).unwrap(),
        );
    }
    let path = "guide.md";
    let sections = acdd_index::docs::extract_with_details(
        path,
        "# Guide\n\n`file0::function_0_0`\n",
        0.0,
        &[],
    )
    .unwrap();
    let (mut doc, _) = sections.into_iter().next().unwrap();
    doc.referenced_symbols = vec!["file0::function_0_0".into()];
    let make_node = |id: &str, kind: &str, details| acdd_index::models::Node {
        native_semantic: None,
        id: id.into(),
        kind: kind.into(),
        name: id.into(),
        qualname: id.into(),
        path: path.into(),
        line: 1,
        end_line: 5,
        is_test: false,
        language: "markdown".into(),
        generated: false,
        details,
    };
    let doc_id = doc.doc_id.clone();
    let document = make_node(
        &doc_id,
        "document",
        serde_json::json!({
            "symbol_claims": [{"name": "file0::function_0_0", "line": 3}]
        }),
    );
    let invariant = make_node("invariant:guide", "invariant", serde_json::json!({}));
    let mut fallback_doc = doc.clone();
    fallback_doc.doc_id = "doc:fallback".into();
    fallback_doc.referenced_symbols = vec!["file2::function_2_0".into()];
    let fallback = make_node(&fallback_doc.doc_id, "document", serde_json::json!({}));
    large.insert(
        path.into(),
        TypedFacts::from_public(Facts {
            nodes: vec![document, invariant, fallback],
            docs: vec![doc, fallback_doc],
            ..Facts::default()
        }),
    );
    assert!(
        large.values().map(|facts| facts.nodes.len()).sum::<usize>()
            >= linker::PARALLEL_LINKER_PREP_MIN_NODES
    );
    let mut catalog = acdd_index::semantic::SymbolCatalog::default();
    for (path, facts) in &large {
        catalog.insert_typed(path.clone(), facts.clone(), false);
    }
    workspace.write(
        "source/guide.md",
        "# Guide\n\n- INV-LARGE: `file0::function_0_0`\n",
    );
    let mut roots = Vec::new();
    let mut results = Vec::new();
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        roots.push(pool.install(|| {
            contextunity_acdd::engine::pipeline::build(
                &workspace.path("source"),
                &workspace.path(format!("index-{threads}.sqlite")),
                None,
            )
            .unwrap()["output_root"]
                .clone()
        }));
        results.push(pool.install(|| {
            let typed: acdd_index::models::Graph =
                linker::link_compact_with_typed_registry(&large, None, None, &registry).into();
            let catalog_graph: acdd_index::models::Graph =
                linker::link_compact_catalog_with_registry(&catalog, None, &registry).into();
            let interner = LinkerInterner::from_catalog(&large);
            let ids: Vec<_> = large.values().flat_map(|facts| facts.nodes.iter())
                .map(|node| interner.lookup_symbol(&node.id).unwrap()).collect();
            (
                serde_json::to_value(typed.edges).unwrap(),
                serde_json::to_value(typed.coverage).unwrap(),
                serde_json::to_value(catalog_graph.edges).unwrap(),
                serde_json::to_value(catalog_graph.coverage).unwrap(),
                ids,
            )
        }));
    }
    assert_eq!(roots[0], roots[1]);
    assert_eq!(results[0], results[1]);
    let edges = results[0].0.as_array().unwrap();
    assert!(edges.iter().filter(|edge| edge["kind"] == "documents").count() >= 2);
    for kind in ["documents", "governs"] {
        assert!(
            edges.iter().any(|edge| edge["kind"] == kind),
            "missing {kind}"
        );
    }

    // Legacy un-tabled Facts::encode compatibility check.
    let mut legacy_raw = Vec::new();
    let sample_facts = TypedFacts::from_public(Facts::default());
    use acdd_index::typed_facts::binary::BinaryEncode;
    sample_facts.encode(&mut legacy_raw);
    let mut legacy_cursor = 0;
    let legacy_decoded =
        TypedFacts::decode_with_string_table(&legacy_raw, &mut legacy_cursor).unwrap();
    assert_eq!(legacy_cursor, legacy_raw.len());
    assert!(legacy_decoded.nodes.is_empty());
}
