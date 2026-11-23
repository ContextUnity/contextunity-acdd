use crate::db::commitments::{self, read_metadata_optional, MetadataKey};
use acdd_core::config;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
};

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
                        self.linked.push((format!("[{name}]/"), PathBuf::from(path)));
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
