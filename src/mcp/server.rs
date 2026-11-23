use acdd_core::{fs::canonicalize_macos_var, telemetry::unix_epoch_ms};
use acdd_index::{
    db::{cache, cache::Identity, reader},
    scanner,
};
use anyhow::Result;
use rmcp::ServiceExt;
use rusqlite::OptionalExtension;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
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

/// Represents cached connection data.
pub(crate) struct CachedConnection {
    identity: Identity,
    connection: Option<rusqlite::Connection>,
    adapter_digest: String,
    adapter_path: Option<PathBuf>,
    workspace_root: PathBuf,
    database_path: PathBuf,
    adapter_roots: Vec<PathBuf>,
    linked_workspaces: Vec<scanner::LinkedWorkspace>,
    inventory_checked_at: Option<Instant>,
    freshness: Option<Freshness>,
    diagnostics: Vec<String>,
}

/// Names the connection slot type.
pub(crate) type ConnectionSlot = Option<CachedConnection>;

thread_local! {
    static ADMITTING_SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct AdmissionGuard(usize);
impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        ADMITTING_SLOT.with(|slot| slot.set(self.0));
    }
}

fn reader_evictor(slot: &Arc<Mutex<ConnectionSlot>>) -> Arc<cache::IdleReader> {
    let slot = Arc::downgrade(slot);
    Arc::new(move |db| {
        let Some(slot) = slot.upgrade() else {
            return Ok(());
        };
        match slot.try_lock() {
            Ok(mut slot) => {
                if slot.as_ref().is_some_and(|cached| {
                    cache::registry_path(&cached.database_path) == db
                }) {
                    *slot = None;
                }
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let own_slot =
                    ADMITTING_SLOT.with(|current| current.get() == Arc::as_ptr(&slot) as usize);
                if !own_slot {
                    anyhow::bail!("reader admission is busy; retry database publication");
                }
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                anyhow::bail!("database connection lock poisoned")
            }
        }
        Ok(())
    })
}

#[derive(Clone)]
pub(crate) enum RequestAdapter {
    Loaded(Arc<scanner::Adapter>),
    Failed {
        kind: std::io::ErrorKind,
        message: String,
    },
}

#[derive(Clone, serde::Serialize)]
struct Freshness {
    status: &'static str,
    output_root: String,
    corpus_hash: String,
    checked_at_unix_ms: u128,
    files_checked: usize,
    inventory_scan_ms: f64,
    refresh: &'static str,
}

fn rebase_path(path: &std::path::Path, from_root: &std::path::Path, to_root: &std::path::Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(from_root) {
        return to_root.join(relative);
    }
    if path.is_absolute() {
        if let Some(canon_path) = acdd_core::fs::canonicalize_with_missing_tail(path) {
            if let Ok(relative) = canon_path.strip_prefix(from_root) {
                return to_root.join(relative);
            }
        }
    }
    path.to_owned()
}

fn rebase_invocation_settings(
    settings: &crate::state_bridge::InvocationSettings,
    target_root: &std::path::Path,
) -> crate::state_bridge::InvocationSettings {
    let rebased_root = canonicalize_macos_var(target_root);
    let from_root = canonicalize_macos_var(&settings.root);
    crate::state_bridge::InvocationSettings {
        root: rebased_root.clone(),
        config: settings
            .config
            .as_deref()
            .map(|p| rebase_path(p, &from_root, &rebased_root)),
        profile: settings
            .profile
            .as_deref()
            .map(|p| rebase_path(p, &from_root, &rebased_root)),
        state_db: settings.state_db.clone(),
        code_db: rebase_path(&settings.code_db, &from_root, &rebased_root),
    }
}

pub(crate) fn primary_workspace_root_for(
    server_root: &std::path::Path,
    target_root: &std::path::Path,
) -> PathBuf {
    acdd_core::git::primary_worktree_root(target_root)
        .map(|p| canonicalize_macos_var(&p))
        .unwrap_or_else(|| {
            acdd_core::git::primary_worktree_root(server_root)
                .map(|p| canonicalize_macos_var(&p))
                .unwrap_or_else(|| canonicalize_macos_var(server_root))
        })
}

#[derive(Clone)]
/// Represents server data.
pub struct Server {
    /// The root value.
    pub root: PathBuf,
    /// The db value.
    pub db: PathBuf,
    /// The connection value.
    pub(crate) connection: Arc<Mutex<ConnectionSlot>>,
    reader_evictor: Arc<cache::IdleReader>,
    request_adapter: Option<RequestAdapter>,
    request_workspace_root: Option<PathBuf>,
    extraction_cache_dir: Option<PathBuf>,
    pub(crate) invocation: Option<Arc<crate::state_bridge::InvocationSettings>>,
    pub(crate) session_fingerprint: Option<String>,
    extra_domain_providers: Vec<Arc<dyn acdd_core::ports::DomainProvider>>,
    startup_executable: std::result::Result<acdd_core::fs::ExecutableIdentity, String>,
    reload_signal: Option<Arc<ReloadSignal>>,
}

struct ReloadSignal {
    pending: std::sync::atomic::AtomicBool,
    armed: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl ReloadSignal {
    fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn mark_pending(&self) {
        self.pending
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn arm_pending(&self) {
        if self
            .pending
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.armed.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

impl Server {
    /// Returns this server slot's active registry entry count.
    #[doc(hidden)]
    pub fn reader_registration_count(&self) -> Result<usize> {
        cache::idle_reader_registration_count(&self.reader_evictor)
    }
    /// Creates a new instance.
    pub fn new(root: PathBuf, db: PathBuf) -> Self {
        let connection = Arc::new(Mutex::new(None));
        let reader_evictor = reader_evictor(&connection);
        Self {
            root,
            db,
            connection,
            reader_evictor,
            request_adapter: None,
            request_workspace_root: None,
            extraction_cache_dir: None,
            invocation: None,
            session_fingerprint: Some(
                acdd_state::usecases::lifecycle::current_session_fingerprint(),
            ),
            extra_domain_providers: Vec::new(),
            startup_executable: acdd_core::fs::ExecutableIdentity::current()
                .map_err(|err| err.to_string()),
            reload_signal: None,
        }
    }

    fn startup_executable_path(&self) -> Option<&std::path::Path> {
        self.startup_executable
            .as_ref()
            .ok()
            .map(|identity| identity.path.as_path())
    }

    fn with_reload_signal(mut self, signal: Arc<ReloadSignal>) -> Self {
        self.reload_signal = Some(signal);
        self
    }

    pub(super) fn arm_pending_reload(&self) {
        if let Some(signal) = &self.reload_signal {
            signal.arm_pending();
        }
    }

    fn ensure_current_binary_before_rebuild(&self) -> Result<()> {
        let startup = match &self.startup_executable {
            Ok(identity) => identity,
            Err(error) => {
                anyhow::bail!(
                    "BINARY_DRIFT_UNAVAILABLE: cannot verify server executable identity ({error}); refusing stale-binary write to '{}'",
                    self.db.display()
                );
            }
        };
        match startup.check_current(None)? {
            acdd_core::fs::ExecutableDrift::Current => Ok(()),
            acdd_core::fs::ExecutableDrift::Replaced { live_path } => {
                if let Some(signal) = &self.reload_signal {
                    signal.mark_pending();
                }
                anyhow::bail!(
                    "BINARY_DRIFT_RELOAD: server executable changed at '{}' (reloading without rebuilding '{}')",
                    live_path.display(),
                    self.db.display()
                );
            }
            acdd_core::fs::ExecutableDrift::Missing { live_path } => {
                anyhow::bail!(
                    "BINARY_DRIFT_UNAVAILABLE: server executable missing at '{}' (refusing stale-binary rebuild of '{}')",
                    live_path.display(),
                    self.db.display()
                );
            }
        }
    }

    /// Selects an explicit extraction cache for this server and selected worktrees.
    pub fn with_cache_dir(mut self, cache_dir: PathBuf) -> Self {
        self.extraction_cache_dir = Some(cache_dir);
        self
    }

    /// Retains immutable invocation settings for the lifetime of this server.
    pub fn with_invocation_settings(
        mut self,
        settings: crate::state_bridge::InvocationSettings,
    ) -> Self {
        self.invocation = Some(Arc::new(rebase_invocation_settings(&settings, &self.root)));
        self
    }

    /// Attaches additional external `DomainProvider` instances (e.g., plugin tools).
    pub fn with_domain_providers(
        mut self,
        providers: Vec<Arc<dyn acdd_core::ports::DomainProvider>>,
    ) -> Self {
        self.extra_domain_providers = providers;
        self
    }

    pub(crate) fn state_db(&self) -> Option<&std::path::Path> {
        self.invocation
            .as_ref()
            .and_then(|settings| settings.state_db.as_deref())
    }

    pub(crate) fn request_workspace_root(&self) -> Option<&std::path::Path> {
        self.request_workspace_root.as_deref()
    }

    pub(crate) fn domain_providers(
        &self,
        adapter: &scanner::Adapter,
    ) -> std::result::Result<
        Arc<acdd_core::ports::CompositeDomainProvider>,
        acdd_core::errors::StateError,
    > {
        self.domain_providers_for(&self.root, Some(&self.db), adapter)
    }

    pub(crate) fn domain_providers_for(
        &self,
        root: &std::path::Path,
        code_db: Option<&std::path::Path>,
        adapter: &scanner::Adapter,
    ) -> std::result::Result<
        Arc<acdd_core::ports::CompositeDomainProvider>,
        acdd_core::errors::StateError,
    > {
        crate::state_bridge::build_domain_providers(
            root,
            adapter,
            code_db,
            self.extra_domain_providers.clone(),
        )
    }

    pub(crate) fn load_adapter(&self, root: &std::path::Path) -> std::io::Result<scanner::Adapter> {
        let root = scanner::canonical_root(root)?;
        let settings = self
            .invocation
            .as_ref()
            .map(|settings| rebase_invocation_settings(settings, &root));
        scanner::load_adapter(
            &root,
            settings
                .as_ref()
                .and_then(|settings| settings.config.as_deref()),
        )
    }

    /// Supplies explicit session metadata without exposing it to tool JSON.
    pub fn with_session_fingerprint(mut self, session_fingerprint: String) -> Self {
        self.session_fingerprint = Some(session_fingerprint);
        self
    }

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

    pub(crate) fn with_root_and_db(
        &self,
        root: PathBuf,
        db: PathBuf,
        adapter: RequestAdapter,
    ) -> Self {
        let origin_root = self.request_workspace_root.as_deref().unwrap_or(&self.root);
        let request_workspace_root = Some(primary_workspace_root_for(origin_root, &root));
        let invocation = self.invocation.as_deref().map(|settings| {
            let mut rebased = rebase_invocation_settings(settings, &root);
            rebased.code_db = db.clone();
            Arc::new(rebased)
        });
        Self {
            root,
            db,
            connection: Arc::clone(&self.connection),
            reader_evictor: Arc::clone(&self.reader_evictor),
            request_adapter: Some(adapter),
            request_workspace_root,
            extraction_cache_dir: self.extraction_cache_dir.clone(),
            invocation,
            session_fingerprint: self.session_fingerprint.clone(),
            extra_domain_providers: self.extra_domain_providers.clone(),
            startup_executable: self.startup_executable.clone(),
            reload_signal: self.reload_signal.clone(),
        }
    }

    pub(crate) fn adapter_snapshot(&self) -> std::io::Result<Arc<scanner::Adapter>> {
        match &self.request_adapter {
            Some(RequestAdapter::Loaded(adapter)) => Ok(Arc::clone(adapter)),
            Some(RequestAdapter::Failed { kind, message }) => {
                Err(std::io::Error::new(*kind, message.clone()))
            }
            None => self.load_adapter(&self.root).map(Arc::new),
        }
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

    /// Performs read.
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

fn canonicalize_existing_dir(path: &std::path::Path) -> Result<PathBuf> {
    let canon = path.canonicalize()?;
    Ok(canonicalize_macos_var(&canon))
}

impl Server {
    /// Resolves an explicit worktree using a caller-supplied root [`scanner::Adapter`] when available.
    pub(crate) fn resolve_target_root_and_db_with_adapter(
        &self,
        requested_worktree: Option<&str>,
        root_adapter: Option<&scanner::Adapter>,
    ) -> Result<(PathBuf, PathBuf)> {
        let root = canonicalize_existing_dir(&self.root)?;
        let target = match requested_worktree {
            None => root.clone(),
            Some(requested) => {
                let requested = requested.trim();
                if requested.is_empty() {
                    root.clone()
                } else {
                    let repository = acdd_core::git::primary_worktree_root(&root)
                        .map(|p| canonicalize_macos_var(&p))
                        .unwrap_or_else(|| root.clone());
                    let loaded_owned;
                    let adapter_ref = match root_adapter {
                        Some(adapter) => Some(adapter),
                        None => {
                            loaded_owned = self.load_adapter(&root).ok();
                            loaded_owned.as_ref()
                        }
                    };
                    let linked: Vec<(String, PathBuf)> = adapter_ref
                        .map(|adapter| {
                            adapter
                                .linked_workspaces
                                .iter()
                                .filter_map(|lw| {
                                    canonicalize_existing_dir(&lw.path)
                                        .ok()
                                        .map(|p| (lw.name.clone(), p))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    resolve_explicit_worktree_with_linked(&root, &repository, &linked, requested)?
                }
            }
        };
        let db = if target == root {
            self.db.clone()
        } else if let Some(settings) = &self.invocation {
            rebase_invocation_settings(settings, &target).code_db
        } else {
            acdd_core::config::resolve_code_db_path(&target, None)
        };
        Ok((target, db))
    }
}

fn collect_worktree_candidates(
    repository: &std::path::Path,
    repo_common_dir: Option<&std::path::Path>,
    linked_workspaces: &[(String, PathBuf)],
) -> Result<BTreeSet<PathBuf>> {
    let mut candidates = BTreeSet::new();
    if repository.join(".git").exists() || acdd_core::git::git_common_dir(repository).is_ok() {
        let output =
            acdd_core::git::git_command(repository, &["worktree", "list", "--porcelain", "-z"])?;
        if !output.status.success() {
            anyhow::bail!(
                "WORKTREE_RESOLUTION_FAILED: could not list Git worktrees for '{}': {}",
                repository.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let output = String::from_utf8(output.stdout)?;
        for field in output.split('\0') {
            if let Some(path) = field.strip_prefix("worktree ") {
                insert_worktree_candidate(std::path::Path::new(path), &mut candidates)?;
            }
        }
    }

    let worktrees_dir = repository.join(".worktrees");
    match std::fs::symlink_metadata(&worktrees_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(meta) if meta.is_dir() => match std::fs::read_dir(&worktrees_dir) {
            Ok(entries) => {
                let mut paths = Vec::new();
                for entry in entries {
                    let entry = entry?;
                    let path = entry.path();
                    if std::fs::symlink_metadata(&path)
                        .is_ok_and(|entry_meta| entry_meta.file_type().is_symlink())
                    {
                        continue;
                    }
                    paths.push(path);
                }
                paths.sort();
                for path in paths {
                    if let Some(rcd) = repo_common_dir {
                        let Ok(cand_common) = acdd_core::git::git_common_dir(&path) else {
                            continue;
                        };
                        if canonicalize_macos_var(&cand_common) != rcd {
                            continue;
                        }
                    }
                    insert_worktree_candidate(&path, &mut candidates)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        },
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    for (_, lw_path) in linked_workspaces {
        insert_worktree_candidate(lw_path, &mut candidates)?;
    }

    Ok(candidates)
}

fn is_owned_worktree_path(
    canon: &std::path::Path,
    root: &std::path::Path,
    repo_common_dir: Option<&std::path::Path>,
    linked_workspaces: &[(String, PathBuf)],
) -> bool {
    if let Some(rcd) = repo_common_dir {
        if canon == rcd || canon.starts_with(rcd) {
            return false;
        }
        if let Ok(cand_common) = acdd_core::git::git_common_dir(canon) {
            if canonicalize_macos_var(&cand_common) == rcd {
                return true;
            }
        }
    }
    if canon == root {
        return true;
    }
    for (_, lw_path) in linked_workspaces {
        if canon == lw_path || canon.starts_with(lw_path) {
            return true;
        }
        if let (Ok(cand_common), Ok(lw_common)) = (
            acdd_core::git::git_common_dir(canon),
            acdd_core::git::git_common_dir(lw_path),
        ) {
            if canonicalize_macos_var(&cand_common) == canonicalize_macos_var(&lw_common) {
                return true;
            }
        }
    }
    repo_common_dir.is_none() && canon.starts_with(root)
}

fn resolve_explicit_worktree_with_linked(
    root: &std::path::Path,
    repository: &std::path::Path,
    linked_workspaces: &[(String, PathBuf)],
    requested: &str,
) -> Result<PathBuf> {
    let root = canonicalize_existing_dir(root)?;
    let repository = canonicalize_existing_dir(repository)?;
    if requested == "." {
        return Ok(root);
    }

    let repo_common_dir = acdd_core::git::git_common_dir(&repository)
        .ok()
        .map(|p| canonicalize_macos_var(&p));

    let requested_path = std::path::Path::new(requested);
    if requested_path.is_absolute() || requested.contains('/') || requested.contains('\\') {
        let path = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            root.join(requested_path)
        };
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                let canon = canonicalize_existing_dir(&path)?;
                if !is_owned_worktree_path(
                    &canon,
                    &root,
                    repo_common_dir.as_deref(),
                    linked_workspaces,
                ) {
                    anyhow::bail!(
                        "WORKTREE_NOT_FOUND: explicit worktree path '{}' is outside this repository's git_common_dir and linked_workspaces",
                        path.display()
                    );
                }
                return Ok(canon);
            }
            Ok(_) => anyhow::bail!(
                "WORKTREE_NOT_FOUND: explicit worktree path '{}' is not a directory",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("WORKTREE_NOT_FOUND: explicit worktree path '{requested}' not found")
            }
            Err(error) => return Err(error.into()),
        }
    }

    let candidates =
        collect_worktree_candidates(&repository, repo_common_dir.as_deref(), linked_workspaces)?;

    let linked_names: BTreeMap<&std::path::Path, &str> = linked_workspaces
        .iter()
        .map(|(name, path)| (path.as_path(), name.as_str()))
        .collect();

    for tier in [
        WorktreeMatchTier::Exact,
        WorktreeMatchTier::Prefix,
        WorktreeMatchTier::Suffix,
        WorktreeMatchTier::Substring,
    ] {
        let mut first_match = None;
        let mut ambiguous = false;
        for candidate in &candidates {
            let alias = linked_names.get(candidate.as_path()).copied();
            if !candidate_matches_with_alias(candidate, alias, requested, tier) {
                continue;
            }
            if first_match.is_some() {
                ambiguous = true;
                break;
            }
            first_match = Some(candidate);
        }
        if ambiguous {
            let matches = candidates
                .iter()
                .filter(|candidate| {
                    let alias = linked_names.get(candidate.as_path()).copied();
                    candidate_matches_with_alias(candidate, alias, requested, tier)
                })
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "WORKTREE_AMBIGUOUS: '{requested}' matches {matches}. Do: pass a more specific worktree name or path."
            );
        }
        if let Some(candidate) = first_match {
            return Ok(candidate.clone());
        }
    }

    anyhow::bail!(
        "WORKTREE_NOT_FOUND: no worktree matching '{requested}'. Do: pass a path or unique worktree name."
    )
}

fn insert_worktree_candidate(
    path: &std::path::Path,
    candidates: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        Ok(metadata) if metadata.is_dir() => {
            candidates.insert(canonicalize_existing_dir(path)?);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum WorktreeMatchTier {
    Exact,
    Prefix,
    Suffix,
    Substring,
}

fn name_matches(name: &str, requested: &str, tier: WorktreeMatchTier) -> bool {
    match tier {
        WorktreeMatchTier::Exact => name == requested,
        WorktreeMatchTier::Prefix => {
            name.starts_with(requested) || matches_numeric_worktree_prefix(name, requested)
        }
        WorktreeMatchTier::Suffix => name.ends_with(requested),
        WorktreeMatchTier::Substring => name.contains(requested),
    }
}

fn candidate_matches_with_alias(
    path: &std::path::Path,
    alias: Option<&str>,
    requested: &str,
    tier: WorktreeMatchTier,
) -> bool {
    let dir_matches = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name_matches(name, requested, tier));
    dir_matches || alias.is_some_and(|name| name_matches(name, requested, tier))
}

fn matches_numeric_worktree_prefix(name: &str, requested: &str) -> bool {
    if requested.is_empty() || !requested.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let digit_end = name
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(name.len());
    if digit_end == 0 || !name[digit_end..].is_empty() && !name[digit_end..].starts_with('-') {
        return false;
    }
    normalize_milestone_digits(&name[..digit_end]) == normalize_milestone_digits(requested)
}

fn normalize_milestone_digits(digits: &str) -> &str {
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        "0"
    } else {
        digits
    }
}

const MCP_SESSION_ENV: &str = "ACDD_MCP_SESSION";
#[cfg(unix)]
const MCP_RELOAD_PARENT_ENV: &str = "ACDD_MCP_RELOAD_PARENT";

fn resumed_initialize_params(server: &Server) -> Option<rmcp::model::InitializeRequestParams> {
    let raw = std::env::var(MCP_SESSION_ENV).ok()?;
    std::env::remove_var(MCP_SESSION_ENV);
    let params = serde_json::from_str::<rmcp::model::InitializeRequestParams>(&raw).ok()?;
    let supported = rmcp::Service::supported_protocol_versions(server);
    supported
        .iter()
        .any(|version| version == &params.protocol_version)
        .then_some(params)
}

fn preserved_session(
    service: &rmcp::service::RunningService<rmcp::RoleServer, Server>,
) -> Option<String> {
    let info = service.peer_info()?;
    match serde_json::to_string(info.as_ref()) {
        Ok(json) => Some(json),
        Err(error) => {
            eprintln!("SIGHUP session preserve failed: {error}");
            None
        }
    }
}

#[cfg(unix)]
fn parent_is_alive(initial_parent_pid: u32) -> bool {
    initial_parent_pid <= 1 || std::os::unix::process::parent_id() == initial_parent_pid
}

#[cfg(unix)]
async fn parent_exited(initial_parent_pid: u32) {
    while parent_is_alive(initial_parent_pid) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Serves stdio retaining explicit 0.6.0 [`crate::state_bridge::InvocationSettings`] across requests.
pub async fn serve_with_state_config(
    root: PathBuf,
    db: PathBuf,
    cache_dir: Option<PathBuf>,
    settings: Option<crate::state_bridge::InvocationSettings>,
) -> Result<()> {
    #[cfg(unix)]
    {
        let reload_parent = std::env::var(MCP_RELOAD_PARENT_ENV).ok();
        std::env::remove_var(MCP_RELOAD_PARENT_ENV);
        let initial_parent_pid = reload_parent
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_else(std::os::unix::process::parent_id);
        tokio::select! {
            result = serve_session(root, db, cache_dir, settings, initial_parent_pid) => result,
            _ = parent_exited(initial_parent_pid) => {
                eprintln!(
                    "acdd: parent process ({initial_parent_pid}) exited; shutting down"
                );
                // Tokio's blocking stdin read cannot be cancelled; returning would hang
                // runtime shutdown while another process still holds the input open.
                std::process::exit(0)
            }
        }
    }
    #[cfg(not(unix))]
    {
        serve_session(root, db, cache_dir, settings).await
    }
}

async fn serve_session(
    root: PathBuf,
    db: PathBuf,
    cache_dir: Option<PathBuf>,
    settings: Option<crate::state_bridge::InvocationSettings>,
    #[cfg(unix)] initial_parent_pid: u32,
) -> Result<()> {
    let reload_signal = Arc::new(ReloadSignal::new());
    let reload_armed = Arc::clone(&reload_signal.armed);
    let reload_notify = Arc::clone(&reload_signal.notify);
    let mut server = Server::new(root, db).with_reload_signal(reload_signal);
    let startup_exe = server
        .startup_executable_path()
        .map(std::path::Path::to_path_buf);
    if let Some(settings) = &settings {
        server = server.with_invocation_settings(settings.clone());
    }
    if let Some(cache_dir) = cache_dir.as_ref() {
        server = server.with_cache_dir(cache_dir.clone());
    }
    let service = if let Some(params) = resumed_initialize_params(&server) {
        rmcp::service::serve_directly(
            server,
            super::response::stdio_with_flush_reload(reload_armed, Arc::clone(&reload_notify)),
            Some(params),
        )
    } else {
        server
            .serve(super::response::stdio_with_flush_reload(
                reload_armed,
                Arc::clone(&reload_notify),
            ))
            .await?
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let preserved = preserved_session(&service);
        let waiting = service.waiting();
        tokio::pin!(waiting);
        let try_reexec = || -> std::io::Result<()> {
            let exe = startup_exe.clone().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cannot re-exec without verified startup executable launch binding",
                )
            })?;
            let args: Vec<_> = std::env::args_os().skip(1).collect();
            let mut command = std::process::Command::new(exe);
            command
                .args(args)
                .env(MCP_RELOAD_PARENT_ENV, initial_parent_pid.to_string());
            match &preserved {
                Some(json) => {
                    command.env(MCP_SESSION_ENV, json);
                }
                None => {
                    command.env_remove(MCP_SESSION_ENV);
                }
            }
            if let Some(settings) = &settings {
                if let Some(config) = &settings.config {
                    command.env("ACDD_CONFIG", config);
                }
                if let Some(profile) = &settings.profile {
                    command.env("ACDD_PROFILE", profile);
                }
                if let Some(state_db) = &settings.state_db {
                    command.env("ACDD_STATE_DB", state_db);
                }
                command.env("ACDD_DB", &settings.code_db);
            }
            if let Some(cache_dir) = cache_dir.as_ref() {
                command.env("ACDD_EXTRACTION_CACHE_DIR", cache_dir);
            }
            Err(command.exec())
        };
        loop {
            tokio::select! {
                result = &mut waiting => {
                    result?;
                    break;
                }
                Some(()) = hangup.recv() => {
                    if let Err(error) = try_reexec() {
                        eprintln!("SIGHUP reload failed: {error}");
                    }
                }
                _ = reload_notify.notified() => {
                    if let Err(error) = try_reexec() {
                        eprintln!("binary-drift reload failed: {error}");
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (reload_armed, reload_notify);
        service.waiting().await?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[test]
fn parent_identity_contract() {
    let parent = std::os::unix::process::parent_id();
    let current = parent_is_alive(parent);
    let detached = [parent_is_alive(0), parent_is_alive(1)];
    let changed = parent_is_alive(u32::MAX);
    assert!(current);
    assert!(detached.into_iter().all(|alive| alive));
    assert!(!changed);
}

#[cfg(test)]
#[test]
fn worktree_name_contract() {
    let _cleanup = acdd_core::fs::ScopedTempDir::create("acdd-worktree-name").unwrap();
    let base = _cleanup.path();
    let root = base.join("repo");
    let external = base.join("unrelated-external");
    let linked_ws = base.join("linked-ws");
    let selected = root.join(".worktrees/049-selected");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::create_dir_all(&linked_ws).unwrap();

    let canon_root = canonicalize_existing_dir(&root).unwrap();
    let selected = canonicalize_existing_dir(&selected).unwrap();
    let canon_linked = canonicalize_existing_dir(&linked_ws).unwrap();

    let server = Server::new(root.clone(), root.join(".acdd/code-map.sqlite"));
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(None, None).unwrap().0,
        canon_root
    );
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(Some(""), None).unwrap().0,
        canon_root
    );
    assert_eq!(
        server.resolve_target_root_and_db_with_adapter(Some("   "), None).unwrap().0,
        canon_root
    );

    let result = resolve_explicit_worktree_with_linked(&root, &root, &[], "selected");
    let aliases: Vec<_> = ["049", "49", "049-sel", "9-selec"]
        .into_iter()
        .map(|alias| (alias, resolve_explicit_worktree_with_linked(&root, &root, &[], alias)))
        .collect();

    let outside_err = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        external.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(outside_err.to_string().contains("WORKTREE_NOT_FOUND"));

    let linked_by_alias = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        "backend",
    )
    .unwrap();
    assert_eq!(linked_by_alias, canon_linked);

    let linked_by_path = resolve_explicit_worktree_with_linked(
        &root,
        &root,
        &[("backend".to_owned(), canon_linked.clone())],
        canon_linked.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(linked_by_path, canon_linked);

    std::fs::create_dir_all(root.join(".worktrees/050-selected")).unwrap();
    let ambiguous = resolve_explicit_worktree_with_linked(&root, &root, &[], "selected");
    let missing = resolve_explicit_worktree_with_linked(&root, &root, &[], "absent");
    assert_eq!(result.unwrap(), selected);
    for (alias, result) in aliases {
        assert_eq!(
            result.unwrap_or_else(|error| panic!("{alias}: {error}")),
            selected
        );
    }
    assert!(ambiguous
        .unwrap_err()
        .to_string()
        .contains("WORKTREE_AMBIGUOUS"));
    assert!(missing
        .unwrap_err()
        .to_string()
        .contains("WORKTREE_NOT_FOUND"));

    #[cfg(unix)]
    {
        // Symlink inside `.worktrees` targeting an external directory must never be admitted.
        let escaped_link = root.join(".worktrees/999-escaped");
        std::os::unix::fs::symlink(&external, &escaped_link).unwrap();
        for probe in ["999-escaped", "999", ".worktrees/999-escaped"] {
            let err = resolve_explicit_worktree_with_linked(&root, &root, &[], probe).unwrap_err();
            assert!(err.to_string().contains("WORKTREE_NOT_FOUND"), "{probe}: {err}");
        }
        // Symlinked `.worktrees` directory itself must also be skipped.
        let sym_repo = base.join("sym-repo");
        std::fs::create_dir_all(&sym_repo).unwrap();
        assert!(acdd_core::git::git_command(&sym_repo, &["init"]).unwrap().status.success());
        std::os::unix::fs::symlink(&external, sym_repo.join(".worktrees")).unwrap();
        let sym_err =
            resolve_explicit_worktree_with_linked(&sym_repo, &sym_repo, &[], "unrelated-external")
                .unwrap_err();
        assert!(sym_err.to_string().contains("WORKTREE_NOT_FOUND"), "{sym_err}");
        let dotgit_err =
            resolve_explicit_worktree_with_linked(&sym_repo, &sym_repo, &[], ".git").unwrap_err();
        assert!(dotgit_err.to_string().contains("WORKTREE_NOT_FOUND"), "{dotgit_err}");
    }
}
