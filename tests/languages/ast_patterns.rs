use acdd_index::{ast, languages};

#[cfg(any(
    feature = "lang-rust",
    feature = "lang-python",
    feature = "lang-typescript",
    feature = "lang-html",
    feature = "lang-vue",
    feature = "lang-toml"
))]
#[test]
fn ast_pattern_capabilities() {
    let mut cases: Vec<(&str, &str, &str, &str)> = Vec::new();

    #[cfg(feature = "lang-rust")]
    {
        cases.extend([
            (
                "rust",
                "src/lib.rs",
                "fn target() { call(value); }",
                "fn target() { call($VALUE); }",
            ),
            (
                "rust",
                "src/lib.rs",
                "fn target() { call(value); }",
                "fn target()",
            ),
            (
                "rust",
                "src/lib.rs",
                "fn target() { call(value); }",
                "fn target",
            ),
            (
                "rust",
                "src/lib.rs",
                "struct Config { field: u32 }",
                "struct Config",
            ),
            ("rust", "src/lib.rs", "enum Status { Ready }", "enum Status"),
            (
                "rust",
                "src/lib.rs",
                "impl Config { fn run() {} }",
                "impl Config",
            ),
            (
                "rust",
                "src/lib.rs",
                "#[derive(Debug)] struct Item { field: u32 }",
                "#[derive(Debug)] struct Item {}",
            ),
        ]);
        let capabilities = languages::require("rust")
            .unwrap()
            .ast_search_capabilities();
        assert!(capabilities.fragment_probe.is_some());
        assert!(capabilities.attribute_kinds.contains(&"attribute_item"));
        for kind in [
            "block",
            "field_declaration_list",
            "enum_variant_list",
            "declaration_list",
        ] {
            assert!(capabilities.declaration_body_kinds.contains(&kind));
        }
    }

    #[cfg(feature = "lang-python")]
    {
        cases.extend([
            (
                "python",
                "service.py",
                "def target():\n    return value\nclass Widget:\n    pass\n",
                "def target():",
            ),
            (
                "python",
                "service.py",
                "def target():\n    return value\nclass Widget:\n    pass\n",
                "def target()",
            ),
            (
                "python",
                "service.py",
                "def target(value):\n    return value\n",
                "def target($$$)",
            ),
            (
                "python",
                "service.py",
                "def target():\n    return value\nclass Widget:\n    pass\n",
                "class Widget:",
            ),
            (
                "python",
                "service.py",
                "def target():\n    return value\nclass Widget:\n    pass\n",
                "class Widget",
            ),
            (
                "python",
                "service.py",
                "def use(client):\n    client.fetch(value)\n",
                "client.fetch($ARG)",
            ),
            (
                "python",
                "service.py",
                "@property\ndef value(self):\n    return 1\n",
                "@property\ndef value(self):\n    return 1",
            ),
        ]);
        let capabilities = languages::require("python")
            .unwrap()
            .ast_search_capabilities();
        assert!(capabilities.attribute_kinds.contains(&"decorator"));
        assert!(capabilities.declaration_body_kinds.contains(&"block"));
    }

    #[cfg(feature = "lang-typescript")]
    {
        cases.extend([
            (
                "typescript",
                "service.ts",
                "function target() { return value; }",
                "function target()",
            ),
            (
                "typescript",
                "service.ts",
                "function use(client) { client.fetch(value); }",
                "client.fetch($ARG)",
            ),
            (
                "typescript",
                "service.ts",
                "class Widget { run() {} }",
                "class Widget",
            ),
            (
                "typescript",
                "service.ts",
                "interface Contract { value: string }",
                "interface Contract",
            ),
            (
                "typescript",
                "service.ts",
                "@sealed class Widget {}",
                "@sealed class Widget {}",
            ),
        ]);
        let capabilities = languages::require("typescript")
            .unwrap()
            .ast_search_capabilities();
        assert!(capabilities.attribute_kinds.contains(&"decorator"));
        assert!(capabilities
            .declaration_body_kinds
            .contains(&"statement_block"));
        assert!(capabilities.declaration_body_kinds.contains(&"class_body"));
        assert!(capabilities
            .declaration_body_kinds
            .contains(&"interface_body"));
    }

    #[cfg(feature = "lang-html")]
    {
        cases.push((
            "html",
            "index.html",
            "<main><button disabled></button></main>",
            "<main><button disabled></button></main>",
        ));
        let capabilities = languages::require("html")
            .unwrap()
            .ast_search_capabilities();
        assert!(capabilities.attribute_kinds.contains(&"attribute"));
        assert!(capabilities.fragment_probe.is_none());
    }

    #[cfg(feature = "lang-vue")]
    {
        cases.push((
            "vue",
            "src/Widget.vue",
            "export function target() { return 1; }",
            "export function target() { return 1; }",
        ));
        cases.push((
            "vue",
            "src/Widget.vue",
            "export function use(client) { client.fetch(value); }",
            "client.fetch($ARG)",
        ));
        let capabilities = languages::require("vue").unwrap().ast_search_capabilities();
        assert!(capabilities.attribute_kinds.contains(&"decorator"));
    }

    struct Case<'a> {
        case_id: &'a str,
        language: &'a str,
        path: &'a str,
        source: &'a str,
        pattern: &'a str,
        count: Option<usize>,
        text: Option<&'a str>,
        prefix: Option<&'a str>,
        capture: Option<(&'a str, &'a str)>,
    }
    let mut cases: Vec<_> = cases
        .into_iter()
        .map(|(language, path, source, pattern)| Case {
            case_id:
                "ast_search_profile_capabilities_cover_complete_declaration_and_attribute_patterns",
            language,
            path,
            source,
            pattern,
            count: None,
            text: None,
            prefix: None,
            capture: None,
        })
        .collect();
    #[cfg(feature = "lang-rust")]
    {
        cases.push(Case {
            case_id: "ast_search_matches_root_rejected_rust_method_call_fragment",
            language: "rust",
            path: "src/lib.rs",
            source: "fn invoke() { client.fetch(value); }",
            pattern: "client.fetch($ARG)",
            count: Some(1),
            text: Some("client.fetch(value)"),
            prefix: None,
            capture: Some(("ARG", "value")),
        });
        cases.push(Case {
            case_id: "ast_search_matches_root_rejected_rust_macro_statement_fragment",
            language: "rust",
            path: "src/lib.rs",
            source: "fn run() { let value = print!(\"ready\"); }",
            pattern: "let value = print!(\"ready\");",
            count: Some(1),
            text: Some("let value = print!(\"ready\");"),
            prefix: None,
            capture: None,
        });
        let source = concat!(
            "mod rust;\n",
            "pub struct Foo { pub field: usize }\n",
            "pub trait Bar { fn run(&self); }\n",
            "impl Foo { pub fn new() -> Self { Self { field: 1 } } }\n",
            "fn local() { let value = 42; }\n"
        );
        for (pattern, prefix) in [
            ("pub struct Foo { $$$ }", "pub struct Foo"),
            ("pub trait Bar { $$$ }", "pub trait Bar"),
            ("impl Foo { $$$ }", "impl Foo"),
            ("mod rust", "mod rust;"),
            ("let value = $EXPR", "let value = 42;"),
        ] {
            cases.push(Case {
                case_id:
                    "ast_search_matches_rust_wildcard_declaration_bodies_and_semicolonless_items",
                language: "rust",
                path: "src/lib.rs",
                source,
                pattern,
                count: Some(1),
                text: None,
                prefix: Some(prefix),
                capture: None,
            });
        }
    }
    #[cfg(feature = "lang-toml")]
    {
        let capabilities = languages::require("toml")
            .unwrap()
            .ast_search_capabilities();
        assert!(capabilities.declaration_kinds.is_empty());
        assert!(capabilities.fragment_probe.is_none());
        cases.push(Case {
            case_id: "ast_search_profiles_with_only_complete_syntax_use_the_shared_default",
            language: "toml",
            path: "Cargo.toml",
            source: "edition = \"2021\"",
            pattern: "edition = \"2021\"",
            count: Some(1),
            text: None,
            prefix: None,
            capture: None,
        });
    }
    for case in cases {
        let mut matcher = ast::search::PreparedAstMatcher::new(case.language, case.pattern)
            .unwrap_or_else(|error| panic!("{}: {}: {error}", case.case_id, case.pattern));
        let matches = matcher
            .search_page(
                case.source,
                case.path,
                0,
                10,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            )
            .unwrap_or_else(|error| panic!("{}: {}: {error}", case.case_id, case.pattern))
            .items;
        assert!(!matches.is_empty(), "{}: {}", case.case_id, case.pattern);
        if let Some(count) = case.count {
            assert_eq!(matches.len(), count, "{}", case.case_id);
        }
        if let Some(text) = case.text {
            assert_eq!(matches[0]["text"], text, "{}", case.case_id);
        }
        if let Some(prefix) = case.prefix {
            assert!(
                matches[0]["text"].as_str().unwrap().starts_with(prefix),
                "{}: {:?}",
                case.case_id,
                matches
            );
        }
        if let Some((name, value)) = case.capture {
            assert_eq!(matches[0]["captures"][name], value, "{}", case.case_id);
        }
    }
}
