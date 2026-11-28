use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub use acdd_core::fs::{canonical_root, checked_child, safe_relative};
use rayon::prelude::*;

use serde::Serialize;

pub mod discovery;
mod walk;
use crate::models::FileEntry;
pub use crate::models::INDEX_SEMANTICS_VERSION;
pub use acdd_core::config::ENGINE_SCHEMA_VERSION;
pub use acdd_core::config::*;
pub use walk::dependency_manifests_for_build;
use walk::{error, language, walk_candidate_paths, WatchedEntries};

#[derive(Debug, Clone, Serialize)]
/// Inventory counts, selected entries, and watched-path observations from a scan.
pub struct ScanReport {
    /// Number of files selected for indexing.
    pub files: usize,
    /// Combined byte size of the selected files.
    pub bytes: u64,
    /// Time spent producing the scan report, in milliseconds.
    pub elapsed_ms: u128,
    /// File metadata records selected for indexing.
    pub entries: Vec<FileEntry>,
    /// Dependency manifest paths discovered during the same inventory walk.
    pub dependency_manifests: Vec<PathBuf>,
    /// Watched exact entries observed during the inventory traversal.
    #[serde(skip)]
    pub matched_excluded: BTreeSet<String>,
}

/// Source metadata captured by the cold-build walk before opening file content.
#[derive(Debug, Clone)]
pub struct BuildCandidate {
    pub path: String,
    pub bytes: u64,
    pub mtime_ns: u128,
    pub identity: [u64; 3],
    pub language: String,
    pub is_doc: bool,
}

impl BuildCandidate {
    pub fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && self.bytes == metadata.len()
            && self.mtime_ns == metadata_mtime_ns(metadata)
            && self.identity == metadata_identity(metadata)
    }

    pub fn into_entry(self, digest: String) -> FileEntry {
        FileEntry {
            path: self.path,
            digest,
            bytes: self.bytes,
            mtime_ns: self.mtime_ns,
            identity: self.identity,
            language: self.language,
            is_doc: self.is_doc,
        }
    }
}

/// Summary for the cold-build walk that sends candidates to a live consumer.
#[derive(Debug, Clone)]
pub struct BuildStreamReport {
    pub files: usize,
    pub bytes: u64,
}

/// Candidates, dependency manifests, and counters from a cold-build walk.
#[derive(Debug)]
pub struct ColdScanResult {
    /// Source and documentation candidates selected for extraction.
    pub candidates: Vec<BuildCandidate>,
    /// Dependency manifests discovered during the same walk.
    pub dependency_manifests: Vec<PathBuf>,
    /// Counts observed during the walk.
    pub report: BuildStreamReport,
}

#[derive(Debug, Clone, Serialize)]
/// Counts and output location recorded after a completed index build.
pub struct BuildReport {
    /// Path to the generated index database.
    pub output: String,
    /// Number of files included in the build.
    pub files: usize,
    /// Number of graph nodes written to the index.
    pub nodes: usize,
    /// Number of documentation sections written to the index.
    pub doc_sections: usize,
    /// Total build duration, in milliseconds.
    pub elapsed_ms: u128,
    /// Schema version used by the engine to create the database.
    pub engine_schema_version: &'static str,
}

pub use acdd_core::config::resolve_file_path;

/// Classify a batch of keys using one bounded cache of directory entries.
pub fn exact_paths_exist<'a>(
    root: &Path,
    adapter: &Adapter,
    paths: impl IntoIterator<Item = &'a str>,
) -> std::io::Result<BTreeSet<String>> {
    let mut directories: BTreeMap<PathBuf, BTreeMap<std::ffi::OsString, fs::FileType>> =
        BTreeMap::new();
    let mut count = 0usize;
    let mut existing = BTreeSet::new();
    for path in paths {
        let context =
            |err: std::io::Error| std::io::Error::new(err.kind(), format!("inspect {path}: {err}"));
        let resolved = resolve_file_path(root, adapter, path).map_err(context)?;
        let base = if let Some((prefix, _)) = path.split_once("]/") {
            adapter
                .linked_workspaces
                .iter()
                .find(|workspace| prefix.strip_prefix('[') == Some(workspace.name.as_str()))
                .map(|workspace| workspace.path.as_path())
                .unwrap_or(root)
        } else {
            root
        };
        let relative = resolved
            .strip_prefix(base)
            .map_err(|_| error(format!("inspect {path}: path outside workspace")))?;
        let mut current = base.to_path_buf();
        let mut found = true;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(error(format!("inspect {path}: invalid path")));
            };
            if !directories.contains_key(&current) {
                let mut entries = BTreeMap::new();
                for entry in fs::read_dir(&current).map_err(context)? {
                    let entry = entry.map_err(context)?;
                    count += 1;
                    if count > adapter.limits.max_files {
                        return Err(error(format!(
                            "inspect {path}: exact directory cache exceeds max_files {}",
                            adapter.limits.max_files
                        )));
                    }
                    entries.insert(entry.file_name(), entry.file_type().map_err(context)?);
                }
                directories.insert(current.clone(), entries);
            }
            let Some(kind) = directories
                .get(&current)
                .and_then(|entries| entries.get(name))
            else {
                found = false;
                break;
            };
            if kind.is_symlink() {
                return Err(error(format!(
                    "inspect {path}: path must not traverse a symlink"
                )));
            }
            current.push(name);
            if !kind.is_dir() && current != resolved {
                found = false;
                break;
            }
        }
        if found {
            existing.insert(path.to_owned());
        }
    }
    Ok(existing)
}

/// Determine whether every component has the exact spelling reported by its directory.
/// Errors inspecting a boundary are propagated rather than interpreted as absence.
#[cfg(test)]
pub(crate) fn exact_path_exists(
    root: &Path,
    adapter: &Adapter,
    path: &str,
) -> std::io::Result<bool> {
    Ok(exact_paths_exist(root, adapter, [path])?.contains(path))
}

fn ensure_bounds(files: usize, bytes: u64, limits: &ScannerLimits) -> std::io::Result<()> {
    if files > limits.max_files {
        return Err(error(format!(
            "workspace exceeds file limit ({} > max_files {})",
            files, limits.max_files
        )));
    }
    if bytes > limits.max_total_bytes {
        return Err(error(format!(
            "workspace exceeds total byte limit ({} > max_total_bytes {})",
            bytes, limits.max_total_bytes
        )));
    }
    Ok(())
}

pub(crate) fn bounded_batch_size(
    memory_budget: u64,
    max_item_bytes: u64,
    expansion_factor: u64,
    max_items: usize,
) -> usize {
    let estimated_item_bytes = max_item_bytes
        .max(1)
        .saturating_mul(expansion_factor.max(1));
    ((memory_budget.max(1) / estimated_item_bytes)
        .max(1)
        .min(max_items.max(1) as u64)) as usize
}

/// Computes the SHA-256 digest of file bytes.
pub fn digest(bytes: &[u8]) -> String {
    acdd_core::format::sha256_hex(bytes)
}

fn is_symlink_open_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    if err.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    err.kind() == std::io::ErrorKind::InvalidInput && err.to_string().contains("symlink")
}

/// Reads a confined file entry and skips files that cannot be indexed.
pub(crate) fn entry_with_rel(
    path: &Path,
    relative: String,
    max_file_bytes: u64,
) -> std::io::Result<Option<FileEntry>> {
    let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) else {
        return Ok(None);
    };
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    let file = match acdd_core::fs::open_confined(parent, Path::new(file_name), &mut opts) {
        Ok(file) => file,
        Err(err) if is_symlink_open_error(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let Some((language, is_doc)) = language(path) else {
        return Ok(None);
    };
    if metadata.len() > max_file_bytes {
        return Err(error(format!(
            "file exceeds byte limit: {} ({} > max_file_bytes {})",
            path.display(),
            metadata.len(),
            max_file_bytes
        )));
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(64 * 1024));
    file.take(max_file_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual_bytes = bytes.len() as u64;
    if actual_bytes > max_file_bytes {
        return Err(error(format!(
            "file exceeds byte limit: {} ({} > max_file_bytes {})",
            path.display(),
            actual_bytes,
            max_file_bytes
        )));
    }
    let mtime_ns = metadata_mtime_ns(&metadata);
    Ok(Some(FileEntry {
        path: relative,
        digest: digest(&bytes),
        bytes: actual_bytes,
        mtime_ns,
        identity: metadata_identity(&metadata),
        language: language.to_owned(),
        is_doc,
    }))
}

fn metadata_identity(metadata: &fs::Metadata) -> [u64; 3] {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        [
            metadata.dev(),
            metadata.ino(),
            (metadata.ctime() as u64)
                .wrapping_mul(1_000_000_000)
                .wrapping_add(metadata.ctime_nsec() as u64),
        ]
    }
    #[cfg(not(unix))]
    {
        [0; 3]
    }
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn cached_entry(
    full_path: &Path,
    rel_path: &str,
    previous: &std::collections::BTreeMap<&str, &FileEntry>,
    max_file_bytes: u64,
) -> std::io::Result<Option<FileEntry>> {
    if let Some(old) = previous.get(rel_path) {
        let metadata = fs::symlink_metadata(full_path)?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        if metadata.is_file()
            && !metadata.file_type().is_symlink()
            && old.bytes == metadata.len()
            && old.mtime_ns == modified
            && old.identity == metadata_identity(&metadata)
            && old.identity != [0; 3]
        {
            return Ok(Some((*old).clone()));
        }
    }
    entry_with_rel(full_path, rel_path.to_owned(), max_file_bytes)
}

/// Scans a workspace using the supplied adapter configuration.
pub fn scan_with_adapter(root: &Path, adapter: &Adapter) -> std::io::Result<ScanReport> {
    check_root_scope(root, adapter.limits.allow_broad_root)?;
    scan_reusing(root, adapter, &[])
}

/// Scans a workspace while reusing metadata from previous file entries.
pub fn scan_reusing(
    root: &Path,
    adapter: &Adapter,
    previous: &[FileEntry],
) -> std::io::Result<ScanReport> {
    scan_reusing_watching(root, adapter, previous, &BTreeSet::new())
}

/// Scan while observing exact spellings of requested inventory keys.
pub fn scan_reusing_watching(
    root: &Path,
    adapter: &Adapter,
    previous: &[FileEntry],
    watched: &BTreeSet<String>,
) -> std::io::Result<ScanReport> {
    let started = Instant::now();
    let watch = std::sync::Arc::new(WatchedEntries {
        keys: watched.clone(),
        found: std::sync::Mutex::new(BTreeSet::new()),
        symlinks: std::sync::Mutex::new(BTreeSet::new()),
    });
    let (candidates, dependency_manifests) = collect_candidates(root, adapter, Some(&watch))?;
    if let Some(path) = watch.symlinks.lock().unwrap().first() {
        return Err(error(format!(
            "inspect {path}: path must not traverse a symlink"
        )));
    }
    let matched_excluded = watch.found.lock().unwrap().clone();

    let previous: std::collections::BTreeMap<_, _> =
        previous.iter().map(|f| (f.path.as_str(), f)).collect();
    let max_file_bytes = adapter.limits.max_file_bytes;
    let scan_budget =
        resolve_memory_budget_bytes(&adapter.limits)?.min(adapter.limits.max_total_bytes.max(1));
    let batch_size = bounded_batch_size(scan_budget, max_file_bytes, 1, candidates.len().max(1));
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    for batch in candidates.chunks(batch_size) {
        let batch_entries: std::io::Result<Vec<_>> = batch
            .par_iter()
            .map(|(full_path, rel_path)| {
                cached_entry(full_path, rel_path, &previous, max_file_bytes).map_err(|err| {
                    std::io::Error::new(err.kind(), format!("inspect {rel_path}: {err}"))
                })
            })
            .collect();
        let batch_entries = batch_entries?;
        let batch_bytes = batch_entries
            .iter()
            .flatten()
            .try_fold(0u64, |total, entry| total.checked_add(entry.bytes))
            .ok_or_else(|| error("workspace byte count overflow"))?;
        bytes = bytes
            .checked_add(batch_bytes)
            .ok_or_else(|| error("workspace byte count overflow"))?;
        if bytes > adapter.limits.max_total_bytes {
            return Err(error(format!(
                "workspace exceeds total byte limit ({} > max_total_bytes {})",
                bytes, adapter.limits.max_total_bytes
            )));
        }
        entries.extend(batch_entries.into_iter().flatten());
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    ensure_bounds(entries.len(), bytes, &adapter.limits)?;
    Ok(ScanReport {
        files: entries.len(),
        bytes,
        elapsed_ms: started.elapsed().as_millis(),
        entries,
        dependency_manifests,
        matched_excluded,
    })
}

/// Streams metadata-only cold-build candidates to the pipeline as the walker discovers them.
pub fn stream_for_build(
    root: &Path,
    adapter: &Adapter,
    visit: impl FnMut(BuildCandidate) -> std::io::Result<()>,
) -> std::io::Result<BuildStreamReport> {
    stream_for_build_with_manifests(root, adapter, visit).map(|(_, report)| report)
}

fn stream_for_build_with_manifests(
    root: &Path,
    adapter: &Adapter,
    mut visit: impl FnMut(BuildCandidate) -> std::io::Result<()>,
) -> std::io::Result<(Vec<PathBuf>, BuildStreamReport)> {
    let code_enabled = adapter.modules.is_enabled("code");
    let docs_enabled = adapter.modules.is_enabled("docs");
    if !code_enabled && !docs_enabled {
        return Ok((Vec::new(), BuildStreamReport { files: 0, bytes: 0 }));
    }
    let mut files = 0usize;
    let mut bytes = 0u64;
    let dependency_manifests =
        walk_candidate_paths(root, adapter, None, true, |full_path, relative_path| {
            let metadata = fs::symlink_metadata(&full_path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Ok(());
            }
            let Some((language, is_doc)) = language(&full_path) else {
                return Ok(());
            };
            if (is_doc && !docs_enabled) || (!is_doc && !code_enabled) {
                return Ok(());
            }
            if metadata.len() > adapter.limits.max_file_bytes {
                return Err(error(format!(
                    "file exceeds byte limit: {} ({} > max_file_bytes {})",
                    full_path.display(),
                    metadata.len(),
                    adapter.limits.max_file_bytes
                )));
            }
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| error("workspace byte count overflow"))?;
            if bytes > adapter.limits.max_total_bytes {
                return Err(error(format!(
                    "workspace exceeds total byte limit ({} > max_total_bytes {})",
                    bytes, adapter.limits.max_total_bytes
                )));
            }
            files += 1;
            visit(BuildCandidate {
                path: relative_path,
                bytes: metadata.len(),
                mtime_ns: metadata_mtime_ns(&metadata),
                identity: metadata_identity(&metadata),
                language: language.to_owned(),
                is_doc,
            })?;
            Ok(())
        })?;
    ensure_bounds(files, bytes, &adapter.limits)?;
    Ok((dependency_manifests, BuildStreamReport { files, bytes }))
}

/// Collects cold-build candidates and dependency manifests in one walk.
pub fn collect_for_build(root: &Path, adapter: &Adapter) -> std::io::Result<ColdScanResult> {
    let mut candidates = Vec::new();
    let (manifests, report) = stream_for_build_with_manifests(root, adapter, |candidate| {
        candidates.push(candidate);
        Ok(())
    })?;
    Ok(ColdScanResult {
        candidates,
        dependency_manifests: manifests,
        report,
    })
}

type ScanCandidates = (Vec<(PathBuf, String)>, Vec<PathBuf>);

fn collect_candidates(
    root: &Path,
    adapter: &Adapter,
    watch: Option<&std::sync::Arc<WatchedEntries>>,
) -> std::io::Result<ScanCandidates> {
    let mut candidates = Vec::new();
    let dependency_manifests =
        walk_candidate_paths(root, adapter, watch, true, |full, relative| {
            candidates.push((full, relative));
            Ok(())
        })?;
    Ok((candidates, dependency_manifests))
}

#[cfg(test)]
mod tests {
    use super::walk::is_milestone_or_plan_path;
    use super::*;
    use acdd_core::fs::ScopedTempDir;

    #[test]
    fn exact_path_requires_reported_component_spelling() {
        let _cleanup = ScopedTempDir::create("acdd_exact_spelling").unwrap();
        let root = _cleanup.path();
        fs::create_dir_all(root.join("Parent")).unwrap();
        fs::write(root.join("Parent/Foo.rs"), "").unwrap();
        let root = root.canonicalize().unwrap();
        let adapter = load_adapter(&root, None).unwrap();
        assert!(exact_path_exists(&root, &adapter, "Parent/Foo.rs").unwrap());
        assert!(!exact_path_exists(&root, &adapter, "parent/Foo.rs").unwrap());
        assert!(!exact_path_exists(&root, &adapter, "Parent/foo.rs").unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fs::write(root.join("Foo.rs"), "pub fn source() {}\n").unwrap();
            fs::hard_link(root.join("Foo.rs"), root.join("other.rs")).unwrap();
            assert_eq!(
                fs::metadata(root.join("Foo.rs")).unwrap().ino(),
                fs::metadata(root.join("other.rs")).unwrap().ino()
            );
            assert!(exact_path_exists(&root, &adapter, "Foo.rs").unwrap());
            assert!(exact_path_exists(&root, &adapter, "other.rs").unwrap());
            assert!(!exact_path_exists(&root, &adapter, "foo.rs").unwrap());
        }
    }

    #[test]
    fn watched_inventory_entries() {
        let _cleanup = ScopedTempDir::create("acdd_watch").unwrap();
        let root = _cleanup.path();
        fs::write(root.join("notes.txt"), "excluded").unwrap();
        fs::write(root.join("acdd.yaml"), "ignore: [ignored.rs]\n").unwrap();
        fs::write(root.join("ignored.rs"), "pub fn ignored() {}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let adapter = load_adapter(&root, None).unwrap();
        for (case_id, paths, rejected_key) in [
            (
                "watched_excluded_entries_are_collected_by_inventory_walk",
                vec!["notes.txt", "ignored.rs"],
                None::<&str>,
            ),
            #[cfg(unix)]
            (
                "watched_symlink_is_rejected_with_its_key",
                vec!["leaf.rs"],
                Some("leaf.rs"),
            ),
        ] {
            #[cfg(unix)]
            if rejected_key.is_some() {
                fs::write(root.join("target.rs"), "pub fn target() {}\n").unwrap();
                std::os::unix::fs::symlink(root.join("target.rs"), root.join("leaf.rs")).unwrap();
            }
            let watched = paths.into_iter().map(str::to_owned).collect();
            let result = scan_reusing_watching(&root, &adapter, &[], &watched);
            if let Some(key) = rejected_key {
                let err = result.unwrap_err();
                assert!(err.to_string().contains(key), "{case_id}: {err}");
            } else {
                assert_eq!(result.unwrap().matched_excluded, watched, "{case_id}");
            }
        }
    }

    #[test]
    fn bounded_batch_size_accounts_for_item_expansion_and_caps() {
        assert_eq!(
            bounded_batch_size(256 * 1024 * 1024, 5 * 1024 * 1024, 8, 25_000),
            6
        );
        assert_eq!(
            bounded_batch_size(16 * 1024 * 1024 * 1024, 5 * 1024 * 1024, 8, 25_000),
            409
        );
        assert_eq!(bounded_batch_size(1, 0, 0, 10), 1);
        assert_eq!(bounded_batch_size(1_000, 1, 1, 4), 4);
    }

    #[test]
    fn build_scan_excludes_ignored_manifests_from_source_root_ancestors() {
        let _cleanup = ScopedTempDir::create("acdd_ignored_ancestor_manifests").unwrap();
        let root = _cleanup.path();
        let source = root.join("packages/pkg/src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            root.join("acdd.yaml"),
            "roots: [packages/pkg/src]\nignore: [package.json]\n",
        )
        .unwrap();
        fs::write(root.join("packages/pkg/package.json"), "{}").unwrap();
        fs::write(root.join("packages/package.json"), "{}").unwrap();
        fs::write(
            root.join("packages/pyproject.toml"),
            "[project]\nname='kept'\n",
        )
        .unwrap();
        fs::write(source.join("index.ts"), "export const value = 1;\n").unwrap();

        let root = root.canonicalize().unwrap();
        let adapter = load_adapter(&root, None).unwrap();
        let manifests = dependency_manifests_for_build(&root, &adapter).unwrap();

        assert!(
            manifests
                .iter()
                .any(|path| path.ends_with("packages/pyproject.toml")),
            "ancestor traversal should retain an eligible manifest: {manifests:?}"
        );
        assert!(
            manifests
                .iter()
                .all(|path| path.file_name().is_none_or(|name| name != "package.json")),
            "ignored basenames must be excluded at every source-root ancestor: {manifests:?}"
        );
    }

    #[test]
    fn is_milestone_or_plan_path_supports_wildcards_trailing_slashes_and_custom_specs() {
        let milestones = vec![
            "extensions/*/docs/milestones/".to_owned(),
            "docs/milestones".to_owned(),
        ];
        let plans = vec!["services/*-api/docs/plans".to_owned()];

        for (rel, expected_spec) in [
            ("extensions/commerce/docs/milestones/010-test.md", true),
            ("services/auth-api/docs/plans/arch.md", true),
            ("docs/milestones/020.md", true),
            ("docs/milestones/README.md", true),
            ("extensions/commerce/docs/milestones/README.md", true),
            ("docs/milestones/archive/README.md", true),
            ("docs/other/020.md", false),
            ("docs/milestones_extra/020.md", false),
            ("extensions/commerce/src/lib.rs", false),
        ] {
            assert_eq!(
                is_milestone_or_plan_path(rel, &milestones, &plans),
                expected_spec,
                "path match: {rel}"
            );
        }

        let mut adapter = Adapter::default();
        adapter.specs.insert(
            "rfcs".to_owned(),
            SpecConfig {
                path: Some("docs/rfcs".to_owned()),
                prefixes: vec!["RFC".to_owned()],
                ..Default::default()
            },
        );
        assert!(super::walk::is_spec_collection_path(
            "docs/rfcs/helper.py",
            &adapter
        ));
        assert!(!super::walk::is_spec_collection_path(
            "docs/rfcs_extra/helper.py",
            &adapter
        ));
    }

    #[test]
    fn scan_respects_modules_code_and_docs_flags() {
        let _cleanup = ScopedTempDir::create("acdd_modules_scan").unwrap();
        let root = _cleanup.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").unwrap();
        fs::write(root.join("docs/guide.md"), "# Guide\n").unwrap();
        let root = root.canonicalize().unwrap();

        for (cfg_yaml, expect_code, expect_docs) in [
            ("modules:\n  code: true\n  docs: false\n", true, false),
            ("modules:\n  code: false\n  docs: true\n", false, true),
            ("modules:\n  code: false\n  docs: false\n", false, false),
        ] {
            fs::write(root.join("acdd.yaml"), cfg_yaml).unwrap();
            let adapter = load_adapter(&root, None).unwrap();
            let report = scan_with_adapter(&root, &adapter).unwrap();
            let has_code = report.entries.iter().any(|e| !e.is_doc);
            let has_docs = report.entries.iter().any(|e| e.is_doc);
            assert_eq!(
                (has_code, has_docs),
                (expect_code, expect_docs),
                "{cfg_yaml}"
            );
        }
    }
}
