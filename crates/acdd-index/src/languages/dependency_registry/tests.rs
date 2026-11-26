use super::parser::filename_matches;
use super::*;
use crate::languages::LanguageFamily;
use anyhow::Result;
use std::fs;

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
    let registry = DependencyRegistry::collect(None);
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/languages/manifests");
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("toml" | "yaml" | "yml")
        ) {
            let name = path.file_stem().unwrap().to_str().unwrap();
            assert!(
                registry.standard_framework_manifests.contains_key(name),
                "missing bundled manifest {name}"
            );
        }
    }
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
        registry.declares_for_path(LanguageFamily("python"), path, "django")
            && registry
                .effective_framework_manifest(path, "django")
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
    assert!(local
        .effective_framework_manifest("app.py", "django")
        .is_some());
    let deleted = source_registry(&[dependency]).unwrap();
    assert!(admits(&deleted, "app.py"));
}

#[cfg(feature = "lang-python")]
#[test]
fn source_local_rules_merge_and_replace_bundled_rules() {
    let local_rule =
        "[[route_calls]]\ncallee='LocalRoute'\nmethod='GET'\npath_argument=0\nhandler_argument=1\n";
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
        .effective_framework_manifest("[frontend]/app.py", "local")
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
    let cold =
        DependencyRegistry::try_collect_with_paths(&first_root, Some(&adapter), &[]).unwrap();
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
    for (extension, source) in [("toml", INLINE_RULES_TOML), ("yaml", INLINE_RULES_YAML)] {
        let parsed = FrameworkManifest::parse("django".to_owned(), extension, source)
            .unwrap_or_else(|error| panic!("invalid documented {extension} example: {error:#}"));
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
        include_str!("../manifests/python/gridviewspec.toml"),
    )
    .unwrap_or_else(|error| panic!("invalid bundled GridViewSpec manifest: {error:#}"));
    assert_eq!(gridviewspec.class_hierarchy_receivers.len(), 28);

    let temporary = acdd_core::fs::ScopedTempDir::create("acdd-framework-packs").unwrap();
    let semantic_root = temporary.path().join("semantic-replacement");
    fs::create_dir_all(semantic_root.join(".acdd/frameworks")).unwrap();
    let mut documented = Vec::new();
    for block in include_str!("../../../../../docs/reference/framework-manifests.md").split("```") {
        let Some((extension @ ("toml" | "yaml"), source)) = block.split_once('\n') else {
            continue;
        };
        if source.contains("call_to_file") {
            documented
                .push(FrameworkManifest::parse("documented".into(), extension, source).unwrap());
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
                let workspace = temporary
                    .path()
                    .join(format!("{extension}-{mode}-{declared}"));
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
                let Some(manifest) =
                    registry.effective_framework_manifest("src/views.py", "django")
                else {
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
                        !registry.declares_for_path(family, "[other]/src/views.py", "django"),
                    ),
                ] {
                    if !satisfied {
                        failures.push(format!("{label}: {claim}"));
                    }
                }
                let again = DependencyRegistry::try_collect_with_paths(root, None, &[]).unwrap();
                if registry.digest() != again.digest()
                    || again.effective_framework_manifest("src/views.py", "django")
                        != Some(manifest)
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
    if undeclared.declares_for_path(family, "src/app.py", "django") {
        failures.push("undeclared bundled framework activated without a project pack".to_owned());
    }
    if undeclared.declares_for_path(family, "src/app.py", "grid_view_spec") {
        failures.push("undeclared GridViewSpec bundle activated without a project pack".to_owned());
    }
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = 'consumer'\ndependencies = ['grid-view-spec>=0.1']\n",
    )
    .unwrap();
    let registry = DependencyRegistry::try_collect_with_paths(root, None, &[]).unwrap();
    assert!(registry.declares_for_path(family, "src/app.py", "grid_view_spec"));
    match registry.effective_framework_manifest("src/app.py", "gridviewspec") {
        Some(manifest)
            if manifest.class_hierarchy_receivers.iter().any(|rule| {
                rule.bases
                    .iter()
                    .any(|base| base == "grid_view_spec.types.spec.GridViewSpec")
            }) => {}
        _ => failures.push(
            "declared grid-view-spec did not activate its verified GridViewSpec bundle receiver"
                .to_owned(),
        ),
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn requirement_patterns_are_filename_scoped() {
    assert!(filename_matches(
        "requirements*.txt",
        "requirements-dev.txt"
    ));
    assert!(!filename_matches(
        "requirements*.txt",
        "requirements-dev.yaml"
    ));
    assert!(filename_matches("Cargo.toml", "Cargo.toml"));
}

#[test]
fn manifest_parsers_and_module_prefixes_matrix() {
    use super::parser::ParsedManifest;
    use super::parsers::MANIFEST_PARSERS;
    use crate::languages::{self, types::Ecosystem};

    let _: Option<ParsedManifest> = None;

    struct ParserCase {
        ecosystem: Ecosystem,
        filename: &'static str,
        content: &'static str,
        expected_deps: &'static [&'static str],
        expected_prefixes: &'static [(&'static str, &'static str)],
    }

    let cases = [
        ParserCase {
            ecosystem: Ecosystem::Rust,
            filename: "Cargo.toml",
            content: "[package]\nname = \"demo-pkg\"\n[dependencies]\nserde-json = \"1\"\n",
            expected_deps: &["serde_json"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::JavaScript,
            filename: "package.json",
            content: r#"{"name":"web","dependencies":{"zod":"^3"},"devDependencies":{"vitest":"1"},"optionalDependencies":{"opt":"1"}}"#,
            expected_deps: &["zod", "vitest"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::JavaScript,
            filename: "package.json",
            content: r#"{"name":"web-tolerant","main":"index.js","bin":["ignored"],"dependencies":null,"devDependencies":{"vitest":"1"}}"#,
            expected_deps: &["vitest"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::JavaScript,
            filename: "package-lock.json",
            content: r#"{"dependencies":null,"packages":{"node_modules/chalk":{"version":"5.3.0"}}}"#,
            expected_deps: &["chalk"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::Python,
            filename: "pyproject.toml",
            content: "[project]\nname='svc'\ndependencies=['fastapi>=0.110']\n",
            expected_deps: &["fastapi"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::Go,
            filename: "go.mod",
            content: "module example.com/acme/api\n\ngo 1.22\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgolang.org/x/sync v0.7.0 // indirect\n)\n",
            expected_deps: &["github.com/gin-gonic/gin", "golang.org/x/sync"],
            expected_prefixes: &[("example.com/acme/api", "")],
        },
        ParserCase {
            ecosystem: Ecosystem::Go,
            filename: "go.mod",
            content: "module example.com/acme/inline\nrequire ( example.com/inline v1.2.3 )\nrequire github.com/after/inline v0.1.0\n",
            expected_deps: &["example.com/inline", "github.com/after/inline"],
            expected_prefixes: &[("example.com/acme/inline", "")],
        },
        ParserCase {
            ecosystem: Ecosystem::Jvm,
            filename: "pom.xml",
            content: "<project><dependencies><dependency><groupId>io.ktor</groupId><artifactId>ktor-core</artifactId></dependency></dependencies></project>",
            expected_deps: &["io.ktor"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::Jvm,
            filename: "build.gradle.kts",
            content: "dependencies { implementation(\"org.springframework.boot:spring-boot-starter:3.0.0\") }",
            expected_deps: &["org.springframework.boot"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::Php,
            filename: "composer.json",
            content: r#"{"name":"acme/shop","require":{"php":"^8.3","laravel/framework":"^11.0"},"require-dev":{"phpunit/phpunit":"^11.0"},"autoload":{"psr-4":{"App\\":"src/","Acme\\Core\\":["core/","shared/"]}},"autoload-dev":{"psr-4":{"App\\Tests\\":"tests/"}}}"#,
            expected_deps: &["laravel/framework", "phpunit/phpunit"],
            expected_prefixes: &[
                ("Acme\\Core", "core"),
                ("Acme\\Core", "shared"),
                ("App", "src"),
                ("App\\Tests", "tests"),
            ],
        },
        ParserCase {
            ecosystem: Ecosystem::Php,
            filename: "composer.json",
            content: r#"{"name":"acme/tolerant","require":{"laravel/framework":"^11.0"},"require-dev":[],"autoload":{"psr-4":[]}}"#,
            expected_deps: &["laravel/framework"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::CSharp,
            filename: "Billing.csproj",
            content: r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><RootNamespace>Acme.Billing</RootNamespace></PropertyGroup><ItemGroup><!-- <PackageReference Include="Ignored" /> --><PackageReference Include="Newtonsoft.Json" Version="13.0.3" /><PackageReference Include="Serilog" Version="3.1.1" /></ItemGroup></Project>"#,
            expected_deps: &["Newtonsoft.Json", "Serilog"],
            expected_prefixes: &[("Acme.Billing", "")],
        },
        ParserCase {
            ecosystem: Ecosystem::Ruby,
            filename: "Gemfile",
            content: "# comment\nsource 'https://rubygems.org'\ngem 'rails', '~> 7.1'\ngem(\"sidekiq\")\n",
            expected_deps: &["rails", "sidekiq"],
            expected_prefixes: &[],
        },
        ParserCase {
            ecosystem: Ecosystem::CCpp,
            filename: "CMakeLists.txt",
            content: "cmake_minimum_required(VERSION 3.20)\n# find_package(Ignored)\nfind_package(OpenSSL REQUIRED)\nFIND_PACKAGE(fmt CONFIG REQUIRED)\n",
            expected_deps: &["OpenSSL", "fmt"],
            expected_prefixes: &[],
        },
    ];

    for case in &cases {
        let parser = MANIFEST_PARSERS
            .iter()
            .copied()
            .find(|parser| {
                parser.ecosystem() == case.ecosystem && parser.matches_filename(case.filename)
            })
            .unwrap_or_else(|| panic!("missing ManifestParser for {}", case.filename));
        let parsed = parser.parse(case.filename, case.content);
        let actual_deps: std::collections::BTreeSet<_> =
            parsed.dependencies.iter().map(String::as_str).collect();
        let expected_deps: std::collections::BTreeSet<_> =
            case.expected_deps.iter().copied().collect();
        assert_eq!(actual_deps, expected_deps, "{}", case.filename);
        let actual_prefixes: Vec<_> = parsed
            .module_prefixes
            .iter()
            .map(|item| (item.prefix.as_str(), item.source_root.as_str()))
            .collect();
        assert_eq!(actual_prefixes, case.expected_prefixes, "{}", case.filename);
    }

    #[cfg(feature = "lang-rust")]
    {
        let autolib_registry =
            DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
                name: "",
                manifests: &[ManifestSource {
                    path: "Cargo.toml",
                    source: "[package]\nname = \"demo-lib\"\nautolib = false\n[lib]\n",
                }],
            }])
            .unwrap();
        assert_eq!(
            autolib_registry.rust_package_for_path("src/lib.rs"),
            Some(("", "demo_lib"))
        );
    }

    let registry = DependencyRegistry::try_collect_from_sources([
        WorkspaceManifestSources {
            name: "",
            manifests: &[
                ManifestSource {
                    path: "go.mod",
                    source: "module example.com/root\nrequire github.com/pkg/errors v0.9.1\n",
                },
                ManifestSource {
                    path: "services/auth/go.mod",
                    source: "module example.com/root/services/auth\nrequire golang.org/x/crypto v0.20.0\n",
                },
                ManifestSource {
                    path: "backend/composer.json",
                    source: r#"{"require":{"monolog/monolog":"^3.0"},"autoload":{"psr-4":{"App\\":"src/"}},"autoload-dev":{"psr-4":{"App\\Tests\\":"tests/"}}}"#,
                },
                ManifestSource {
                    path: "dotnet/Billing.csproj",
                    source: r#"<Project><PropertyGroup><RootNamespace>Acme.Billing</RootNamespace></PropertyGroup><ItemGroup><PackageReference Include="Dapper" Version="2.1.0" /></ItemGroup></Project>"#,
                },
                ManifestSource {
                    path: "jvm/pom.xml",
                    source: "<project><dependencies><dependency><groupId>com.squareup.okhttp3</groupId></dependency></dependencies></project>",
                },
                ManifestSource {
                    path: "ruby/Gemfile",
                    source: "gem 'puma'\n",
                },
                ManifestSource {
                    path: "native/CMakeLists.txt",
                    source: "find_package(ZLIB REQUIRED)\n",
                },
            ],
        },
        WorkspaceManifestSources {
            name: "other",
            manifests: &[ManifestSource {
                path: "go.mod",
                source: "module example.com/other\n",
            }],
        },
    ])
    .unwrap();

    assert_eq!(
        registry
            .module_prefix_for_path(LanguageFamily("go"), "services/auth/handler.go")
            .map(|p| p.prefix.as_str()),
        Some("example.com/root/services/auth")
    );
    assert_eq!(
        registry
            .module_prefixes_for_path(LanguageFamily("go"), "services/auth/handler.go")
            .into_iter()
            .map(|p| p.prefix.as_str())
            .collect::<Vec<_>>(),
        vec!["example.com/root/services/auth", "example.com/root"]
    );
    assert_eq!(
        registry
            .module_prefix_for_path(LanguageFamily("go"), "[other]/main.go")
            .map(|p| p.prefix.as_str()),
        Some("example.com/other")
    );
    assert_eq!(
        registry
            .module_prefix_for_path(LanguageFamily("php"), "backend/src/User.php")
            .map(|p| (p.prefix.as_str(), p.source_root.as_str())),
        Some(("App", "backend/src"))
    );
    assert_eq!(
        registry
            .module_prefix_for_path(LanguageFamily("php"), "backend/tests/UserTest.php")
            .map(|p| (p.prefix.as_str(), p.source_root.as_str())),
        Some(("App\\Tests", "backend/tests"))
    );
    assert_eq!(
        registry
            .module_prefix_for_path(LanguageFamily("csharp"), "dotnet/Invoice.cs")
            .map(|p| (p.prefix.as_str(), p.source_root.as_str())),
        Some(("Acme.Billing", "dotnet"))
    );
    assert!(registry
        .module_prefix_for_path(LanguageFamily("php"), "[other]/backend/src/User.php")
        .is_none());

    for (lang_id, module, path, expected_origin) in [
        ("go", "github.com/pkg/errors", "main.go", "go.mod"),
        (
            "php",
            "monolog/monolog",
            "backend/src/User.php",
            "backend/composer.json",
        ),
        (
            "csharp",
            "Dapper",
            "dotnet/Invoice.cs",
            "dotnet/Billing.csproj",
        ),
        (
            "java",
            "com.squareup.okhttp3",
            "jvm/App.java",
            "jvm/pom.xml",
        ),
        (
            "kotlin",
            "com.squareup.okhttp3",
            "jvm/App.kt",
            "jvm/pom.xml",
        ),
        ("ruby", "puma", "ruby/app.rb", "ruby/Gemfile"),
        ("c", "ZLIB", "native/main.c", "native/CMakeLists.txt"),
        ("cpp", "ZLIB", "native/main.cpp", "native/CMakeLists.txt"),
    ] {
        if let Some(profile) = languages::by_id(lang_id) {
            assert_eq!(
                registry
                    .classification_for_path(profile, module, path)
                    .as_deref(),
                Some(format!("external dependency (manifest: {expected_origin})").as_str()),
                "{lang_id}: {module}"
            );
        }
    }
}

#[cfg(all(feature = "lang-python", feature = "lang-typescript"))]
#[test]
fn source_registry_scope_parity_matrix() {
    use crate::languages;
    use crate::linker;
    use std::collections::BTreeMap;

    const MANIFESTS: &[(&str, &str)] = &[
        ("requirements.txt", "django\nfastapi\n"),
        ("nested/requirements.txt", ""),
        (
            "package.json",
            "{\"name\":\"primary\",\"dependencies\":{\"vue\":\"3\"}}",
        ),
        (
            "package-lock.json",
            "{\"lockfileVersion\":3,\"packages\":{\"node_modules/vue\":{\"version\":\"3\"}}}",
        ),
        (
            ".acdd/frameworks/django.toml",
            "[[route_calls]]\ncallee='LocalRoute'\nmethod='GET'\npath_argument=0\nhandler_argument=1\n",
        ),
        (".acdd/frameworks/fastapi.yaml", "enabled: true\n"),
    ];
    const SOURCES: &[(&str, &str, &str)] = &[
        ("app.py", "python", "from fastapi import FastAPI\nclass Record: pass\napp=FastAPI()\n@app.get('/records/', response_model=Record)\ndef records(): return []\nLocalRoute('/local/', records)\n"),
        ("nested/app.py", "python", "class Record: pass\nclass View:\n    model=Record\n"),
    ];
    let extract_with_registry = |registry: &DependencyRegistry, files: &[(&str, &str, &str)]| {
        files
            .iter()
            .map(|&(path, language, source)| {
                crate::ast::extract_typed_with_registry(path, language, source, registry)
                    .map(|facts| (path.to_owned(), facts))
            })
            .collect::<Result<BTreeMap<_, _>>>()
    };

    let tmp = acdd_core::fs::ScopedTempDir::create("acdd-source-parity").unwrap();
    let workspace = tmp.path().join("primary");
    let linked = tmp.path().join("linked");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&linked).unwrap();
    for (rel, content) in MANIFESTS {
        let p = workspace.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }
    fs::write(linked.join("requirements.txt"), "fastapi\n").unwrap();

    for name in [
        "my-service",
        "two words",
        "команда",
        "a/b",
        "[other]",
        "../other",
    ] {
        fs::write(
            workspace.join("acdd.yaml"),
            format!(
                "roots: [.]\nlinked_workspaces:\n  - name: '{name}'\n    path: '{}'\n    roots: [.]\n",
                linked.display()
            ),
        )
        .unwrap();
        let main: Vec<_> = MANIFESTS
            .iter()
            .map(|&(path, source)| ManifestSource { path, source })
            .collect();
        let linked_sources = [ManifestSource {
            path: "requirements.txt",
            source: "fastapi\n",
        }];
        let source = DependencyRegistry::try_collect_from_sources([
            WorkspaceManifestSources {
                name: "",
                manifests: &main,
            },
            WorkspaceManifestSources {
                name,
                manifests: &linked_sources,
            },
        ])
        .unwrap();
        let adapter = crate::scanner::load_adapter(&workspace, None).unwrap();
        let disk =
            DependencyRegistry::try_collect_with_scan_config(&workspace, Some(&adapter)).unwrap();
        assert_eq!(source.digest(), disk.digest());
        for (path, pack) in [
            ("app.py", "django"),
            ("nested/app.py", "django"),
            (&format!("[{name}]/app.py"), "fastapi"),
        ] {
            let source_m = source
                .effective_framework_manifest(path, pack)
                .map(|m| serde_json::to_value(m).unwrap());
            let disk_m = disk
                .effective_framework_manifest(path, pack)
                .map(|m| serde_json::to_value(m).unwrap());
            assert_eq!(source_m, disk_m);
            assert_eq!(
                source.declares_for_path(LanguageFamily("python"), path, pack),
                disk.declares_for_path(LanguageFamily("python"), path, pack)
            );
        }
        let source_facts = extract_with_registry(&source, SOURCES).unwrap();
        let disk_facts = extract_with_registry(&disk, SOURCES).unwrap();
        for (path, facts) in &source_facts {
            assert_eq!(
                serde_json::to_value(facts.clone().into_public()).unwrap(),
                serde_json::to_value(disk_facts[path].clone().into_public()).unwrap()
            );
        }
        let actual: crate::models::Graph =
            linker::link_compact_with_typed_registry(&source_facts, None, None, &source).into();
        let expected: crate::models::Graph =
            linker::link_compact_with_typed_registry(&disk_facts, None, None, &disk).into();
        assert_eq!(
            serde_json::to_value(&actual.edges).unwrap(),
            serde_json::to_value(expected.edges).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&actual.coverage).unwrap(),
            serde_json::to_value(expected.coverage).unwrap()
        );
        let nodes = &source_facts["app.py"].facts.nodes;
        let route = nodes
            .iter()
            .find(|node| node.name == "GET /records/")
            .unwrap();
        let handler = nodes.iter().find(|node| node.name == "records").unwrap();
        assert!(actual
            .edges
            .iter()
            .any(|edge| edge.kind == "handles" && edge.src == route.id && edge.dst == handler.id));
        assert!(actual.edges.iter().any(|edge| edge.kind == "uses_model"));
        assert!(!actual
            .edges
            .iter()
            .any(|edge| edge.kind == "uses_model" && edge.src.contains("nested/app.py")));
        if !matches!(name, "a/b" | "../other") {
            let app = format!("[{name}]/app.py");
            let nested = format!("[{name}]/nested/app.py");
            let files = [
                (app.as_str(), "python", SOURCES[0].2),
                (nested.as_str(), "python", SOURCES[1].2),
            ];
            let actual_facts = extract_with_registry(&source, &files).unwrap();
            let expected_facts = extract_with_registry(&disk, &files).unwrap();
            let actual: crate::models::Graph =
                linker::link_compact_with_typed_registry(&actual_facts, None, None, &source).into();
            let expected: crate::models::Graph =
                linker::link_compact_with_typed_registry(&expected_facts, None, None, &disk).into();
            assert_eq!(
                serde_json::to_value(&actual.edges).unwrap(),
                serde_json::to_value(&expected.edges).unwrap(),
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&actual.coverage).unwrap(),
                serde_json::to_value(&expected.coverage).unwrap(),
                "{name}"
            );
            let nodes = &actual_facts[&app].facts.nodes;
            let route = nodes
                .iter()
                .find(|node| node.name == "GET /records/")
                .unwrap();
            let handler = nodes.iter().find(|node| node.name == "records").unwrap();
            let model = nodes.iter().find(|node| node.name == "Record").unwrap();
            for (kind, src, dst) in [
                ("handles", &route.id, &handler.id),
                ("uses_model", &handler.id, &model.id),
            ] {
                assert_eq!(
                    actual
                        .edges
                        .iter()
                        .filter(|edge| edge.kind == kind && edge.src == *src && edge.dst == *dst)
                        .count(),
                    1,
                    "{name}: exact linked {kind} target"
                );
            }
            let view = actual_facts[&nested]
                .facts
                .nodes
                .iter()
                .find(|node| node.name == "View")
                .unwrap();
            assert!(
                !actual
                    .edges
                    .iter()
                    .any(|edge| edge.kind == "uses_model" && edge.src == view.id),
                "{name}: linked FastAPI scope must not activate primary Django policy"
            );
        }
        let _ = languages::LanguageFamily("python");
    }
}
