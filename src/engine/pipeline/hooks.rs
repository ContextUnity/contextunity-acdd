use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicUsize, Arc};

type StageCleanupHook = Box<dyn Fn(&Path) + Send + Sync>;

/// Test synchronization and fault-injection hooks for pipeline concurrency.
pub struct PipelineTestHooks {
    /// Overrides the resolved memory budget for in-process pipeline tests.
    pub memory_budget_override: Option<u64>,
    /// Overrides the extraction cache location for in-process pipeline tests.
    pub cache_dir_override: Option<PathBuf>,
    /// Forces producer, worker, and consumer synchronization.
    pub overlap: bool,
    /// Simulates SQLite consumer failure.
    pub fail_consumer: bool,
    /// Coordination barrier for concurrency overlap testing.
    pub overlap_barrier: std::sync::Barrier,
    /// Tracks count of participants that reached the overlap barrier.
    pub overlap_participants: AtomicUsize,
    /// Runs immediately before stage cleanup for filesystem fault injection.
    pub(super) before_stage_cleanup: Option<StageCleanupHook>,
    /// Runs a filesystem action before candidate database verification.
    pub before_candidate_verify: Option<StageCleanupHook>,
    pub(super) before_delta_persist: Option<StageCleanupHook>,
    pub(super) after_delta_failure: Option<StageCleanupHook>,
    pub(super) during_delta_verification: Option<StageCleanupHook>,
    pub(super) before_delta_receipt_publish: Option<StageCleanupHook>,
    pub(super) before_generation_lock: Option<StageCleanupHook>,
}

impl PipelineTestHooks {
    fn new_base(overlap: bool, fail_consumer: bool) -> Self {
        Self {
            overlap,
            fail_consumer,
            overlap_barrier: std::sync::Barrier::new(3),
            overlap_participants: AtomicUsize::new(0),
            before_stage_cleanup: None,
            before_candidate_verify: None,
            memory_budget_override: None,
            cache_dir_override: None,
            before_delta_persist: None,
            after_delta_failure: None,
            during_delta_verification: None,
            before_delta_receipt_publish: None,
            before_generation_lock: None,
        }
    }

    /// Creates overlap synchronization hooks.
    pub fn overlap() -> Arc<Self> {
        Arc::new(Self::new_base(true, false))
    }

    /// Creates consumer-failure fault injection hooks.
    pub fn fail_consumer() -> Arc<Self> {
        Arc::new(Self::new_base(false, true))
    }

    /// Injects a consumer failure and a filesystem action before stage cleanup.
    pub fn fail_consumer_before_cleanup(
        action: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::new_base(false, true);
        hooks.before_stage_cleanup = Some(Box::new(action));
        Arc::new(hooks)
    }

    /// Runs a filesystem action before cleanup after a normal build.
    pub fn before_cleanup(action: impl Fn(&Path) + Send + Sync + 'static) -> Arc<Self> {
        let mut hooks = Self::new_base(false, false);
        hooks.before_stage_cleanup = Some(Box::new(action));
        Arc::new(hooks)
    }

    /// Runs immediately before a cold build acquires the generation lock.
    pub fn before_generation_lock(action: impl Fn(&Path) + Send + Sync + 'static) -> Arc<Self> {
        let mut hooks = Self::new_base(false, false);
        hooks.before_generation_lock = Some(Box::new(action));
        Arc::new(hooks)
    }

    /// Runs test actions immediately before delta persistence and after its failure.
    #[cfg(test)]
    pub fn delta_failure(
        before: impl Fn(&Path) + Send + Sync + 'static,
        after: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::new_base(false, false);
        hooks.before_delta_persist = Some(Box::new(before));
        hooks.after_delta_failure = Some(Box::new(after));
        Arc::new(hooks)
    }

    /// Adds a filesystem action before rollback receipt publication.
    #[cfg(test)]
    pub fn delta_failure_before_publication(
        after: impl Fn(&Path) + Send + Sync + 'static,
        before_publication: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::new_base(false, false);
        hooks.before_delta_persist = Some(Box::new(|_| {}));
        hooks.after_delta_failure = Some(Box::new(after));
        hooks.before_delta_receipt_publish = Some(Box::new(before_publication));
        Arc::new(hooks)
    }

    /// Injects an identity change between rollback recovery samples.
    #[cfg(test)]
    pub fn delta_failure_during_verification(
        after: impl Fn(&Path) + Send + Sync + 'static,
        during: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::new_base(false, false);
        hooks.before_delta_persist = Some(Box::new(|_| {}));
        hooks.after_delta_failure = Some(Box::new(after));
        hooks.during_delta_verification = Some(Box::new(during));
        Arc::new(hooks)
    }
}
