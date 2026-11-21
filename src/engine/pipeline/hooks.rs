use super::*;

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
    fn new_base(overlap: bool, fail_consumer: bool) -> Arc<Self> {
        Arc::new(Self {
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
        })
    }

    /// Creates overlap synchronization hooks.
    pub fn overlap() -> Arc<Self> {
        Self::new_base(true, false)
    }

    /// Creates consumer-failure fault injection hooks.
    pub fn fail_consumer() -> Arc<Self> {
        Self::new_base(false, true)
    }

    /// Injects a consumer failure and a filesystem action before stage cleanup.
    pub fn fail_consumer_before_cleanup(
        action: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::fail_consumer();
        Arc::get_mut(&mut hooks)
            .expect("new hook is uniquely owned")
            .before_stage_cleanup = Some(Box::new(action));
        hooks
    }

    /// Runs a filesystem action before cleanup after a normal build.
    pub fn before_cleanup(action: impl Fn(&Path) + Send + Sync + 'static) -> Arc<Self> {
        let mut hooks = Self::fail_consumer_before_cleanup(action);
        Arc::get_mut(&mut hooks)
            .expect("new hook is uniquely owned")
            .fail_consumer = false;
        hooks
    }

    /// Runs immediately before a cold build acquires the generation lock.
    pub fn before_generation_lock(action: impl Fn(&Path) + Send + Sync + 'static) -> Arc<Self> {
        let mut hooks = Self::before_cleanup(|_| {});
        Arc::get_mut(&mut hooks)
            .expect("new hook is uniquely owned")
            .before_generation_lock = Some(Box::new(action));
        hooks
    }

    /// Runs test actions immediately before delta persistence and after its failure.
    pub fn delta_failure(
        before: impl Fn(&Path) + Send + Sync + 'static,
        after: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::fail_consumer();
        let hooks_mut = Arc::get_mut(&mut hooks).expect("new hook is uniquely owned");
        hooks_mut.fail_consumer = false;
        hooks_mut.before_delta_persist = Some(Box::new(before));
        hooks_mut.after_delta_failure = Some(Box::new(after));
        hooks
    }

    /// Adds a filesystem action before rollback receipt publication.
    pub fn delta_failure_before_publication(
        after: impl Fn(&Path) + Send + Sync + 'static,
        before_publication: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::delta_failure(|_| {}, after);
        Arc::get_mut(&mut hooks)
            .expect("new hook is uniquely owned")
            .before_delta_receipt_publish = Some(Box::new(before_publication));
        hooks
    }

    /// Injects an identity change between rollback recovery samples.
    pub fn delta_failure_during_verification(
        after: impl Fn(&Path) + Send + Sync + 'static,
        during: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let mut hooks = Self::delta_failure(|_| {}, after);
        Arc::get_mut(&mut hooks)
            .expect("new hook is uniquely owned")
            .during_delta_verification = Some(Box::new(during));
        hooks
    }
}

