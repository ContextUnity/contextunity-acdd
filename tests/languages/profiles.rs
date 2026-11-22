use crate::languages::harness::ExtractorHarness;
use crate::languages::PersistedWorkspaceHarness;
use acdd_index::ast;
use std::fs;

#[cfg(feature = "lang-typescript")]
mod legacy_hooks;

#[cfg(all(
    feature = "lang-python",
    feature = "lang-rust",
    feature = "lang-typescript"
))]
mod baseline;
#[cfg(all(feature = "lang-python", feature = "lang-rust"))]
mod cache;

fn persisted_graph(w: &PersistedWorkspaceHarness) -> Vec<Vec<String>> {
    let conn = w.open();
    [
        "SELECT id||'|'||qualname||'|'||details FROM nodes ORDER BY id",
        "SELECT (SELECT id FROM nodes WHERE node_hash=edge_occurrences.src_hash)||'|'||(SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash)||'|'||kind||'|'||(SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)||'|'||line||'|'||(SELECT evidence FROM coverage_evidence WHERE evidence_id=edge_occurrences.confidence_id) FROM edge_occurrences ORDER BY (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id),(SELECT id FROM nodes WHERE node_hash=edge_occurrences.src_hash),(SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash),kind,line",
        "SELECT (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)||'|'||line||'|'||(SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)||'|'||status||'|'||(SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) FROM resolution_coverage ORDER BY (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id),line,(SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id),status,(SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id)",
    ].iter().map(|sql| {
        conn.prepare(sql).unwrap().query_map([], |r|r.get(0)).unwrap().collect::<Result<Vec<String>,_>>().unwrap()
    }).collect()
}

mod workspaces;

#[cfg(feature = "lang-rust")]
mod forwarding;

#[cfg(any(
    feature = "lang-python",
    feature = "lang-rust",
    feature = "lang-typescript"
))]
#[test]
fn module_conventions_are_language_specific() {
    for (path, language, expected) in [
        #[cfg(feature = "lang-python")]
        ("pkg/index.py", "python", "pkg.index"),
        #[cfg(feature = "lang-python")]
        ("pkg/mod.py", "python", "pkg.mod"),
        #[cfg(feature = "lang-python")]
        ("pkg/__init__.py", "python", "pkg"),
        #[cfg(feature = "lang-typescript")]
        ("pkg/index.ts", "typescript", "pkg"),
        #[cfg(feature = "lang-typescript")]
        ("pkg/mod.ts", "typescript", "pkg.mod"),
        #[cfg(feature = "lang-rust")]
        ("pkg/mod.rs", "rust", "pkg"),
        #[cfg(feature = "lang-rust")]
        ("pkg/index.rs", "rust", "pkg.index"),
    ] {
        let f = ExtractorHarness::for_lang(language)
            .at_path(path)
            .extract("");
        assert_eq!(f.nodes[0].qualname, expected, "{path}");
    }
}

mod imports;

#[test]
fn automatic_registry_has_unique_languages_and_extensions() {
    use acdd_index::languages;
    let profiles: Vec<_> = languages::profiles().collect();
    languages::validate_profiles(profiles.iter().copied()).unwrap();
    if let Some(first) = profiles.first() {
        assert!(languages::validate_profiles([*first, *first]).is_err());
    }
    for profile in profiles {
        for extension in profile.extensions() {
            let path = format!("sample.{extension}");
            assert_eq!(
                languages::for_path(std::path::Path::new(&path))
                    .unwrap()
                    .id(),
                profile.id()
            );
            if profile.has_grammar() {
                profile.create_parser(&path).unwrap();
            } else {
                assert!(profile.create_parser(&path).is_err());
            }
        }
    }
}

#[cfg(any(
    feature = "lang-java",
    feature = "lang-csharp",
    feature = "lang-kotlin",
    feature = "lang-php",
    feature = "lang-ruby",
    feature = "lang-c",
    feature = "lang-cpp"
))]
mod popular;

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn declarative_grammar_types_and_profile_linker_matrix() {
    use acdd_index::languages::{
        self, CallForm, DeclarationRole, ImportKind, SyntaxGrammarSpec, Visibility,
    };
    use acdd_index::models::tags::NodeKind;

    for (role, expected_kind) in [
        (DeclarationRole::Function, NodeKind::Function),
        (DeclarationRole::Method, NodeKind::Method),
        (DeclarationRole::Constructor, NodeKind::Constructor),
        (DeclarationRole::Class, NodeKind::Class),
        (DeclarationRole::Struct, NodeKind::Struct),
        (DeclarationRole::Enum, NodeKind::Enum),
        (DeclarationRole::Interface, NodeKind::Interface),
        (DeclarationRole::Trait, NodeKind::Trait),
        (DeclarationRole::Impl, NodeKind::Impl),
        (DeclarationRole::Module, NodeKind::Module),
        (DeclarationRole::Field, NodeKind::Field),
        (DeclarationRole::Property, NodeKind::Property),
        (DeclarationRole::Constant, NodeKind::Constant),
        (DeclarationRole::Variable, NodeKind::Variable),
        (DeclarationRole::Type, NodeKind::Type),
        (DeclarationRole::Namespace, NodeKind::Namespace),
        (DeclarationRole::Macro, NodeKind::Macro),
    ] {
        assert_eq!(role.node_kind(), expected_kind);
        assert_eq!(role.as_str(), expected_kind.as_str());
        assert_eq!(DeclarationRole::parse(role.as_str()), Some(role));
    }
    assert_eq!(DeclarationRole::parse("unknown_kind"), None);

    let spec = SyntaxGrammarSpec {
        declaration_roles: &[("fn_item", DeclarationRole::Function)],
        call_forms: &[("call", CallForm::Direct)],
        import_forms: &[("use", ImportKind::Static)],
        visibility_modifiers: &[("pub", Visibility::Public)],
        typed_bindings: &["let_declaration"],
        constructors: &["new_expr"],
        member_calls: &["method_call"],
        heritage: &["extends_clause"],
        conditionals: &["if_expr"],
        ..SyntaxGrammarSpec::DEFAULT
    };
    assert_eq!(spec.declaration_roles.len(), 1);
    assert_eq!(spec.call_forms[0].1, CallForm::Direct);
    assert_eq!(spec.import_forms[0].1, ImportKind::Static);
    assert_eq!(spec.visibility_modifiers[0].1, Visibility::Public);

    for profile in languages::profiles() {
        let multi_file = profile.linker().allows_multi_file_modules();
        let expected_multi = !matches!(
            profile.id(),
            "python" | "rust" | "typescript" | "javascript" | "vue"
        );
        assert_eq!(multi_file, expected_multi, "profile {}", profile.id());
    }
}

#[cfg(feature = "lang-go")]
#[test]
fn generic_linker_resolves_multi_file_module_prefix_and_external_alias() {
    let w = PersistedWorkspaceHarness::new();
    w.write("go.mod", "module example.com/app\n\ngo 1.22\n");
    w.write("pkg/a.go", "package pkg\n\nfunc Alpha() {}\n");
    w.write("pkg/b.go", "package pkg\n\nfunc Beta() {}\n");
    w.write(
        "cmd/main.go",
        "package main\n\nimport (\n  \"example.com/app/pkg\"\n  ext \"github.com/ext/lib\"\n)\n\nfunc run() {\n  pkg.Alpha()\n  pkg.Beta()\n  ext.Ping()\n}\n",
    );
    w.build();
    let conn = w.open();

    let mut import_stmt = conn
        .prepare(
            "SELECT (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash)
             FROM edge_occurrences
             WHERE (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)='cmd/main.go'
               AND kind='imports'
             ORDER BY 1",
        )
        .unwrap();
    let imported: Vec<String> = import_stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        imported.len(),
        2,
        "prefixed Go multi-file package import must resolve both package files: {imported:?}"
    );
    assert!(imported.iter().any(|dst| dst.contains("pkg/a.go")));
    assert!(imported.iter().any(|dst| dst.contains("pkg/b.go")));

    let mut call_stmt = conn
        .prepare(
            "SELECT (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash)
             FROM edge_occurrences
             WHERE (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)='cmd/main.go'
               AND kind='calls'
             ORDER BY 1",
        )
        .unwrap();
    let calls: Vec<String> = call_stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        calls.iter().any(|dst| dst.contains("pkg/a.go") && dst.contains("Alpha")),
        "expected call to Alpha in pkg/a.go: {calls:?}"
    );
    assert!(
        calls.iter().any(|dst| dst.contains("pkg/b.go") && dst.contains("Beta")),
        "expected call to Beta in pkg/b.go: {calls:?}"
    );

    let external_ping: i64 = count(
        &conn,
        "SELECT count(*) FROM resolution_coverage
         WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='cmd/main.go'
           AND status='external'
           AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='ext.Ping'",
    );
    assert_eq!(
        external_ping, 1,
        "external Go import alias calls must be classified as external"
    );

    #[cfg(feature = "lang-php")]
    {
        let php_ws = PersistedWorkspaceHarness::new();
        php_ws.write(
            "composer.json",
            "{\"autoload\":{\"psr-4\":{\"App\\\\\":\"src/\"}}}\n",
        );
        php_ws.write("src/Service/User.php", "<?php\nclass User {}\n");
        php_ws.write(
            "public/index.php",
            "<?php\nuse App\\Service\\User;\n",
        );
        php_ws.build();
        let php_conn = php_ws.open();
        let resolved: i64 = count(
            &php_conn,
            "SELECT count(*) FROM edge_occurrences
             WHERE (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)='public/index.php'
               AND kind='imports'
               AND (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash) LIKE '%src/Service/User.php%'",
        );
        assert!(resolved >= 1, "PSR-4 composer.json prefix must resolve src/Service/User.php");
    }

    #[cfg(feature = "lang-csharp")]
    {
        let cs_ws = PersistedWorkspaceHarness::new();
        cs_ws.write(
            "App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><RootNamespace>Acme.Core</RootNamespace></PropertyGroup></Project>\n",
        );
        cs_ws.write("Billing.cs", "namespace Acme.Core.Billing { public class Invoice {} }\n");
        cs_ws.write("Program.cs", "using Acme.Core.Billing;\nclass Program {}\n");
        cs_ws.build();
        let cs_conn = cs_ws.open();
        let resolved: i64 = count(
            &cs_conn,
            "SELECT count(*) FROM edge_occurrences
             WHERE (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)='Program.cs'
               AND kind='imports'
               AND (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash) LIKE '%Billing.cs%'",
        );
        assert!(resolved >= 1, ".csproj RootNamespace prefix must resolve Billing.cs");
    }
}

#[cfg(feature = "lang-typescript")]
#[test]
fn file_activation_signals_persist_binary_v4_and_gate_link_time_implicit_receivers() {
    use acdd_index::languages::dependency_registry::DependencyRegistry;
    use acdd_index::linker;
    use acdd_index::models::Facts;
    use acdd_index::typed_facts::{decode_facts, encode_facts, TypedFacts};
    use std::collections::BTreeMap;

    let w = PersistedWorkspaceHarness::new();
    w.write(
        ".acdd/frameworks/alpine-probe.toml",
        "[[syntax_rules]]\nid = \"x-data-implicit\"\nlanguage = \"javascript\"\nactivation = [{ kind = \"syntax_attribute\", name = \"x-data\" }]\noperation = { kind = \"implicit_receivers\", names = [\"$el\"] }\n",
    );
    let registry = DependencyRegistry::collect(Some(w.root()));

    let js_source = "function onClick() {\n  $el();\n}\n";
    let mut active_facts = ExtractorHarness::for_lang("javascript")
        .at_path("active.js")
        .extract(js_source);
    active_facts
        .activation_signals
        .syntax_attributes
        .insert("x-data".into());
    let inactive_facts = ExtractorHarness::for_lang("javascript")
        .at_path("inactive.js")
        .extract(js_source);

    // Round-trip both files through binary v4 encode_facts / decode_facts before linking.
    let active_blob = encode_facts(&TypedFacts::from_public(active_facts)).unwrap();
    let inactive_blob = encode_facts(&TypedFacts::from_public(inactive_facts)).unwrap();
    assert!(active_blob.starts_with(b"ACDD-LOCAL-FACTS\0\x04"));
    assert!(inactive_blob.starts_with(b"ACDD-LOCAL-FACTS\0\x04"));

    let mut facts_map: BTreeMap<String, Facts> = BTreeMap::new();
    facts_map.insert(
        "active.js".into(),
        decode_facts(&active_blob).unwrap().into_public(),
    );
    facts_map.insert(
        "inactive.js".into(),
        decode_facts(&inactive_blob).unwrap().into_public(),
    );

    let graph = linker::link_with_registry(&facts_map, None, Some(w.root()), &registry);
    let cov = |file: &str| {
        graph
            .coverage
            .iter()
            .find(|c| c.path == file && c.expression == "$el")
            .unwrap_or_else(|| panic!("missing coverage row for {file}: {:?}", graph.coverage))
    };
    let active_cov = cov("active.js");
    assert_eq!(
        active_cov.status, "external",
        "active.js with persisted x-data signal must activate ImplicitReceivers at link time: {active_cov:?}"
    );
    assert!(
        active_cov.evidence.contains("configured implicit receiver"),
        "expected configured implicit receiver evidence for active.js: {active_cov:?}"
    );

    let inactive_cov = cov("inactive.js");
    assert_ne!(
        inactive_cov.status, "external",
        "inactive.js without x-data signal must NOT activate ImplicitReceivers at link time: {inactive_cov:?}"
    );
}
