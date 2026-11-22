use super::*;

#[test]
fn dependency_manifest_matrix() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        #[cfg(feature = "lang-typescript")]
        PackageJsonDependenciesIncludeRuntimeDevelopmentAndPeerPackages,
        #[cfg(feature = "lang-typescript")]
        NpmLockfilesRegisterDirectTransitiveAndScopedDependencies,
        #[cfg(feature = "lang-typescript")]
        PackageLockV1NestedDependenciesRegisterTransitivePackages,
        #[cfg(feature = "lang-rust")]
        CargoDependenciesIncludeWorkspaceAndTargetScopedDeclarations,
        #[cfg(feature = "lang-go")]
        GoModDependenciesIncludeModulesAndVersionedPaths,
        #[cfg(feature = "lang-java")]
        GradleAndMavenDependenciesIncludeConfigurationsAndGroupIds,
        #[cfg(feature = "lang-python")]
        PyprojectAndPipfileDependenciesIncludeOptionalAndPoetryGroups,
        #[cfg(feature = "lang-python")]
        RequirementsAndSetupCfgParsePackageNamesWithoutOptionsOrVersions,
        #[cfg(feature = "lang-python")]
        PyprojectDependenciesAcceptTomlMultilineStringsAndUnicodeEscapes,
        #[cfg(feature = "lang-python")]
        RequirementsRejectBareUrlsAndLocalArtifacts,
    }
    for case in [
        #[cfg(feature = "lang-typescript")]
        Case::PackageJsonDependenciesIncludeRuntimeDevelopmentAndPeerPackages,
        #[cfg(feature = "lang-typescript")]
        Case::NpmLockfilesRegisterDirectTransitiveAndScopedDependencies,
        #[cfg(feature = "lang-typescript")]
        Case::PackageLockV1NestedDependenciesRegisterTransitivePackages,
        #[cfg(feature = "lang-rust")]
        Case::CargoDependenciesIncludeWorkspaceAndTargetScopedDeclarations,
        #[cfg(feature = "lang-go")]
        Case::GoModDependenciesIncludeModulesAndVersionedPaths,
        #[cfg(feature = "lang-java")]
        Case::GradleAndMavenDependenciesIncludeConfigurationsAndGroupIds,
        #[cfg(feature = "lang-python")]
        Case::PyprojectAndPipfileDependenciesIncludeOptionalAndPoetryGroups,
        #[cfg(feature = "lang-python")]
        Case::RequirementsAndSetupCfgParsePackageNamesWithoutOptionsOrVersions,
        #[cfg(feature = "lang-python")]
        Case::PyprojectDependenciesAcceptTomlMultilineStringsAndUnicodeEscapes,
        #[cfg(feature = "lang-python")]
        Case::RequirementsRejectBareUrlsAndLocalArtifacts,
    ] {
        match case {
            #[cfg(feature = "lang-typescript")]
            // package_json_dependencies_include_runtime_development_and_peer_packages
            Case::PackageJsonDependenciesIncludeRuntimeDevelopmentAndPeerPackages => {
                let profile = languages::require("typescript").unwrap();
                assert!(profile.manifest_filenames().contains(&"package.json"));
                let dependencies = profile.extract_manifest_dependencies(
        "package.json",
        r#"{"dependencies":{"react":"^18","@scope/pkg":"1","local":"workspace:*"},"devDependencies":{"vitest":"1"},"peerDependencies":{"typescript":"5"},"optionalDependencies":{"ignored":"1"}}"#,
    );
                assert_eq!(
                    dependencies,
                    ["react", "@scope/pkg", "vitest", "typescript"]
                );
                assert!(profile.is_stdlib("fs"));
                assert!(profile.is_stdlib("node:fs/promises"));
                assert!(profile.is_stdlib("node:custom-built-in"));
                assert!(!profile.is_stdlib("@scope/fs"));
                assert!(profile.builtin("process"));
                assert!(profile.builtin("Buffer"));
            }
            #[cfg(feature = "lang-typescript")]
            // npm_lockfiles_register_direct_transitive_and_scoped_dependencies
            Case::NpmLockfilesRegisterDirectTransitiveAndScopedDependencies => {
                use std::collections::BTreeSet;

                let profile = languages::require("typescript").unwrap();
                for filename in ["package-lock.json", "pnpm-lock.yaml", "yarn.lock"] {
                    assert!(
                        profile.manifest_filenames().contains(&filename),
                        "{filename}"
                    );
                }
                let cases: [(&str, &str, &[&str]); 3] = [
        (
            "package-lock.json",
            r#"{
  "lockfileVersion": 3,
  "packages": {
    "": {"name": "app", "dependencies": {"axios": "^1"}},
    "node_modules/axios": {"version": "1.0.0"},
    "node_modules/@vue/runtime-core": {"version": "3.5.0"},
    "node_modules/axios/node_modules/follow-redirects": {"version": "1.0.0"},
    "node_modules/local-link": {"resolved": "packages/local", "link": true}
  },
  "dependencies": {"legacy-only": {"version": "1.0.0"}}
}"#,
            &["axios", "@vue/runtime-core", "follow-redirects", "legacy-only"],
        ),
        (
            "pnpm-lock.yaml",
            "lockfileVersion: '9.0'\ndependencies:\n  legacy-only: 1.0.0\nimporters:\n  .:\n    dependencies:\n      '@types/node':\n        specifier: ^22.0.0\n        version: 22.0.0\n      workspace-local:\n        specifier: workspace:*\n        version: link:../local\n      linked-local:\n        specifier: ^1.0.0\n        version: link:../linked\npackages:\n  '@vue/runtime-core@3.5.0':\n    resolution: {integrity: sha512-example}\n  axios@1.0.0:\n    resolution: {integrity: sha512-example}\n  'workspace-local@link:../local':\n    resolution: {directory: ../local, type: directory}\n  'empty-selector@':\n",
            &["@types/node", "@vue/runtime-core", "axios", "legacy-only"],
        ),
        (
            "yarn.lock",
            "# yarn lockfile v1\n\"@vue/runtime-core@^3.5.0\":\n  version \"3.5.0\"\naxios@^1.0.0:\n  version \"1.0.0\"\n  dependencies:\n    follow-redirects \"^1.0.0\"\nfollow-redirects@^1.0.0:\n  version \"1.0.0\"\nlocal@workspace:*:\n  version \"0.0.0-use.local\"\nempty-selector@:\n",
            &["@vue/runtime-core", "axios", "follow-redirects"],
        ),
    ];
                let mut mismatches = Vec::new();
                for (filename, content, expected) in cases {
                    let actual: BTreeSet<_> = profile
                        .extract_manifest_dependencies(filename, content)
                        .into_iter()
                        .collect();
                    let expected: BTreeSet<_> =
                        expected.iter().map(|name| (*name).to_owned()).collect();
                    if actual != expected {
                        mismatches.push(format!(
                            "{filename}: actual={actual:?}, expected={expected:?}"
                        ));
                    }
                }
                assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
            }
            #[cfg(feature = "lang-typescript")]
            // package_lock_v1_nested_dependencies_register_transitive_packages
            Case::PackageLockV1NestedDependenciesRegisterTransitivePackages => {
                use std::collections::BTreeSet;

                let profile = languages::require("javascript").unwrap();
                let content = r#"{
  "lockfileVersion": 1,
  "dependencies": {
    "legacy-parent": {
      "version": "1.0.0",
      "dependencies": {
        "legacy-child": {
          "version": "2.0.0",
          "dependencies": {"@types/nested": {"version": "3.0.0"}}
        }
      }
    }
  }
}"#;
                let actual: BTreeSet<_> = profile
                    .extract_manifest_dependencies("package-lock.json", content)
                    .into_iter()
                    .collect();
                assert_eq!(
                    actual,
                    BTreeSet::from([
                        "legacy-parent".to_owned(),
                        "legacy-child".to_owned(),
                        "@types/nested".to_owned(),
                    ])
                );
            }
            #[cfg(feature = "lang-rust")]
            // cargo_dependencies_include_workspace_and_target_scoped_declarations
            Case::CargoDependenciesIncludeWorkspaceAndTargetScopedDeclarations => {
                let profile = languages::require("rust").unwrap();
                let manifest = "[dependencies]\nserde = \"1\"\n\"my-crate\" = { version = \"2\", package = \"actual-crate\" }\n\n[dev-dependencies]\npretty_assertions = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nunix-only = \"1\"\n\n[workspace.dependencies]\nshared-crate = \"1\"\n\n[package.metadata.dependencies]\nnot-a-dependency = \"1\"\n\n[package]\nname = \"not-a-dependency\"\n";
                let dependencies = profile.extract_manifest_dependencies("Cargo.toml", manifest);
                assert_eq!(
                    dependencies,
                    [
                        "my_crate",
                        "pretty_assertions",
                        "serde",
                        "shared_crate",
                        "unix_only"
                    ]
                );
                assert!(profile.is_stdlib("std::collections"));
                assert!(profile.is_stdlib("core::fmt"));
                assert!(profile.is_stdlib("proc_macro::TokenStream"));
                assert!(!profile.is_stdlib("std_extra::thing"));
            }
            #[cfg(feature = "lang-go")]
            // go_mod_dependencies_include_modules_and_versioned_paths
            Case::GoModDependenciesIncludeModulesAndVersionedPaths => {
                let profile = languages::require("go").unwrap();
                assert_eq!(profile.manifest_filenames(), ["go.mod"]);
                let manifest = "module example.com/app\n\ngo 1.22\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgolang.org/x/sync v0.7.0 // indirect\n)\n\nrequire rsc.io/quote/v3 v3.1.0\n";
                let dependencies = profile.extract_manifest_dependencies("go.mod", manifest);
                assert_eq!(
                    dependencies,
                    [
                        "github.com/gin-gonic/gin",
                        "golang.org/x/sync",
                        "rsc.io/quote/v3"
                    ]
                );
            }
            #[cfg(feature = "lang-java")]
            // gradle_and_maven_dependencies_include_configurations_and_group_ids
            Case::GradleAndMavenDependenciesIncludeConfigurationsAndGroupIds => {
                let profile = languages::require("java").unwrap();
                assert_eq!(
                    profile.manifest_filenames(),
                    ["pom.xml", "build.gradle", "build.gradle.kts"]
                );
                let pom = r#"<project>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>33.0.0-jre</version>
    </dependency>
  </dependencies>
</project>"#;
                assert_eq!(
                    profile.extract_manifest_dependencies("pom.xml", pom),
                    ["com.google.guava"]
                );

                let gradle = r#"dependencies {
    implementation("org.springframework.boot:spring-boot-starter-web")
    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.0'
}"#;
                let mut dependencies =
                    profile.extract_manifest_dependencies("build.gradle.kts", gradle);
                dependencies.sort();
                assert_eq!(
                    dependencies,
                    ["org.junit.jupiter", "org.springframework.boot"]
                );
            }
            #[cfg(feature = "lang-python")]
            // pyproject_and_pipfile_dependencies_include_optional_and_poetry_groups
            Case::PyprojectAndPipfileDependenciesIncludeOptionalAndPoetryGroups => {
                let profile = languages::require("python").unwrap();
                let pyproject = "[project]\nname='ignored'\ndependencies = [\n 'New-Package[extra]>=1; python_version > \\\"3\\\"', # comment\n \"another_package @ https://example.test/archive.whl\",\n]\n[project.optional-dependencies]\ntest=['pytest>=8']\n[tool.poetry.dependencies]\npython='^3.12'\nrequests={version='*',extras=['security']}\n[tool.poetry.group.dev.dependencies]\nhttpx='*'\n[tool.unrelated]\nrequests='ignored'\n";
                assert_eq!(
                    profile.extract_manifest_dependencies("pyproject.toml", pyproject),
                    vec![
                        "another_package",
                        "httpx",
                        "new_package",
                        "pytest",
                        "requests"
                    ]
                );
                assert_eq!(
        profile.extract_manifest_dependencies(
            "Pipfile",
            "[packages]\n\"new-package\"='*'\n[dev-packages]\npytest={version='*'}\n[requires]\npython_version='3.12'\n"
        ),
        vec!["new_package", "pytest"]
    );
            }
            #[cfg(feature = "lang-python")]
            // requirements_and_setup_cfg_parse_package_names_without_options_or_versions
            Case::RequirementsAndSetupCfgParsePackageNamesWithoutOptionsOrVersions => {
                let profile = languages::require("python").unwrap();
                assert_eq!(
        profile.extract_manifest_dependencies(
            "requirements-dev.txt",
            "# comment\nnew-package[feature]>=1\nrequests @ https://example.test/requests.whl\n-r requirements.txt\n--index-url https://example.test\n-e git+https://example.test/repo#egg=editable-package\n./local-package\n"
        ),
        vec!["editable_package", "new_package", "requests"]
    );
                assert_eq!(
        profile.extract_manifest_dependencies(
            "setup.cfg",
            "[metadata]\nname=ignore\n[options]\ninstall_requires =\n    new-package>=1\n    requests\npython_requires = >=3.10\n[options.extras_require]\ntest =\n    pytest>=8\n"
        ),
        vec!["new_package", "pytest", "requests"]
    );
                assert!(profile.is_stdlib("pathlib.Path"));
                assert!(profile.external_import("unregistered_package").is_none());
            }
            #[cfg(feature = "lang-python")]
            // pyproject_dependencies_accept_toml_multiline_strings_and_unicode_escapes
            Case::PyprojectDependenciesAcceptTomlMultilineStringsAndUnicodeEscapes => {
                let profile = languages::require("python").unwrap();
                let source = r#"[project]
dependencies = [
    '''
literal-package>=1''',
    """
basic-package>=1""",
    "\U0000006Eovel-package>=1",
]
"#;
                assert_eq!(
                    profile.extract_manifest_dependencies("pyproject.toml", source),
                    vec!["basic_package", "literal_package", "novel_package"]
                );
            }
            #[cfg(feature = "lang-python")]
            // requirements_reject_bare_urls_and_local_artifacts
            Case::RequirementsRejectBareUrlsAndLocalArtifacts => {
                let profile = languages::require("python").unwrap();
                assert_eq!(
        profile.extract_manifest_dependencies(
            "requirements.txt",
            "git+https://example.test/repository\nhttps://example.test/archive.whl\nfile:///tmp/archive.whl\nrelative/archive.whl\narchive.whl\narchive.tar.gz\nvalid-package @ git+https://example.test/repository\n"
        ),
        vec!["valid_package"]
    );
            }
        }
    }
}

#[test]
fn manifest_parsers_and_module_prefixes_matrix() {
    use languages::dependency_registry::{
        Ecosystem, ManifestSource, WorkspaceManifestSources, MANIFEST_PARSERS,
    };

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
            .find(|parser| parser.ecosystem() == case.ecosystem && parser.matches_filename(case.filename))
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
        let autolib_registry = DependencyRegistry::try_collect_from_sources([WorkspaceManifestSources {
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
        ("php", "monolog/monolog", "backend/src/User.php", "backend/composer.json"),
        ("csharp", "Dapper", "dotnet/Invoice.cs", "dotnet/Billing.csproj"),
        ("java", "com.squareup.okhttp3", "jvm/App.java", "jvm/pom.xml"),
        ("kotlin", "com.squareup.okhttp3", "jvm/App.kt", "jvm/pom.xml"),
        ("ruby", "puma", "ruby/app.rb", "ruby/Gemfile"),
        ("c", "ZLIB", "native/main.c", "native/CMakeLists.txt"),
        ("cpp", "ZLIB", "native/main.cpp", "native/CMakeLists.txt"),
    ] {
        if let Some(profile) = languages::by_id(lang_id) {
            assert_eq!(
                registry.classification_for_path(profile, module, path).as_deref(),
                Some(format!("external dependency (manifest: {expected_origin})").as_str()),
                "{lang_id}: {module}"
            );
        }
    }
}
