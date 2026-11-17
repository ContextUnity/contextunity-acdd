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
        assert_eq!(CompactTag::known(tag), Some(tag), "missing static tag {tag}");
        let compact = CompactTag::from(tag);
        assert!(matches!(compact, CompactTag::Static(_)));
        assert_eq!(compact.as_str(), tag);
        assert_eq!(String::from(compact), tag);
    }
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
        let extracted = ast::extract_typed_with_registry(path, language, source, &registry).unwrap();
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
        cold_public
            .coverage
            .iter()
            .any(|c| c.status == "resolved")
            && cold_public
                .coverage
                .iter()
                .any(|c| c.status == "unresolved"),
        "corpus must exercise both resolved and unresolved coverage"
    );

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
