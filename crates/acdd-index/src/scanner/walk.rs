use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use acdd_core::config::Adapter;
use acdd_core::fs::checked_child;
use ignore::WalkBuilder;

pub(super) fn error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn dir_pattern_matches(rel_path: &str, dir_pattern: &str) -> bool {
    let dir_norm = dir_pattern.trim_matches(['/', '\\']).replace('\\', "/");
    let rel_norm = rel_path.trim_matches(['/', '\\']).replace('\\', "/");
    if dir_norm.is_empty() || rel_norm.is_empty() {
        return false;
    }
    let descendant_glob = format!("{}/**", dir_norm.trim_end_matches("/**"));
    acdd_core::path_policy::glob_matches(&dir_norm, &rel_norm)
        || acdd_core::path_policy::glob_matches(&descendant_glob, &rel_norm)
}

/// Checks if a relative path belongs to any configured milestone or plan directories.
pub(super) fn is_milestone_or_plan_path(rel_str: &str, milestones: &[String], plans: &[String]) -> bool {
    let check = |dirs: &[String]| dirs.iter().any(|d| dir_pattern_matches(rel_str, d));
    check(milestones) || check(plans)
}

pub(super) fn is_spec_collection_path(rel_str: &str, adapter: &Adapter) -> bool {
    is_milestone_or_plan_path(rel_str, &adapter.milestones, &adapter.plans)
        || adapter
            .specs
            .values()
            .any(|spec| spec.matches_rel_path(rel_str))
}

/// Performs language.
pub(crate) fn language(path: &Path) -> Option<(&'static str, bool)> {
    // ACDD verification receipt sidecars (`<db>.verified.json`) live next to caches
    // (sometimes inside test or workspace roots) and must never be indexed as JSON source.
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".verified.json"))
    {
        return None;
    }
    if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "mdx")
    ) {
        return Some(("markdown", true));
    }
    crate::languages::for_path(path).map(|p| (p.id(), false))
}

/// Discovers dependency manifests without retaining the cold-build candidate list.
pub fn dependency_manifests_for_build(
    root: &Path,
    adapter: &Adapter,
) -> std::io::Result<Vec<PathBuf>> {
    walk_candidate_paths(root, adapter, None, false, |_, _| Ok(()))
}

pub(super) struct WatchedEntries {
    pub(super) keys: BTreeSet<String>,
    pub(super) found: std::sync::Mutex<BTreeSet<String>>,
    pub(super) symlinks: std::sync::Mutex<BTreeSet<String>>,
}

impl WatchedEntries {
    fn observe(&self, item: &ignore::DirEntry, root: &Path, prefix: &str) {
        let Ok(relative) = item.path().strip_prefix(root) else {
            return;
        };
        let Some(relative) = relative.to_str() else {
            return;
        };
        let key = format!("{prefix}{}", relative.replace('\\', "/"));
        if self.keys.contains(&key) {
            let target = if item.path_is_symlink() {
                &self.symlinks
            } else {
                &self.found
            };
            target.lock().unwrap().insert(key);
        }
    }
}

struct WalkContext<'a, F> {
    adapter: &'a Adapter,
    code_enabled: bool,
    docs_enabled: bool,
    include_sources: bool,
    configuration_paths: Option<&'a BTreeSet<PathBuf>>,
    candidate_count: &'a mut usize,
    seen_rel_paths: &'a mut BTreeSet<String>,
    dependency_manifests: &'a mut BTreeSet<PathBuf>,
    visit: &'a mut F,
}

fn walk_roots<F>(
    roots: &[PathBuf],
    ignored_names: &BTreeSet<String>,
    strip_base: &Path,
    rel_prefix: &str,
    watch: Option<&std::sync::Arc<WatchedEntries>>,
    watch_root: &Path,
    ctx: &mut WalkContext<'_, F>,
) -> std::io::Result<()>
where
    F: FnMut(PathBuf, String) -> std::io::Result<()>,
{
    for source_root in roots {
        if !source_root.exists() {
            continue;
        }
        let mut builder = WalkBuilder::new(source_root);
        builder
            .hidden(false)
            .follow_links(false)
            .same_file_system(true)
            .git_ignore(true)
            .require_git(false);
        let ignored = ignored_names.clone();
        let watch_entry = watch.cloned();
        let w_root = watch_root.to_path_buf();
        let w_prefix = rel_prefix.to_owned();
        builder.filter_entry(move |item| {
            if let Some(watch) = &watch_entry {
                watch.observe(item, &w_root, &w_prefix);
            }
            !item.path_is_symlink()
                && !ignored.contains(&item.file_name().to_string_lossy().to_string())
        });
        for result in builder.build() {
            let item = result.map_err(|e| error(e.to_string()))?;
            if item.path_is_symlink()
                || ignored_names.contains(&item.file_name().to_string_lossy().to_string())
                || !item.file_type().is_some_and(|kind| kind.is_file())
            {
                continue;
            }
            let full = item.into_path();
            if ctx
                .configuration_paths
                .is_some_and(|paths| paths.contains(&full))
            {
                continue;
            }
            let filename = full
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let is_manifest = ctx.code_enabled
                && crate::languages::dependency_registry::is_dependency_manifest_filename(filename);
            let file_language = ctx.include_sources.then(|| language(&full)).flatten();
            let is_doc = file_language.is_some_and(|(_, is_doc)| is_doc);
            let is_source = file_language.is_some()
                && ((is_doc && ctx.docs_enabled) || (!is_doc && ctx.code_enabled));
            if !is_manifest && !is_source {
                continue;
            }
            let Ok(relative) = full.strip_prefix(strip_base) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !is_doc && is_spec_collection_path(&relative, ctx.adapter) {
                continue;
            }
            if is_manifest {
                ctx.dependency_manifests.insert(full.clone());
            }
            if is_source {
                visit_candidate(
                    ctx.candidate_count,
                    ctx.seen_rel_paths,
                    full,
                    format!("{rel_prefix}{relative}"),
                    ctx.adapter.limits.max_files,
                    ctx.visit,
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn walk_candidate_paths(
    root: &Path,
    adapter: &Adapter,
    watch: Option<&std::sync::Arc<WatchedEntries>>,
    include_sources: bool,
    mut visit: impl FnMut(PathBuf, String) -> std::io::Result<()>,
) -> std::io::Result<Vec<PathBuf>> {
    let code_enabled = adapter.modules.is_enabled("code");
    let docs_enabled = adapter.modules.is_enabled("docs");
    if !code_enabled && !docs_enabled {
        return Ok(Vec::new());
    }

    let mut candidate_count = 0usize;
    let mut seen_rel_paths = BTreeSet::new();
    let mut dependency_manifests = BTreeSet::new();
    let configuration_paths: BTreeSet<_> = [root.join("acdd.yaml")]
        .into_iter()
        .chain(adapter.adapter_path.iter().cloned())
        .collect();

    if docs_enabled && include_sources {
        collect_documentation_paths(
            root,
            "",
            (&adapter.ignored_names, watch),
            adapter.limits.max_files,
            &mut candidate_count,
            &mut seen_rel_paths,
            &mut visit,
        )?;
    }

    let mut ctx = WalkContext {
        adapter,
        code_enabled,
        docs_enabled,
        include_sources,
        configuration_paths: Some(&configuration_paths),
        candidate_count: &mut candidate_count,
        seen_rel_paths: &mut seen_rel_paths,
        dependency_manifests: &mut dependency_manifests,
        visit: &mut visit,
    };
    walk_roots(
        &adapter.roots,
        &adapter.ignored_names,
        root,
        "",
        watch,
        root,
        &mut ctx,
    )?;

    for workspace in &adapter.linked_workspaces {
        let prefix = format!("[{}]/", workspace.name);
        ctx.configuration_paths = None;
        walk_roots(
            &workspace.roots,
            &workspace.ignored_names,
            &workspace.path,
            &prefix,
            watch,
            &workspace.path,
            &mut ctx,
        )?;
        if code_enabled {
            collect_source_root_manifests(
                &workspace.path,
                &workspace.roots,
                &workspace.ignored_names,
                ctx.dependency_manifests,
            )?;
        }
        if docs_enabled && include_sources {
            collect_documentation_paths(
                &workspace.path,
                &prefix,
                (&workspace.ignored_names, watch),
                adapter.limits.max_files,
                ctx.candidate_count,
                ctx.seen_rel_paths,
                ctx.visit,
            )?;
        }
    }
    if code_enabled {
        collect_source_root_manifests(
            root,
            &adapter.roots,
            &adapter.ignored_names,
            &mut dependency_manifests,
        )?;
    }
    Ok(dependency_manifests.into_iter().collect())
}

fn collect_documentation_paths(
    workspace_root: &Path,
    prefix: &str,
    policy: (&BTreeSet<String>, Option<&std::sync::Arc<WatchedEntries>>),
    max_files: usize,
    candidate_count: &mut usize,
    seen_rel_paths: &mut BTreeSet<String>,
    visit: &mut impl FnMut(PathBuf, String) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let (ignored_names, watch) = policy;
    let agents = workspace_root.join("AGENTS.md");
    if !ignored_names.contains("AGENTS.md") && agents.is_file() && !agents.is_symlink() {
        visit_candidate(
            candidate_count,
            seen_rel_paths,
            agents,
            format!("{prefix}AGENTS.md"),
            max_files,
            visit,
        )?;
    }

    let docs_root = workspace_root.join("docs");
    if ignored_names.contains("docs") || !docs_root.is_dir() || docs_root.is_symlink() {
        return Ok(());
    }
    let mut builder = WalkBuilder::new(&docs_root);
    builder
        .hidden(false)
        .follow_links(false)
        .same_file_system(true)
        .git_ignore(true)
        .require_git(false);
    let ignored = ignored_names.clone();
    let watch_entry = watch.cloned();
    let watch_root = workspace_root.to_path_buf();
    let watch_prefix = prefix.to_owned();
    builder.filter_entry(move |item| {
        if let Some(watch) = &watch_entry {
            watch.observe(item, &watch_root, &watch_prefix);
        }
        !item.path_is_symlink()
            && !ignored.contains(&item.file_name().to_string_lossy().to_string())
    });
    for result in builder.build() {
        let item = result.map_err(|walk_error| error(walk_error.to_string()))?;
        if item.path_is_symlink()
            || !item.file_type().is_some_and(|kind| kind.is_file())
            || !item
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "md" | "mdx"))
        {
            continue;
        }
        let full = item.into_path();
        let Ok(relative) = full.strip_prefix(workspace_root) else {
            continue;
        };
        let relative = format!("{prefix}{}", relative.to_string_lossy().replace('\\', "/"));
        visit_candidate(
            candidate_count,
            seen_rel_paths,
            full,
            relative,
            max_files,
            visit,
        )?;
    }
    Ok(())
}

fn visit_candidate(
    candidate_count: &mut usize,
    seen_rel_paths: &mut BTreeSet<String>,
    full_path: PathBuf,
    relative_path: String,
    max_files: usize,
    visit: &mut impl FnMut(PathBuf, String) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if seen_rel_paths.insert(relative_path.clone()) {
        *candidate_count += 1;
        if *candidate_count > max_files {
            return Err(error(format!(
                "workspace exceeds file limit ({} > max_files {}) at {}",
                *candidate_count, max_files, relative_path
            )));
        }
        visit(full_path, relative_path)?;
    }
    Ok(())
}

fn collect_source_root_manifests(
    workspace_root: &Path,
    source_roots: &[PathBuf],
    ignored_names: &BTreeSet<String>,
    output: &mut BTreeSet<PathBuf>,
) -> std::io::Result<()> {
    let mut directories = BTreeSet::new();
    for source_root in source_roots {
        let mut directory = source_root.parent();
        while let Some(path) = directory {
            if !path.starts_with(workspace_root) || path == workspace_root {
                break;
            }
            let relative = path
                .strip_prefix(workspace_root)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if relative.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|name| ignored_names.contains(name))
            }) {
                break;
            }
            directories.insert(path.to_path_buf());
            directory = path.parent();
        }
    }
    for directory in directories {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let filename = entry.file_name();
            let Some(filename) = filename.to_str() else {
                continue;
            };
            if ignored_names.contains(filename)
                || !crate::languages::dependency_registry::is_dependency_manifest_filename(filename)
            {
                continue;
            }
            let candidate = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let relative = candidate
                .strip_prefix(workspace_root)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            output.insert(checked_child(workspace_root, relative)?);
        }
    }
    Ok(())
}
