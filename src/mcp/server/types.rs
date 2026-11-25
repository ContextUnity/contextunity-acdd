//! Core MCP server handle, connection slot state, adapter loading, and worktree path rebasing.

use acdd_core::{errors::AcddError, fs::canonicalize_macos_var};
use acdd_index::{
    db::{cache, cache::Identity},
    scanner,
};
use anyhow::Result;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Cached per-request workspace adapter resolution outcome.
#[derive(Clone)]
pub(crate) enum RequestAdapter {
    Loaded(Arc<scanner::Adapter>),
    Failed {
        code: Option<acdd_core::errors::ErrorCode>,
        message: String,
    },
}

#[derive(Clone, serde::Serialize)]
pub(super) struct Freshness {
    pub(super) status: &'static str,
    pub(super) output_root: String,
    pub(super) corpus_hash: String,
    pub(super) checked_at_unix_ms: u128,
    pub(super) files_checked: usize,
    pub(super) inventory_scan_ms: f64,
    pub(super) refresh: &'static str,
}

/// Cached read-only SQLite connection and workspace inventory freshness metadata.
pub(crate) struct CachedConnection {
    pub(super) identity: Identity,
    pub(super) connection: Option<rusqlite::Connection>,
    pub(super) adapter_digest: String,
    pub(super) adapter_path: Option<PathBuf>,
    pub(super) workspace_root: PathBuf,
    pub(super) database_path: PathBuf,
    pub(super) adapter_roots: Vec<PathBuf>,
    pub(super) linked_workspaces: Vec<scanner::LinkedWorkspace>,
    pub(super) inventory_checked_at: Option<Instant>,
    pub(super) freshness: Option<Freshness>,
    pub(super) diagnostics: Vec<String>,
}

/// Guarded optional reader slot shared across MCP query invocations.
pub(crate) type ConnectionSlot = Option<CachedConnection>;

thread_local! {
    pub(super) static ADMITTING_SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) struct AdmissionGuard(pub(super) usize);
impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        ADMITTING_SLOT.with(|slot| slot.set(self.0));
    }
}

pub(super) fn reader_evictor(slot: &Arc<Mutex<ConnectionSlot>>) -> Arc<cache::IdleReader> {
    let slot = Arc::downgrade(slot);
    Arc::new(move |db| {
        let Some(slot) = slot.upgrade() else {
            return Ok(());
        };
        match slot.try_lock() {
            Ok(mut slot) => {
                if slot
                    .as_ref()
                    .is_some_and(|cached| cache::registry_path(&cached.database_path) == db)
                {
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

pub(super) fn rebase_path(
    path: &std::path::Path,
    from_root: &std::path::Path,
    to_root: &std::path::Path,
) -> PathBuf {
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

pub(super) fn rebase_invocation_settings(
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

/// MCP server handle bound to a workspace root, code-map SQLite path, and shared reader pool.
#[derive(Clone)]
pub struct Server {
    /// Primary workspace root bound at server startup (or selected per-request worktree).
    pub root: PathBuf,
    /// SQLite code-map database path backing queries for `root`.
    pub db: PathBuf,
    /// Shared lazily-admitted SQLite reader connection slot for warm queries.
    pub(crate) connection: Arc<Mutex<ConnectionSlot>>,
    pub(super) reader_evictor: Arc<cache::IdleReader>,
    pub(super) request_adapter: Option<RequestAdapter>,
    pub(super) request_workspace_root: Option<PathBuf>,
    pub(super) extraction_cache_dir: Option<PathBuf>,
    pub(crate) invocation: Option<Arc<crate::state_bridge::InvocationSettings>>,
    pub(crate) session_fingerprint: Option<String>,
    #[cfg(test)]
    pub(crate) extra_domain_providers: Vec<Arc<dyn acdd_core::ports::DomainProvider>>,
    pub(super) startup_executable: std::result::Result<acdd_core::fs::ExecutableIdentity, String>,
    pub(super) reload_signal: Option<Arc<ReloadSignal>>,
}

pub(super) struct ReloadSignal {
    pending: std::sync::atomic::AtomicBool,
    pub(super) armed: Arc<std::sync::atomic::AtomicBool>,
    pub(super) notify: Arc<tokio::sync::Notify>,
}

impl ReloadSignal {
    pub(super) fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(super) fn mark_pending(&self) {
        self.pending
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn arm_pending(&self) {
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

    /// Binds a new MCP [`Server`] handle to `root` and its code-map database path `db`.
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
            #[cfg(test)]
            extra_domain_providers: Vec::new(),
            startup_executable: acdd_core::fs::ExecutableIdentity::current()
                .map_err(|err| err.to_string()),
            reload_signal: None,
        }
    }

    pub(super) fn startup_executable_path(&self) -> Option<&std::path::Path> {
        self.startup_executable
            .as_ref()
            .ok()
            .map(|identity| identity.path.as_path())
    }

    pub(super) fn with_reload_signal(mut self, signal: Arc<ReloadSignal>) -> Self {
        self.reload_signal = Some(signal);
        self
    }

    pub(crate) fn arm_pending_reload(&self) {
        if let Some(signal) = &self.reload_signal {
            signal.arm_pending();
        }
    }

    pub(super) fn ensure_current_binary_before_rebuild(&self) -> Result<()> {
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
        #[cfg(test)]
        let extras = self.extra_domain_providers.clone();
        #[cfg(not(test))]
        let extras = Vec::new();
        crate::state_bridge::build_domain_providers(root, adapter, code_db, extras)
    }

    pub(crate) fn load_adapter(&self, root: &std::path::Path) -> Result<scanner::Adapter> {
        let root = scanner::canonical_root(root)?;
        let settings = self
            .invocation
            .as_ref()
            .map(|settings| rebase_invocation_settings(settings, &root));
        Ok(scanner::load_adapter(
            &root,
            settings
                .as_ref()
                .and_then(|settings| settings.config.as_deref()),
        )?)
    }

    /// Supplies explicit session metadata without exposing it to tool JSON.
    pub fn with_session_fingerprint(mut self, session_fingerprint: String) -> Self {
        self.session_fingerprint = Some(session_fingerprint);
        self
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
            #[cfg(test)]
            extra_domain_providers: self.extra_domain_providers.clone(),
            startup_executable: self.startup_executable.clone(),
            reload_signal: self.reload_signal.clone(),
        }
    }

    pub(crate) fn adapter_snapshot(&self) -> Result<Arc<scanner::Adapter>> {
        match &self.request_adapter {
            Some(RequestAdapter::Loaded(adapter)) => Ok(Arc::clone(adapter)),
            Some(RequestAdapter::Failed { code, message }) => match code {
                Some(code) => Err(AcddError::new(*code, message.clone()).into()),
                None => anyhow::bail!("{message}"),
            },
            None => self.load_adapter(&self.root).map(Arc::new),
        }
    }
}
