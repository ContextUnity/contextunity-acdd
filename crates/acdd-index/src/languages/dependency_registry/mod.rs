use super::manifests::{FrameworkManifest, RuleIRHandle};
use super::registries::{
    javascript_packages::JavascriptPackageRegistry, typescript_paths::TypeScriptPathsRegistry,
};
use super::{profiles, ImportPath, LanguageFamily};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

mod framework_packs;
/// Trait-isolated ecosystem manifest parser interface and registry.
pub mod parser;
/// Built-in ecosystem manifest parser implementations.
pub mod parsers;
#[cfg(test)]
mod tests;

pub use parser::{Ecosystem, ManifestParser, ModulePrefix, ParsedManifest, MANIFEST_PARSERS};

pub(crate) type ManifestSnapshots = BTreeMap<String, sha2::digest::Output<Sha256>>;

/// A manifest's UTF-8 contents at a canonical workspace-relative path.
#[derive(Clone, Copy, Debug)]
pub struct ManifestSource<'a> {
    /// A dependency filename or `.acdd/frameworks/<identity>.{toml,yaml,yml}`.
    pub path: &'a str,
    /// The manifest contents, bounded to 4 MiB.
    pub source: &'a str,
}

/// An explicit manifest inventory isolated to one workspace.
pub struct WorkspaceManifestSources<'a> {
    /// Empty for the primary workspace; otherwise an opaque linked-workspace name.
    pub name: &'a str,
    /// Each canonical path occurs once within this workspace.
    pub manifests: &'a [ManifestSource<'a>],
}

pub(crate) type LanguageContextKey = (String, Vec<(String, String)>);

#[derive(Clone, Debug)]
pub(crate) struct ManifestEntrypoint {
    pub origin: String,
    pub scope: String,
    pub name: String,
    pub target: String,
    pub python: bool,
}

#[derive(Default)]
/// Workspace dependency manifests, module prefixes, and compiled framework rule contexts.
pub struct DependencyRegistry {
    packages: HashMap<LanguageFamily, HashSet<String>>,
    pub(crate) scoped_packages: HashMap<LanguageFamily, HashMap<String, HashSet<String>>>,
    scoped_origins: HashMap<LanguageFamily, HashMap<String, HashMap<String, String>>>,
    module_prefixes: HashMap<LanguageFamily, HashMap<String, Vec<ModulePrefix>>>,
    pub(crate) own_packages: HashMap<String, HashSet<String>>,
    pub(crate) workspaces: HashSet<String>,
    pub(crate) named_provider_scopes:
        HashMap<super::manifests::projections::NamedProviderScope, String>,
    rust_libraries: HashMap<String, String>,
    rust_package_libraries: HashMap<String, String>,
    pub(crate) framework_manifests: HashMap<(String, String), FrameworkManifest>,
    pub(crate) standard_framework_manifests: HashMap<String, FrameworkManifest>,
    pub(crate) language_contexts: HashMap<LanguageContextKey, Arc<super::LanguageContext>>,
    pub(crate) rule_ir: HashMap<String, RuleIRHandle>,
    javascript_packages: JavascriptPackageRegistry,
    typescript_paths: TypeScriptPathsRegistry,
    #[cfg(feature = "lang-rust")]
    rust_root_crates: HashMap<String, String>,
    pub(crate) digest: String,
    manifest_entrypoints: Vec<ManifestEntrypoint>,
}

impl DependencyRegistry {
    /// Collects validated rules and dependency scopes without accessing the filesystem.
    ///
    /// # Errors
    ///
    /// Rejects duplicate workspaces, invalid or duplicate paths, unsupported filenames,
    /// oversized inputs, and invalid framework rules or cross-workspace policies.
    pub fn try_collect_from_sources<'a>(
        workspaces: impl IntoIterator<Item = WorkspaceManifestSources<'a>>,
    ) -> Result<Self> {
        let mut registry = Self::default();
        framework_packs::load_standard_framework_manifests(&mut registry)?;
        let mut snapshots = ManifestSnapshots::new();
        for workspace in workspaces {
            let label = if workspace.name.is_empty() {
                String::new()
            } else {
                format!("[{}]", workspace.name)
            };
            anyhow::ensure!(
                registry.workspaces.insert(label.clone()),
                "duplicate manifest workspace: {label}"
            );
            let prefix = if label.is_empty() {
                String::new()
            } else {
                format!("{label}/")
            };
            let mut paths = HashSet::new();
            for manifest in workspace.manifests {
                anyhow::ensure!(
                    super::manifests::semantic::normal_relative_path(manifest.path)
                        && super::manifests::projections::confined_path(manifest.path, false)
                        && !manifest.path.starts_with('['),
                    "invalid manifest source path: {}",
                    manifest.path
                );
                anyhow::ensure!(
                    paths.insert(manifest.path),
                    "duplicate manifest source path: {}",
                    manifest.path
                );
                anyhow::ensure!(
                    manifest.source.len() <= 4 * 1024 * 1024,
                    "manifest source exceeds the 4 MiB limit: {}",
                    manifest.path
                );
                let path = Path::new(manifest.path);
                let is_framework = path.parent() == Some(Path::new(".acdd/frameworks"))
                    && matches!(
                        path.extension().and_then(|value| value.to_str()),
                        Some("toml" | "yaml" | "yml")
                    );
                if is_framework {
                    framework_packs::record_framework_manifest(
                        &label,
                        &prefix,
                        path,
                        manifest.source,
                        &mut registry,
                        &mut snapshots,
                    )
                    .with_context(|| {
                        format!("failed to load framework manifest {}", manifest.path)
                    })?;
                } else {
                    anyhow::ensure!(
                        is_dependency_manifest_filename(
                            path.file_name().and_then(|value| value.to_str()).unwrap()
                        ),
                        "unsupported manifest source path: {}",
                        manifest.path
                    );
                    record_dependency_manifest(
                        &prefix,
                        path,
                        manifest.source,
                        &mut registry,
                        &mut snapshots,
                    );
                }
            }
        }
        finish_registry(registry, snapshots)
    }

    pub(crate) fn manifest_entrypoints(&self) -> &[ManifestEntrypoint] {
        &self.manifest_entrypoints
    }

    /// Resolves local package exports and TypeScript path aliases from the dependency snapshot.
    pub(crate) fn normalize_javascript_import(
        &self,
        profile_id: &str,
        owner: &str,
        module: &str,
        indexed_modules: &HashSet<&str>,
    ) -> Option<ImportPath> {
        if !matches!(profile_id, "javascript" | "typescript")
            || module.starts_with('.')
            || module.starts_with('/')
        {
            return None;
        }
        if profile_id == "typescript" {
            if let Some(path) = self
                .typescript_paths
                .resolve_import(owner, module, indexed_modules)
            {
                return Some(path);
            }
        }
        self.javascript_packages.normalize_import(owner, module)
    }

    #[cfg(feature = "lang-rust")]
    pub(crate) fn root_rust_crate_for_path(&self, path: &str) -> Option<&str> {
        if !self.is_rust_sibling_target_path(path) {
            return None;
        }
        let workspace = super::workspace_path(path).0;
        let name = self.rust_root_crates.get(workspace)?;
        if self.nearest_manifest_scope_for_path(LanguageFamily("rust"), path)? != workspace {
            return None;
        }
        Some(name)
    }

    /// Returns candidate workspace-relative entrypoint paths discovered from manifests.
    pub fn workspace_entrypoint_candidates(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        let join_scope = |scope: &str, rel: &str| {
            let rel = rel.trim_start_matches("./");
            if scope.is_empty() {
                rel.to_owned()
            } else {
                format!("{scope}/{rel}")
            }
        };

        for entry in &self.manifest_entrypoints {
            if entry.scope.starts_with('[') {
                continue;
            }
            if entry.python {
                let mod_path = entry
                    .target
                    .split(':')
                    .next()
                    .unwrap_or(&entry.target)
                    .trim()
                    .replace('.', "/");
                if !mod_path.is_empty() {
                    out.insert(join_scope(&entry.scope, &format!("{mod_path}.py")));
                    out.insert(join_scope(&entry.scope, &format!("{mod_path}/__init__.py")));
                }
            } else if !entry.target.trim().is_empty() {
                let target = entry.target.trim().trim_start_matches("./");
                out.insert(join_scope(&entry.scope, target));
                for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
                    out.insert(join_scope(&entry.scope, &format!("{target}.{ext}")));
                    out.insert(join_scope(&entry.scope, &format!("{target}/index.{ext}")));
                }
            }
        }

        if let Some(scopes) = self.scoped_packages.get(&LanguageFamily("rust")) {
            for scope in scopes.keys().filter(|s| !s.starts_with('[')) {
                out.insert(join_scope(scope, "src/lib.rs"));
                out.insert(join_scope(scope, "src/main.rs"));
            }
        }
        if let Some(scopes) = self.scoped_packages.get(&LanguageFamily("javascript")) {
            for scope in scopes.keys().filter(|s| !s.starts_with('[')) {
                for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
                    out.insert(join_scope(scope, &format!("index.{ext}")));
                    out.insert(join_scope(scope, &format!("src/index.{ext}")));
                }
            }
        }
        if let Some(scopes) = self.scoped_packages.get(&LanguageFamily("python")) {
            for scope in scopes.keys().filter(|s| !s.starts_with('[')) {
                out.insert(join_scope(scope, "__init__.py"));
                out.insert(join_scope(scope, "src/__init__.py"));
                if let Some(names) = self.own_packages.get(scope) {
                    for name in names {
                        let pkg = name.replace('-', "_");
                        out.insert(join_scope(scope, &format!("{pkg}/__init__.py")));
                        out.insert(join_scope(scope, &format!("src/{pkg}/__init__.py")));
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// Resolves bounded worktree-relative file candidates referenced by an import expression.
    pub fn import_file_candidates(
        &self,
        root: Option<&Path>,
        owner_path: &str,
        module: &str,
    ) -> Vec<String> {
        let Some(profile) = super::for_path(Path::new(owner_path)) else {
            return Vec::new();
        };
        let mut out = BTreeSet::new();
        let owner_dir = Path::new(owner_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));

        let push_namespace = |ns: &str, out: &mut BTreeSet<String>| {
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
        };

        if matches!(profile.family().0, "javascript") {
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
            } else {
                if profile.id() == "typescript" {
                    for cand in self.typescript_paths.candidate_paths(owner_path, module) {
                        out.insert(cand.clone());
                        for ext in profile.extensions() {
                            out.insert(format!("{cand}.{ext}"));
                            out.insert(format!("{cand}/index.{ext}"));
                        }
                    }
                }
                if let Some(pkg) = self.javascript_packages.normalize_import(owner_path, module) {
                    push_namespace(&pkg.namespace, &mut out);
                }
            }
        }

        #[cfg(feature = "lang-rust")]
        if profile.id() == "rust" {
            if let Some(root_crate) = self.root_rust_crate_for_path(owner_path) {
                if let Some(norm) = super::rust::RUST.normalize_root_crate_import(root_crate, module) {
                    push_namespace(&norm.namespace, &mut out);
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
                push_namespace(&ns, &mut out);
            };
            if let Some((pkg_scope, own_lib)) =
                self.sibling_rust_package_library_for_path(owner_path)
            {
                if own_lib == first {
                    push_rust_scope(pkg_scope, true);
                }
            }
            for (target_workspace, lib_name) in &self.rust_libraries {
                if lib_name == first
                    && (self.sibling_rust_library_for_path(owner_path) == Some(lib_name.as_str())
                        || self.declares_for_path(LanguageFamily("rust"), owner_path, first))
                {
                    push_rust_scope(target_workspace, false);
                }
            }
        }

        if let Some(norm) = profile.normalize_import_with_root(root, owner_path, module) {
            push_namespace(&norm.namespace, &mut out);
        }
        out.into_iter().collect()
    }

    /// Collects workspace dependencies and framework rules.
    pub fn collect(root: Option<&Path>) -> Self {
        let Some(root) = root else {
            return Self::bundled_framework_rules();
        };
        let adapter = crate::scanner::load_adapter(root, None).ok();
        Self::collect_with_scan_config(root, adapter.as_ref())
    }

    /// Collects workspace dependencies and framework rules with scan configuration.
    pub fn collect_with_scan_config(
        root: &Path,
        adapter: Option<&crate::scanner::Adapter>,
    ) -> Self {
        Self::try_collect_with_scan_config(root, adapter).unwrap_or_default()
    }

    /// Collects dependency metadata and framework manifests, returning explicit parse errors.
    ///
    /// # Errors
    ///
    /// Returns an error when an explicit `.acdd/frameworks/*.{toml,yaml,yml}`
    /// file cannot be read or does not satisfy the typed framework-manifest schema.
    pub fn try_collect_with_scan_config(
        root: &Path,
        adapter: Option<&crate::scanner::Adapter>,
    ) -> Result<Self> {
        if let Some(adapter) = adapter {
            let manifests = crate::scanner::dependency_manifests_for_build(root, adapter)?;
            Self::try_collect(root, Some(adapter), Some(&manifests))
        } else {
            Self::try_collect(root, None, None)
        }
    }

    /// Collects dependency metadata from scanner-discovered manifests and direct workspace roots.
    pub fn try_collect_with_paths(
        root: &Path,
        adapter: Option<&crate::scanner::Adapter>,
        discovered_manifests: &[PathBuf],
    ) -> Result<Self> {
        Self::try_collect(root, adapter, Some(discovered_manifests))
    }

    fn try_collect(
        root: &Path,
        adapter: Option<&crate::scanner::Adapter>,
        discovered_manifests: Option<&[PathBuf]>,
    ) -> Result<Self> {
        let mut registry = Self::default();
        framework_packs::load_standard_framework_manifests(&mut registry)?;
        let mut roots = vec![(
            root.to_path_buf(),
            String::new(),
            adapter
                .map(|adapter| adapter.ignored_names.clone())
                .unwrap_or_default(),
        )];
        if let Some(adapter) = adapter {
            roots.extend(adapter.linked_workspaces.iter().map(|workspace| {
                (
                    workspace.path.clone(),
                    format!("[{}]/", workspace.name),
                    workspace.ignored_names.clone(),
                )
            }));
        }
        let mut visited = HashSet::new();
        let mut snapshots = ManifestSnapshots::new();
        for (root, prefix, ignored_names) in roots {
            registry
                .workspaces
                .insert(prefix.trim_end_matches('/').to_owned());
            let manifest_paths = if let Some(discovered) = discovered_manifests {
                scanner_manifest_paths(&root, &ignored_names, discovered)?
            } else {
                walked_manifest_paths(&root, ignored_names)
            };
            for path in manifest_paths {
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        metadata
                    }
                    _ => continue,
                };
                let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !is_dependency_manifest_filename(filename) || !visited.insert(path.clone()) {
                    continue;
                }
                if metadata.len() > 4 * 1024 * 1024 {
                    continue;
                }
                let Some(relative_path) = path.strip_prefix(&root).ok() else {
                    continue;
                };
                let Ok(content) = read_confined_manifest_string(&root, relative_path) else {
                    continue;
                };
                record_dependency_manifest(
                    &prefix,
                    relative_path,
                    &content,
                    &mut registry,
                    &mut snapshots,
                );
            }
            framework_packs::load_framework_manifests(
                &root,
                prefix.trim_end_matches('/'),
                &prefix,
                &mut registry,
                &mut snapshots,
            )?;
        }
        finish_registry(registry, snapshots)
    }

    /// Returns the deterministic SHA-256 digest of collected dependency and framework manifests.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the root Cargo library name normalized for Rust import syntax.
    pub(crate) fn rust_library_for_path(&self, path: &str) -> Option<&str> {
        let workspace = super::workspace_path(path).0;
        if self.nearest_manifest_scope_for_path(LanguageFamily("rust"), path) != Some(workspace) {
            return None;
        }
        self.rust_libraries.get(workspace).map(String::as_str)
    }

    /// Returns the Cargo package scope directory and normalized crate name for `path`.
    pub fn rust_package_for_path(&self, path: &str) -> Option<(&str, &str)> {
        let scope = self.nearest_manifest_scope_for_path(LanguageFamily("rust"), path)?;
        let name = self
            .rust_package_libraries
            .get(scope)
            .or_else(|| self.rust_libraries.get(scope))
            .map(String::as_str);
        #[cfg(feature = "lang-rust")]
        let name = name.or_else(|| self.rust_root_crates.get(scope).map(String::as_str));
        Some((scope, name?))
    }

    /// Returns whether `path` is a sibling non-library Cargo target (`tests/`, `examples/`,
    /// `benches/`, `src/main.rs`, `src/bin/`) that implicitly imports its own package's library crate.
    pub fn is_rust_sibling_target_path(&self, path: &str) -> bool {
        let Some(scope) = self.nearest_manifest_scope_for_path(LanguageFamily("rust"), path)
        else {
            return false;
        };
        let rel = if scope.is_empty() {
            path
        } else if let Some(rest) = path
            .strip_prefix(scope)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            rest
        } else {
            super::workspace_path(path).1
        };
        rel == "src/main.rs"
            || rel.starts_with("src/bin/")
            || rel.starts_with("tests/")
            || rel.starts_with("examples/")
            || rel.starts_with("benches/")
    }

    /// Returns `(pkg_scope, lib_name)` ONLY when `path` is a sibling Cargo target.
    pub(crate) fn sibling_rust_package_library_for_path(&self, path: &str) -> Option<(&str, &str)> {
        if !self.is_rust_sibling_target_path(path) {
            return None;
        }
        let scope = self.nearest_manifest_scope_for_path(LanguageFamily("rust"), path)?;
        let lib = self.rust_package_libraries.get(scope).map(String::as_str)?;
        Some((scope, lib))
    }

    /// Returns the root package library name ONLY when `path` is a sibling Cargo target.
    pub(crate) fn sibling_rust_library_for_path(&self, path: &str) -> Option<&str> {
        if !self.is_rust_sibling_target_path(path) {
            return None;
        }
        self.rust_library_for_path(path)
    }

    /// Returns ordered workspace-relative [`ModulePrefix`] entries applicable to `path` in `family`.
    pub fn module_prefixes_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
    ) -> Vec<&ModulePrefix> {
        let Some(scopes) = self.module_prefixes.get(&family) else {
            return Vec::new();
        };
        let workspace = super::workspace_path(path).0;
        let mut directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        let mut collected = Vec::new();
        loop {
            if let Some(entries) = scopes.get(directory) {
                let mut scoped: Vec<&ModulePrefix> = entries.iter().collect();
                scoped.sort_by(|left, right| {
                    let left_matches = is_directory_ancestor(&left.source_root, path);
                    let right_matches = is_directory_ancestor(&right.source_root, path);
                    right_matches
                        .cmp(&left_matches)
                        .then_with(|| right.source_root.len().cmp(&left.source_root.len()))
                        .then_with(|| right.prefix.len().cmp(&left.prefix.len()))
                        .then_with(|| left.prefix.cmp(&right.prefix))
                });
                collected.extend(scoped);
            }
            if directory.is_empty() || (!workspace.is_empty() && directory == workspace) {
                break;
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        collected
    }

    /// Returns the highest-priority workspace-relative [`ModulePrefix`] for `path` in `family`.
    pub fn module_prefix_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
    ) -> Option<&ModulePrefix> {
        self.module_prefixes_for_path(family, path)
            .into_iter()
            .next()
    }

    pub(crate) fn declares_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
        package: &str,
    ) -> bool {
        let Some(scope) = self.nearest_manifest_scope_for_path(family, path) else {
            return false;
        };
        self.scoped_packages
            .get(&family)
            .and_then(|scopes| scopes.get(scope))
            .is_some_and(|packages| packages.contains(&package_identity(family, package)))
    }

    /// Resolves nearest manifest package scope for a given file path.
    pub fn nearest_manifest_scope_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
    ) -> Option<&str> {
        let scopes = self.scoped_packages.get(&family)?;
        let workspace = super::workspace_path(path).0;
        let mut directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        loop {
            if let Some((scope, _)) = scopes.get_key_value(directory) {
                return Some(scope.as_str());
            }
            if directory.is_empty() || (!workspace.is_empty() && directory == workspace) {
                return None;
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }

    /// Classifies an external module without workspace-origin context.
    pub fn classification(
        &self,
        profile: &dyn super::LanguageProfile,
        module: &str,
    ) -> Option<&'static str> {
        if module.is_empty() {
            return None;
        }
        if profile.is_stdlib(module) {
            return Some("standard library");
        }
        let declared = self
            .packages
            .get(&profile.family())
            .is_some_and(|packages| {
                if packages.contains(module) {
                    return true;
                }
                module.char_indices().any(|(index, character)| {
                    matches!(character, '.' | '/' | ':') && packages.contains(&module[..index])
                })
            });
        Some(if declared {
            "external dependency (manifest)"
        } else {
            "external dependency"
        })
    }

    /// Classifies an import using the nearest declaring manifest in its workspace.
    pub fn classification_for_path(
        &self,
        profile: &dyn super::LanguageProfile,
        module: &str,
        path: &str,
    ) -> Option<String> {
        let classification = self.classification(profile, module)?;
        if classification != "external dependency (manifest)" {
            return Some(classification.to_owned());
        }
        let workspace = super::workspace_path(path).0;
        let mut directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        loop {
            if let Some(origins) = self
                .scoped_origins
                .get(&profile.family())
                .and_then(|scopes| scopes.get(directory))
            {
                let origin = origins.get(module).or_else(|| {
                    module.char_indices().rev().find_map(|(index, character)| {
                        matches!(character, '.' | '/' | ':')
                            .then(|| origins.get(&module[..index]))
                            .flatten()
                    })
                });
                if let Some(origin) = origin {
                    return Some(format!("external dependency (manifest: {origin})"));
                }
            }
            if directory.is_empty() || (!workspace.is_empty() && directory == workspace) {
                return Some("external dependency".to_owned());
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
}

fn is_directory_ancestor(dir: &str, path: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn join_workspace_root(scope: &str, relative: &str) -> String {
    match (scope.is_empty(), relative.is_empty()) {
        (true, _) => relative.to_owned(),
        (false, true) => scope.to_owned(),
        (false, false) => format!("{scope}/{relative}"),
    }
}

fn record_family_dependencies(
    registry: &mut DependencyRegistry,
    family: LanguageFamily,
    scope: &str,
    origin: &str,
    dependencies: &[String],
) {
    let origins = registry
        .scoped_origins
        .entry(family)
        .or_default()
        .entry(scope.to_owned())
        .or_default();
    for dependency in dependencies {
        let recorded = origins.entry(dependency.clone()).or_default();
        if recorded.is_empty() || preferred_manifest(origin, recorded) {
            *recorded = origin.to_owned();
        }
    }
    registry
        .scoped_packages
        .entry(family)
        .or_default()
        .entry(scope.to_owned())
        .or_default()
        .extend(dependencies.iter().cloned());
    registry
        .packages
        .entry(family)
        .or_default()
        .extend(dependencies.iter().cloned());
}

fn record_dependency_manifest(
    prefix: &str,
    relative_path: &Path,
    content: &str,
    registry: &mut DependencyRegistry,
    snapshots: &mut ManifestSnapshots,
) {
    let filename = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let directory = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let origin = format!(
        "{}{}",
        prefix,
        relative_path.to_string_lossy().replace('\\', "/")
    );
    let mut scope = prefix.to_owned();
    scope.push_str(&directory.to_string_lossy().replace('\\', "/"));
    let scope = scope.trim_end_matches('/').to_owned();

    if filename == "package.json" {
        registry
            .javascript_packages
            .record_manifest(&scope, content);
    } else if filename == "pnpm-workspace.yaml" {
        registry
            .javascript_packages
            .record_pnpm_workspace(&scope, content);
    } else if filename.starts_with("tsconfig") && filename.ends_with(".json") {
        registry
            .typescript_paths
            .record_config(&scope, filename, content);
    }

    snapshots.insert(origin.clone(), Sha256::digest(content.as_bytes()));

    let mut parsed_families = Vec::new();
    let mut skip_profile_fallback = false;
    for parser in MANIFEST_PARSERS {
        if !parser.matches_filename(filename) {
            continue;
        }
        let parsed = parser.parse(filename, content);
        if !parsed.own_packages.is_empty() {
            registry
                .own_packages
                .entry(scope.clone())
                .or_default()
                .extend(parsed.own_packages);
        }
        for entry in parsed.entrypoints {
            registry.manifest_entrypoints.push(ManifestEntrypoint {
                origin: origin.clone(),
                scope: scope.clone(),
                name: entry.name,
                target: entry.target,
                python: entry.python,
            });
        }
        if let Some(rust_info) = parsed.rust_info {
            if let Some(lib_name) = rust_info.package_library {
                registry
                    .rust_package_libraries
                    .insert(scope.clone(), lib_name.clone());
                if scope == prefix.trim_end_matches('/') {
                    registry.rust_libraries.insert(scope.clone(), lib_name);
                }
            }
            #[cfg(feature = "lang-rust")]
            if directory.as_os_str().is_empty() {
                if let Some(crate_name) = rust_info.root_crate {
                    registry.rust_root_crates.insert(scope.clone(), crate_name);
                }
            }
        }
        if !parsed.module_prefixes.is_empty() {
            for mut prefix_entry in parsed.module_prefixes {
                prefix_entry.source_root =
                    join_workspace_root(&scope, &prefix_entry.source_root);
                prefix_entry.origin = origin.clone();
                for &family in parsed.ecosystem.families() {
                    registry
                        .module_prefixes
                        .entry(family)
                        .or_default()
                        .entry(scope.clone())
                        .or_default()
                        .push(prefix_entry.clone());
                }
            }
        }
        if parsed.skip_dependency_recording {
            skip_profile_fallback = true;
            continue;
        }
        for &family in parsed.ecosystem.families() {
            if parsed_families.contains(&family) {
                continue;
            }
            parsed_families.push(family);
            record_family_dependencies(
                registry,
                family,
                &scope,
                &origin,
                &parsed.dependencies,
            );
        }
    }

    if skip_profile_fallback {
        return;
    }

    for profile in profiles().filter(|profile| {
        profile
            .manifest_filenames()
            .iter()
            .any(|pattern| parser::filename_matches(pattern, filename))
    }) {
        let family = profile.family();
        if parsed_families.contains(&family) {
            continue;
        }
        parsed_families.push(family);
        let dependencies = profile.extract_manifest_dependencies(filename, content);
        record_family_dependencies(registry, family, &scope, &origin, &dependencies);
    }
}

fn finish_registry(
    mut registry: DependencyRegistry,
    snapshots: ManifestSnapshots,
) -> Result<DependencyRegistry> {
    registry.javascript_packages.apply_pnpm_workspaces();
    registry.typescript_paths.resolve_all();
    registry.prepare_scoped_policies()?;
    let mut hasher = Sha256::new();
    hasher.update(b"acdd-dependency-registry-v1\0");
    let standard_digest = DependencyRegistry::standard_manifests_digest_static();
    hasher.update((standard_digest.len() as u64).to_be_bytes());
    hasher.update(standard_digest.as_bytes());
    for (path, digest) in snapshots {
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update(digest);
    }
    registry.digest = hex::encode(hasher.finalize());
    registry.prepare_language_contexts();
    registry.validate_type_flow_policies()?;
    Ok(registry)
}

pub(crate) fn package_identity(family: LanguageFamily, name: &str) -> String {
    if family == LanguageFamily("python") {
        return parsers::python::normalize_package_name(name).unwrap_or_else(|| name.to_owned());
    }
    name.to_owned()
}

fn scanner_manifest_paths(
    root: &Path,
    ignored_names: &BTreeSet<String>,
    discovered: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let filename = entry.file_name();
            let Some(filename) = filename.to_str() else {
                continue;
            };
            if ignored_names.contains(filename) || !is_dependency_manifest_filename(filename) {
                continue;
            }
            let candidate = entry.path();
            let is_regular_file = fs::symlink_metadata(&candidate)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
            if !is_regular_file {
                continue;
            }
            let relative = candidate
                .strip_prefix(root)
                .context("workspace manifest escaped its root")?;
            paths.insert(crate::scanner::checked_child(root, relative)?);
        }
    }
    for discovered_path in discovered {
        let Ok(relative) = discovered_path.strip_prefix(root) else {
            continue;
        };
        let Some(filename) = discovered_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if ignored_names.contains(filename) || !is_dependency_manifest_filename(filename) {
            continue;
        }
        let path = crate::scanner::checked_child(root, relative)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn walked_manifest_paths(root: &Path, ignored_names: BTreeSet<String>) -> Vec<PathBuf> {
    let ignored_names: HashSet<std::ffi::OsString> =
        ignored_names.into_iter().map(Into::into).collect();
    let mut walk = ignore::WalkBuilder::new(root);
    walk.hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(move |entry| {
            !entry.path_is_symlink()
                && !ignored_names.contains(entry.file_name())
                && !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__")
                )
        });
    walk.build()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(is_dependency_manifest_filename)
        })
        .map(|entry| entry.into_path())
        .collect()
}

fn preferred_manifest(candidate: &str, recorded: &str) -> bool {
    let rank = |path: &str| match path.rsplit('/').next().unwrap_or(path) {
        "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" => 0,
        _ => 1,
    };
    (rank(candidate), candidate) < (rank(recorded), recorded)
}

pub(super) fn read_confined_manifest_string(root: &Path, relative: &Path) -> Result<String> {
    use std::io::Read;
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    let mut file = acdd_core::fs::open_confined(root, relative, &mut opts)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        anyhow::bail!("manifest is not a regular file");
    }
    if meta.len() > 4 * 1024 * 1024 {
        anyhow::bail!("manifest exceeds the 4 MiB limit");
    }
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

/// Reports whether a filename matches any registered parser or language profile manifest pattern.
pub fn is_dependency_manifest_filename(filename: &str) -> bool {
    MANIFEST_PARSERS
        .iter()
        .any(|parser| parser.matches_filename(filename))
        || profiles().any(|profile| {
            profile
                .manifest_filenames()
                .iter()
                .any(|pattern| parser::filename_matches(pattern, filename))
        })
}
