use super::framework_packs;
use super::parsers::MANIFEST_PARSERS;
#[cfg(test)]
use super::registry::WorkspaceManifestSources;
use super::registry::{
    finish_registry, join_workspace_root, read_confined_manifest_string,
    record_family_dependencies, DependencyRegistry, ManifestEntrypoint, ManifestSnapshots,
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

impl DependencyRegistry {
    /// Collects validated rules and dependency scopes without accessing the filesystem.
    ///
    /// # Errors
    ///
    /// Rejects duplicate workspaces, invalid or duplicate paths, unsupported filenames,
    /// oversized inputs, and invalid framework rules or cross-workspace policies.
    #[cfg(test)]
    pub(crate) fn try_collect_from_sources<'a>(
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
                    crate::languages::manifests::semantic::normal_relative_path(manifest.path)
                        && crate::languages::manifests::projections::confined_path(
                            manifest.path,
                            false
                        )
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

    pub(super) fn try_collect(
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
                prefix_entry.source_root = join_workspace_root(&scope, &prefix_entry.source_root);
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
            continue;
        }
        for &family in parsed.ecosystem.families() {
            if parsed_families.contains(&family) {
                continue;
            }
            parsed_families.push(family);
            record_family_dependencies(registry, family, &scope, &origin, &parsed.dependencies);
        }
    }
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

/// Reports whether a filename matches any registered parser manifest pattern.
pub fn is_dependency_manifest_filename(filename: &str) -> bool {
    MANIFEST_PARSERS
        .iter()
        .any(|parser| parser.matches_filename(filename))
}
