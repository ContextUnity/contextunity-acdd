use crate::db::commitments::{self, read_metadata_optional, MetadataKey};
use acdd_core::{
    config,
    fs::is_case_insensitive_volume,
    path_policy::{normalize_rel_path, WorkspacePathPolicy},
};
use anyhow::{bail, ensure, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
};

/// Compiled canonical matcher for adapter `sensitive` glob patterns (`INV-SINGLE-OWNER`).
pub struct SensitivePathMatcher {
    matcher: Gitignore,
}

impl SensitivePathMatcher {
    /// Compiles `patterns` using the requested volume case-sensitivity semantics.
    pub fn new(patterns: &[String], case_insensitive: bool) -> Result<Self> {
        let mut builder = GitignoreBuilder::new("");
        builder.case_insensitive(case_insensitive)?;
        for pattern in patterns {
            builder.add_line(None, pattern)?;
        }
        Ok(Self {
            matcher: builder.build()?,
        })
    }

    /// Returns `true` when `path` or any parent directory matches the sensitive policy.
    pub fn is_sensitive(&self, path: &str) -> bool {
        self.matcher
            .matched_path_or_any_parents(path, false)
            .is_ignore()
    }
}

/// Validates that `path` is not blocked by `adapter.sensitive` under `case_insensitive` matching.
pub fn check_sensitive_path_with_case(
    adapter: &config::Adapter,
    path: &str,
    case_insensitive: bool,
) -> Result<()> {
    let matcher = SensitivePathMatcher::new(&adapter.sensitive, case_insensitive)?;
    ensure!(
        !matcher.is_sensitive(path),
        "SENSITIVE_FILE: blocked by sensitive policy"
    );
    Ok(())
}

/// Validates that `path` is not blocked by `adapter.sensitive` using `root`'s volume case semantics.
pub fn check_sensitive_path(adapter: &config::Adapter, root: &Path, path: &str) -> Result<()> {
    check_sensitive_path_with_case(adapter, path, is_case_insensitive_volume(root))
}

/// Combined canonical sensitive-path and [`WorkspacePathPolicy`] exposure filter for Git diagnostics.
pub struct PathExposureFilter {
    sensitive: SensitivePathMatcher,
    policy: WorkspacePathPolicy,
    fingerprint: String,
}

impl PathExposureFilter {
    /// Builds an exposure filter and deterministic policy fingerprint for `root`.
    pub fn new(
        root: &Path,
        adapter: &config::Adapter,
        policy: Option<&WorkspacePathPolicy>,
    ) -> Result<Self> {
        let case_insensitive = is_case_insensitive_volume(root);
        let sensitive = SensitivePathMatcher::new(&adapter.sensitive, case_insensitive)?;
        let policy = policy.cloned().unwrap_or_default();
        let bytes = serde_json::to_vec(&json!({
            "case_insensitive": case_insensitive,
            "sensitive": &adapter.sensitive,
            "policy": &policy,
        }))?;
        Ok(Self {
            sensitive,
            policy,
            fingerprint: commitments::hash(&bytes),
        })
    }

    /// Returns the deterministic SHA-256 fingerprint of the active sensitive + path policy.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Returns `true` when `rel_path` is a safe relative path admitted by diagnostic exposure rules.
    ///
    /// Rejects all internal `.acdd` paths (including profile/framework whitelists), policy-excluded
    /// paths, SQLite DB/sidecars, and adapter `sensitive` matches.
    pub fn is_allowed(&self, rel_path: &str) -> bool {
        let Some(normalized) = normalize_rel_path(rel_path) else {
            return false;
        };
        let lower = normalized.to_ascii_lowercase();
        if lower == ".acdd" || lower.starts_with(".acdd/") {
            return false;
        }
        self.policy.is_candidate_included(&normalized) && !self.sensitive.is_sensitive(&normalized)
    }

    /// Returns all indexed `path_dictionary.path_id` entries excluded by this filter.
    pub(crate) fn disallowed_path_ids(&self, conn: &Connection) -> Result<Vec<i64>> {
        let mut stmt = conn.prepare("SELECT path_id, path FROM path_dictionary")?;
        let mut rows = stmt.query([])?;
        let mut excluded = Vec::new();
        while let Some(row) = rows.next()? {
            let path_id: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            if path != "." && !self.is_allowed(&path) {
                excluded.push(path_id);
            }
        }
        Ok(excluded)
    }
}

pub(crate) fn read_confined_source_bytes(
    root: &Path,
    adapter: &config::Adapter,
    path: &str,
    max_bytes: u64,
    unavailable_context: &'static str,
    limit_message: &'static str,
) -> Result<Vec<u8>> {
    let resolved = config::resolve_file_path(root, adapter, path)?;
    let parent = resolved
        .parent()
        .context("source file path requires a parent directory")?;
    let name = resolved
        .file_name()
        .context("source file path requires a file name")?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    let file = acdd_core::fs::open_confined(parent, Path::new(name), &mut opts)
        .context(unavailable_context)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("source must be a regular file");
    }
    if metadata.len() > max_bytes {
        bail!("{limit_message}");
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(64 * 1024));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("{limit_message}");
    }
    Ok(bytes)
}

pub(crate) struct VerifiedSourceCache {
    linked_loaded: bool,
    linked: Vec<(String, PathBuf)>,
    adapters: HashMap<PathBuf, config::Adapter>,
}

impl VerifiedSourceCache {
    pub(crate) fn new() -> Self {
        Self {
            linked_loaded: false,
            linked: Vec::new(),
            adapters: HashMap::new(),
        }
    }

    fn linked_workspaces(&mut self, conn: &Connection) -> Result<&[(String, PathBuf)]> {
        if !self.linked_loaded {
            let linked_json = read_metadata_optional(conn, MetadataKey::AdapterLinkedWorkspaces)?;
            let linked: Value = linked_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?
                .unwrap_or_else(|| json!([]));
            if let Some(workspaces) = linked.as_array() {
                for workspace in workspaces {
                    if let (Some(name), Some(path)) = (
                        workspace.get("name").and_then(Value::as_str),
                        workspace.get("path").and_then(Value::as_str),
                    ) {
                        self.linked
                            .push((format!("[{name}]/"), PathBuf::from(path)));
                    }
                }
            }
            self.linked_loaded = true;
        }
        Ok(&self.linked)
    }

    pub(crate) fn read(&mut self, conn: &Connection, root: &Path, path: &str) -> Result<String> {
        let digest: String =
            conn.query_row("SELECT digest FROM files WHERE path=?1", [path], |r| {
                r.get(0)
            })?;
        let linked_root = self
            .linked_workspaces(conn)?
            .iter()
            .find_map(|(prefix, linked_path)| {
                let relative = path.strip_prefix(prefix.as_str())?;
                Some((linked_path.clone(), relative.to_owned()))
            });
        let (source_root, source_path) = match linked_root {
            Some(pair) => pair,
            None => (root.to_path_buf(), path.to_owned()),
        };
        if !self.adapters.contains_key(&source_root) {
            let adapter = config::load_adapter(&source_root, None)?;
            self.adapters.insert(source_root.clone(), adapter);
        }
        let adapter = self
            .adapters
            .get(&source_root)
            .context("cached adapter missing")?;
        let bytes = read_confined_source_bytes(
            &source_root,
            adapter,
            &source_path,
            config::MAX_FILE_BYTES,
            "source file unavailable; rebuild index",
            "source exceeds file byte limit",
        )?;
        if commitments::hash(&bytes) != digest {
            bail!("source changed since indexing; update or rebuild index");
        }
        String::from_utf8(bytes).context("source is not UTF-8")
    }
}

pub(crate) fn verified_source(conn: &Connection, root: &Path, path: &str) -> Result<String> {
    VerifiedSourceCache::new().read(conn, root, path)
}
