use super::parser::ModulePrefix;
use super::parsers;
use crate::languages::registries::{
    javascript_packages::JavascriptPackageRegistry, typescript_paths::TypeScriptPathsRegistry,
};
use crate::languages::context::{LanguageContext, RuleIRHandle};
use crate::languages::manifests::FrameworkManifest;
use crate::languages::types::{workspace_path, ImportPath, LanguageFamily};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
};

pub(crate) type ManifestSnapshots = BTreeMap<String, sha2::digest::Output<Sha256>>;

/// A manifest's UTF-8 contents at a canonical workspace-relative path.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ManifestSource<'a> {
    /// A dependency filename or `.acdd/frameworks/<identity>.{toml,yaml,yml}`.
    pub path: &'a str,
    /// The manifest contents, bounded to 4 MiB.
    pub source: &'a str,
}

/// An explicit manifest inventory isolated to one workspace.
#[cfg(test)]
pub(crate) struct WorkspaceManifestSources<'a> {
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
    pub(in crate::languages) packages: HashMap<LanguageFamily, HashSet<String>>,
    pub(crate) scoped_packages: HashMap<LanguageFamily, HashMap<String, HashSet<String>>>,
    pub(in crate::languages) scoped_origins:
        HashMap<LanguageFamily, HashMap<String, HashMap<String, String>>>,
    pub(in crate::languages) module_prefixes: HashMap<LanguageFamily, HashMap<String, Vec<ModulePrefix>>>,
    pub(crate) own_packages: HashMap<String, HashSet<String>>,
    pub(crate) workspaces: HashSet<String>,
    pub(crate) named_provider_scopes:
        HashMap<crate::languages::manifests::projections::NamedProviderScope, String>,
    pub(in crate::languages) rust_libraries: HashMap<String, String>,
    pub(in crate::languages) rust_package_libraries: HashMap<String, String>,
    pub(crate) framework_manifests: HashMap<(String, String), FrameworkManifest>,
    pub(crate) standard_framework_manifests: HashMap<String, FrameworkManifest>,
    pub(crate) language_contexts: HashMap<LanguageContextKey, Arc<LanguageContext>>,
    pub(crate) rule_ir: HashMap<String, RuleIRHandle>,
    pub(in crate::languages) javascript_packages: JavascriptPackageRegistry,
    pub(in crate::languages) typescript_paths: TypeScriptPathsRegistry,
    #[cfg(feature = "lang-rust")]
    pub(in crate::languages) rust_root_crates: HashMap<String, String>,
    pub(crate) digest: String,
    pub(in crate::languages) manifest_entrypoints: Vec<ManifestEntrypoint>,
}

impl DependencyRegistry {
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
        let workspace = workspace_path(path).0;
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

    /// Returns the deterministic SHA-256 digest of collected dependency and framework manifests.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the root Cargo library name normalized for Rust import syntax.
    pub(crate) fn rust_library_for_path(&self, path: &str) -> Option<&str> {
        let workspace = workspace_path(path).0;
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
            workspace_path(path).1
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
    pub(crate) fn module_prefixes_for_path(
        &self,
        family: LanguageFamily,
        path: &str,
    ) -> Vec<&ModulePrefix> {
        let Some(scopes) = self.module_prefixes.get(&family) else {
            return Vec::new();
        };
        let workspace = workspace_path(path).0;
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
    #[cfg(test)]
    pub(crate) fn module_prefix_for_path(
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
        let workspace = workspace_path(path).0;
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
}

pub(super) fn is_directory_ancestor(dir: &str, path: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub(super) fn join_workspace_root(scope: &str, relative: &str) -> String {
    match (scope.is_empty(), relative.is_empty()) {
        (true, _) => relative.to_owned(),
        (false, true) => scope.to_owned(),
        (false, false) => format!("{scope}/{relative}"),
    }
}

pub(super) fn record_family_dependencies(
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

pub(super) fn finish_registry(
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

pub(super) fn package_identity(family: LanguageFamily, name: &str) -> String {
    if family == LanguageFamily("python") {
        return parsers::python::normalize_package_name(name).unwrap_or_else(|| name.to_owned());
    }
    name.to_owned()
}

pub(super) fn preferred_manifest(candidate: &str, recorded: &str) -> bool {
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
