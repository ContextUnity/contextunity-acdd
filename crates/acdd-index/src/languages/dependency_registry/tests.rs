use super::parser::filename_matches;
use super::*;

fn source_registry(files: &[(&str, &str)]) -> Result<DependencyRegistry> {
    let manifests: Vec<_> = files
        .iter()
        .map(|&(path, source)| ManifestSource { path, source })
        .collect();
    DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
        name: "",
        manifests: &manifests,
    }])
}

#[test]
fn source_inventory_rejects_noncanonical_and_unsupported_paths() {
    for path in [
        "",
        "/package.json",
        "../package.json",
        "a/../package.json",
        "./package.json",
        "a//package.json",
        "a/./package.json",
        "a\\package.json",
        "[other]/package.json",
        "a/[other]/package.json",
        "C:/package.json",
        "package.json\0",
        "package.json?x",
        "unknown.txt",
        ".acdd/frameworks/pack.json",
        ".acdd/frameworks/nested/pack.toml",
    ] {
        assert!(source_registry(&[(path, "")]).is_err(), "{path:?}");
    }
    for name in ["../other", "[other]", "a/b", "two words", "", "other"] {
        let inventories = [
            WorkspaceManifestSources {
                name,
                manifests: &[],
            },
            WorkspaceManifestSources {
                name,
                manifests: &[],
            },
        ];
        assert!(
            DependencyRegistry::try_collect_from_sources(inventories).is_err(),
            "{name:?}"
        );
    }
}

#[test]
fn source_inventory_rejects_duplicate_paths_and_pack_identities() {
    for files in [
        vec![("package.json", "{}"), ("package.json", "{}")],
        vec![
            (".acdd/frameworks/local.toml", "enabled=false"),
            (".acdd/frameworks/local.yaml", "enabled: false"),
        ],
    ] {
        assert!(source_registry(&files).is_err(), "{files:?}");
    }
}

#[test]
fn source_inventory_enforces_framework_parser_and_byte_limit() {
    for (extension, source) in [
        ("toml", "extend=true\nreplace=true"),
        ("toml", "enabled='false'"),
        ("toml", "syntax_rules=["),
        ("yaml", "enabled: 0"),
        ("yml", "syntax_rules: ["),
        ("toml", "receivers=[]"),
    ] {
        assert!(
            source_registry(&[(&format!(".acdd/frameworks/local.{extension}"), source)]).is_err(),
            "{source}"
        );
    }
    for bytes in [4 * 1024 * 1024 - 1, 4 * 1024 * 1024, 4 * 1024 * 1024 + 1] {
        let source = format!("#{}", "x".repeat(bytes - 1));
        assert_eq!(
            source_registry(&[(".acdd/frameworks/local.toml", &source)]).is_ok(),
            bytes <= 4 * 1024 * 1024,
            "{bytes}"
        );
    }
}

#[cfg(feature = "lang-python")]
#[test]
fn source_scopes_and_local_tombstones_preserve_bundle_activation() {
    let dependency = ("requirements.txt", "django\n");
    let bundled = source_registry(&[dependency, ("nested/requirements.txt", "")]).unwrap();
    let admits = |registry: &DependencyRegistry, path| {
        registry
            .framework_manifest_for_path(LanguageFamily("python"), path, "django", "django")
            .is_some()
    };
    assert!(admits(&bundled, "app.py"));
    assert!(!admits(&bundled, "nested/app.py"));
    for (extension, disabled, enabled) in [
        ("toml", "enabled=false", "enabled=true"),
        ("yaml", "enabled: false", "enabled: true"),
        ("yml", "enabled: false", "enabled: true"),
    ] {
        let path = format!(".acdd/frameworks/django.{extension}");
        let disabled = source_registry(&[dependency, (&path, disabled)]).unwrap();
        assert!(!admits(&disabled, "app.py"));
        let restored = source_registry(&[dependency, (&path, enabled)]).unwrap();
        assert!(admits(&restored, "app.py"));
        assert_ne!(disabled.digest(), restored.digest());
        assert!(admits(&bundled, "app.py"));
        assert!(!admits(&disabled, "app.py"));
    }
    let empty = source_registry(&[]).unwrap();
    assert!(!admits(&empty, "app.py"));
    let local = source_registry(&[(".acdd/frameworks/django.toml", "")]).unwrap();
    assert!(admits(&local, "app.py"));
    let deleted = source_registry(&[dependency]).unwrap();
    assert!(admits(&deleted, "app.py"));
}

#[cfg(feature = "lang-python")]
#[test]
fn source_local_rules_merge_and_replace_bundled_rules() {
    let local_rule = "[[route_calls]]\ncallee='LocalRoute'\nmethod='GET'\npath_argument=0\nhandler_argument=1\n";
    let registry = |mode: &str| {
        source_registry(&[
            ("requirements.txt", "django"),
            (
                ".acdd/frameworks/django.toml",
                &format!("{mode}{local_rule}"),
            ),
        ])
        .unwrap()
    };
    let bundled = DependencyRegistry::collect(None);
    let bundled_count = bundled.standard_framework_manifests["django"]
        .route_calls
        .len();
    let merged = registry("extend=true\n");
    let replaced = registry("replace=true\n");
    assert_eq!(
        merged
            .effective_framework_manifest("app.py", "django")
            .unwrap()
            .route_calls
            .len(),
        bundled_count + 1
    );
    assert_eq!(
        replaced
            .effective_framework_manifest("app.py", "django")
            .unwrap()
            .route_calls
            .len(),
        1
    );
    assert_ne!(merged.digest(), replaced.digest());
}

#[cfg(feature = "lang-python")]
#[test]
fn source_registries_remain_independent_across_threads() {
    let mut manifest = "enabled=true".to_owned();
    let retained = source_registry(&[(".acdd/frameworks/django.toml", &manifest)]).unwrap();
    manifest.replace_range(.., "enabled=false");
    let changed = source_registry(&[(".acdd/frameworks/django.toml", &manifest)]).unwrap();
    assert!(changed
        .effective_framework_manifest("app.py", "django")
        .is_none());
    std::thread::scope(|scope| {
        let disabled = scope.spawn(|| {
            source_registry(&[(".acdd/frameworks/django.toml", "enabled=false")]).unwrap()
        });
        let enabled = scope.spawn(|| {
            source_registry(&[(".acdd/frameworks/django.toml", "enabled=true")]).unwrap()
        });
        assert!(disabled
            .join()
            .unwrap()
            .effective_framework_manifest("app.py", "django")
            .is_none());
        assert!(enabled
            .join()
            .unwrap()
            .effective_framework_manifest("app.py", "django")
            .is_some());
    });
    assert!(retained
        .effective_framework_manifest("app.py", "django")
        .is_some());
}

#[cfg(feature = "lang-python")]
#[test]
fn source_registry_digests_bind_paths_and_bytes_without_input_order() {
    let first = ("requirements.txt", "django\n");
    let second = ("nested/requirements.txt", "");
    let registry = source_registry(&[first, second]).unwrap();
    assert_eq!(
        registry.digest(),
        source_registry(&[second, first]).unwrap().digest()
    );
    for files in [
        vec![("requirements.txt", "django\n# changed bytes\n"), second],
        vec![("other/requirements.txt", "django\n"), second],
    ] {
        assert_ne!(registry.digest(), source_registry(&files).unwrap().digest());
    }
}

#[cfg(all(feature = "lang-python", feature = "lang-typescript"))]
#[test]
fn source_registry_validates_named_workspace_scope_before_rule_preparation() {
    let rule = serde_json::json!({"syntax_rules":[{"id":"named","language":"python","operation":{"kind":"named_binding","domain":"paint","binding":{"kind":"call_reference","callee":{"kind":"imported","module":"tablekit","export":"Column"},"key":{"kind":"keyword","name":"renderer"},"provider_scopes":[{"workspace":"frontend","project":"","package":"runtime"}]}}}]});
    let source = toml::to_string(&rule).unwrap();
    let main = [ManifestSource {
        path: ".acdd/frameworks/local.toml",
        source: &source,
    }];
    let linked = [ManifestSource {
        path: "package.json",
        source: "{\"name\":\"runtime\"}",
    }];
    let inventory = |name| WorkspaceManifestSources {
        name,
        manifests: &linked,
    };
    let primary = || WorkspaceManifestSources {
        name: "",
        manifests: &main,
    };
    assert!(DependencyRegistry::try_collect_from_sources([primary()]).is_err());
    let registry =
        DependencyRegistry::try_collect_from_sources([primary(), inventory("frontend")]).unwrap();
    let provider = super::super::manifests::projections::NamedProviderScope {
        workspace: "frontend".into(),
        project: "".into(),
        package: "runtime".into(),
    };
    assert_eq!(registry.named_provider_scope(&provider), Some("[frontend]"));
    assert_eq!(
        registry.digest(),
        DependencyRegistry::try_collect_from_sources([inventory("frontend"), primary()])
            .unwrap()
            .digest()
    );
    assert!(registry
        .framework_manifest_for_path(
            LanguageFamily("python"),
            "[frontend]/app.py",
            "local",
            "tablekit"
        )
        .is_none());
}

#[cfg(feature = "lang-python")]
#[test]
fn dependency_registry_digest_is_stable_across_worktrees() {
    let temporary = acdd_core::fs::ScopedTempDir::create("acdd-cross-worktree-registry").unwrap();
    let first_root = temporary.path().join("commerce");
    let second_root = temporary.path().join("commerce-worktree-b");
    for root in [&first_root, &second_root] {
        fs::create_dir_all(root.join(".acdd/frameworks")).unwrap();
        fs::write(
            root.join("pyproject.toml"),
            "[project]\nname='commerce-fixture'\ndependencies=['django']\n",
        )
        .unwrap();
        fs::write(
            root.join(".acdd/frameworks/django.toml"),
            "replace = true\n",
        )
        .unwrap();
        #[cfg(feature = "lang-rust")]
        {
            fs::create_dir_all(root.join("src")).unwrap();
            fs::create_dir_all(root.join("tests")).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname='commerce-fixture'\nversion='0.1.0'\n",
            )
            .unwrap();
            fs::write(root.join("src/lib.rs"), "pub mod api;\n").unwrap();
            fs::write(root.join("src/api.rs"), "pub fn route() {}\n").unwrap();
        }
    }

    let first = DependencyRegistry::try_collect(&first_root, None, None).unwrap();
    let second = DependencyRegistry::try_collect(&second_root, None, None).unwrap();
    assert_eq!(
        first.digest(),
        second.digest(),
        "identical manifests must produce one registry digest across worktree roots"
    );

    fs::write(
        first_root.join("pyproject.toml"),
        "[project]\nname='commerce-fixture'\ndependencies=['django', 'fastapi']\n",
    )
    .unwrap();
    let changed = DependencyRegistry::try_collect(&first_root, None, None).unwrap();
    assert_ne!(
        changed.digest(),
        second.digest(),
        "manifest content changes must produce a new registry digest"
    );

    fs::create_dir_all(first_root.join("src")).unwrap();
    fs::create_dir_all(first_root.join("unindexed")).unwrap();
    fs::write(first_root.join("acdd.yaml"), "roots: [src]\n").unwrap();
    fs::write(
        first_root.join("unindexed/requirements.txt"),
        "outside_source_roots>=1\n",
    )
    .unwrap();
    let adapter = crate::scanner::load_adapter(&first_root, None).unwrap();
    let cold = DependencyRegistry::try_collect_with_paths(&first_root, Some(&adapter), &[]).unwrap();
    let admission =
        DependencyRegistry::try_collect_with_scan_config(&first_root, Some(&adapter)).unwrap();
    assert_eq!(
        cold.digest(),
        admission.digest(),
        "cold and admission discovery must exclude manifests outside configured source roots"
    );
    fs::write(
        first_root.join("unindexed/requirements.txt"),
        "outside_source_roots>=2\n",
    )
    .unwrap();
    let unchanged =
        DependencyRegistry::try_collect_with_scan_config(&first_root, Some(&adapter)).unwrap();
    assert_eq!(cold.digest(), unchanged.digest());
}

#[cfg(feature = "lang-python")]
#[test]
fn project_framework_packs_merge_replace_and_activate() {
    use super::super::manifests::{syntax::SyntaxOperation, FrameworkManifest};

    const INLINE_RULES_TOML: &str = r#"
enabled = true
[[class_hierarchy_receivers]]
bases = ["django.custom.Base"]
provider = "django.custom.Base"
members = ["deliver"]
returns = { deliver = { kind = "self" } }
[[route_decorators]]
callee = "portal.expose"
method = "POST"
path_argument = 0
receiver_names = []
allow_relative_path = false
[[route_calls]]
callee = "register_endpoint"
method = "GET"
path_argument = 0
handler_argument = 1
receiver_names = []
allow_relative_path = false
[[di_calls]]
callee = "InjectService"
argument = 0
[[semantic_rules]]
id = "portal-render"
namespace = "templates"
scope = "project"
activation = []
operation = { kind = "call_to_file", language = "python", callee = { kind = "imported", module = "portal.responses", export = "render_file" }, argument = 1, target = { kind = "project_path_suffix", prefix = "templates/", language = "html" }, relation = "renders_template" }
"#;
    const INLINE_RULES_YAML: &str = r#"
enabled: true
class_hierarchy_receivers:
  - bases: [django.custom.Base]
    provider: django.custom.Base
    members: [deliver]
    returns: {deliver: {kind: self}}
route_decorators:
  - callee: portal.expose
    method: POST
    path_argument: 0
    receiver_names: []
    allow_relative_path: false
route_calls:
  - callee: register_endpoint
    method: GET
    path_argument: 0
    handler_argument: 1
    receiver_names: []
    allow_relative_path: false
di_calls:
  - callee: InjectService
    argument: 0
semantic_rules:
  - id: portal-render
    namespace: templates
    scope: project
    activation: []
    operation:
      kind: call_to_file
      language: python
      callee: {kind: imported, module: portal.responses, export: render_file}
      argument: 1
      target: {kind: project_path_suffix, prefix: templates/, language: html}
      relation: renders_template
"#;

    let mut documented_examples = Vec::new();
    for (extension, source) in [
        ("toml", INLINE_RULES_TOML),
        ("yaml", INLINE_RULES_YAML),
    ] {
        let parsed = FrameworkManifest::parse("django".to_owned(), extension, source)
            .unwrap_or_else(|error| {
                panic!("invalid documented {extension} example: {error:#}")
            });
        assert_eq!(parsed.class_hierarchy_receivers.len(), 1, "{extension}");
        assert_eq!(parsed.route_decorators.len(), 1, "{extension}");
        assert_eq!(parsed.route_calls.len(), 1, "{extension}");
        assert_eq!(parsed.di_calls.len(), 1, "{extension}");
        assert_eq!(parsed.semantic_rules.len(), 1, "{extension}");
        documented_examples.push(parsed);
    }
    assert_eq!(
        documented_examples[0], documented_examples[1],
        "complete documented TOML/YAML payloads, including semantic rules, must agree"
    );
    let gridviewspec = FrameworkManifest::parse(
        "gridviewspec".to_owned(),
        "toml",
        include_str!("../manifests/gridviewspec.toml"),
    )
    .unwrap_or_else(|error| panic!("invalid bundled GridViewSpec manifest: {error:#}"));
    assert_eq!(gridviewspec.class_hierarchy_receivers.len(), 28);

    let temporary = acdd_core::fs::ScopedTempDir::create("acdd-framework-packs").unwrap();
    let semantic_root = temporary.path().join("semantic-replacement");
    fs::create_dir_all(semantic_root.join(".acdd/frameworks")).unwrap();
    fs::write(
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
            fs::write(&replacement, "replace = true\n").unwrap();
        } else if phase == "restored" {
            fs::remove_file(&replacement).unwrap();
        }
        let registry = DependencyRegistry::try_collect_with_paths(&semantic_root, None, &[]).unwrap();
        let manifest = registry
            .framework_manifest_for_path(LanguageFamily("python"), "views.py", "django", "django")
            .expect("package/project pack remains available");
        let _ = manifest;
        let extracted = crate::ast::extract_typed_with_registry(
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
                crate::typed_facts::SemanticState::Valid { facts, .. } => facts.iter(),
                crate::typed_facts::SemanticState::Invalid { .. } => {
                    panic!("{phase}: canonical native semantic facts must remain valid")
                }
            })
            .filter(|fact| {
                matches!(
                    fact.role,
                    crate::semantic::SemanticFactRole::CallToFile { .. }
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
                    serde_json::from_value::<super::super::semantic::SemanticFact>(malformed)
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
    let mut documented = Vec::new();
    for block in include_str!("../../../../../docs/reference/framework-manifests.md").split("```")
    {
        let Some((extension @ ("toml" | "yaml"), source)) = block.split_once('\n') else {
            continue;
        };
        if source.contains("call_to_file") {
            documented.push(
                FrameworkManifest::parse("documented".into(), extension, source).unwrap(),
            );
        }
    }
    assert_eq!(
        documented.len(),
        2,
        "both complete shipped semantic examples parse"
    );
    let valid = serde_json::to_value(&documented[0]).unwrap();
    for extension in ["toml", "yaml", "yml"] {
        let replacement = semantic_root.join(format!(".acdd/frameworks/django.{extension}"));
        for (pointer, value) in [
            ("/unexpected", true.into()),
            ("/semantic_rules/0/unexpected", true.into()),
            ("/semantic_rules/0/scope", "cross_language_project".into()),
            ("/semantic_rules/0/namespace", "".into()),
            ("/semantic_rules/0/activation/0/unexpected", true.into()),
            ("/semantic_rules/0/activation/0/kind", "import".into()),
            ("/semantic_rules/0/operation/unexpected", true.into()),
            ("/semantic_rules/0/operation/kind", "member_provider".into()),
            ("/semantic_rules/0/operation/language", "javascript".into()),
            ("/semantic_rules/0/operation/argument", (-1).into()),
            ("/semantic_rules/0/operation/callee/unexpected", true.into()),
            ("/semantic_rules/0/operation/callee/kind", "global".into()),
            (
                "/semantic_rules/0/operation/callee/module",
                "django..shortcuts".into(),
            ),
            ("/semantic_rules/0/operation/target/unexpected", true.into()),
            ("/semantic_rules/0/operation/target/kind", "basename".into()),
            (
                "/semantic_rules/0/operation/target/language",
                "python".into(),
            ),
            (
                "/semantic_rules/0/operation/target/prefix",
                "../templates/".into(),
            ),
            (
                "/semantic_rules/0/operation/target/prefix",
                "/templates/".into(),
            ),
            (
                "/semantic_rules/0/operation/target/prefix",
                "templates".into(),
            ),
            ("/semantic_rules/0/operation/relation", "calls".into()),
        ] {
            let mut invalid = valid.clone();
            invalid["replace"] = true.into();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            invalid
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            let source = if extension == "toml" {
                toml::to_string(&invalid).unwrap()
            } else {
                serde_yaml::to_string(&invalid).unwrap()
            };
            fs::write(&replacement, source).unwrap();
            let error = DependencyRegistry::try_collect_with_paths(&semantic_root, None, &[])
                .err()
                .expect("malformed replacement must fail instead of resurrecting the bundle");
            assert!(
                format!("{error:#}").contains(&replacement.display().to_string()),
                "actionable source diagnostic: {error:#}"
            );
        }
        fs::remove_file(replacement).unwrap();
    }
    let mut bundled = documented[0].clone();
    let mut project = documented[0].clone();
    project.semantic_rules[0].namespace = "override".into();
    let mut independent = bundled.semantic_rules[0].clone();
    independent.id = "independent".into();
    bundled.semantic_rules.push(independent);
    let merged = project.with_bundled_rules(&bundled);
    assert_eq!(merged.semantic_rules.len(), 2);
    assert_eq!(merged.semantic_rules[0].namespace, "override");
    let mut duplicate = valid.clone();
    let rule = duplicate["semantic_rules"][0].clone();
    duplicate["semantic_rules"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    assert!(FrameworkManifest::parse(
        "duplicate".into(),
        "toml",
        &toml::to_string(&duplicate).unwrap()
    )
    .unwrap_err()
    .to_string()
    .contains("duplicate semantic rule id"));
    let family = LanguageFamily("python");
    let mut failures = Vec::new();
    for extension in ["toml", "yaml", "yml"] {
        for (mode, replace) in [("default", false), ("extend", false), ("replace", true)] {
            for declared in [false, true] {
                let workspace = temporary.path().join(format!("{extension}-{mode}-{declared}"));
                let root = workspace.as_path();
                let directory = root.join(".acdd/frameworks");
                fs::create_dir_all(&directory).unwrap();
                if declared {
                    fs::write(root.join("requirements.txt"), "django>=5\n").unwrap();
                }
                let directive = match (extension, mode) {
                    (_, "default") => "",
                    ("toml", "extend") => "extend = true\n",
                    ("toml", "replace") => "replace = true\n",
                    (_, "extend") => "extend: true\n",
                    (_, "replace") => "replace: true\n",
                    _ => unreachable!(),
                };
                let rules = if extension == "toml" {
                    r#"[[syntax_rules]]
 id = "project-template-symbols"
 language = "html"
  operation = { kind = "template_symbols", requires_loader = false, tags = ["project", "if"], filters = ["project", "upper"], libraries = [], library_routes = { project_route = "project_catalog" } }
[[class_hierarchy_receivers]]
bases = ["django.custom.Base"]
provider = "django.custom.Base"
members = ["deliver"]
[[class_hierarchy_receivers]]
bases = ["django.core.management.base.BaseCommand"]
provider = "project.management.BaseCommand"
members = ["write"]
[[route_decorators]]
callee = "portal.expose"
method = "POST"
path_argument = 0
[[route_calls]]
callee = "register_endpoint"
method = "GET"
path_argument = 0
handler_argument = 1
[[route_calls]]
callee = "path"
method = "ANY"
path_argument = 0
handler_argument = 2
[[di_calls]]
callee = "InjectService"
argument = 0
"#
                } else {
                    r#"syntax_rules:
  - id: project-template-symbols
    language: html
    operation:
      kind: template_symbols
      requires_loader: false
      tags: [project, if]
      filters: [project, upper]
      libraries: []
      library_routes: { project_route: project_catalog }
class_hierarchy_receivers:
  - bases: [django.custom.Base]
    provider: django.custom.Base
    members: [deliver]
  - bases: [django.core.management.base.BaseCommand]
    provider: project.management.BaseCommand
    members: [write]
route_decorators:
  - callee: portal.expose
    method: POST
    path_argument: 0
route_calls:
  - callee: register_endpoint
    method: GET
    path_argument: 0
    handler_argument: 1
  - callee: path
    method: ANY
    path_argument: 0
    handler_argument: 2
di_calls:
  - callee: InjectService
    argument: 0
"#
                };
                let project_path = directory.join(format!("django.{extension}"));
                fs::write(&project_path, format!("{directive}{rules}")).unwrap();
                let label = format!("{extension}/{mode}/declared={declared}");
                let registry = match DependencyRegistry::try_collect_with_paths(root, None, &[]) {
                    Ok(registry) => registry,
                    Err(error) => {
                        failures.push(format!(
                            "{label}: collection rejected valid pack: {error:#}"
                        ));
                        continue;
                    }
                };
                let Some(manifest) = registry.framework_manifest_for_path(
                    family,
                    "src/views.py",
                    "django",
                    "django",
                ) else {
                    failures.push(format!("{label}: explicit project pack did not activate"));
                    continue;
                };
                for (claim, satisfied) in [
                    (
                        "project template rule is retained as typed data",
                        manifest.syntax_rules.iter().any(|rule| {
                            rule.id == "project-template-symbols"
                                && matches!(
                                    &rule.operation,
                                    SyntaxOperation::TemplateSymbols { tags, filters, library_routes, .. }
                                        if tags.iter().any(|tag| tag == "project")
                                            && filters.iter().any(|filter| filter == "project")
                                            && library_routes.get("project_route").is_some_and(|library| library == "project_catalog")
                                )
                        }),
                    ),
                    (
                        "bundled template tag follows merge mode",
                        manifest.syntax_rules.iter().any(|rule| matches!(
                            &rule.operation,
                            SyntaxOperation::TemplateSymbols { tags, .. }
                                if tags.iter().any(|tag| tag == "block")
                        )) != replace,
                    ),
                    (
                        "bundled template filter follows merge mode",
                        manifest.syntax_rules.iter().any(|rule| matches!(
                            &rule.operation,
                            SyntaxOperation::TemplateSymbols { filters, .. }
                                if filters.iter().any(|filter| filter == "lower")
                        )) != replace,
                    ),
                    (
                        "bundled library route follows merge mode",
                        manifest.syntax_rules.iter().any(|rule| matches!(
                            &rule.operation,
                            SyntaxOperation::TemplateSymbols { library_routes, .. }
                                if library_routes.get("static").is_some_and(|library| library == "static")
                        )) != replace,
                    ),
                    (
                        "typed class rule collision uses project provider",
                        manifest
                            .class_hierarchy_receivers
                            .iter()
                            .find(|rule| {
                                rule.bases == ["django.core.management.base.BaseCommand"]
                            })
                            .is_some_and(|rule| {
                                rule.provider == "project.management.BaseCommand"
                            }),
                    ),
                    (
                        "typed class rule collision is unique",
                        manifest
                            .class_hierarchy_receivers
                            .iter()
                            .filter(|rule| {
                                rule.bases == ["django.core.management.base.BaseCommand"]
                            })
                            .count()
                            == 1,
                    ),
                    (
                        "noncolliding bundled class rule follows replace mode",
                        manifest
                            .class_hierarchy_receivers
                            .iter()
                            .any(|rule| rule.bases.contains(&"django.test.TestCase".to_owned()))
                            != replace,
                    ),
                    (
                        "typed route rule collision uses project handler position",
                        manifest
                            .route_calls
                            .iter()
                            .find(|rule| rule.callee == "path" && rule.method == "ANY")
                            .is_some_and(|rule| rule.handler_argument == Some(2)),
                    ),
                    (
                        "typed route rule collision is unique",
                        manifest
                            .route_calls
                            .iter()
                            .filter(|rule| rule.callee == "path" && rule.method == "ANY")
                            .count()
                            == 1,
                    ),
                    (
                        "noncolliding bundled route rule follows replace mode",
                        manifest
                            .route_calls
                            .iter()
                            .any(|rule| rule.callee == "re_path" && rule.method == "ANY")
                            != replace,
                    ),
                    (
                        "project decorator rule remains under replace mode",
                        manifest
                            .route_decorators
                            .iter()
                            .any(|rule| rule.callee == "portal.expose" && rule.method == "POST"),
                    ),
                    (
                        "project dependency rule remains under replace mode",
                        manifest
                            .di_calls
                            .iter()
                            .any(|rule| rule.callee == "InjectService" && rule.argument == 0),
                    ),
                    (
                        "project pack stays in its workspace",
                        registry
                            .framework_manifest_for_path(
                                family,
                                "[other]/src/views.py",
                                "django",
                                "django",
                            )
                            .is_none(),
                    ),
                ] {
                    if !satisfied {
                        failures.push(format!("{label}: {claim}"));
                    }
                }
                let again = DependencyRegistry::try_collect_with_paths(root, None, &[]).unwrap();
                if registry.digest() != again.digest()
                    || again.framework_manifest_for_path(
                        family,
                        "src/views.py",
                        "django",
                        "django",
                    ) != Some(manifest)
                {
                    failures.push(format!("{label}: re-collection is not deterministic"));
                }
                for (invalid, content) in [
                    ("removed legacy section", "receivers = []\n"),
                    ("unknown section", "unknown = []\n"),
                    ("wrong rule shape", "semantic_rules = {}\n"),
                    ("conflicting modes", "extend = true\nreplace = true\n"),
                    ("wrong mode shape", "extend = \"true\"\n"),
                ] {
                    fs::write(directory.join("invalid.toml"), content).unwrap();
                    if DependencyRegistry::try_collect_with_paths(root, None, &[]).is_ok() {
                        failures.push(format!("{label}: accepted {invalid}"));
                    }
                }
                for (invalid, typed_rule) in [
                    (
                        "empty class hierarchy base list",
                        "[[class_hierarchy_receivers]]\nbases = []\nprovider = \"provider.Base\"\nmembers = [\"send\"]\n",
                    ),
                    (
                        "empty class hierarchy provider",
                        "[[class_hierarchy_receivers]]\nbases = [\"provider.Base\"]\nprovider = \" \"\nmembers = [\"send\"]\n",
                    ),
                    (
                        "invalid route method",
                        "[[route_decorators]]\ncallee = \"router.get\"\nmethod = \"get\"\npath_argument = 0\n",
                    ),
                    (
                        "same path and handler argument",
                        "[[route_calls]]\ncallee = \"register\"\nmethod = \"GET\"\npath_argument = 0\nhandler_argument = 0\n",
                    ),
                    (
                        "negative dependency argument",
                        "[[di_calls]]\ncallee = \"Inject\"\nargument = -1\n",
                    ),
                    (
                        "unknown typed rule field",
                        "[[di_calls]]\ncallee = \"Inject\"\nargument = 0\nextra = true\n",
                    ),
                ] {
                    let content = typed_rule.to_string();
                    fs::write(directory.join("invalid.toml"), content).unwrap();
                    if DependencyRegistry::try_collect_with_paths(root, None, &[]).is_ok() {
                        failures.push(format!("{label}: accepted {invalid}"));
                    }
                }
            }
        }
    }

    let workspace = temporary.path().join("dependency-only");
    fs::create_dir(&workspace).unwrap();
    let root = workspace.as_path();
    let undeclared = DependencyRegistry::try_collect_with_paths(root, None, &[]).unwrap();
    if undeclared
        .framework_manifest_for_path(family, "src/app.py", "django", "django")
        .is_some()
    {
        failures.push("undeclared bundled framework activated without a project pack".to_owned());
    }
    if undeclared
        .framework_manifest_for_path(family, "src/app.py", "gridviewspec", "grid_view_spec")
        .is_some()
    {
        failures
            .push("undeclared GridViewSpec bundle activated without a project pack".to_owned());
    }
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = 'consumer'\ndependencies = ['grid-view-spec>=0.1']\n",
    )
    .unwrap();
    let registry = DependencyRegistry::try_collect_with_paths(root, None, &[]).unwrap();
    match registry.framework_manifest_for_path(family, "src/app.py", "gridviewspec", "grid_view_spec") {
        Some(manifest)
            if manifest
                .class_hierarchy_receivers
                .iter()
                .any(|rule| rule.bases.iter().any(|base| base == "grid_view_spec.types.spec.GridViewSpec")) => {}
        _ => failures.push(
            "declared grid-view-spec did not activate its verified GridViewSpec bundle receiver"
                .to_owned(),
        ),
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn requirement_patterns_are_filename_scoped() {
    assert!(filename_matches("requirements*.txt", "requirements-dev.txt"));
    assert!(!filename_matches("requirements*.txt", "requirements-dev.yaml"));
    assert!(filename_matches("Cargo.toml", "Cargo.toml"));
}
