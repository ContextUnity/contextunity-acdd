//! Shared in-flight pipeline coordination structs for cold extraction and staging.

use super::budget::{ByteBudget, MemoryPermit};
use super::hooks::PipelineTestHooks;
use acdd_core::config::Adapter;
use acdd_index::{
    db::writer::{cache::ExtractionCache, StagedFilePayload},
    languages::dependency_registry::DependencyRegistry,
    models::FileEntry,
    scanner::{self, BuildCandidate},
    typed_facts::SymbolCatalog,
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender},
        Arc, Mutex,
    },
};

pub(super) struct PipelineControl {
    pub(super) cancelled: AtomicBool,
    failure: Mutex<Option<String>>,
    pub(super) budget: Arc<ByteBudget>,
}

impl PipelineControl {
    pub(super) fn new(budget: Arc<ByteBudget>) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            failure: Mutex::new(None),
            budget,
        }
    }

    pub(super) fn cancel(&self, failure: impl std::fmt::Display) {
        self.cancelled.store(true, Ordering::Release);
        let mut current = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.is_none() {
            *current = Some(failure.to_string());
        }
        drop(current);
        self.budget.wake_waiters();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Default)]
pub(super) struct PipelineCounters {
    pub(super) extracted_files: AtomicUsize,
    pub(super) ingested_files: AtomicUsize,
    pub(super) extraction_cache_hits: AtomicUsize,
    pub(super) extraction_nanos: AtomicU64,
    pub(super) ingestion_nanos: AtomicU64,
}

pub(super) struct PipelineWork {
    pub(super) sequence: usize,
    pub(super) candidate: BuildCandidate,
    pub(super) permit: MemoryPermit,
}

pub(super) struct PipelineItem {
    pub(super) file: FileEntry,
    pub(super) catalog: SymbolCatalog,
    pub(super) encoded_facts: Vec<u8>,
    pub(super) skeleton_blob: Option<Vec<u8>>,
    pub(super) local_facts_leaf: (String, String),
    pub(super) storage_rows: acdd_index::db::writer::cache::PreparedCatalogStorage,
    pub(super) permit: MemoryPermit,
}

pub(super) struct ExtractionWorkerContext {
    pub(super) root: PathBuf,
    pub(super) adapter: Arc<Adapter>,
    pub(super) dependencies: Arc<DependencyRegistry>,
    pub(super) extraction_cache: Arc<ExtractionCache>,
    pub(super) effective_ruleset_hash: Arc<str>,
    pub(super) work_rx: Arc<Mutex<Receiver<PipelineWork>>>,
    pub(super) result_tx: SyncSender<std::result::Result<PipelineItem, String>>,
    pub(super) control: Arc<PipelineControl>,
    pub(super) counters: Arc<PipelineCounters>,
    pub(super) test_hooks: Option<Arc<PipelineTestHooks>>,
}

pub(super) struct ColdPipelineInputs<'a> {
    pub(super) root: &'a Path,
    pub(super) payload_spool_path: &'a Path,
    pub(super) adapter: &'a Adapter,
    pub(super) dependencies: Arc<DependencyRegistry>,
    pub(super) extraction_cache: Arc<ExtractionCache>,
    pub(super) effective_ruleset_hash: Arc<str>,
    pub(super) connection: Connection,
    pub(super) budget_bytes: u64,
    pub(super) candidates: Vec<BuildCandidate>,
    pub(super) scan: scanner::BuildStreamReport,
    pub(super) scan_ms: f64,
    pub(super) test_hooks: Option<Arc<PipelineTestHooks>>,
}

pub(super) struct PipelineConsumerOutput {
    pub(super) connection: Connection,
    pub(super) entries: Vec<FileEntry>,
    pub(super) catalog: SymbolCatalog,
    pub(super) staged_payloads: BTreeMap<String, StagedFilePayload>,
}

#[derive(Clone)]
pub(super) struct PipelineMetrics {
    pub(super) budget_bytes: u64,
    pub(super) peak_in_flight_bytes: u64,
    pub(super) max_item_bytes: u64,
    pub(super) oversized_admissions: u64,
    pub(super) extracted_files: usize,
    pub(super) ingested_files: usize,
    pub(super) extraction_cache_hits: usize,
    pub(super) scan_ms: f64,
    pub(super) extraction_ms: f64,
    pub(super) sqlite_ingest_ms: f64,
    pub(super) scanned_files: usize,
    pub(super) scanned_bytes: u64,
}
