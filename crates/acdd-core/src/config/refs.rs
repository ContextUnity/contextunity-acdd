//! Shared confined `<name> | <path>[:<sha256_prefix>]` reference resolution and directory YAML collection.

use crate::errors::{AcddError, ErrorCode};
use crate::format::sha256_hex;
use crate::fs::{checked_child, is_normalized_relative, open_confined, safe_relative};
use crate::ids::is_valid_short_hex_prefix;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

/// Confined file reference resolved and read from workspace directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfinedRef {
    /// Normalized workspace-relative POSIX path to the resolved file.
    pub rel_path: String,
    /// Raw UTF-8/byte contents read via `O_NOFOLLOW` confined open.
    pub bytes: Vec<u8>,
}

/// Validate a configured search-directory list (`specs_dirs`, `workflows_dirs`, `lint_dirs`).
pub fn validate_confined_search_dirs(
    dirs: &[String],
    key_name: &str,
    error_code: ErrorCode,
) -> Result<(), AcddError> {
    for dir in dirs {
        let trimmed = dir.trim();
        if trimmed.is_empty() || trimmed.contains('\\') || !safe_relative(Path::new(trimmed)) {
            return Err(AcddError::new(
                error_code,
                format!("{key_name} entry '{dir}' must be a relative plain path"),
            ));
        }
    }
    Ok(())
}

/// Parse `<name> | <path>[:<sha256_prefix>]`, rejecting traversal and malformed SHA-256 pins.
pub fn parse_confined_ref(
    raw: &str,
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<(String, Option<String>), AcddError> {
    let (path_part, pin_part) = match raw.rsplit_once(':') {
        Some((p, pin)) if !p.is_empty() && !pin.is_empty() => (p, Some(pin)),
        Some((_, _)) => {
            return Err(AcddError::new(
                error_code,
                format!("malformed {kind_label} reference '{raw}'"),
            ));
        }
        None => (raw, None),
    };
    if path_part.contains('\\') || !is_normalized_relative(Path::new(path_part)) {
        return Err(AcddError::new(
            error_code,
            format!(
                "{kind_label} path '{path_part}' must be a normalized workspace-relative path"
            ),
        ));
    }
    let normalized_pin = match pin_part {
        Some(pin) => {
            if !is_valid_short_hex_prefix(pin) {
                return Err(AcddError::new(
                    error_code,
                    format!(
                        "{kind_label} SHA-256 prefix '{pin}' must be 7..=64 hexadecimal characters"
                    ),
                ));
            }
            Some(pin.to_ascii_lowercase())
        }
        None => None,
    };
    Ok((path_part.to_owned(), normalized_pin))
}

/// Resolve a normalized `<name> | <path>` against workspace root and `search_dirs` without symlink escapes.
pub fn resolve_confined_ref_path(
    root: &Path,
    raw_rel: &str,
    search_dirs: &[String],
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<String, AcddError> {
    if raw_rel.contains('\\') || !is_normalized_relative(Path::new(raw_rel)) {
        return Err(AcddError::new(
            error_code,
            format!("{kind_label} path '{raw_rel}' must be a normalized workspace-relative path"),
        ));
    }
    match fs::symlink_metadata(root.join(raw_rel)) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(AcddError::new(
                error_code,
                format!("{kind_label} path '{raw_rel}' must not be a symlink"),
            ));
        }
        Ok(meta) if !meta.is_dir() => {
            checked_child(root, Path::new(raw_rel)).map_err(|err| {
                AcddError::new(
                    error_code,
                    format!("{kind_label} path '{raw_rel}' failed confinement check: {err}"),
                )
            })?;
            return Ok(raw_rel.to_owned());
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(AcddError::new(
                error_code,
                format!("failed to inspect {kind_label} path '{raw_rel}': {err}"),
            ));
        }
    }

    for dir in search_dirs {
        let dir_clean = dir
            .trim()
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();
        if dir_clean.is_empty() {
            continue;
        }
        checked_child(root, Path::new(&dir_clean)).map_err(|err| {
            AcddError::new(
                error_code,
                format!("{kind_label} search dir '{dir_clean}' failed confinement check: {err}"),
            )
        })?;
        let mut candidates = Vec::new();
        if raw_rel.ends_with(".yaml") || raw_rel.ends_with(".yml") {
            candidates.push(format!("{dir_clean}/{raw_rel}"));
        } else {
            candidates.push(format!("{dir_clean}/{raw_rel}.yaml"));
            candidates.push(format!("{dir_clean}/{raw_rel}.yml"));
            candidates.push(format!("{dir_clean}/{raw_rel}"));
        }
        for cand in candidates {
            if !is_normalized_relative(Path::new(&cand)) {
                continue;
            }
            match fs::symlink_metadata(root.join(&cand)) {
                Ok(_) => {
                    let checked = checked_child(root, Path::new(&cand)).map_err(|err| {
                        AcddError::new(
                            error_code,
                            format!("{kind_label} path '{cand}' failed confinement check: {err}"),
                        )
                    })?;
                    if checked.is_file() {
                        return Ok(cand);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(AcddError::new(
                        error_code,
                        format!("failed to inspect {kind_label} path '{cand}': {err}"),
                    ));
                }
            }
        }
    }
    Ok(raw_rel.to_owned())
}

/// Read raw bytes from a confined workspace-relative path using `open_confined` (`O_NOFOLLOW`).
pub fn read_confined_ref_bytes(
    root: &Path,
    rel_path: &str,
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<Vec<u8>, AcddError> {
    let mut file = open_confined(root, Path::new(rel_path), OpenOptions::new().read(true))
        .map_err(|err| {
            AcddError::new(
                error_code,
                format!("failed to open {kind_label} '{rel_path}': {err}"),
            )
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|err| {
        AcddError::new(
            error_code,
            format!("failed to read {kind_label} '{rel_path}': {err}"),
        )
    })?;
    Ok(bytes)
}

/// Verify that `raw_bytes`'s lowercase SHA-256 hex digest starts with `expected_prefix_lower`.
pub fn verify_confined_sha256_prefix(
    raw_bytes: &[u8],
    expected_prefix_lower: &str,
    rel_path: &str,
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<(), AcddError> {
    let actual = sha256_hex(raw_bytes);
    if !actual.starts_with(expected_prefix_lower) {
        return Err(AcddError::new(
            error_code,
            format!(
                "{kind_label} SHA-256 pin mismatch for '{rel_path}': expected prefix '{expected_prefix_lower}', got '{actual}'"
            ),
        ));
    }
    Ok(())
}

/// Parse, resolve, read, and verify optional SHA-256 pin for a confined `<name> | <path>[:<sha256_prefix>]` reference.
pub fn resolve_and_read_confined_ref(
    root: &Path,
    raw_ref: &str,
    search_dirs: &[String],
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<ResolvedConfinedRef, AcddError> {
    let (raw_path, expected_pin) = parse_confined_ref(raw_ref, error_code, kind_label)?;
    let rel_path =
        resolve_confined_ref_path(root, &raw_path, search_dirs, error_code, kind_label)?;
    let bytes = read_confined_ref_bytes(root, &rel_path, error_code, kind_label)?;
    if let Some(pin) = expected_pin {
        verify_confined_sha256_prefix(&bytes, &pin, &rel_path, error_code, kind_label)?;
    }
    Ok(ResolvedConfinedRef { rel_path, bytes })
}

/// Collect and read all `.yaml`/`.yml` files across confined `dirs` (or pinned file refs) in deterministic lexicographical order.
pub fn collect_confined_yaml_files(
    root: &Path,
    entries: Option<&[String]>,
    default_search_dirs: &[String],
    error_code: ErrorCode,
    kind_label: &str,
) -> Result<Vec<ResolvedConfinedRef>, AcddError> {
    let mut by_rel_path: BTreeSet<String> = BTreeSet::new();
    let mut explicit_pinned: Vec<ResolvedConfinedRef> = Vec::new();

    let strict_missing = entries.is_some();
    let scan_entries = entries.unwrap_or(default_search_dirs);
    for entry in scan_entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            return Err(AcddError::new(
                error_code,
                format!("{kind_label} directory entry must not be empty"),
            ));
        }
        if trimmed.contains(':') {
            let resolved = resolve_and_read_confined_ref(
                root,
                trimmed,
                default_search_dirs,
                error_code,
                kind_label,
            )?;
            if by_rel_path.insert(resolved.rel_path.clone()) {
                explicit_pinned.push(resolved);
            }
            continue;
        }
        let norm = trimmed
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();
        if norm.is_empty() || trimmed.contains('\\') || !is_normalized_relative(Path::new(&norm)) {
            return Err(AcddError::new(
                error_code,
                format!(
                    "{kind_label} path '{entry}' must be a normalized workspace-relative path"
                ),
            ));
        }
        let abs = match checked_child(root, Path::new(&norm)) {
            Ok(abs) => abs,
            Err(err)
                if !strict_missing
                    && matches!(
                        err.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
            {
                continue;
            }
            Err(err) => {
                return Err(AcddError::new(
                    error_code,
                    format!("{kind_label} path '{norm}' failed confinement check: {err}"),
                ));
            }
        };
        match fs::symlink_metadata(&abs) {
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                if strict_missing {
                    return Err(AcddError::new(
                        error_code,
                        format!("{kind_label} path '{norm}' does not exist"),
                    ));
                }
                continue;
            }
            Err(err) => {
                return Err(AcddError::new(
                    error_code,
                    format!("failed to inspect {kind_label} path '{norm}': {err}"),
                ));
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(AcddError::new(
                    error_code,
                    format!("{kind_label} path '{norm}' must not be a symlink"),
                ));
            }
            Ok(meta) if meta.is_file() => {
                by_rel_path.insert(norm);
            }
            Ok(meta) if meta.is_dir() => {
                let rd = fs::read_dir(&abs).map_err(|err| {
                    AcddError::new(
                        error_code,
                        format!("failed to read {kind_label} directory '{norm}': {err}"),
                    )
                })?;
                for dir_entry in rd {
                    let dir_entry = dir_entry.map_err(|err| {
                        AcddError::new(
                            error_code,
                            format!("failed to enumerate {kind_label} directory '{norm}': {err}"),
                        )
                    })?;
                    let ft = dir_entry.file_type().map_err(|err| {
                        AcddError::new(
                            error_code,
                            format!("failed to inspect entry in '{norm}': {err}"),
                        )
                    })?;
                    if ft.is_symlink() {
                        return Err(AcddError::new(
                            error_code,
                            format!(
                                "{kind_label} file '{norm}/{}' must not be a symlink",
                                dir_entry.file_name().to_string_lossy()
                            ),
                        ));
                    }
                    if !ft.is_file() {
                        continue;
                    }
                    let Some(file_name) = dir_entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if file_name.ends_with(".yaml") || file_name.ends_with(".yml") {
                        by_rel_path.insert(format!("{norm}/{file_name}"));
                    }
                }
            }
            Ok(_) => {}
        }
    }

    let mut out = Vec::with_capacity(by_rel_path.len());
    for rel_path in by_rel_path {
        if let Some(pos) = explicit_pinned.iter().position(|r| r.rel_path == rel_path) {
            out.push(explicit_pinned.swap_remove(pos));
        } else {
            let bytes = read_confined_ref_bytes(root, &rel_path, error_code, kind_label)?;
            out.push(ResolvedConfinedRef { rel_path, bytes });
        }
    }
    Ok(out)
}

impl super::schema::SpecConfig {
    /// Expand all configured patterns into deterministic, sorted existing directories confined to `root`.
    ///
    /// Non-existent literal directories are omitted gracefully; symlinks along literal paths
    /// or escaping `root` fail closed, and wildcard (`*` / `**`) expansion never follows symlinks.
    pub fn resolve_dirs(&self, root: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        let mut dirs = BTreeSet::new();
        for pat in self.patterns() {
            for variant in crate::path_policy::expand_glob_braces(pat) {
                let norm = super::schema::normalize_spec_collection_path(&variant).map_err(|err| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
                })?;
                let segs: Vec<&str> = norm.split('/').collect();
                expand_spec_dir_segments(root, Path::new(""), &segs, &mut dirs)?;
            }
        }
        Ok(dirs.into_iter().collect())
    }
}

fn expand_spec_dir_segments(
    root: &Path,
    current_rel: &Path,
    segs: &[&str],
    out: &mut BTreeSet<std::path::PathBuf>,
) -> std::io::Result<()> {
    if segs.is_empty() {
        if current_rel.as_os_str().is_empty()
            || current_rel.components().any(|c| c.as_os_str() == "archive")
        {
            return Ok(());
        }
        let candidate = checked_child(root, current_rel)?;
        match fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "spec collection '{}' must not be a symlink",
                        current_rel.to_string_lossy()
                    ),
                ));
            }
            Ok(meta) if meta.is_dir() => {
                out.insert(candidate);
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        return Ok(());
    }

    let (first, rest) = (segs[0], &segs[1..]);
    if first == "**" || first.contains('*') || first.contains('?') {
        if first == "**" {
            expand_spec_dir_segments(root, current_rel, rest, out)?;
        }
        let base = if current_rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            checked_child(root, current_rel)?
        };
        let entries = match fs::read_dir(&base) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let next_segs = if first == "**" { segs } else { rest };
        for entry in entries {
            let entry = entry?;
            if entry.file_name() == "archive" {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            if first == "**" {
                expand_spec_dir_segments(
                    root,
                    &current_rel.join(entry.file_name()),
                    next_segs,
                    out,
                )?;
            } else {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if crate::path_policy::glob_matches(first, &name) {
                    expand_spec_dir_segments(root, &current_rel.join(name), next_segs, out)?;
                }
            }
        }
    } else {
        expand_spec_dir_segments(root, &current_rel.join(first), rest, out)?;
    }
    Ok(())
}
