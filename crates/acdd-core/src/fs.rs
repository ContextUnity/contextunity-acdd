use anyhow::{bail, Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NONCE: AtomicU64 = AtomicU64::new(0);

fn path_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

/// Resolve a workspace root to a canonical directory.
pub fn canonical_root(path: &Path) -> std::io::Result<PathBuf> {
    let root = path.canonicalize()?;
    if !root.is_dir() {
        return Err(path_error("workspace root must be a directory"));
    }
    Ok(root)
}

/// Lexically normalize a path without requiring it to exist on disk.
/// Resolves `.` and `..` components.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                } else if !matches!(components.last(), Some(Component::RootDir)) {
                    components.push(component);
                }
            }
            c => components.push(c),
        }
    }
    components.into_iter().collect()
}

/// Resolve a workspace-relative path, using its canonical location when present.
pub fn resolve_workspace_path(root: &Path, path: &Path) -> PathBuf {
    let root = root.canonicalize().unwrap_or_else(|_| normalize_path(root));
    let joined = normalize_path(&root.join(path));
    joined.canonicalize().unwrap_or(joined)
}

/// Return whether a path consists only of normal relative components.
pub fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|part| {
            matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

/// Return whether a path consists only of normal relative components (no `.`, `..`, root, or prefix).
pub fn is_normalized_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Return whether a path represents a documentation file.
pub fn is_doc_path(p: &str) -> bool {
    p.starts_with("docs/")
        || p.starts_with("docs\\")
        || p.ends_with(".md")
        || p.ends_with(".markdown")
        || p.ends_with(".txt")
        || p.ends_with(".rst")
        || p.ends_with(".adoc")
}

/// Resolve a relative path without traversing a symlink component.
pub fn checked_child(root: &Path, relative: &Path) -> std::io::Result<PathBuf> {
    if !safe_relative(relative) {
        return Err(path_error("path must be a relative plain path"));
    }
    let target = root.join(relative);
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let name = match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => name,
            _ => return Err(path_error("path must be relative")),
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(path_error("path must not traverse a symlink"));
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                // Remaining nonexistent components cannot contain symlinks; recorded deletions retain admission.
                return Ok(target);
            }
            Err(source) => return Err(source),
        }
    }
    if target.exists() {
        let canonical_root = root.canonicalize()?;
        if !target.canonicalize()?.starts_with(&canonical_root) {
            return Err(path_error("path escapes workspace root"));
        }
    }
    Ok(target)
}

/// Returns `true` when `path` is a macOS OS-level root symlink (`/var`, `/tmp`, `/etc`
/// aliasing `/private/...`) that system temp and workspace paths traverse legitimately.
pub fn is_system_root_symlink(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(path.to_str(), Some("/var" | "/tmp" | "/etc"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// Opens `relative` confined within `root` without traversing any symlink component,
/// enforcing `O_NOFOLLOW` on Unix (`ADR-0002`).
pub fn open_confined(
    root: &Path,
    relative: &Path,
    options: &mut OpenOptions,
) -> std::io::Result<fs::File> {
    let target = checked_child(root, relative)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(target)
}

/// Atomically stage and publish a new file confined within `root` without overwriting
/// any existing target (`AlreadyExists` if the target already exists).
pub fn write_new_confined(root: &Path, relative: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = checked_child(root, relative)?;
    let parent = target
        .parent()
        .ok_or_else(|| path_error("missing parent"))?;
    ensure_confined_dir_all_abs(parent).map_err(std::io::Error::other)?;
    let temporary = parent.join(format!(
        ".acdd-new-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    commit_atomic_temp(&target, &temporary, bytes, Some(false), true, false)
        .map_err(|err| err.downcast::<std::io::Error>().unwrap_or_else(std::io::Error::other))
}

/// Validates that `path` and its existing ancestor directories do not traverse a
/// non-system-root symlink, returning `path` with its parent canonicalized so
/// `SQLITE_OPEN_NOFOLLOW` can enforce `O_NOFOLLOW` on the final file even under
/// macOS `/var` -> `/private/var` or `/tmp` -> `/private/tmp` paths.
pub fn confined_database_open_path(path: &Path) -> Result<PathBuf> {
    confined_sqlite_open_path(path, false)
}

/// Variant of `confined_database_open_path` for opening or creating a SQLite database
/// (`SQLITE_OPEN_CREATE | SQLITE_OPEN_NOFOLLOW`) when the target file may not exist yet.
pub fn confined_database_create_path(path: &Path) -> Result<PathBuf> {
    confined_sqlite_open_path(path, true)
}

fn reject_parent_dir_components(path: &Path) -> std::io::Result<()> {
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(path_error(format!(
            "path must not contain parent traversal: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Prepares a symlink-confined path for `SQLITE_OPEN_NOFOLLOW`, rejecting symlink
/// targets and symlinked ancestor directories (`ADR-0002`).
pub fn confined_sqlite_open_path(path: &Path, allow_missing: bool) -> Result<PathBuf> {
    reject_parent_dir_components(path)?;
    let absolute = if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&std::env::current_dir()?.join(path))
    };
    check_ancestors_no_symlinks_abs(&absolute)?;
    match fs::symlink_metadata(&absolute) {
        Ok(meta) if meta.file_type().is_symlink() => bail!("database must not be a symlink"),
        Ok(_) => {}
        Err(err) if allow_missing && err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let parent = absolute.parent().context("database path needs a parent")?;
    let file_name = absolute
        .file_name()
        .context("database path needs a file name")?;
    let resolved_parent = if parent.exists() {
        parent.canonicalize()?
    } else {
        resolve_path_with_existing_prefix(parent)
    };
    Ok(resolved_parent.join(file_name))
}

fn ensure_target_not_symlink(absolute: &Path) -> Result<()> {
    match fs::symlink_metadata(absolute) {
        Ok(meta) if meta.file_type().is_symlink() => bail!("output must not be a symlink"),
        Ok(meta) if meta.is_dir() => bail!("output must not be a directory"),
        Ok(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut probe = OpenOptions::new();
                probe.read(true).custom_flags(libc::O_NOFOLLOW);
                if let Err(err) = probe.open(absolute) {
                    if err.raw_os_error() == Some(libc::ELOOP) {
                        bail!("output must not be a symlink");
                    }
                }
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Performs atomic write.
pub fn atomic_write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    reject_parent_dir_components(path)?;
    let absolute = if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&std::env::current_dir()?.join(path))
    };
    let parent = absolute.parent().context("missing parent")?;
    ensure_confined_dir_all_abs(parent)?;
    ensure_target_not_symlink(&absolute)?;
    let temporary = parent.join(format!(
        ".acdd-write-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    commit_atomic_temp(&absolute, &temporary, bytes, Some(executable), true, true)
}

/// Atomically replace a file with the supplied contents.
pub fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    reject_parent_dir_components(path)?;
    let absolute = if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&std::env::current_dir()?.join(path))
    };
    check_ancestors_no_symlinks_abs(&absolute)?;
    ensure_target_not_symlink(&absolute)?;
    let name = absolute
        .file_name()
        .context("milestone filename missing")?
        .to_string_lossy();
    let temporary = absolute.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    commit_atomic_temp(&absolute, &temporary, contents, None, true, true)
}

fn commit_atomic_temp(
    path: &Path,
    temporary: &Path,
    bytes: &[u8],
    executable: Option<bool>,
    sync_parent: bool,
    replace_existing: bool,
) -> Result<()> {
    let result = (|| {
        #[cfg(unix)]
        let existing_permissions = if executable.is_none() {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    bail!("output must not be a symlink");
                }
                Ok(meta) => Some(meta.permissions()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(err.into()),
            }
        } else {
            None
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(if executable == Some(true) { 0o755 } else { 0o600 })
                .custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(temporary)?;
        file.write_all(bytes)?;
        #[cfg(unix)]
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        ensure_target_not_symlink(path)?;
        let parent_file = if sync_parent {
            let parent = path.parent().context("missing parent")?;
            let mut parent_opts = OpenOptions::new();
            parent_opts.read(true);
            #[cfg(unix)]
            if !is_system_root_symlink(parent) {
                use std::os::unix::fs::OpenOptionsExt;
                parent_opts.custom_flags(libc::O_NOFOLLOW);
            }
            Some(parent_opts.open(parent)?)
        } else {
            None
        };
        if replace_existing {
            fs::rename(temporary, path)?;
            if let Some(pf) = parent_file {
                pf.sync_all()?;
            }
        } else {
            fs::hard_link(temporary, path)?;
            if let Some(pf) = parent_file {
                if let Err(err) = pf.sync_all() {
                    if same_file_identity(path, temporary) {
                        let _ = fs::remove_file(path);
                    }
                    return Err(err.into());
                }
            }
            let _ = fs::remove_file(temporary);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn same_file_identity(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Resolve a path by canonicalizing its deepest existing ancestor and appending
/// the remaining normalized components.
pub fn resolve_path_with_existing_prefix(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_owned())
    };
    let normalized = normalize_path(&absolute);
    canonicalize_with_missing_tail(&normalized).unwrap_or(normalized)
}

/// Normalize macOS `/var` -> `/private/var` (and `/tmp` -> `/private/tmp`, `/etc` -> `/private/etc`) alias prefixes.
pub fn canonicalize_macos_var(path: &Path) -> PathBuf {
    let resolved = resolve_path_with_existing_prefix(path);
    #[cfg(target_os = "macos")]
    {
        if let Some(raw) = resolved.to_str() {
            for prefix in ["/var", "/tmp", "/etc"] {
                if raw == prefix {
                    return PathBuf::from(format!("/private{prefix}"));
                }
                if let Some(rest) = raw.strip_prefix(&format!("{prefix}/")) {
                    return PathBuf::from(format!("/private{prefix}/{rest}"));
                }
            }
        }
    }
    resolved
}

fn resolve_confined_path(root: &Path, target: &Path) -> std::io::Result<PathBuf> {
    if target.as_os_str().is_empty() {
        return Err(path_error("path must not be empty"));
    }
    reject_parent_dir_components(target)?;
    let abs_root = if root.is_absolute() {
        normalize_path(root)
    } else {
        normalize_path(&std::env::current_dir()?.join(root))
    };
    if target.is_relative() {
        return Ok(normalize_path(&abs_root.join(target)));
    }
    let norm_target = normalize_path(target);
    if norm_target.starts_with(&abs_root)
        || canonicalize_macos_var(&norm_target).starts_with(canonicalize_macos_var(&abs_root))
    {
        Ok(norm_target)
    } else {
        Err(path_error(format!(
            "path '{}' escapes confined root '{}'",
            target.display(),
            root.display()
        )))
    }
}

pub(crate) fn check_ancestors_no_symlinks_abs(abs_target: &Path) -> std::io::Result<()> {
    reject_parent_dir_components(abs_target)?;
    let normalized = normalize_path(abs_target);
    let parent = normalized
        .parent()
        .ok_or_else(|| path_error("path requires a parent directory"))?;
    let mut ancestor = PathBuf::new();
    for part in parent.components() {
        ancestor.push(part);
        match fs::symlink_metadata(&ancestor) {
            Ok(meta) if meta.file_type().is_symlink() && !is_system_root_symlink(&ancestor) => {
                return Err(path_error(format!(
                    "path parent must not traverse a symlink: {}",
                    ancestor.display()
                )));
            }
            Ok(meta) if !meta.is_dir() && !is_system_root_symlink(&ancestor) => {
                return Err(path_error(format!(
                    "expected directory at '{}'",
                    ancestor.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

pub(crate) fn ensure_confined_dir_all_abs(abs_dir: &Path) -> std::io::Result<()> {
    reject_parent_dir_components(abs_dir)?;
    let normalized = normalize_path(abs_dir);
    let mut current = PathBuf::new();
    for component in normalized.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() && !is_system_root_symlink(&current) => {
                return Err(path_error(format!(
                    "directory must not traverse a symlink: {}",
                    current.display()
                )));
            }
            Ok(meta) if !meta.is_dir() && !is_system_root_symlink(&current) => {
                return Err(path_error(format!(
                    "expected directory at '{}'",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if let Err(create_err) = fs::create_dir(&current) {
                    if create_err.kind() == std::io::ErrorKind::AlreadyExists {
                        let meta = fs::symlink_metadata(&current)?;
                        if meta.file_type().is_symlink() && !is_system_root_symlink(&current) {
                            return Err(path_error(format!(
                                "directory must not traverse a symlink: {}",
                                current.display()
                            )));
                        }
                        if !meta.is_dir() {
                            return Err(path_error(format!(
                                "expected directory at '{}'",
                                current.display()
                            )));
                        }
                    } else {
                        return Err(create_err);
                    }
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Verifies that `target` is confined within `root` and that none of its existing ancestor
/// directories traverse a non-system-root symlink (`ADR-0002`).
pub fn check_ancestors_no_symlinks(root: &Path, target: &Path) -> std::io::Result<()> {
    let abs_target = resolve_confined_path(root, target)?;
    check_ancestors_no_symlinks_abs(&abs_target)
}

/// Verifies that `root` and its `<root>/.git` entry do not traverse or resolve through a
/// non-system-root symlink, while still permitting a regular-file `.git` pointer in linked
/// worktrees or an absent `.git` entry (`ADR-0002`).
pub fn check_git_entry_no_symlinks(root: &Path) -> std::io::Result<()> {
    let abs_git = resolve_confined_path(root, Path::new(".git"))?;
    check_ancestors_no_symlinks_abs(&abs_git)?;
    match fs::symlink_metadata(&abs_git) {
        Ok(meta) if meta.file_type().is_symlink() => Err(path_error(format!(
            "git entry must not be a symlink: {}",
            abs_git.display()
        ))),
        Ok(meta) if meta.is_dir() || meta.is_file() => Ok(()),
        Ok(_) => Err(path_error(format!(
            "unexpected git entry at '{}'",
            abs_git.display()
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Creates `dir` (and missing parent directories) confined within `root`, rejecting any
/// non-system-root symlink or non-directory ancestor (`ADR-0002`).
pub fn ensure_confined_dir_all(root: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let abs_dir = resolve_confined_path(root, dir)?;
    ensure_confined_dir_all_abs(&abs_dir)?;
    Ok(abs_dir)
}

/// Canonicalize the deepest resolvable ancestor and retain the unresolved tail.
/// Returns `None` when no ancestor can be resolved.
pub fn canonicalize_with_missing_tail(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Some(canonical);
    }
    let mut tail = Vec::new();
    let mut cursor = path;
    while let (Some(parent), Some(name)) = (cursor.parent(), cursor.file_name()) {
        tail.push(name.to_os_string());
        cursor = parent;
        if let Ok(mut resolved) = fs::canonicalize(cursor) {
            for part in tail.into_iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
    }
    None
}

#[cfg(unix)]
fn flip_first_ascii_alpha(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut flipped = false;
    for ch in name.chars() {
        if !flipped && ch.is_ascii_alphabetic() {
            out.push(if ch.is_ascii_lowercase() {
                ch.to_ascii_uppercase()
            } else {
                ch.to_ascii_lowercase()
            });
            flipped = true;
        } else {
            out.push(ch);
        }
    }
    flipped.then_some(out)
}

/// Returns `true` when the volume backing `path` (or its deepest existing ancestor)
/// resolves ASCII case variants to the same directory entry without writing to disk.
pub fn is_case_insensitive_volume(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let _ = path;
        true
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Some(existing) = path.ancestors().find(|p| fs::symlink_metadata(p).is_ok()) else {
            return false;
        };
        let Ok(base_meta) = fs::symlink_metadata(existing) else {
            return false;
        };
        let target_dev = base_meta.dev();

        let same_inode_under_flip =
            |entry_path: &Path, entry_meta: &fs::Metadata| -> Option<bool> {
                let parent = entry_path.parent()?;
                let name = entry_path.file_name()?.to_str()?;
                let flipped = flip_first_ascii_alpha(name)?;
                let alt = parent.join(flipped);
                match fs::symlink_metadata(&alt) {
                    Ok(alt_meta) => {
                        Some(alt_meta.dev() == target_dev && alt_meta.ino() == entry_meta.ino())
                    }
                    Err(_) => Some(false),
                }
            };

        for ancestor in existing.ancestors() {
            let Ok(meta) = fs::symlink_metadata(ancestor) else {
                break;
            };
            if meta.dev() != target_dev {
                break;
            }
            if let Some(matched) = same_inode_under_flip(ancestor, &meta) {
                return matched;
            }
        }
        if existing.is_dir() {
            if let Ok(entries) = fs::read_dir(existing) {
                for entry in entries.flatten().take(16) {
                    let Ok(meta) = entry.metadata() else {
                        continue;
                    };
                    if meta.dev() != target_dev {
                        continue;
                    }
                    if let Some(matched) = same_inode_under_flip(&entry.path(), &meta) {
                        return matched;
                    }
                }
            }
        }
        false
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

/// Compares two paths by filesystem identity when existing, and by canonicalized
/// existing-ancestor resolution (respecting case-insensitive volumes) when absent.
pub fn same_filesystem_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    #[cfg(unix)]
    if let (Ok(left_meta), Ok(right_meta)) =
        (fs::symlink_metadata(left), fs::symlink_metadata(right))
    {
        use std::os::unix::fs::MetadataExt;
        if left_meta.dev() == right_meta.dev() && left_meta.ino() == right_meta.ino() {
            return true;
        }
    }
    let resolved_left = resolve_path_with_existing_prefix(left);
    let resolved_right = resolve_path_with_existing_prefix(right);
    if resolved_left == resolved_right {
        return true;
    }
    if is_case_insensitive_volume(&resolved_left) || is_case_insensitive_volume(&resolved_right) {
        let mut left_parts = resolved_left.components();
        let mut right_parts = resolved_right.components();
        loop {
            match (left_parts.next(), right_parts.next()) {
                (None, None) => return true,
                (Some(l), Some(r)) if l == r => continue,
                (Some(std::path::Component::Normal(l)), Some(std::path::Component::Normal(r))) => {
                    let (Some(ls), Some(rs)) = (l.to_str(), r.to_str()) else {
                        return false;
                    };
                    if ls.to_lowercase() != rs.to_lowercase() {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
    false
}

/// Returns the canonical workspace temporary root directory (`std::env::temp_dir()`).
pub fn temp_dir_root() -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// RAII temporary directory created under `std::env::temp_dir()` with mode `0o700` on Unix.
pub struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    /// Creates a unique private temporary directory prefixed with `prefix`.
    pub fn create(prefix: &str) -> std::io::Result<Self> {
        if prefix.is_empty()
            || prefix.contains('/')
            || prefix.contains('\\')
            || prefix.contains("..")
        {
            return Err(path_error("invalid temporary directory prefix"));
        }
        let base = temp_dir_root();
        for _ in 0..128 {
            let seq = NONCE.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("{prefix}-{}-{seq}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to allocate unique scoped temporary directory",
        ))
    }

    /// Returns the path of this temporary directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates a new regular file inside this directory with `0o600` permissions on Unix.
    pub fn write_private_file(&self, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let rel = Path::new(name);
        if !is_normalized_relative(rel) || rel.components().count() != 1 {
            return Err(path_error("temporary filename must be a single path component"));
        }
        let file_path = self.path.join(rel);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&file_path)?;
        file.write_all(bytes)?;
        Ok(file_path)
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_normalized_relative() {
        assert!(is_normalized_relative(Path::new("src/lib.rs")));
        assert!(is_normalized_relative(Path::new("tests/acdd/tasks.rs")));
        assert!(is_normalized_relative(Path::new("src/space # λ.rs")));
        assert!(!is_normalized_relative(Path::new("")));
        assert!(!is_normalized_relative(Path::new("/src/lib.rs")));
        assert!(!is_normalized_relative(Path::new("../src/lib.rs")));
        assert!(!is_normalized_relative(Path::new("./src/lib.rs")));
        assert!(!is_normalized_relative(Path::new("foo/../bar.rs")));
    }

    #[test]
    fn test_is_system_root_symlink_only_exempts_macos_os_roots() {
        assert!(!is_system_root_symlink(Path::new("/")));
        assert!(!is_system_root_symlink(Path::new("/var/folders")));
        assert!(!is_system_root_symlink(Path::new("/tmp/evil-link")));
        assert_eq!(
            is_system_root_symlink(Path::new("/var")),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            is_system_root_symlink(Path::new("/tmp")),
            cfg!(target_os = "macos")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_confined_database_open_path_canonicalizes_parent_and_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = ScopedTempDir::create("acdd_core_db_open").unwrap();
        let base = dir.path();
        let real_dir = base.join("real");
        fs::create_dir_all(&real_dir).unwrap();
        let db = real_dir.join("code-map.sqlite");
        fs::write(&db, b"sqlite").unwrap();

        let resolved = confined_database_open_path(&db).unwrap();
        assert_eq!(resolved, db);

        let symlink_file = base.join("alias.sqlite");
        symlink(&db, &symlink_file).unwrap();
        assert!(confined_database_open_path(&symlink_file).is_err());

        let symlink_dir = base.join("link_dir");
        symlink(&real_dir, &symlink_dir).unwrap();
        assert!(confined_database_open_path(&symlink_dir.join("code-map.sqlite")).is_err());

        check_ancestors_no_symlinks(base, Path::new("real/missing/state.sqlite")).unwrap();
        assert_eq!(
            check_ancestors_no_symlinks(base, Path::new("link_dir/state.sqlite"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            check_ancestors_no_symlinks(base, Path::new("../outside.sqlite"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        // `check_git_entry_no_symlinks`: allows missing, dir, and regular-file `.git`; rejects symlink `.git` and symlink ancestor.
        check_git_entry_no_symlinks(&real_dir).unwrap();
        let wt_ptr = base.join("wt_ptr");
        fs::create_dir_all(&wt_ptr).unwrap();
        fs::write(wt_ptr.join(".git"), "gitdir: ../real/.git\n").unwrap();
        check_git_entry_no_symlinks(&wt_ptr).unwrap();
        let repo_dir = base.join("repo_dir");
        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        check_git_entry_no_symlinks(&repo_dir).unwrap();
        let sym_git = base.join("sym_git");
        fs::create_dir_all(sym_git.join(".git.real")).unwrap();
        symlink(".git.real", sym_git.join(".git")).unwrap();
        assert_eq!(
            check_git_entry_no_symlinks(&sym_git).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            check_git_entry_no_symlinks(&symlink_dir).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        let created_dir = ensure_confined_dir_all(base, Path::new("real/nested/logs")).unwrap();
        assert_eq!(created_dir, real_dir.join("nested/logs"));
        assert!(created_dir.is_dir());
        assert_eq!(
            ensure_confined_dir_all(base, Path::new("link_dir/escape"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            canonicalize_macos_var(&real_dir.join("missing.txt")),
            real_dir.join("missing.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_checked_child_allows_uncanonicalized_root_and_rejects_child_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = ScopedTempDir::create("acdd_core_checked_child").unwrap();
        let base = dir.path();
        let real_root = base.join("private_var/ws");
        fs::create_dir_all(real_root.join("src")).unwrap();
        fs::write(real_root.join("src/lib.rs"), b"pub fn ok() {}").unwrap();

        let alias_root = base.join("var_ws");
        symlink(&real_root, &alias_root).unwrap();
        assert_eq!(
            checked_child(&alias_root, Path::new("src/lib.rs")).unwrap(),
            alias_root.join("src/lib.rs")
        );

        let outside = base.join("outside.rs");
        fs::write(&outside, b"pub fn secret() {}").unwrap();
        symlink(&outside, real_root.join("src/escape.rs")).unwrap();
        assert!(checked_child(&alias_root, Path::new("src/escape.rs")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_replace_preserves_existing_permissions_and_applies_default_for_new() {
        use std::os::unix::fs::PermissionsExt;

        let dir = ScopedTempDir::create("acdd_core_atomic_replace").unwrap();
        let base = dir.path();

        let private_file = base.join("secret.md");
        fs::write(&private_file, b"v1").unwrap();
        fs::set_permissions(&private_file, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_replace(&private_file, b"v2").unwrap();
        assert_eq!(
            fs::metadata(&private_file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let script_file = base.join("run.sh");
        fs::write(&script_file, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&script_file, fs::Permissions::from_mode(0o755)).unwrap();
        atomic_replace(&script_file, b"#!/bin/sh\necho ok\n").unwrap();
        assert_eq!(
            fs::metadata(&script_file).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let new_file = base.join("brand-new.md");
        atomic_replace(&new_file, b"hello").unwrap();
        assert_eq!(fs::read(&new_file).unwrap(), b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn test_same_filesystem_path_matches_absent_files_under_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let dir = ScopedTempDir::create("acdd_core_same_fs").unwrap();
        let base = dir.path();
        let real_root = base.join("real");
        fs::create_dir_all(&real_root).unwrap();
        let alias_root = base.join("alias");
        symlink(&real_root, &alias_root).unwrap();

        let absent_real = real_root.join(".acdd/state.sqlite");
        let absent_alias = alias_root.join(".acdd/sub/../state.sqlite");
        assert!(same_filesystem_path(&absent_real, &absent_alias));

        let other_absent = alias_root.join(".acdd/code-map.sqlite");
        assert!(!same_filesystem_path(&absent_real, &other_absent));
    }
}
