//! Confined workspace and Git archive specification discovery helpers.

use crate::spec::{extract_spec_number, format_next_spec_id, parse_spec, SpecDocument, SpecStatus};
use acdd_core::config::{load_adapter, Adapter, SpecConfig};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::format::sha256_bytes;
use acdd_core::fs::{canonical_root, checked_child, open_confined};
use acdd_core::git::{
    git_common_dir, list_git_tree_entries_multi, read_git_blobs_utf8_batch, resolve_git_tree_oid,
    GitTreeEntry,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Return `(col_name, rel_path, SpecDocument)` entries plus non-fatal I/O warnings.
pub type CollectedArchiveScan = (Vec<(String, String, SpecDocument)>, Vec<String>);

/// Validated specification document discovered inside a configured collection.
#[derive(Debug, Clone)]
pub struct CollectedWorkspaceSpec {
    /// Normalized workspace-relative path.
    pub rel_path: String,
    /// Owning collection configuration, if matched.
    pub collection: Option<SpecConfig>,
    /// Parsed specification document.
    pub doc: SpecDocument,
}

/// Non-fatal or structural scan error collected during workspace spec scanning.
#[derive(Debug, Clone)]
pub struct WorkspaceSpecScanError {
    /// Machine error code.
    pub code: ErrorCode,
    /// Stable diagnostic string code.
    pub diag_code: &'static str,
    /// Workspace-relative file or collection path.
    pub path: String,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Result of scanning all active spec collections under a workspace root.
#[derive(Debug, Clone)]
pub struct WorkspaceSpecScan {
    /// Successfully parsed specification documents.
    pub specs: Vec<CollectedWorkspaceSpec>,
    /// Collection or parse errors encountered during scanning.
    pub errors: Vec<WorkspaceSpecScanError>,
}

/// Read and lexicographically sort directory entries (returns empty `Vec` on unreadable/missing dir).
pub(crate) fn sorted_dir_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

/// Read a confined file inside `root` as UTF-8.
pub fn read_confined_utf8(root: &Path, relpath: &str) -> Result<String, AcddError> {
    let mut file =
        open_confined(root, Path::new(relpath), OpenOptions::new().read(true)).map_err(|err| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("failed to read confined file '{relpath}': {err}"),
            )
        })?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("failed to decode UTF-8 in '{relpath}': {err}"),
        )
    })?;
    Ok(buf)
}

/// Return configured spec collections or the default `milestones` + `plans` map.
pub fn effective_spec_collections(
    specs: &BTreeMap<String, SpecConfig>,
) -> BTreeMap<String, SpecConfig> {
    if specs.is_empty() {
        acdd_core::config::default_specs()
    } else {
        specs.clone()
    }
}

/// Match a workspace-relative spec path to its owning collection key by longest matching pattern.
pub fn match_spec_collection(
    collections: &BTreeMap<String, SpecConfig>,
    spec_relpath: &str,
) -> Option<String> {
    let norm = spec_relpath.trim().replace('\\', "/");
    let norm = norm.strip_prefix("./").unwrap_or(&norm);
    collections
        .iter()
        .filter(|(_, cfg)| cfg.matches_rel_path(norm))
        .max_by_key(|(_, cfg)| cfg.patterns().into_iter().map(str::len).max().unwrap_or(0))
        .map(|(name, _)| name.clone())
}

fn max_spec_num_in_dir(primary: &Path, scan_dir: &Path, prefix: &str, max_num: &mut Option<u32>) {
    if fs::symlink_metadata(scan_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return;
    }
    for p in sorted_dir_paths(scan_dir) {
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            if let Some(num) = extract_spec_number(stem, prefix) {
                *max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
            }
        }
        if let Ok(rel) = p.strip_prefix(primary) {
            if let Ok(raw) = read_confined_utf8(primary, &rel.to_string_lossy()) {
                if let Ok(doc) = parse_spec(&raw) {
                    if let Some(num) = extract_spec_number(&doc.id, prefix) {
                        *max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
                    }
                }
            }
        }
    }
}

/// Compute the next collision-free sequential spec ID (`<PREFIX>-<NNN>`) across `db_max` and active + `archive/` files.
pub fn next_spec_id_for_prefix(
    primary: &Path,
    collection: &str,
    col: &SpecConfig,
    prefix: &str,
    db_max: Option<u32>,
) -> Result<String, AcddError> {
    let mut max_num = db_max;
    let resolved_dirs = col.resolve_dirs(primary).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("failed to resolve directories for '{collection}': {err}"),
        )
    })?;
    for dir_abs in resolved_dirs {
        for scan_dir in [dir_abs.clone(), dir_abs.join("archive")] {
            max_spec_num_in_dir(primary, &scan_dir, prefix, &mut max_num);
        }
    }
    Ok(format_next_spec_id(prefix, col.digits, max_num))
}

/// Return `true` when `ws_path` is inside a Git repository.
pub fn is_git_repo(ws_path: &Path) -> bool {
    ws_path.join(".git").exists() || git_common_dir(ws_path).is_ok()
}

/// Return `true` when `doc` represents a completed archive specification.
pub fn is_completed_archive_doc(doc: &SpecDocument) -> bool {
    doc.status == SpecStatus::Completed
        && doc
            .closure
            .as_ref()
            .is_none_or(|c| c.status == SpecStatus::Completed)
}

/// Load the workspace `acdd.yaml` adapter configuration at `ws_root`.
pub fn load_ws_adapter(ws_root: &Path) -> Result<Adapter, AcddError> {
    load_adapter(ws_root, None)
}

/// Resolve a linked workspace alias from `linked_workspaces` or per-spec `repositories:`.
pub fn resolve_linked_workspace_root(
    ws_alias: &str,
    raw_dep: &str,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
) -> Result<PathBuf, AcddError> {
    let ws_path = linked_workspaces
        .get(ws_alias)
        .or_else(|| spec_repos.get(ws_alias))
        .ok_or_else(|| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("dependency '{raw_dep}' references unknown linked workspace '{ws_alias}'"),
            )
        })?;
    canonical_root(ws_path).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "linked workspace '{ws_alias}' path '{}' is invalid or confined-check failed: {err}",
                ws_path.display()
            ),
        )
    })
}

/// Resolve relative/absolute paths declared in `SpecDocument.repositories`.
pub fn resolve_spec_repositories(
    primary_root: &Path,
    spec_doc: &SpecDocument,
) -> BTreeMap<String, PathBuf> {
    let base = acdd_core::git::primary_worktree_root(primary_root)
        .filter(|primary| {
            primary != primary_root && {
                let rels: Vec<&Path> = spec_doc
                    .repositories
                    .iter()
                    .map(|r| Path::new(&r.path))
                    .filter(|p| p.is_relative())
                    .collect();
                !rels.is_empty()
                    && rels.iter().all(|p| primary.join(p).exists())
                    && !rels.iter().all(|p| primary_root.join(p).exists())
            }
        })
        .unwrap_or_else(|| primary_root.to_path_buf());
    let mut map = BTreeMap::new();
    for r in &spec_doc.repositories {
        let p = Path::new(&r.path);
        map.insert(
            r.name.clone(),
            if p.is_relative() {
                base.join(p)
            } else {
                p.to_path_buf()
            },
        );
    }
    map
}

fn literal_git_prefix(pat: &str) -> String {
    let mut prefix_parts = Vec::new();
    for seg in pat.split('/') {
        if seg.contains('*') || seg.contains('?') || seg.contains('{') || seg.contains('[') {
            break;
        }
        prefix_parts.push(seg);
    }
    prefix_parts.join("/")
}

fn probe_disk_archive_dirs(
    ws_path: &Path,
    col: &SpecConfig,
    warnings: &mut Vec<String>,
) -> Result<Vec<(String, PathBuf)>, AcddError> {
    let resolved_dirs = col.resolve_dirs(ws_path).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("spec collection failed confinement check: {err}"),
        )
    })?;
    let mut out = Vec::new();
    for dir in resolved_dirs {
        let Ok(rel_dir) = dir.strip_prefix(ws_path) else {
            continue;
        };
        let arch_rel_join = rel_dir.join("archive");
        match fs::symlink_metadata(ws_path.join(&arch_rel_join)) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!(
                    "{}: unable to stat archive directory: {err}",
                    arch_rel_join.display()
                ));
                continue;
            }
            Ok(_) => {}
        }
        let arch_dir = checked_child(ws_path, &arch_rel_join).map_err(|err| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "archive directory '{}' failed confinement check: {err}",
                    arch_rel_join.display()
                ),
            )
        })?;
        if !arch_dir.is_dir() {
            continue;
        }
        let read_dir = match fs::read_dir(&arch_dir) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!(
                    "{}: unable to read archive directory: {err}",
                    arch_rel_join.display()
                ));
                continue;
            }
        };
        let mut entries = Vec::new();
        for item in read_dir {
            let entry = item.map_err(|err| {
                AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "{}: unable to read archive entry: {err}",
                        arch_rel_join.display()
                    ),
                )
            })?;
            entries.push(entry);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            let ft = entry.file_type().map_err(|err| {
                AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("archive entry '{}' file_type failed: {err}", p.display()),
                )
            })?;
            if ft.is_symlink() {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("archive entry '{}' must not be a symlink", p.display()),
                ));
            }
            let Ok(rel_native) = p.strip_prefix(ws_path) else {
                continue;
            };
            let checked = checked_child(ws_path, rel_native).map_err(|err| {
                AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "archive entry '{}' failed confinement check: {err}",
                        rel_native.display()
                    ),
                )
            })?;
            let Some(rel_utf8) = rel_native.to_str() else {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "archive entry '{}' is not valid UTF-8",
                        rel_native.display()
                    ),
                ));
            };
            let rel_posix = rel_utf8.replace('\\', "/");
            if !col.is_direct_archive_file(&rel_posix) {
                continue;
            }
            out.push((rel_posix, checked));
        }
    }
    Ok(out)
}

fn is_admitted_git_collection_root(
    col: &SpecConfig,
    root_bytes: &[u8],
) -> Result<bool, AcddError> {
    if root_bytes.is_empty()
        || root_bytes
            .split(|&b| b == b'/')
            .any(|seg| seg.is_empty() || seg == b"." || seg == b".." || seg.contains(&b'\\'))
    {
        return Ok(false);
    }
    match std::str::from_utf8(root_bytes) {
        Ok(root_str) => Ok(col.is_direct_archive_file(&format!("{root_str}/archive/_probe.md"))),
        Err(_) => {
            let lossy_root = String::from_utf8_lossy(root_bytes);
            if col.is_direct_archive_file(&format!("{lossy_root}/archive/_probe.md")) {
                Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "committed Git archive directory '{lossy_root}/archive' is not valid UTF-8"
                    ),
                ))
            } else {
                Ok(false)
            }
        }
    }
}

fn list_collection_git_entries(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    disk_only: bool,
    head_oid: Option<&str>,
) -> Result<Option<Vec<GitTreeEntry>>, AcddError> {
    let in_git = !disk_only && (head_oid.is_some() || is_git_repo(ws_path));
    if !in_git {
        return Ok(None);
    }
    let tree_oid = match head_oid {
        Some(oid) => oid.to_owned(),
        None => resolve_git_tree_oid(ws_path, "HEAD")?,
    };
    let mut prefixes = Vec::new();
    for col in collections.values() {
        for pat in col.patterns() {
            let is_literal = !pat.contains(['*', '?', '{', '[']);
            let tree_prefix = if is_literal {
                pat.to_owned()
            } else {
                literal_git_prefix(pat)
            };
            if !prefixes.contains(&tree_prefix) {
                prefixes.push(tree_prefix);
            }
        }
    }
    let prefix_refs: Vec<&str> = prefixes.iter().map(String::as_str).collect();
    Ok(Some(list_git_tree_entries_multi(
        ws_path,
        &tree_oid,
        &prefix_refs,
    )?))
}

fn validate_and_collect_git_archive_entry(
    col_name: &str,
    col: &SpecConfig,
    entry: &GitTreeEntry,
    seen_paths: &mut BTreeSet<String>,
    git_entries: &mut Vec<(String, String, String)>,
) -> Result<(), AcddError> {
    let Some(slash_pos) = entry.raw_path.iter().rposition(|&b| b == b'/') else {
        return Ok(());
    };
    let (parent_bytes, file_bytes) = (
        &entry.raw_path[..slash_pos],
        &entry.raw_path[slash_pos + 1..],
    );
    if file_bytes == b"archive" {
        if is_admitted_git_collection_root(col, parent_bytes)? && entry.mode == "120000" {
            let raw_lossy = String::from_utf8_lossy(&entry.raw_path);
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("archive directory '{raw_lossy}' in Git tree must not be a symlink"),
            ));
        }
        return Ok(());
    }
    if !parent_bytes.ends_with(b"/archive") {
        return Ok(());
    }
    let root_bytes = &parent_bytes[..parent_bytes.len() - b"/archive".len()];
    if !is_admitted_git_collection_root(col, root_bytes)? {
        return Ok(());
    }
    let Ok(file_str) = std::str::from_utf8(file_bytes) else {
        if file_bytes.ends_with(b".md") || entry.mode == "120000" {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "committed Git archive entry '{}' is not valid UTF-8",
                    String::from_utf8_lossy(&entry.raw_path)
                ),
            ));
        }
        return Ok(());
    };
    let raw_str = std::str::from_utf8(&entry.raw_path).map_err(|_| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "committed Git archive entry '{}' is not valid UTF-8",
                String::from_utf8_lossy(&entry.raw_path)
            ),
        )
    })?;
    if entry.mode == "120000"
        && (file_str.trim_end().ends_with(".md")
            || (file_str.contains('\\') && file_str.ends_with(".md")))
    {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("archive entry '{raw_str}' in Git tree must not be a symlink"),
        ));
    }
    if file_str.contains('\\') && file_str.ends_with(".md") {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "archive entry '{raw_str}' in Git tree has invalid or unsupported path characters"
            ),
        ));
    }
    if col.is_direct_archive_file(raw_str) && seen_paths.insert(raw_str.to_owned()) {
        git_entries.push((col_name.to_owned(), raw_str.to_owned(), entry.oid.clone()));
    }
    Ok(())
}

/// Visit all archived specification Markdown documents across `collections` (Git `HEAD` or confined disk).
pub fn visit_archived_specs<T>(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    disk_only: bool,
    head_oid: Option<&str>,
    warnings: &mut Vec<String>,
    mut visitor: impl FnMut(&str, String, &str) -> Option<T>,
) -> Result<Option<T>, AcddError> {
    let all_git_entries = list_collection_git_entries(ws_path, collections, disk_only, head_oid)?;
    let mut seen_paths = BTreeSet::new();
    let mut git_entries = Vec::new();
    for (col_name, col) in collections {
        let disk_entries = probe_disk_archive_dirs(ws_path, col, warnings)?;
        if let Some(tree_entries) = all_git_entries.as_deref() {
            for entry in tree_entries {
                validate_and_collect_git_archive_entry(
                    col_name,
                    col,
                    entry,
                    &mut seen_paths,
                    &mut git_entries,
                )?;
            }
            continue;
        }

        for (rel_posix, _) in disk_entries {
            if !seen_paths.insert(rel_posix.clone()) {
                continue;
            }
            let raw = read_confined_utf8(ws_path, &rel_posix)?;
            if let Some(res) = visitor(col_name, rel_posix, &raw) {
                return Ok(Some(res));
            }
        }
    }
    if !git_entries.is_empty() {
        let oids: Vec<_> = git_entries.iter().map(|(_, _, oid)| oid.as_str()).collect();
        let blobs = read_git_blobs_utf8_batch(ws_path, &oids)
            .map_err(|err| AcddError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
        for ((col, path, oid), blob) in git_entries.into_iter().zip(blobs) {
            match blob {
                Ok(Some(raw)) => {
                    if let Some(res) = visitor(&col, path, &raw) {
                        return Ok(Some(res));
                    }
                }
                Ok(None) => warnings.push(format!(
                    "enumerated committed archive '{path}' blob '{oid}' disappeared from HEAD"
                )),
                Err(err) => warnings.push(format!("{path}: failed reading git blob: {err}")),
            }
        }
    }
    Ok(None)
}

/// Collect all archived spec documents across `collections`, ignoring unparseable legacy frontmatter in archive files.
pub fn collect_archived_specs(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    disk_only: bool,
    head_oid: Option<&str>,
) -> Result<CollectedArchiveScan, AcddError> {
    let mut out = Vec::new();
    let mut io_warnings = Vec::new();
    visit_archived_specs(
        ws_path,
        collections,
        disk_only,
        head_oid,
        &mut io_warnings,
        |col_name, rel_posix, raw| {
            if let Ok(doc) = parse_spec(raw) {
                out.push((col_name.to_owned(), rel_posix, doc));
            }
            None::<()>
        },
    )?;
    Ok((out, io_warnings))
}

/// Scan all active non-archive spec documents across `collections`.
pub fn collect_workspace_specs(
    root: &Path,
    collections: &BTreeMap<String, SpecConfig>,
) -> WorkspaceSpecScan {
    collect_workspace_specs_cached(root, collections, &mut BTreeMap::new())
}

/// Scan all active non-archive spec documents across `collections`, reusing `parsed` memoized by content SHA-256.
pub fn collect_workspace_specs_cached(
    root: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, AcddError>>,
) -> WorkspaceSpecScan {
    fn walk(
        root: &Path,
        dir: &Path,
        col: &SpecConfig,
        found: &mut BTreeMap<String, CollectedWorkspaceSpec>,
        errors: &mut Vec<WorkspaceSpecScanError>,
        parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, AcddError>>,
    ) {
        let entries_iter = match fs::read_dir(dir) {
            Ok(iter) => iter,
            Err(err) => {
                let rel_str = dir
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| dir.to_string_lossy().replace('\\', "/"));
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::SpecSchemaInvalid,
                    diag_code: "VALIDATION_IO",
                    path: rel_str,
                    message: format!("unable to read spec directory: {err}"),
                });
                return;
            }
        };
        let mut entries = Vec::new();
        for item in entries_iter {
            match item {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    let rel_str = dir
                        .strip_prefix(root)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| dir.to_string_lossy().replace('\\', "/"));
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::SpecSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to read spec directory entry: {err}"),
                    });
                }
            }
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel_str = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(err) => {
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::SpecSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to inspect spec collection entry: {err}"),
                    });
                    continue;
                }
            };
            if ft.is_symlink() {
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::SpecSchemaInvalid,
                    diag_code: ErrorCode::SpecSchemaInvalid.as_str(),
                    path: rel_str.clone(),
                    message: format!("spec collection entry '{rel_str}' must not be a symlink"),
                });
                continue;
            }
            if ft.is_dir() {
                if entry.file_name() != "archive" {
                    walk(root, &path, col, found, errors, parsed);
                }
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !ft.is_file()
                || !name.ends_with(".md")
                || matches!(name.as_str(), "README.md" | "AGENTS.md" | "CLAUDE.md")
            {
                continue;
            }
            match read_confined_utf8(root, &rel_str) {
                Ok(content) => match parsed
                    .entry(sha256_bytes(content.as_bytes()))
                    .or_insert_with(|| parse_spec(&content))
                    .clone()
                {
                    Ok(doc) => {
                        found.insert(
                            rel_str.clone(),
                            CollectedWorkspaceSpec {
                                rel_path: rel_str,
                                collection: Some(col.clone()),
                                doc,
                            },
                        );
                    }
                    Err(err) => {
                        errors.push(WorkspaceSpecScanError {
                            code: err.code,
                            diag_code: err.code.as_str(),
                            path: rel_str,
                            message: err.to_string(),
                        });
                    }
                },
                Err(err) => {
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::SpecSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to read spec manifest: {err}"),
                    });
                }
            }
        }
    }

    let mut found = BTreeMap::new();
    let mut errors = Vec::new();
    for col in collections.values() {
        let dirs = match col.resolve_dirs(root) {
            Ok(d) => d,
            Err(err) => {
                let label = col.patterns().join(", ");
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::SpecSchemaInvalid,
                    diag_code: ErrorCode::SpecSchemaInvalid.as_str(),
                    path: label.clone(),
                    message: format!("spec collection '{label}' failed confinement check: {err}"),
                });
                continue;
            }
        };
        for dir in dirs {
            walk(root, &dir, col, &mut found, &mut errors, parsed);
        }
    }
    WorkspaceSpecScan {
        specs: found.into_values().collect(),
        errors,
    }
}
