use super::*;

#[cfg(feature = "lang-html")]
#[test]
fn framework_manifest_accepts_generic_typed_rules_and_formats() {
    let mut failures = Vec::new();
    for (name, extension, source) in [
        (
            "django",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/django.toml"),
        ),
        (
            "starlette",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/starlette.toml"),
        ),
        (
            "fastapi",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/fastapi.toml"),
        ),
        (
            "nuxt",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/nuxt.toml"),
        ),
        (
            "vue-runtime",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/vue-runtime.toml"),
        ),
        (
            "htmx",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/htmx.toml"),
        ),
        (
            "alpinejs",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/alpinejs.toml"),
        ),
        (
            "gridviewspec",
            "toml",
            include_str!("../../../crates/acdd-index/src/languages/manifests/gridviewspec.toml"),
        ),
        (
            "declarative-rules",
            "toml",
            include_str!(
                "../../../skills/acdd/references/frameworks/declarative-rules.toml.example"
            ),
        ),
        (
            "declarative-rules",
            "yaml",
            include_str!(
                "../../../skills/acdd/references/frameworks/declarative-rules.yaml.example"
            ),
        ),
    ] {
        let identity = if name == "declarative-rules" {
            "project-rules"
        } else {
            name
        };
        let manifest = format!(".acdd/frameworks/{identity}.{extension}");
        let result =
            FrameworkHarness::from_manifests(&[(&manifest, source)]).and_then(|harness| {
                harness.link_sources(&[("index.html", "html", "<p>Example manifest</p>")])
            });
        if let Err(error) = result {
            failures.push(format!(
                "{name}.{extension} ({manifest}) failed production loading: {error:#}"
            ));
        }
    }
    for (language, operation) in [
        (
            "python",
            serde_json::json!({"kind":"named_binding","domain":"paint","binding":{"kind":"call_reference","callee":{"kind":"imported","module":"tablekit","export":"Column"},"key":{"kind":"keyword","name":"renderer"},"provider_scopes":[{"workspace":"","project":"../frontend","package":"runtime"}]}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"named_binding","domain":"paint","binding":{"kind":"call_reference","callee":{"kind":"imported","module":"tablekit","export":"Column"},"key":{"kind":"keyword","name":"renderer"},"provider_scopes":[{"workspace":"unknown","project":"frontend","package":"runtime"}]}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"named_binding","domain":"filters","binding":{"kind":"assignment_definition","receiver_type":{"module":"jinja2","export":"Environment","extra":true},"member":"filters","template_root":"templates"}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"named_binding","domain":"filters","binding":{"kind":"assignment_definition","receiver_type":{"module":"jinja2","export":"Environment"},"member":"filters","template_root":"../templates"}}),
        ),
        (
            "html",
            serde_json::json!({"kind":"named_binding","domain":"filters","binding":{"kind":"template_reference","symbol_kind":"tag"}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"http_client_call","callee":"fetch","receivers":[],"methods":{},"default_method":"GET","url_key":"url","method_key":"method","relative_prefixes":["/"],"target_context":{"request_owners":[],"route_owners":["app.py"]}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"http_client_call","callee":"fetch","receivers":[],"methods":{},"default_method":"GET","url_key":"url","method_key":"method","relative_prefixes":["/"],"target_context":{"request_owners":["test.py"],"route_owners":["../app.py"]}}),
        ),
        (
            "python",
            serde_json::json!({"kind":"http_client_call","callee":"fetch","receivers":[],"methods":{},"default_method":"GET","url_key":"url","method_key":"method","relative_prefixes":["/"],"target_context":{"request_owners":["test.py"],"route_owners":["app.py","app.py"]}}),
        ),
    ] {
        let pack = serde_json::json!({"syntax_rules":[{"id":"strict","language":language,"operation":operation}]});
        let mut sources =
            VirtualWorkspace::from_files(&[("index.html", "<p>Strict residual schema</p>")]);
        sources.write(
            ".acdd/frameworks/strict.toml",
            &toml::to_string(&pack).unwrap(),
        );
        if sources.index().is_ok() {
            failures.push(format!(
                "malformed residual descriptor accepted by the in-memory production linker: {pack}"
            ));
        }
    }
    let complete_toml = r#"replace = true
[[syntax_rules]]
id = "template-symbols"
language = "html"
activation = [{ kind = "package", name = "django" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["csrf_token"], filters = ["safe"], libraries = [], library_routes = { static = "static" } }
"#;
    let complete_yaml = r#"replace: true
syntax_rules:
  - id: template-symbols
    language: html
    activation:
      - kind: package
        name: django
    operation:
      kind: template_symbols
      requires_loader: false
      tags: [csrf_token]
      filters: [safe]
      libraries: []
      library_routes:
        static: static
"#;

    for (extension, complete_manifest) in [
        ("toml", complete_toml),
        ("yaml", complete_yaml),
        ("yml", complete_yaml),
    ] {
        let mut sources = VirtualWorkspace::from_files(&[
            (
                "pyproject.toml",
                "[project]\nname = 'sample'\ndependencies = ['django>=5']\n",
            ),
            ("index.html", "{% csrf_token %}{{ value|safe }}\n"),
        ]);
        sources.write(
            format!(".acdd/frameworks/django.{extension}"),
            complete_manifest,
        );
        let indexed = match sources.index() {
            Ok(indexed) => indexed,
            Err(error) => {
                failures.push(format!(
                    "valid .{extension} generic manifest failed to build: {error:#}"
                ));
                continue;
            }
        };
        if indexed
            .registry
            .framework_manifest_for_path(LanguageFamily("python"), "index.html", "django", "django")
            .is_none()
        {
            failures.push(format!(
                "generic .{extension} manifest did not reach its linker"
            ));
        }
        let indexed_file = query_count(
            &indexed.conn,
            "SELECT count(*) FROM path_dictionary WHERE path=?1",
            ["index.html"],
        );
        if indexed_file != 1 {
            failures.push(format!(
                "generic .{extension} manifest published {indexed_file} index rows"
            ));
        }
    }

    for extension in ["toml", "yaml", "yml"] {
        for legacy_section in ["receivers", "builtins", "filters", "routes"] {
            let manifest = if extension == "toml" {
                format!("{legacy_section} = []\n")
            } else {
                format!("{legacy_section}: []\n")
            };
            let manifest_path = format!(".acdd/frameworks/django.{extension}");
            if FrameworkHarness::from_manifests(&[(manifest_path.as_str(), manifest.as_str())])
                .is_ok()
            {
                failures.push(format!(
                    ".{extension} legacy section {legacy_section} was accepted by the production manifest parser"
                ));
            }
        }
    }

    let empty =
        FrameworkHarness::from_manifests(&[(".acdd/frameworks/django.toml", "enabled = false\n")])
            .and_then(|harness| {
                harness.link_sources(&[(
                    "index.html",
                    "html",
                    "<p>Empty typed rule manifest</p>\n",
                )])
            });
    if let Err(error) = empty {
        failures.push(format!("empty disabled manifest was rejected: {error:#}"));
    }

    let undeclared =
        FrameworkHarness::from_manifests(&[(".acdd/frameworks/django.toml", complete_toml)])
            .unwrap();
    undeclared
        .link_sources(&[("index.html", "html", "<p>No framework declaration</p>\n")])
        .unwrap();
    if undeclared
        .registry
        .framework_manifest_for_path(LanguageFamily("python"), "index.html", "django", "django")
        .is_none()
    {
        failures
            .push("an undeclared project did not activate its local Django manifest".to_owned());
    }
    assert!(
        failures.is_empty(),
        "manifest contract failures:\n{}",
        failures.join("\n")
    );
}
#[cfg(feature = "lang-html")]
#[test]
fn invalid_user_framework_manifest_fails_before_index_publication() {
    let complete = "";
    let mut failures = Vec::new();
    let invalid_manifests = [
        ("nonboolean enabled", "toml", format!("enabled = 'false'\n{complete}")),
        ("nonboolean enabled", "yaml", "enabled: 'false'\n".to_owned()),
        ("nonboolean enabled", "yml", "enabled: 0\n".to_owned()),
        ("removed legacy section", "toml", "receivers = []\n".to_owned()),
        ("removed legacy section", "yaml", "builtins: []\n".to_owned()),
        ("wrong syntax rule shape", "toml", "syntax_rules = {}\n".to_owned()),
        ("conflicting modes", "toml", "extend = true\nreplace = true\n".to_owned()),
        ("wrong mode shape", "toml", "extend = 'true'\n".to_owned()),
        ("malformed TOML syntax", "toml", "syntax_rules = [\n".to_owned()),
        (
            "duplicate table headers",
            "toml",
            format!("{complete}[unused]\nfirst = 1\n[unused]\nsecond = 2\n"),
        ),
        (
            "duplicate keys",
            "toml",
            format!("{complete}[unused]\nfirst = 1\nfirst = 2\n"),
        ),
        (
            "removed legacy section redeclared as a table",
            "toml",
            format!("{complete}[receivers]\nname = 'duplicate-shape'\n"),
        ),
        (
            "nested table before dotted key",
            "toml",
            format!(
                "{complete}[unused.receivers]\nname = 'declared-as-table'\n[unused]\nreceivers = []\n"
            ),
        ),
        (
            "dotted key before nested table",
            "toml",
            format!(
                "{complete}[unused]\nreceivers = []\n[unused.receivers]\nname = 'declared-as-table'\n"
            ),
        ),
        (
            "malformed YAML syntax",
            "yaml",
            "syntax_rules: [\n".to_owned(),
        ),
        (
            "malformed YAML syntax",
            "yml",
            "syntax_rules: [\n".to_owned(),
        ),
    ];
    let project_files = [
        (
            "pyproject.toml",
            "[project]\nname = 'sample'\ndependencies = ['django>=5']\n",
        ),
        (
            "shop/views.py",
            "from django.shortcuts import render\ndef page(request):\n    return render(request, 'templates/page.html', {'title': 'Welcome'})\n",
        ),
        ("shop/templates/page.html", "<h1>{{ title }}</h1>\n"),
    ];

    let workspace = FrameworkHarness::persisted(&project_files);
    for (invalid_kind, extension, manifest) in invalid_manifests {
        for prior_extension in ["toml", "yaml", "yml"] {
            let prior = workspace.path(format!(".acdd/frameworks/django.{prior_extension}"));
            if prior.exists() {
                std::fs::remove_file(prior).unwrap();
            }
        }
        let manifest_path = format!(".acdd/frameworks/django.{extension}");
        workspace.write(manifest_path.as_str(), manifest.as_str());
        let result = contextunity_acdd::engine::pipeline::build_with_cache_dir(
            workspace.root(),
            &workspace.db(),
            None,
            &workspace.path(".acdd/extraction-cache"),
        );
        if result.is_ok() || workspace.db().exists() {
            failures.push(format!("a declared Django project with {invalid_kind} in its .{extension} manifest was accepted or published index"));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid manifest contract failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn activation_condition_and_language_context_matrix() {
    use languages::dependency_registry::{ManifestSource, WorkspaceManifestSources};
    use languages::manifests::semantic::{ActivationCondition, FileActivationSignals};
    use std::collections::BTreeSet;

    let legacy_toml = r#"
[[syntax_rules]]
id = "legacy-package"
language = "html"
package_family = "python"
activation = [{ kind = "package", name = "django" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["csrf_token"], filters = [], libraries = [], library_routes = {} }
"#;
    let legacy_registry = DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
        name: "",
        manifests: &[
            ManifestSource {
                path: "requirements.txt",
                source: "django>=5\n",
            },
            ManifestSource {
                path: ".acdd/frameworks/legacy.toml",
                source: legacy_toml,
            },
        ],
    }])
    .unwrap();
    let legacy_ctx = legacy_registry.language_context_for_path("index.html");
    let legacy_rule = legacy_ctx
        .syntax_rules
        .iter()
        .find(|rule| rule.pack == "legacy" && rule.id == "legacy-package")
        .expect("legacy package activation rule must load");
    assert_eq!(
        legacy_rule.activation,
        vec![ActivationCondition::Package {
            name: "django".into()
        }]
    );

    let syntax_pack = r#"
[[syntax_rules]]
id = "syntax-magic"
language = "html"
package_family = "javascript"
activation = [{ kind = "any", conditions = [{ kind = "package", name = "alpinejs" }, { kind = "syntax_attribute", name = "x-data" }, { kind = "syntax_attribute_prefix", prefix = "x-on:" }, { kind = "file_extension", extension = "j2" }] }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["alpha"], filters = [], libraries = [], library_routes = {} }
"#;
    let invalid_cases = [
        ("unknown activation kind", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "header", name = "x" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("unknown activation field", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "syntax_attribute", name = "x-data", extra = true }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("empty syntax attribute", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "syntax_attribute", name = "  " }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("empty syntax prefix", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "syntax_attribute_prefix", prefix = "" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("leading dot file extension", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "file_extension", extension = ".j2" }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("empty any conditions", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "any", conditions = [] }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("deeply nested any (>4)", r#"[[syntax_rules]]
id = "bad"
language = "html"
activation = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "package", name = "django" }] }] }] }] }] }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#),
        ("unsupported language id", r#"[[syntax_rules]]
id = "bad"
language = "elixir"
operation = { kind = "implicit_builtins", names = ["foo"] }
"#),
        ("unknown package family", r#"[[syntax_rules]]
id = "bad"
language = "html"
package_family = "unknown_family"
operation = { kind = "implicit_builtins", names = ["foo"] }
"#),
    ];
    for (label, source) in invalid_cases {
        assert!(
            FrameworkHarness::from_manifests(&[(".acdd/frameworks/invalid.toml", source)]).is_err(),
            "expected fail-closed rejection for {label}"
        );
    }

    let max_depth_ok = r#"[[syntax_rules]]
id = "depth4"
language = "html"
activation = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "any", conditions = [{ kind = "file_extension", extension = "j2" }] }] }] }] }]
operation = { kind = "template_symbols", requires_loader = false, tags = ["a"], filters = [], libraries = [], library_routes = {} }
"#;
    assert!(
        FrameworkHarness::from_manifests(&[(".acdd/frameworks/depth4.toml", max_depth_ok)]).is_ok()
    );

    #[cfg(feature = "lang-html")]
    {
        let deferred_registry = DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
            name: "",
            manifests: &[ManifestSource {
                path: ".acdd/frameworks/custom.toml",
                source: syntax_pack,
            }],
        }])
        .unwrap();
        let ctx = deferred_registry.language_context_for_path("templates/page.html");
        assert!(ctx.has_syntax_gated_rules());
        assert_eq!(
            ctx.syntax_activation_keys().exact_attributes,
            BTreeSet::from(["x-data".to_owned()])
        );
        assert_eq!(
            ctx.syntax_activation_keys().attribute_prefixes,
            BTreeSet::from(["x-on:".to_owned()])
        );
        assert!(ctx.syntax_activation_keys().matches_attribute("x-data"));
        assert!(ctx.syntax_activation_keys().matches_attribute("x-on:click"));
        assert!(!ctx.syntax_activation_keys().matches_attribute("class"));

        let empty_signals = FileActivationSignals::default();
        let x_data_signals = FileActivationSignals {
            syntax_attributes: BTreeSet::from(["x-data".to_owned()]),
        };
        let x_on_signals = FileActivationSignals {
            syntax_attributes: BTreeSet::from(["x-on:click".to_owned()]),
        };

        let rule = ctx
            .syntax_rules
            .iter()
            .find(|rule| rule.pack == "custom" && rule.id == "syntax-magic")
            .expect("deferred syntax-gated rule must remain retained in LanguageContext");
        assert!(!ctx.is_syntax_rule_active(rule, "templates/page.html", &empty_signals));
        assert!(ctx.is_syntax_rule_active(rule, "templates/page.html", &x_data_signals));
        assert!(ctx.is_syntax_rule_active(rule, "templates/page.html", &x_on_signals));
        assert!(ctx.is_syntax_rule_active(rule, "templates/page.j2", &empty_signals));
        assert!(ctx
            .syntax_rules_for_signals("templates/page.html", &x_on_signals)
            .any(|active| active.pack == "custom" && active.id == "syntax-magic"));

        #[cfg(feature = "lang-typescript")]
        {
            let pkg_registry =
                DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
                    name: "",
                    manifests: &[
                        ManifestSource {
                            path: "package.json",
                            source: r#"{"dependencies":{"alpinejs":"^3.13"}}"#,
                        },
                        ManifestSource {
                            path: ".acdd/frameworks/custom.toml",
                            source: syntax_pack,
                        },
                    ],
                }])
                .unwrap();
            let pkg_ctx = pkg_registry.language_context_for_path("app.js");
            let pkg_rule = pkg_ctx
                .syntax_rules
                .iter()
                .find(|rule| rule.pack == "custom" && rule.id == "syntax-magic")
                .expect("package-activated rule retained in LanguageContext");
            assert!(pkg_ctx.is_syntax_rule_active(pkg_rule, "app.js", &empty_signals));
        }
    }
}
