pub(crate) mod build_manifest;
/// Per-file language context, rule IR, and scope projections.
pub mod context;
/// Workspace dependency discovery and classification.
pub mod dependency_registry;
/// Typed framework manifests consumed by existing language linkers.
pub mod manifests;
/// Core trait implemented by compiled language profiles.
pub mod profile;
/// Package and TypeScript path registries.
pub mod registries;
/// Framework route extraction and prefix resolution.
pub mod routes;
/// Semantic dependency facts and rule matching.
pub mod semantic;
/// SQL embedded-query profile and literal extractors.
pub(crate) mod sql;
#[doc(hidden)]
pub mod support;
#[cfg(any(feature = "lang-html", feature = "lang-vue"))]
pub mod template;
#[cfg(any(feature = "lang-python", feature = "lang-rust", feature = "lang-toml"))]
pub(crate) mod toml_manifest;
/// Shared leaf language types, parser pool, and grammar selectors.
pub mod types;

pub(crate) use context::file_providers;
pub use context::LanguageContext;
pub(crate) use context::RuleIRHandle;
pub use profile::LanguageProfile;
pub use types::*;

use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

include!(concat!(env!("OUT_DIR"), "/language_profiles.rs"));

fn registry() -> impl Iterator<Item = &'static dyn LanguageProfile> + Clone {
    PROFILE_GROUPS
        .iter()
        .flat_map(|group| group.iter().copied())
        .chain(std::iter::once(&sql::SQL as &'static dyn LanguageProfile))
}

/// Validates uniqueness and extension invariants across language profiles.
fn validate_profiles(
    profiles: impl IntoIterator<Item = &'static dyn LanguageProfile>,
) -> Result<()> {
    let mut ids = HashSet::new();
    let mut extensions = HashSet::new();
    for profile in profiles {
        anyhow::ensure!(
            !profile.id().is_empty() && ids.insert(profile.id()),
            "duplicate or empty language profile id: {}",
            profile.id()
        );
        anyhow::ensure!(
            !profile.extensions().is_empty(),
            "profile {} has no extensions",
            profile.id()
        );
        for extension in profile.extensions() {
            anyhow::ensure!(
                !extension.is_empty()
                    && extension.bytes().all(|b| b.is_ascii_alphanumeric())
                    && extensions.insert(*extension),
                "duplicate or invalid language extension: {extension}"
            );
        }
    }
    Ok(())
}

/// Iterates over all compiled [`LanguageProfile`] instances.
pub fn profiles() -> impl Iterator<Item = &'static dyn LanguageProfile> + Clone {
    static VALIDATED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    VALIDATED.get_or_init(|| {
        validate_profiles(registry()).expect("invalid compiled language registry")
    });
    registry()
}

/// Looks up a compiled language profile by identifier.
pub fn by_id(id: &str) -> Option<&'static dyn LanguageProfile> {
    profiles().find(|p| p.id() == id)
}

/// Locates sorted UTF-8 byte ranges for syntax nodes matching `kind`, `start` (1-based line), and `name`.
pub(crate) fn find_symbol_byte_ranges(
    profile: &dyn LanguageProfile,
    path: &str,
    text: &str,
    kind: &str,
    start: usize,
    name: &str,
) -> Result<Vec<std::ops::Range<usize>>> {
    let mut parser = profile.create_parser(path)?;
    parser.set_timeout_micros(2_000_000);
    let tree = parser
        .parse(text, None)
        .context("source preview parse exceeds time budget")?;
    let mut stack = vec![tree.root_node()];
    let mut matches = Vec::new();
    while let Some(syntax) = stack.pop() {
        if syntax.start_position().row + 1 > start || syntax.end_position().row + 1 < start {
            continue;
        }
        if syntax.start_position().row + 1 == start
            && profile.symbol_with_source(syntax, text) == Some(kind)
        {
            let candidate = profile
                .symbol_name(syntax, text)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("anonymous@{}:{}", start, syntax.start_position().column + 1)
                });
            if candidate == name {
                matches.push(syntax.byte_range());
            }
        }
        let mut cursor = syntax.walk();
        stack.extend(syntax.named_children(&mut cursor));
    }
    matches.sort_by_key(|range| range.start);
    Ok(matches)
}

/// Looks up a required language profile or returns an actionable feature diagnostic.
pub fn require(id: &str) -> Result<&'static dyn LanguageProfile> {
    if let Some(profile) = by_id(id) {
        return Ok(profile);
    }
    let provider = if id == "javascript" { "typescript" } else { id };
    let feature = format!("lang-{provider}");
    let compiled = profiles().map(|p| p.id()).collect::<Vec<_>>().join(", ");
    let compiled = if compiled.is_empty() {
        "none (document-only build)"
    } else {
        &compiled
    };
    if AVAILABLE_LANGUAGE_FEATURES.contains(&feature.as_str()) {
        anyhow::bail!(
            "AST language '{id}' is disabled; rebuild with --features {feature} or all-languages; compiled languages: {compiled}"
        );
    }
    anyhow::bail!("unknown AST language '{id}'; compiled languages: {compiled}");
}

/// Finds the compiled language profile matching a file path's extension.
pub fn for_path(path: &Path) -> Option<&'static dyn LanguageProfile> {
    let extension = path.extension()?.to_str()?;
    profiles().find(|p| p.extensions().contains(&extension))
}

/// Computes a dotted module name for `path` using the matching compiled language profile.
pub fn module_name(path: &str) -> String {
    for_path(Path::new(path)).map_or_else(
        || module_stem(path).replace('/', "."),
        |p| p.module_name(path),
    )
}

fn push_namespace_candidates(
    profile: &dyn LanguageProfile,
    ns: &str,
    out: &mut BTreeSet<String>,
) {
    let mut cur = ns;
    loop {
        if !cur.is_empty() {
            let slash = cur.replace('.', "/");
            for ext in profile.extensions() {
                out.insert(format!("{slash}.{ext}"));
                if *ext == "rs" {
                    out.insert(format!("{slash}/mod.rs"));
                    out.insert(format!("{slash}/lib.rs"));
                    out.insert(format!("{slash}/main.rs"));
                    out.insert(format!("{slash}/src/lib.rs"));
                    out.insert(format!("{slash}/src/main.rs"));
                } else if *ext == "py" {
                    out.insert(format!("{slash}/__init__.py"));
                } else if matches!(profile.family().0, "javascript") {
                    out.insert(format!("{slash}/index.{ext}"));
                }
            }
        }
        match cur.rsplit_once('.') {
            Some((parent, _)) => cur = parent,
            None => break,
        }
    }
}

fn collect_js_import_candidates(
    registry: &dependency_registry::DependencyRegistry,
    profile: &dyn LanguageProfile,
    owner_dir: &Path,
    owner_path: &str,
    module: &str,
    out: &mut BTreeSet<String>,
) {
    if module.starts_with("./") || module.starts_with("../") {
        let joined = acdd_core::fs::normalize_path(&owner_dir.join(module));
        let rel = joined.to_string_lossy().replace('\\', "/");
        if !rel.starts_with("..") && !rel.starts_with('/') && !rel.is_empty() {
            out.insert(rel.clone());
            for ext in profile.extensions() {
                out.insert(format!("{rel}.{ext}"));
                out.insert(format!("{rel}/index.{ext}"));
            }
        }
        return;
    }
    if profile.id() == "typescript" {
        for cand in registry.typescript_paths.candidate_paths(owner_path, module) {
            out.insert(cand.clone());
            for ext in profile.extensions() {
                out.insert(format!("{cand}.{ext}"));
                out.insert(format!("{cand}/index.{ext}"));
            }
        }
    }
    if let Some(pkg) = registry.javascript_packages.normalize_import(owner_path, module) {
        push_namespace_candidates(profile, &pkg.namespace, out);
    }
}

#[cfg(feature = "lang-rust")]
fn collect_rust_import_candidates(
    registry: &dependency_registry::DependencyRegistry,
    profile: &dyn LanguageProfile,
    owner_path: &str,
    module: &str,
    out: &mut BTreeSet<String>,
) {
    if let Some(root_crate) = registry.root_rust_crate_for_path(owner_path) {
        if let Some(norm) = rust::RUST.normalize_root_crate_import(root_crate, module) {
            push_namespace_candidates(profile, &norm.namespace, out);
        }
    }
    let (first, tail) = module.split_once("::").unwrap_or((module, ""));
    let mut push_rust_scope = |pkg_scope: &str, use_slash: bool| {
        let base = if pkg_scope.is_empty() {
            "src".to_owned()
        } else if use_slash {
            format!("{}.src", pkg_scope.replace('/', "."))
        } else {
            format!("{pkg_scope}.src")
        };
        let ns = if tail.is_empty() || tail == "*" {
            base
        } else {
            format!("{base}.{}", tail.replace("::", "."))
        };
        push_namespace_candidates(profile, &ns, out);
    };
    if let Some((pkg_scope, own_lib)) = registry.sibling_rust_package_library_for_path(owner_path) {
        if own_lib == first {
            push_rust_scope(pkg_scope, true);
        }
    }
    for (target_workspace, lib_name) in &registry.rust_libraries {
        if lib_name == first
            && (registry.sibling_rust_library_for_path(owner_path) == Some(lib_name.as_str())
                || registry.declares_for_path(LanguageFamily("rust"), owner_path, first))
        {
            push_rust_scope(target_workspace, false);
        }
    }
}

impl dependency_registry::DependencyRegistry {
    /// Resolves bounded worktree-relative file candidates referenced by an import expression.
    pub fn import_file_candidates(
        &self,
        root: Option<&Path>,
        owner_path: &str,
        module: &str,
    ) -> Vec<String> {
        let Some(profile) = for_path(Path::new(owner_path)) else {
            return Vec::new();
        };
        let mut out = BTreeSet::new();
        let owner_dir = Path::new(owner_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));

        if matches!(profile.family().0, "javascript") {
            collect_js_import_candidates(self, profile, owner_dir, owner_path, module, &mut out);
        }
        #[cfg(feature = "lang-rust")]
        if profile.id() == "rust" {
            collect_rust_import_candidates(self, profile, owner_path, module, &mut out);
        }
        if let Some(norm) = profile.normalize_import_with_root(root, owner_path, module) {
            push_namespace_candidates(profile, &norm.namespace, &mut out);
        }
        out.into_iter().collect()
    }

    fn is_removed_rule_identity_active(
        &self,
        path: &str,
        language: &str,
        bundled: &manifests::FrameworkManifest,
        identity: &manifests::RuleIdentity,
    ) -> bool {
        use manifests::{ActivationCondition, ActivationMatch, RuleIdentity};
        match identity {
            RuleIdentity::Semantic { id } => bundled.semantic_rules.iter().any(|rule| {
                &rule.id == id
                    && rule.language() == language
                    && ActivationCondition::matches_workspace_slice(&rule.activation, |name| {
                        by_id(rule.language())
                            .is_some_and(|p| self.declares_for_path(p.family(), path, name))
                    }) != ActivationMatch::Inactive
            }),
            _ => true,
        }
    }

    fn collect_removed_rules_for_file(
        &self,
        path: &str,
        language: &str,
        removed_by_group: &mut BTreeMap<(String, String, String, manifests::RuleDomain), BTreeSet<manifests::RuleIdentity>>,
    ) {
        use manifests::{named_kind, RuleDomain};
        let Some(profile) = by_id(language) else {
            return;
        };
        if !profile.has_grammar() {
            return;
        }
        let grammar = profile.grammar(path);
        let (workspace, _) = workspace_path(path);
        let call = ["call", "call_expression"]
            .into_iter()
            .any(|name| named_kind(&grammar, name).is_some());
        let has_type_decl = [
            "class_definition",
            "class_declaration",
            "struct_item",
            "enum_item",
            "trait_item",
            "interface_declaration",
            "type_spec",
        ]
        .into_iter()
        .any(|name| named_kind(&grammar, name).is_some());
        let routes = profile.route_extraction_enabled();
        for ((scope, framework), effective) in &self.framework_manifests {
            if scope != workspace {
                continue;
            }
            let Some(bundled) = self.standard_framework_manifests.get(framework) else {
                continue;
            };
            for (domain, eligible) in [
                (RuleDomain::ClassHierarchyReceivers, has_type_decl),
                (RuleDomain::DiCalls, call),
                (RuleDomain::RouteCalls, routes && call),
                (
                    RuleDomain::RouteDecorators,
                    routes && call && named_kind(&grammar, "decorator").is_some(),
                ),
                (
                    RuleDomain::SemanticRules,
                    call && bundled
                        .semantic_rules
                        .iter()
                        .any(|rule| rule.language() == language),
                ),
            ] {
                if !eligible {
                    continue;
                }
                let baseline = bundled.rule_identities(domain);
                let current = effective.rule_identities(domain);
                for identity in baseline.difference(&current) {
                    if self.is_removed_rule_identity_active(path, language, bundled, identity) {
                        removed_by_group
                            .entry((
                                framework.clone(),
                                workspace.to_owned(),
                                language.to_owned(),
                                domain,
                            ))
                            .or_default()
                            .insert(identity.clone());
                    }
                }
            }
        }
    }

    /// Reports removed eligible bundled identities for the complete current file inventory.
    pub fn framework_rule_diagnostics<'a>(
        &self,
        inventory: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<manifests::RuleCoverageDiagnostic> {
        use manifests::{RuleCoverageCode, RuleCoverageDiagnostic, RuleDomain, RuleIdentity};
        let mut removed_by_group =
            BTreeMap::<(String, String, String, RuleDomain), BTreeSet<RuleIdentity>>::new();
        for (path, language) in inventory {
            self.collect_removed_rules_for_file(path, language, &mut removed_by_group);
        }
        removed_by_group
            .into_iter()
            .map(|((framework, workspace, language, rule_domain), removed)| {
                RuleCoverageDiagnostic {
                    framework,
                    workspace,
                    language,
                    rule_domain,
                    code: RuleCoverageCode::EligibleRuleIdentityRemoved,
                    removed_eligible_rule_identities: removed.into_iter().collect(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::manifests::semantic::{ActivationCondition, FileActivationSignals};
    use super::manifests::syntax::{SyntaxOperation, SyntaxRule};
    use super::{profiles, validate_profiles, LanguageContext};

    #[test]
    fn registry_profiles_validate_and_reject_duplicates() {
        use super::profile::ProfileLinker;
        let all: Vec<_> = profiles().collect();
        validate_profiles(all.iter().copied()).unwrap();
        for profile in &all {
            let expected = !matches!(profile.id(), "python" | "rust" | "typescript" | "javascript" | "vue");
            assert_eq!(profile.linker().allows_multi_file_modules(), expected, "{}", profile.id());
        }
        if let Some(first) = all.first() {
            assert!(validate_profiles([*first, *first]).is_err());
        }
    }

    #[test]
    fn language_context_gates_syntax_rules_by_persisted_activation_signals() {
        let rule = SyntaxRule {
            pack: "alpinejs".into(),
            id: "implicit-receivers".into(),
            language: "javascript".into(),
            package_family: None,
            activation: vec![ActivationCondition::SyntaxAttribute {
                name: "x-data".into(),
            }],
            display: Default::default(),
            operation: SyntaxOperation::ImplicitReceivers {
                names: vec!["$el".into()],
            },
        };
        let mut ctx = LanguageContext::default();
        ctx.syntax_rules = vec![rule.clone()];
        ctx.prepare_named_member_rules();

        let mut active_signals = FileActivationSignals::default();
        active_signals.syntax_attributes.insert("x-data".into());

        assert!(ctx.is_syntax_rule_active(&rule, "app.js", &active_signals));
        assert!(!ctx.is_syntax_rule_active(&rule, "app.js", &FileActivationSignals::default()));
    }

    #[cfg(feature = "lang-typescript")]
    #[test]
    fn syntax_grammar_spec_compiles_valid_and_rejects_unknown_selectors() {
        use super::{CallForm, DeclarationRole, ImportKind, SyntaxGrammarSpec, Visibility};

        let grammar = tree_sitter_typescript::language_typescript();
        let valid = SyntaxGrammarSpec {
            declaration_roles: &[("function_declaration", DeclarationRole::Function)],
            call_forms: &[
                ("call_expression", CallForm::Member),
                ("new_expression", CallForm::Constructor),
            ],
            import_forms: &[("import_statement", ImportKind::Module)],
            visibility_modifiers: &[("accessibility_modifier", Visibility::Public)],
            typed_bindings: &["variable_declarator"],
            assignments: &["assignment_expression"],
            constructors: &["new_expression"],
            member_calls: &["call_expression"],
            heritage: &["extends_clause"],
            conditionals: &["if_statement"],
            ..SyntaxGrammarSpec::DEFAULT
        };
        let compiled = valid.compile(&grammar).expect("valid spec compiles");
        assert!(compiled.has_value_flow_selectors());
        assert_eq!(
            compiled
                .classify(grammar.id_for_node_kind("function_declaration", true))
                .and_then(|k| k.declaration_role()),
            Some(DeclarationRole::Function)
        );
        assert_eq!(
            compiled
                .classify(grammar.id_for_node_kind("call_expression", true))
                .and_then(|k| k.call_form()),
            Some(CallForm::Member)
        );
        assert_eq!(
            compiled
                .classify(grammar.id_for_node_kind("import_statement", true))
                .and_then(|k| k.import_kind()),
            Some(ImportKind::Module)
        );
        assert_eq!(
            compiled
                .classify(grammar.id_for_node_kind("accessibility_modifier", true))
                .and_then(|k| k.visibility()),
            Some(Visibility::Public)
        );
        let assign = compiled
            .classify(grammar.id_for_node_kind("assignment_expression", true))
            .unwrap();
        assert!(assign.is_assignment());
        let extends = compiled
            .classify(grammar.id_for_node_kind("extends_clause", true))
            .unwrap();
        assert!(extends.is_heritage());

        let invalid_cases = [
            SyntaxGrammarSpec { declaration_roles: &[("bogus_kind", DeclarationRole::Function)], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { call_forms: &[("bogus_kind", CallForm::Direct)], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { import_forms: &[("bogus_kind", ImportKind::Static)], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { visibility_modifiers: &[("bogus_kind", Visibility::Private)], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { typed_bindings: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { assignments: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { constructors: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { member_calls: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { heritage: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
            SyntaxGrammarSpec { conditionals: &["bogus_kind"], ..SyntaxGrammarSpec::DEFAULT },
        ];
        for (idx, invalid) in invalid_cases.iter().enumerate() {
            assert!(
                invalid.compile(&grammar).is_err(),
                "invalid spec case {idx} must fail closed"
            );
        }
    }
}
