//! Freshness-verified SQLite reader admission, delta/rebuild triggering, and transaction management.

use super::types::{
    AdmissionGuard, CachedConnection, ConnectionSlot, Freshness, Server, ADMITTING_SLOT,
};
use acdd_core::telemetry::unix_epoch_ms;
use acdd_index::{
    db::{cache, reader},
    scanner,
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

const INVENTORY_FRESHNESS_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn same_request_adapter(left: &scanner::Adapter, right: &scanner::Adapter) -> bool {
    left.digest == right.digest
        && left.adapter_path == right.adapter_path
        && left.response == right.response
        && left.modules == right.modules
        && left.debug == right.debug
        && left.roots == right.roots
        && left.linked_workspaces == right.linked_workspaces
}

impl Server {
    fn rebuild(
        &self,
        root: &std::path::Path,
        adapter: Option<&std::path::Path>,
    ) -> Result<serde_json::Value> {
        self.ensure_current_binary_before_rebuild()?;
        let guard = || self.ensure_current_binary_before_rebuild();
        let selected = self
            .invocation
            .as_ref()
            .and_then(|settings| settings.config.as_deref());
        let adapter = selected.or(adapter);
        crate::engine::pipeline::build_with_selection_and_guard(
            root,
            &self.db,
            adapter,
            self.extraction_cache_dir.as_deref(),
            None,
            Some(&guard),
        )
    }

    fn delta(&self, root: &std::path::Path, modified: &[PathBuf]) -> Result<serde_json::Value> {
        self.ensure_current_binary_before_rebuild()?;
        let guard = || self.ensure_current_binary_before_rebuild();
        crate::engine::pipeline::delta_with_selection_and_guard(
            root,
            &self.db,
            modified,
            self.invocation
                .as_ref()
                .and_then(|settings| settings.config.as_deref()),
            self.extraction_cache_dir.as_deref(),
            Some(&guard),
        )
    }

    fn check_rebuild_needed(&self, current_adapter: &scanner::Adapter) -> Result<bool> {
        match std::fs::symlink_metadata(&self.db) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_file() => anyhow::bail!("database must be a regular file"),
            Ok(_) => {}
        }
        let _generation_lock = cache::shared_lock(&self.db)?;
        let conn = reader::open_raw_confined_readonly(&self.db)?;
        conn.execute_batch("BEGIN DEFERRED")?;
        let has_metadata_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='metadata')",
            [],
            |row| row.get(0),
        )?;
        if !has_metadata_table {
            reader::validate_database_header(&self.db)?;
            reader::validate_database_integrity(&conn)?;
            return Ok(true);
        }
        reader::validate_workspace(&conn, &self.root)?;
        let engine: String = conn.query_row(
            "SELECT value FROM metadata WHERE key='indexer_engine'",
            [],
            |r| r.get(0),
        )?;
        if engine != "contextunity-acdd-rust" {
            anyhow::bail!("incompatible database engine");
        }
        let schema_version: String = conn.query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )?;
        if schema_version != scanner::ENGINE_SCHEMA_VERSION {
            reader::validate_database_integrity(&conn)?;
            return Ok(true);
        }
        let semantics: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='index_semantics_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if semantics.as_deref() != Some(scanner::INDEX_SEMANTICS_VERSION) {
            reader::validate_database_integrity(&conn)?;
            return Ok(true);
        }
        let admit_rebuild = || -> Result<bool> {
            reader::validate_database_integrity(&conn)?;
            reader::open(&self.db, &self.root)?;
            Ok(true)
        };
        let persisted = reader::read_policy(&self.root, &conn)?;
        if current_adapter.adapter_path != persisted.adapter_path {
            return admit_rebuild();
        }
        let persisted_digest: String = conn.query_row(
            "SELECT value FROM metadata WHERE key='adapter_digest'",
            [],
            |r| r.get(0),
        )?;
        if current_adapter.digest != persisted_digest {
            return admit_rebuild();
        }
        let persisted_version: String = conn.query_row(
            "SELECT value FROM metadata WHERE key='adapter_version'",
            [],
            |r| r.get(0),
        )?;
        if current_adapter.adapter_version.as_deref().unwrap_or("") != persisted_version {
            return admit_rebuild();
        }
        let persisted_policy: String = conn.query_row(
            "SELECT value FROM metadata WHERE key='adapter'",
            [],
            |row| row.get(0),
        )?;
        let persisted_policy: serde_json::Value = serde_json::from_str(&persisted_policy)?;
        let persisted_roots: Vec<String> =
            serde_json::from_value(persisted_policy["roots"].clone())?;
        let root = scanner::canonical_root(&self.root)?;
        let current_roots: Vec<_> = current_adapter
            .roots
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .or_else(|_| path.strip_prefix(&self.root))
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        if current_roots != persisted_roots {
            return admit_rebuild();
        }
        let current_linked_json = serde_json::to_string(&current_adapter.linked_workspaces)?;
        let persisted_linked: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='adapter_linked_workspaces'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match persisted_linked {
            Some(persisted) => {
                if persisted != current_linked_json {
                    return admit_rebuild();
                }
            }
            None => {
                if !current_adapter.linked_workspaces.is_empty() {
                    return admit_rebuild();
                }
            }
        }
        Ok(false)
    }

    fn admit(
        &self,
        slot: &mut ConnectionSlot,
        request_adapter: &scanner::Adapter,
    ) -> Result<Freshness> {
        let root = scanner::canonical_root(&self.root)?;
        let mut refresh = "none";
        let mut reloaded_adapter = None;
        for attempt in 0..5 {
            if attempt > 0 {
                let latest = self.load_adapter(&root)?;
                anyhow::ensure!(
                    same_request_adapter(&latest, request_adapter),
                    "adapter changed during source admission; retry against the current snapshot"
                );
                reloaded_adapter = Some(latest);
            }
            let adapter = reloaded_adapter.as_ref().unwrap_or(request_adapter);
            let admitted_identity = slot
                .as_ref()
                .filter(|cached| {
                    cached.workspace_root == root
                        && cached.database_path == self.db
                        && cached.adapter_digest == adapter.digest
                        && cached.adapter_path == adapter.adapter_path
                        && cached.adapter_roots == adapter.roots
                        && cached.linked_workspaces == adapter.linked_workspaces
                })
                .and_then(|cached| {
                    if !std::fs::symlink_metadata(&self.db).is_ok_and(|metadata| metadata.is_file())
                    {
                        return None;
                    }
                    cache::identity(&self.db)
                        .ok()
                        .filter(|identity| *identity == cached.identity)
                });
            if admitted_identity.is_none() {
                self.ensure_current_binary_before_rebuild()?;
            }
            if admitted_identity.is_none() && self.check_rebuild_needed(adapter)? {
                *slot = None;
                self.rebuild(&root, None)?;
                refresh = "rebuild";
                continue;
            }
            let mut identity =
                admitted_identity.map_or_else(|| cache::identity(&self.db), Ok)?;
            if slot
                .as_ref()
                .is_none_or(|cached| cached.identity != identity)
            {
                *slot = None;
                // Sample identity after closing any stale reader connection so WAL close
                // cleanup on the dropped connection does not invalidate our own baseline.
                identity = cache::identity(&self.db)?;
                let mut diagnostics = Vec::new();
                let connection =
                    reader::open_with_diagnostics(&self.db, &root, &mut |message| {
                        eprintln!("{message}");
                        diagnostics.push(message);
                    })?;
                *slot = Some(CachedConnection {
                    identity: identity.clone(),
                    connection: Some(connection),
                    adapter_digest: adapter.digest.clone(),
                    adapter_path: adapter.adapter_path.clone(),
                    workspace_root: root.clone(),
                    database_path: self.db.clone(),
                    adapter_roots: adapter.roots.clone(),
                    linked_workspaces: adapter.linked_workspaces.clone(),
                    inventory_checked_at: None,
                    freshness: None,
                    diagnostics,
                });
            } else if let Some(cached) = slot.as_mut() {
                if let Some(connection) = &cached.connection {
                    connection.execute_batch("BEGIN DEFERRED")?;
                } else {
                    cached.connection = Some(reader::open_with_diagnostics(
                        &self.db,
                        &root,
                        &mut |message| {
                            eprintln!("{message}");
                            cached.diagnostics.push(message);
                        },
                    )?);
                }
            }
            let conn = &slot
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("database unavailable"))?
                .connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("database connection unavailable"))?;
            if let Some(cached) = slot.as_ref().filter(|cached| {
                cached.adapter_digest == adapter.digest
                    && cached
                        .inventory_checked_at
                        .is_some_and(|checked| checked.elapsed() < INVENTORY_FRESHNESS_TTL)
            }) {
                if let Some(freshness) = &cached.freshness {
                    let mut freshness = freshness.clone();
                    freshness.refresh = "none";
                    freshness.inventory_scan_ms = 0.0;
                    return Ok(freshness);
                }
            }
            let metadata = |key: &str| -> Result<String> {
                Ok(
                    conn.query_row("SELECT value FROM metadata WHERE key=?1", [key], |row| {
                        row.get(0)
                    })?,
                )
            };
            let previous = reader::inventory_snapshot(conn)?;
            let started = Instant::now();
            let watched = previous.iter().map(|entry| entry.path.clone()).collect();
            let scan = scanner::scan_reusing_watching(&root, adapter, &previous, &watched)?;
            let manifests =
                acdd_index::languages::dependency_registry::DependencyRegistry::try_collect_with_paths(
                    &root,
                    Some(adapter),
                    &scan.dependency_manifests,
                )?;
            let previous_manifest_digest = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='manifest_digest'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if previous_manifest_digest.as_deref() != Some(manifests.digest()) {
                *slot = None;
                self.rebuild(&root, adapter.adapter_path.as_deref())?;
                refresh = "rebuild";
                continue;
            }
            let inventory_scan_ms = started.elapsed().as_secs_f64() * 1000.;
            let previous: BTreeMap<_, _> = previous
                .iter()
                .map(|entry| (entry.path.as_str(), entry))
                .collect();
            let current: BTreeMap<_, _> = scan
                .entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry))
                .collect();
            let changed: BTreeSet<_> = previous
                .keys()
                .chain(current.keys())
                .filter(|path| match (previous.get(**path), current.get(**path)) {
                    (Some(old), Some(new)) => {
                        old.digest != new.digest
                            || old.language != new.language
                            || old.is_doc != new.is_doc
                    }
                    _ => true,
                })
                .copied()
                .collect();
            let current_identity = cache::identity(&self.db)?;
            if current_identity != identity {
                anyhow::bail!(
                    "generation changed during source admission ({}); retry against the current snapshot",
                    cache::describe_diff(&identity, &current_identity)
                );
            }
            let latest_adapter = self.load_adapter(&root)?;
            if !same_request_adapter(&latest_adapter, adapter) {
                anyhow::bail!(
                    "adapter changed during source admission; retry against the current snapshot"
                );
            }
            if changed.is_empty() {
                let freshness = Freshness {
                    status: "source_inventory_matched",
                    output_root: metadata("output_root")?,
                    corpus_hash: metadata("corpus_hash")?,
                    checked_at_unix_ms: unix_epoch_ms(),
                    files_checked: scan.files,
                    inventory_scan_ms,
                    refresh,
                };
                if let Some(cached) = slot.as_mut() {
                    cached.adapter_digest = adapter.digest.clone();
                    cached.inventory_checked_at = Some(Instant::now());
                    cached.freshness = Some(freshness.clone());
                }
                return Ok(freshness);
            }
            let has_excluded = changed
                .iter()
                .any(|path| !current.contains_key(path) && scan.matched_excluded.contains(*path));
            let exact_unresolved = scanner::exact_paths_exist(
                &root,
                adapter,
                changed.iter().copied().filter(|path| {
                    !current.contains_key(path) && !scan.matched_excluded.contains(*path)
                }),
            )?;
            let rebuild = has_excluded || !exact_unresolved.is_empty();
            let modified: Vec<_> = changed.into_iter().map(PathBuf::from).collect();
            *slot = None;
            if rebuild {
                self.rebuild(&root, None)?;
                refresh = "rebuild";
            } else {
                match self.delta(&root, &modified) {
                    Ok(_) => {
                        refresh = "delta";
                    }
                    Err(error) if error.is::<crate::engine::pipeline::SourceSnapshotMismatch>() => {
                        let backoff_ms = (25 * (1 << attempt)).min(200);
                        std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                        continue;
                    }
                    Err(error) => {
                        return Err(error);
                    }
                }
            }
        }
        anyhow::bail!(
            "workspace changed repeatedly during source admission; retry against a stable snapshot"
        )
    }

    /// Admits the current workspace index and runs a read-only SQLite query closure against it.
    pub fn read(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let adapter = self.load_adapter(&scanner::canonical_root(&self.root)?)?;
        self.read_with_adapter(&adapter, f)
    }

    /// Runs a query with the adapter snapshot used by its MCP response policy.
    pub(crate) fn read_with_adapter(
        &self,
        adapter: &scanner::Adapter,
        f: impl FnOnce(&rusqlite::Connection) -> Result<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let mut slot = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("database connection lock poisoned"))?;
        cache::register_idle_reader(&self.db, &self.reader_evictor)?;
        let previous =
            ADMITTING_SLOT.with(|current| current.replace(Arc::as_ptr(&self.connection) as usize));
        let admission_guard = AdmissionGuard(previous);
        let admitted = self.admit(&mut slot, adapter);
        drop(admission_guard);
        let freshness = match admitted {
            Ok(freshness) => freshness,
            Err(error) => {
                *slot = None;
                return Err(error);
            }
        };
        // Initial admission can create the database parent; refresh its canonical key.
        cache::register_idle_reader(&self.db, &self.reader_evictor)?;
        let _read_lock = cache::shared_lock(&self.db)?;
        let cached = slot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("database unavailable"))?;
        let connection = cached
            .connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("database connection unavailable"))?;
        let result = f(connection);
        let after = cache::identity(&self.db);
        let ended = connection.execute_batch("ROLLBACK");
        if let Err(error) = ended {
            *slot = None;
            return match result {
                Err(original) => Err(original.context(format!("query rollback failed: {error}"))),
                Ok(_) => Err(error.into()),
            };
        }
        let after = match after {
            Ok(after) => after,
            Err(error) => {
                *slot = None;
                return Err(error);
            }
        };
        if after != cached.identity {
            let diff = cache::describe_diff(&cached.identity, &after);
            *slot = None;
            anyhow::bail!(
                "generation changed during query ({diff}); retry against the current snapshot"
            );
        }
        let mut result = result?;
        if let Some(object) = result.as_object_mut() {
            object.insert("freshness".into(), serde_json::to_value(freshness)?);
            if let Some(cached) = slot.as_mut() {
                if !cached.diagnostics.is_empty() {
                    object.insert(
                        "diagnostics".into(),
                        serde_json::to_value(std::mem::take(&mut cached.diagnostics))?,
                    );
                }
            }
        }
        drop(slot);
        Ok(result)
    }
}
