use super::super::manifests::{
    named_kind, ActivationCondition, ActivationMatch, FrameworkManifest, RuleCoverageCode,
    RuleCoverageDiagnostic, RuleDomain, RuleIRHandle, RuleIdentity,
};
use super::super::{profiles, LanguageFamily};
use super::{package_identity, DependencyRegistry, LanguageContextKey, ManifestSnapshots};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
    sync::Arc,
};

struct DirectoryContextEntry {
    registry_digest: String,
    dir: String,
    context: Arc<super::super::LanguageContext>,
}

thread_local! {
    static LEGACY_LANGUAGE_CONTEXT_CACHE: RefCell<HashMap<(String, LanguageContextKey), Arc<super::super::LanguageContext>>> =
        RefCell::new(HashMap::new());
    static DIRECTORY_LANGUAGE_CONTEXT_CACHE: RefCell<HashMap<u64, DirectoryContextEntry>> =
        RefCell::new(HashMap::new());
}

const STANDARD_FRAMEWORK_MANIFESTS: &[(&str, &str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/framework_manifests.rs"));

static STANDARD_FRAMEWORK_MANIFESTS_DIGEST: std::sync::OnceLock<String> =
    std::sync::OnceLock::new();

static PARSED_STANDARD_FRAMEWORK_MANIFESTS: std::sync::OnceLock<
    std::result::Result<HashMap<String, FrameworkManifest>, String>,
> = std::sync::OnceLock::new();

pub(super) fn load_standard_framework_manifests(registry: &mut DependencyRegistry) -> Result<()> {
    let manifests = PARSED_STANDARD_FRAMEWORK_MANIFESTS.get_or_init(|| {
        STANDARD_FRAMEWORK_MANIFESTS
            .iter()
            .map(|&(name, extension, source)| {
                FrameworkManifest::parse(name.to_owned(), extension, source)
                    .with_context(|| format!("invalid standard framework manifest {name}"))
                    .map(|manifest| (name.to_owned(), manifest))
                    .map_err(|error| format!("{error:#}"))
            })
            .collect()
    });
    registry.standard_framework_manifests = manifests
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .clone();
    Ok(())
}

pub(super) fn record_framework_manifest(
    workspace: &str,
    origin_prefix: &str,
    relative_path: &Path,
    content: &str,
    registry: &mut DependencyRegistry,
    snapshots: &mut ManifestSnapshots,
) -> Result<()> {
    let extension = relative_path
        .extension()
        .and_then(|value| value.to_str())
        .context("framework manifest extension missing")?;
    let name = relative_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .context("framework manifest filename must have a UTF-8 identifier")?
        .to_owned();
    let manifest = FrameworkManifest::parse(name.clone(), extension, content)?;
    let manifest = registry
        .standard_framework_manifests
        .get(&name)
        .map_or(manifest.clone(), |bundled| {
            manifest.with_bundled_rules(bundled)
        });
    if registry
        .framework_manifests
        .insert((workspace.to_owned(), name.clone()), manifest)
        .is_some()
    {
        bail!("duplicate framework manifest {name} in workspace {workspace}");
    }
    let origin = format!(
        "{}{}",
        origin_prefix,
        relative_path.to_string_lossy().replace('\\', "/")
    );
    snapshots.insert(origin, Sha256::digest(content.as_bytes()));
    Ok(())
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {label} {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() {
        bail!("{label} must not be a symlink: {}", path.display());
    }
    if !metadata.is_dir() {
        bail!("{label} path is not a directory: {}", path.display());
    }
    Ok(true)
}

pub(super) fn load_framework_manifests(
    root: &Path,
    workspace: &str,
    origin_prefix: &str,
    registry: &mut DependencyRegistry,
    snapshots: &mut ManifestSnapshots,
) -> Result<()> {
    if !ensure_real_directory(&root.join(".acdd"), "configuration directory")? {
        return Ok(());
    }
    let directory = root.join(".acdd/frameworks");
    if !ensure_real_directory(&directory, "framework manifest directory")? {
        return Ok(());
    }

    for entry in fs::read_dir(&directory).with_context(|| {
        format!(
            "failed to read framework manifest directory {}",
            directory.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect entries in framework manifest directory {}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let Some("toml" | "yaml" | "yml") =
            path.extension().and_then(|extension| extension.to_str())
        else {
            continue;
        };
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect framework manifest {}", path.display()))?;
        if file_type.is_symlink() {
            bail!(
                "framework manifest must not be a symlink: {}",
                path.display()
            );
        }
        if !file_type.is_file() {
            continue;
        }
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .context("framework manifest filename must have a UTF-8 identifier")?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to inspect framework manifest {}", path.display()))?;
        if metadata.len() > 4 * 1024 * 1024 {
            bail!(
                "framework manifest exceeds the 4 MiB limit: {}",
                path.display()
            );
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read framework manifest {}", path.display()))?;
        let relative_path = path.strip_prefix(root).with_context(|| {
            format!(
                "framework manifest {} escaped its workspace root {}",
                path.display(),
                root.display()
            )
        })?;
        record_framework_manifest(
            workspace,
            origin_prefix,
            relative_path,
            &content,
            registry,
            snapshots,
        )
        .with_context(|| format!("failed to load framework manifest {}", path.display()))?;
    }
    Ok(())
}

fn push_route_origins(
    origins: &mut HashMap<(bool, usize), crate::semantic::ExtractionRuleOrigin>,
    is_decorator: bool,
    base: usize,
    pack: &str,
    identities: impl Iterator<Item = RuleIdentity>,
) {
    for (index, identity) in identities.enumerate() {
        origins.insert(
            (is_decorator, base + index),
            crate::semantic::ExtractionRuleOrigin {
                pack: pack.to_owned(),
                rule: serde_json::to_string(&identity).expect("typed route identity"),
            },
        );
    }
}

impl DependencyRegistry {
    /// Loads the built-in framework rules without requiring dependency manifests.
    pub(crate) fn bundled_framework_rules() -> Self {
        let mut registry = Self::default();
        load_standard_framework_manifests(&mut registry)
            .expect("bundled framework manifests are validated at compile time");
        registry
    }

    fn effective_manifest_names(&self, workspace: &str) -> BTreeSet<&str> {
        let mut names: BTreeSet<&str> = self
            .standard_framework_manifests
            .keys()
            .map(String::as_str)
            .collect();
        names.extend(
            self.framework_manifests
                .keys()
                .filter(|(scope, _)| scope == workspace)
                .map(|(_, name)| name.as_str()),
        );
        names
    }

    /// Reuses merged extraction rules for files with the same workspace and package scope.
    pub fn language_context_for_path(&self, path: &str) -> Arc<super::super::LanguageContext> {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(self.digest.as_str(), dir), &mut hasher);
        let hash = std::hash::Hasher::finish(&hasher);
        if let Some(context) = DIRECTORY_LANGUAGE_CONTEXT_CACHE.with(|cache| {
            cache.borrow().get(&hash).and_then(|entry| {
                (entry.registry_digest == self.digest && entry.dir == dir)
                    .then(|| Arc::clone(&entry.context))
            })
        }) {
            return context;
        }

        let key = self.language_context_key(path);
        let context = if let Some(context) = self.language_contexts.get(&key) {
            Arc::clone(context)
        } else {
            let key = (self.digest.clone(), key);
            if let Some(context) =
                LEGACY_LANGUAGE_CONTEXT_CACHE.with(|cache| cache.borrow().get(&key).cloned())
            {
                context
            } else {
                let mut context = self.build_language_context(path);
                if !self
                    .rule_ir
                    .contains_key(super::super::workspace_path(path).0)
                {
                    context.rule_ir = RuleIRHandle::prepare(&context);
                }
                let context = Arc::new(context);
                LEGACY_LANGUAGE_CONTEXT_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 128 {
                        cache.clear();
                    }
                    cache.insert(key, Arc::clone(&context));
                });
                context
            }
        };

        DIRECTORY_LANGUAGE_CONTEXT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 512 {
                cache.clear();
            }
            cache.insert(
                hash,
                DirectoryContextEntry {
                    registry_digest: self.digest.clone(),
                    dir: dir.to_owned(),
                    context: Arc::clone(&context),
                },
            );
        });
        context
    }

    fn language_context_key(&self, path: &str) -> LanguageContextKey {
        let (workspace, _) = super::super::workspace_path(path);
        let families: BTreeSet<_> = profiles().map(|profile| profile.family().0).collect();
        let scopes = families
            .into_iter()
            .map(|family| {
                let scope = self
                    .nearest_manifest_scope_for_path(LanguageFamily(family), path)
                    .unwrap_or(workspace);
                (family.to_owned(), scope.to_owned())
            })
            .collect();
        (workspace.to_owned(), scopes)
    }

    fn build_language_context(&self, path: &str) -> super::super::LanguageContext {
        let (workspace, _) = super::super::workspace_path(path);
        let names = self.effective_manifest_names(workspace);

        let mut context = super::super::LanguageContext::default();
        for name in names {
            let Some(manifest) = self.effective_framework_manifest(path, name) else {
                continue;
            };
            push_route_origins(
                &mut context.route_rule_origins,
                true,
                context.route_decorators.len(),
                &manifest.name,
                manifest.route_decorators.iter().map(|rule| rule.identity()),
            );
            push_route_origins(
                &mut context.route_rule_origins,
                false,
                context.route_calls.len(),
                &manifest.name,
                manifest.route_calls.iter().map(|rule| rule.identity()),
            );
            context
                .class_hierarchy_receivers
                .extend(manifest.class_hierarchy_receivers.iter().cloned());
            context
                .route_decorators
                .extend(manifest.route_decorators.iter().cloned());
            context
                .route_calls
                .extend(manifest.route_calls.iter().cloned().map(|mut rule| {
                    rule.registration_guard = rule.registration_guard.map(|id| {
                        serde_json::to_string(&(&manifest.name, id))
                            .expect("rule identity strings serialize")
                    });
                    rule
                }));
            for (index, rule) in manifest.di_calls.iter().enumerate() {
                context.di_rule_origins.insert(
                    context.di_calls.len() + index,
                    crate::semantic::ExtractionRuleOrigin {
                        pack: manifest.name.clone(),
                        rule: serde_json::to_string(&rule.identity()).expect("typed DI identity"),
                    },
                );
            }
            context.di_calls.extend(manifest.di_calls.iter().cloned());

            for rule in &manifest.syntax_rules {
                let rule_family = rule.activation_family();
                let outcome = ActivationCondition::matches_workspace_slice(&rule.activation, |pkg| {
                    rule_family.is_some_and(|family| self.declares_for_path(family, path, pkg))
                });
                if outcome == ActivationMatch::Inactive {
                    continue;
                }
                let mut cloned = rule.clone();
                cloned.pack = manifest.name.clone();
                if outcome == ActivationMatch::Active {
                    context
                        .workspace_activated_rules
                        .entry(cloned.pack.clone())
                        .or_default()
                        .insert(cloned.id.clone());
                }
                context.syntax_rules.push(cloned);
            }

            for rule in &manifest.semantic_rules {
                let family = super::super::by_id(rule.language()).map(|profile| profile.family());
                let scope = family
                    .and_then(|family| self.nearest_manifest_scope_for_path(family, path))
                    .unwrap_or(workspace);
                let declared_packages = family.and_then(|family| {
                    self.scoped_packages
                        .get(&family)
                        .and_then(|scopes| scopes.get(scope))
                });
                let active = matches!(
                    ActivationCondition::matches_workspace_slice(&rule.activation, |name| {
                        family.is_some_and(|family| {
                            declared_packages.is_some_and(|packages| {
                                packages.contains(&package_identity(family, name))
                            })
                        })
                    }),
                    ActivationMatch::Active
                );
                context
                    .semantic_rules
                    .push(super::super::semantic::EffectiveSemanticRule {
                        pack: manifest.name.clone(),
                        rule: rule.clone(),
                        scope: scope.to_owned(),
                        active,
                    });
            }
        }
        context.prepare_named_member_rules();
        if let Some(rule_ir) = self.rule_ir.get(workspace) {
            context.rule_ir = rule_ir.clone();
        }
        context
    }

    pub(super) fn prepare_language_contexts(&mut self) {
        let scopes: BTreeSet<String> = std::iter::once(String::new())
            .chain(self.workspaces.iter().cloned())
            .chain(
                self.framework_manifests
                    .keys()
                    .map(|(scope, _)| scope.clone()),
            )
            .chain(
                self.scoped_packages
                    .values()
                    .flat_map(|scopes| scopes.keys().cloned()),
            )
            .collect();
        self.language_contexts.clear();
        self.rule_ir.clear();
        for scope in scopes {
            let path = if scope.is_empty() {
                "__context_probe__".to_owned()
            } else {
                format!("{scope}/__context_probe__")
            };
            let key = self.language_context_key(&path);
            if self.language_contexts.contains_key(&key) {
                continue;
            }
            let mut context = self.build_language_context(&path);
            let rule_ir = self
                .rule_ir
                .entry(key.0.clone())
                .or_insert_with(|| RuleIRHandle::prepare(&context));
            context.rule_ir = rule_ir.clone();
            self.language_contexts.insert(key, Arc::new(context));
        }
    }

    /// Reports removed eligible bundled identities for the complete current file inventory.
    pub fn framework_rule_diagnostics<'a>(
        &self,
        inventory: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<RuleCoverageDiagnostic> {
        let mut removed_by_group =
            BTreeMap::<(String, String, String, RuleDomain), BTreeSet<RuleIdentity>>::new();
        for (path, language) in inventory {
            let Some(profile) = super::super::by_id(language) else {
                continue;
            };
            if !profile.has_grammar() {
                continue;
            }
            let grammar = profile.grammar(path);
            let (workspace, _) = super::super::workspace_path(path);
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
                    let removed = baseline.difference(&current).filter(|identity| {
                        if let RuleIdentity::Semantic { id } = identity {
                            bundled.semantic_rules.iter().any(|rule| {
                                &rule.id == id
                                    && rule.language() == language
                                    && ActivationCondition::matches_workspace_slice(
                                        &rule.activation,
                                        |name| {
                                            super::super::by_id(rule.language()).is_some_and(
                                                |profile| {
                                                    self.declares_for_path(
                                                        profile.family(),
                                                        path,
                                                        name,
                                                    )
                                                },
                                            )
                                        },
                                    ) != ActivationMatch::Inactive
                            })
                        } else {
                            true
                        }
                    });
                    for identity in removed {
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

    /// Selects one surviving finite hierarchy identity, optionally within its originating pack.
    pub(crate) fn class_hierarchy_authority_for_path(
        &self,
        path: &str,
        base: &str,
        pack: Option<&str>,
    ) -> Option<(&str, &super::super::manifests::ClassHierarchyReceiverRule)> {
        let mut candidates = self.hierarchy_rows_for_path(path).filter(|(name, rule)| {
            pack.is_none_or(|pack| pack == *name)
                && rule.bases.iter().any(|candidate| candidate == base)
        });
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    }

    /// Distinct explicit return targets retain their originating effective pack identity.
    pub(crate) fn hierarchy_return_targets_for_path(&self, path: &str) -> BTreeSet<(&str, &str)> {
        self.hierarchy_rows_for_path(path)
            .flat_map(|(pack, row)| {
                row.returns.values().filter_map(move |policy| match policy {
                    super::super::manifests::ReturnPolicy::Receiver { base } => {
                        Some((pack, base.as_str()))
                    }
                    super::super::manifests::ReturnPolicy::SelfReceiver => None,
                })
            })
            .collect()
    }

    fn hierarchy_rows_for_path<'s, 'p>(
        &'s self,
        path: &'p str,
    ) -> impl Iterator<
        Item = (
            &'s str,
            &'s super::super::manifests::ClassHierarchyReceiverRule,
        ),
    > + 'p
    where
        's: 'p,
    {
        let (workspace, _) = super::super::workspace_path(path);
        let names = self.effective_manifest_names(workspace);
        names
            .into_iter()
            .filter_map(move |name| {
                self.effective_framework_manifest(path, name)
                    .map(|manifest| (name, manifest))
            })
            .flat_map(|(name, manifest)| {
                manifest
                    .class_hierarchy_receivers
                    .iter()
                    .map(move |rule| (name, rule))
            })
    }

    pub(super) fn validate_type_flow_policies(&self) -> Result<()> {
        let scopes: BTreeSet<&str> = std::iter::once("")
            .chain(
                self.framework_manifests
                    .keys()
                    .map(|(scope, _)| scope.as_str()),
            )
            .chain(
                self.scoped_packages
                    .values()
                    .flat_map(|scopes| scopes.keys().map(String::as_str)),
            )
            .collect();
        for scope in scopes {
            let path = if scope.is_empty() {
                "__type_flow_probe__".to_owned()
            } else {
                format!("{scope}/__type_flow_probe__")
            };
            let context = self.language_context_for_path(&path);
            anyhow::ensure!(
                context
                    .class_hierarchy_receivers
                    .iter()
                    .filter(|rule| !rule.returns.is_empty())
                    .count()
                    <= 128,
                "effective return-policy row bound exceeded"
            );
            for row in &context.class_hierarchy_receivers {
                row.validate_returns(4096)?;
            }
            let mut receiver_type_limits = BTreeMap::<&str, (usize, usize)>::new();
            for rule in &context.syntax_rules {
                let super::super::manifests::syntax::SyntaxOperation::ReceiverType { receiver } =
                    &rule.operation
                else {
                    continue;
                };
                let (clauses, literals) = receiver_type_limits
                    .entry(rule.language.as_str())
                    .or_default();
                *clauses += receiver.argument_factories.len();
                *literals += receiver
                    .argument_factories
                    .iter()
                    .map(|factory| factory.literals.len())
                    .sum::<usize>();
            }
            for (clauses, literals) in receiver_type_limits.values() {
                anyhow::ensure!(
                    *clauses <= 128 && *literals <= 4096,
                    "effective literal-factory bounds exceeded"
                );
            }
        }
        Ok(())
    }

    pub(super) fn prepare_scoped_policies(&mut self) -> Result<()> {
        use super::super::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        for manifest in self
            .framework_manifests
            .values()
            .chain(self.standard_framework_manifests.values())
        {
            for rule in &manifest.syntax_rules {
                match &rule.operation {
                    SyntaxOperation::NamedBinding {
                        binding:
                            NamedBindingForm::CallReference {
                                provider_scopes, ..
                            },
                        ..
                    } => {
                        for provider in provider_scopes {
                            let workspace = if provider.workspace.is_empty() {
                                String::new()
                            } else {
                                format!("[{}]", provider.workspace)
                            };
                            anyhow::ensure!(
                                self.workspaces.contains(&workspace),
                                "unknown or disabled named provider workspace: {}",
                                provider.workspace
                            );
                            let scope = match (workspace.is_empty(), provider.project.is_empty()) {
                                (true, _) => provider.project.clone(),
                                (false, true) => workspace,
                                (false, false) => format!("{workspace}/{}", provider.project),
                            };
                            if self
                                .own_packages
                                .get(&scope)
                                .is_some_and(|packages| packages.contains(&provider.package))
                            {
                                self.named_provider_scopes.insert(provider.clone(), scope);
                            }
                        }
                    }
                    SyntaxOperation::HttpClientCall {
                        target_context: Some(context),
                        ..
                    } => {
                        for path in context.request_owners.iter().chain(&context.route_owners) {
                            anyhow::ensure!(
                                self.workspaces
                                    .contains(super::super::workspace_path(path).0),
                                "unknown or disabled HTTP context workspace: {path}"
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Resolves only explicit, already collected project/package identities.
    pub(crate) fn named_provider_scope(
        &self,
        provider: &super::super::manifests::projections::NamedProviderScope,
    ) -> Option<&str> {
        self.named_provider_scopes.get(provider).map(String::as_str)
    }

    /// Existing scoped policy relation used by both linking and selective hydration.
    pub fn named_scope_admits(&self, consumer: &str, provider: &str, domain: &str) -> bool {
        use super::super::manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        let provider_scope = crate::linker::named_binding_scope(provider, self);
        if crate::linker::named_binding_scope(consumer, self) == provider_scope {
            return true;
        }
        self.language_context_for_path(consumer)
            .syntax_rules
            .iter()
            .any(|rule| {
                matches!(
                    &rule.operation,
                    SyntaxOperation::NamedBinding {
                        domain: configured,
                        binding: NamedBindingForm::CallReference { provider_scopes, .. },
                    } if configured == domain
                        && provider_scopes.iter().any(|scope| {
                            self.named_provider_scope(scope) == Some(provider_scope.as_str())
                        })
                )
            })
    }

    /// Exposes the deterministic digest of bundled standard framework manifests.
    pub fn standard_manifests_digest(&self) -> &str {
        Self::standard_manifests_digest_static()
    }

    /// Exposes the static deterministic digest of bundled standard framework manifests.
    pub fn standard_manifests_digest_static() -> &'static str {
        STANDARD_FRAMEWORK_MANIFESTS_DIGEST.get_or_init(|| {
            let mut hasher = Sha256::new();
            hasher.update(b"acdd-standard-framework-manifests-v1\0");
            for (name, extension, source) in STANDARD_FRAMEWORK_MANIFESTS {
                hasher.update((name.len() as u64).to_be_bytes());
                hasher.update(name.as_bytes());
                hasher.update((extension.len() as u64).to_be_bytes());
                hasher.update(extension.as_bytes());
                hasher.update((source.len() as u64).to_be_bytes());
                hasher.update(source.as_bytes());
            }
            hex::encode(hasher.finalize())
        })
    }

    /// Returns a workspace-local manifest unconditionally, otherwise a bundled manifest.
    pub(crate) fn effective_framework_manifest(
        &self,
        path: &str,
        pack: &str,
    ) -> Option<&FrameworkManifest> {
        let workspace = super::super::workspace_path(path).0;
        self.framework_manifests
            .get(&(workspace.to_owned(), pack.to_owned()))
            .or_else(|| self.standard_framework_manifests.get(pack))
            .filter(|manifest| manifest.enabled)
    }

    /// Returns a workspace-local manifest unconditionally, otherwise a bundled
    /// manifest only when the workspace declares its package.
    pub fn framework_manifest_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
        framework: &str,
        package: &str,
    ) -> Option<&FrameworkManifest> {
        let (workspace, _) = super::super::workspace_path(path);
        if self
            .framework_manifests
            .contains_key(&(workspace.to_owned(), framework.to_owned()))
            || self.declares_for_path(family, path, package)
        {
            self.effective_framework_manifest(path, framework)
        } else {
            None
        }
    }
}
