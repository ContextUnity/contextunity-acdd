#[path = "common/mod.rs"]
pub mod common;
#[path = "linker/callback_contracts.rs"]
mod callback_contracts;
#[path = "linker/early_interning_and_compact_facts.rs"]
mod early_interning_and_compact_facts;
#[path = "linker/receivers/inherited.rs"]
mod inherited_receivers;
#[path = "linker/method_semantics.rs"]
mod method_semantics;
#[path = "linker/optimizations.rs"]
mod optimizations;
#[path = "linker/overload_disambiguation.rs"]
mod overload_disambiguation;
#[path = "linker/public_output.rs"]
mod public_output;
#[path = "linker/receivers/typed.rs"]
mod typed_receivers;
#[path = "linker/value_flow.rs"]
mod value_flow;

#[cfg(feature = "lang-python")]
mod local_field_reference_contract {
    use acdd_index::ast;

    fn linked_graph(source: &str) -> acdd_index::models::Graph {
        let facts = ast::extract("service.py", "python", source).unwrap();
        let all = std::collections::BTreeMap::from([("service.py".to_owned(), facts)]);
        crate::common::link_facts(&all)
    }

    #[test]
    fn same_class_field_references_retain_exact_resolution() {
        let graph = linked_graph(
            "class Host:\n    request = None\n    def current(self): return self.request\n",
        );
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == "self.request")
            .unwrap();
        assert_eq!(coverage.status, "resolved", "{coverage:?}");
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == "references" && edge.evidence == "self.request")
            .unwrap();
        assert_eq!(edge.confidence, "exact", "{edge:?}");
    }

    #[test]
    fn constructor_assigned_field_reads_keep_field_identity() {
        let source = "class Host:\n    def __init__(self, request):\n        self.request = request\n    def current(self):\n        return self.request\n";
        let graph = linked_graph(source);
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == "self.request" && coverage.line == 5)
            .unwrap();
        assert_eq!(coverage.status, "resolved", "{coverage:?}");
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.kind == "references" && edge.evidence == "self.request" && edge.line == 5
            })
            .unwrap();
        assert_eq!(
            edge.dst, "field:service.py:3:service.Host.request",
            "{edge:?}"
        );
    }

    #[test]
    fn field_values_do_not_become_callable_targets() {
        let graph = linked_graph(
            "class Host:\n    def __init__(self, request):\n        self.request = request\n    def current(self):\n        return self.request()\n",
        );
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == "self.request" && coverage.line == 5)
            .unwrap();
        assert_eq!(coverage.status, "unresolved", "{coverage:?}");
        assert!(!graph
            .edges
            .iter()
            .any(|edge| { edge.kind == "calls" && edge.evidence == "self.request()" }));
    }
}

#[cfg(feature = "lang-rust")]
mod self_crate_resolution_tests {
    use acdd_core::fs::ScopedTempDir;
    use acdd_index::ast::extract;
    use acdd_index::languages::dependency_registry::DependencyRegistry;
    use acdd_index::linker;
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn rust_self_crate_resolution_excludes_library_files_and_keeps_sibling_targets() {
        let dir = ScopedTempDir::create("contextunity-test-rust-self-crate").unwrap();
        let root = dir.path();

        for (pkg_prefix, crate_name) in [("", "my_crate"), ("crates/sub_crate/", "sub_crate")] {
            fs::create_dir_all(root.join(format!("{pkg_prefix}src/bin"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}tests"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}examples"))).unwrap();
            fs::create_dir_all(root.join(format!("{pkg_prefix}benches"))).unwrap();
            fs::write(
                root.join(format!("{pkg_prefix}Cargo.toml")),
                format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }

        let files = [
            (
                "src/lib.rs",
                "pub mod sub;\npub fn target() {}\npub fn bad_self_call() { my_crate::target(); }\npub fn good_crate_call() { crate::target(); }\n",
            ),
            (
                "src/sub.rs",
                "use my_crate::target as imported_target;\npub fn bad_sub_direct() { my_crate::target(); }\npub fn bad_sub_use() { imported_target(); }\n",
            ),
            ("src/main.rs", "fn main_calls() { my_crate::target(); }\n"),
            ("src/bin/cli.rs", "fn bin_calls() { my_crate::target(); }\n"),
            ("tests/it.rs", "#[test]\nfn test_calls() { my_crate::target(); }\n"),
            ("examples/demo.rs", "fn example_calls() { my_crate::target(); }\n"),
            ("benches/perf.rs", "fn bench_calls() { my_crate::target(); }\n"),
            (
                "crates/sub_crate/src/lib.rs",
                "pub fn target() {}\npub fn bad_subcrate_self() { sub_crate::target(); }\n",
            ),
            (
                "crates/sub_crate/tests/it.rs",
                "#[test]\nfn subcrate_test_calls() { sub_crate::target(); }\n",
            ),
        ];

        let mut facts = BTreeMap::new();
        for &(rel, src) in &files {
            fs::write(root.join(rel), src).unwrap();
            facts.insert(rel.to_owned(), extract(rel, "rust", src).unwrap());
        }
        let registry = DependencyRegistry::collect(Some(root));
        let graph = linker::link_with_registry(&facts, None, Some(root), &registry);

        let has_call = |src_path: &str, src_fn: &str, dst_path: &str| {
            let src_id = &facts[src_path]
                .nodes
                .iter()
                .find(|n| n.name == src_fn)
                .unwrap()
                .id;
            let dst_id = &facts[dst_path]
                .nodes
                .iter()
                .find(|n| n.name == "target")
                .unwrap()
                .id;
            graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && &e.src == src_id && &e.dst == dst_id)
        };

        for (src_path, src_fn, dst_path, expect_linked, label) in [
            ("src/lib.rs", "bad_self_call", "src/lib.rs", false, "src/lib.rs must reject my_crate::target()"),
            ("src/lib.rs", "good_crate_call", "src/lib.rs", true, "src/lib.rs crate::target() links"),
            ("src/sub.rs", "bad_sub_direct", "src/lib.rs", false, "src/sub.rs must reject my_crate::target()"),
            ("src/sub.rs", "bad_sub_use", "src/lib.rs", false, "src/sub.rs must reject use my_crate::target"),
            ("src/main.rs", "main_calls", "src/lib.rs", true, "src/main.rs my_crate::target() links"),
            ("src/bin/cli.rs", "bin_calls", "src/lib.rs", true, "src/bin/cli.rs my_crate::target() links"),
            ("tests/it.rs", "test_calls", "src/lib.rs", true, "tests/it.rs my_crate::target() links"),
            ("examples/demo.rs", "example_calls", "src/lib.rs", true, "examples/demo.rs my_crate::target() links"),
            ("benches/perf.rs", "bench_calls", "src/lib.rs", true, "benches/perf.rs my_crate::target() links"),
            ("crates/sub_crate/src/lib.rs", "bad_subcrate_self", "crates/sub_crate/src/lib.rs", false, "subcrate src/lib.rs must reject sub_crate::target()"),
            ("crates/sub_crate/tests/it.rs", "subcrate_test_calls", "crates/sub_crate/src/lib.rs", true, "subcrate tests/it.rs sub_crate::target() links"),
        ] {
            assert_eq!(has_call(src_path, src_fn, dst_path), expect_linked, "{label}");
        }
    }
}
