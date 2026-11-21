//! Indexing pipeline orchestration for cold builds and incremental delta runs.

use acdd_core::{
    config::{
        self, checked_child, load_adapter, resolve_code_db_path, resolve_file_path,
        resolve_memory_budget_bytes, resolve_state_db_path, Adapter, ENGINE_SCHEMA_VERSION,
    },
    errors::{ErrorCode, StateError},
    fs::canonical_root,
};
use acdd_index::{
    ast, commitments,
    db::{
        cache, commitments as db_commitments, reader as db_reader,
        writer::{
            self as db_writer,
            cache::{self as extraction_cache, ExtractionCache},
            delta_persist, PayloadStore, StagedFilePayload,
        },
    },
    docs,
    languages::{self, dependency_registry::DependencyRegistry, module_name},
    linker,
    models::{stable_hash64, Edge, Facts, FileEntry, Node},
    scanner::{self, BuildCandidate},
    semantic::SymbolCatalog,
    typed_facts::{decode_facts, TypedFacts},
    INDEX_SEMANTICS_VERSION,
};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::Instant,
};

/// The indexing operation being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildMode {
    /// Build an index from a complete scan.
    Cold,
    /// Update an existing index from changed paths.
    IncrementalDelta,
    /// Verify an existing generation without rebuilding it.
    FastVerifyOnly,
}

impl BuildMode {
    /// Returns static string identifier of the build mode.
    pub const fn as_str(&self) -> &'static str {
        match self {
            BuildMode::Cold => "Cold",
            BuildMode::IncrementalDelta => "IncrementalDelta",
            BuildMode::FastVerifyOnly => "FastVerifyOnly",
        }
    }

    /// Returns true if the build mode is Cold.
    pub fn is_cold(&self) -> bool {
        matches!(self, BuildMode::Cold)
    }

    /// Returns true if the build mode is IncrementalDelta.
    pub fn is_delta(&self) -> bool {
        matches!(self, BuildMode::IncrementalDelta)
    }

    /// Returns true if the build mode is FastVerifyOnly.
    pub fn is_fast_verify(&self) -> bool {
        matches!(self, BuildMode::FastVerifyOnly)
    }
}

impl std::fmt::Display for BuildMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Dispatches indexing execution according to the requested `BuildMode`.
pub fn execute_with_mode(
    mode: BuildMode,
    root: &Path,
    output: &Path,
    modified: Option<&[PathBuf]>,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<Value> {
    match mode {
        BuildMode::Cold => {
            cold::build_inner(root, output, adapter_path, cache_dir, None)
        }
        BuildMode::IncrementalDelta => {
            let paths = modified.unwrap_or(&[]);
            delta::delta_inner(root, output, paths, adapter_path, cache_dir, None)
        }
        BuildMode::FastVerifyOnly => {
            let conn = db_reader::open(output, root)?;
            db_commitments::verify(&conn)?;
            let root_str: String = conn.query_row(
                "SELECT value FROM metadata WHERE key='output_root'",
                [],
                |row| row.get(0),
            )?;
            Ok(json!({
                "build_mode": BuildMode::FastVerifyOnly.as_str(),
                "output_root": root_str,
                "verified": true,
            }))
        }
    }
}

pub use scanner::ColdScanResult;

mod budget;
mod delta;
mod hydration;
mod invalidation;
pub use delta::{delta, delta_with_cache_dir, delta_with_selection, delta_with_test_hooks};
use hydration::extract_typed_with_cache;
use invalidation::*;
mod cold;
mod staging;
pub use cold::{build, build_with_cache_dir, build_with_test_hooks, extract};
use cold::*;
use staging::consume_pipeline_results;
mod hooks;
pub use hooks::PipelineTestHooks;
use budget::{ByteBudget, MemoryPermit};

static NONCE: AtomicU64 = AtomicU64::new(0);
const EXTRACTION_MEMORY_MULTIPLIER: u64 = 8;

struct PipelineControl {
    cancelled: AtomicBool,
    failure: Mutex<Option<String>>,
    budget: Arc<ByteBudget>,
}

impl PipelineControl {
    fn new(budget: Arc<ByteBudget>) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            failure: Mutex::new(None),
            budget,
        }
    }

    fn cancel(&self, failure: impl std::fmt::Display) {
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

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Default)]
struct PipelineCounters {
    extracted_files: AtomicUsize,
    ingested_files: AtomicUsize,
    extraction_cache_hits: AtomicUsize,
    extraction_nanos: AtomicU64,
    ingestion_nanos: AtomicU64,
}

struct PipelineWork {
    sequence: usize,
    candidate: BuildCandidate,
    permit: MemoryPermit,
}

struct PipelineItem {
    file: FileEntry,
    catalog: SymbolCatalog,
    encoded_facts: Vec<u8>,
    storage_rows: Vec<u8>,
    permit: MemoryPermit,
}

struct ExtractionWorkerContext {
    root: PathBuf,
    adapter: Arc<Adapter>,
    dependencies: Arc<DependencyRegistry>,
    extraction_cache: Arc<ExtractionCache>,
    effective_ruleset_hash: Arc<str>,
    work_rx: Arc<Mutex<Receiver<PipelineWork>>>,
    result_tx: SyncSender<std::result::Result<PipelineItem, String>>,
    control: Arc<PipelineControl>,
    counters: Arc<PipelineCounters>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
}

struct ColdPipelineInputs<'a> {
    root: &'a Path,
    payload_spool_path: &'a Path,
    adapter: &'a Adapter,
    dependencies: Arc<DependencyRegistry>,
    extraction_cache: Arc<ExtractionCache>,
    effective_ruleset_hash: Arc<str>,
    connection: Connection,
    budget_bytes: u64,
    candidates: Vec<BuildCandidate>,
    scan: scanner::BuildStreamReport,
    scan_ms: f64,
    test_hooks: Option<Arc<PipelineTestHooks>>,
}

struct PipelineConsumerOutput {
    connection: Connection,
    entries: Vec<FileEntry>,
    catalog: SymbolCatalog,
    staged_payloads: BTreeMap<String, StagedFilePayload>,
}

#[derive(Clone)]
struct PipelineMetrics {
    budget_bytes: u64,
    peak_in_flight_bytes: u64,
    max_item_bytes: u64,
    oversized_admissions: u64,
    extracted_files: usize,
    ingested_files: usize,
    extraction_cache_hits: usize,
    scan_ms: f64,
    extraction_ms: f64,
    sqlite_ingest_ms: f64,
    scanned_files: usize,
    scanned_bytes: u64,
}

#[derive(Debug)]
/// Reports that a source file changed after its inventory digest was scanned.
pub struct SourceSnapshotMismatch(pub String);

impl std::fmt::Display for SourceSnapshotMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SourceSnapshotMismatch {}

/// Assigns the full-build component ownership graph to extracted typed facts.
///
/// Replaces existing component nodes and edges using the same deterministic
/// owner selection as a filesystem full build. Returns component owner paths.
pub fn components(facts: &mut BTreeMap<String, TypedFacts>) -> BTreeSet<String> {
    let owners = component_owners(facts.iter().filter_map(|(path, f)| {
        f.nodes
            .iter()
            .any(|n| n.kind == "module")
            .then_some(path.as_str())
    }));
    assign_components(facts, &owners);
    owners.into_values().collect()
}

fn component_owners<'a>(paths: impl Iterator<Item = &'a str>) -> BTreeMap<String, String> {
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for path in paths {
        let root = path.split_once('/').map_or(".", |(root, _)| root);
        owners
            .entry(root.to_owned())
            .and_modify(|owner| {
                if path < owner.as_str() {
                    *owner = path.to_owned();
                }
            })
            .or_insert_with(|| path.to_owned());
    }
    owners
}

fn assign_file_component(
    path: &str,
    f: &mut TypedFacts,
    owners: Option<&BTreeMap<String, String>>,
) {
    f.facts.retain_nodes(|n| n.kind != "component");
    f.edges.retain(|e| !e.src.starts_with("component:"));
    if !f.nodes.iter().any(|n| n.kind == "module") {
        return;
    }
    let root = path.split_once('/').map_or(".", |(root, _)| root);
    let id = format!("component:{root}");
    if owners.is_some_and(|owners| owners.get(root).map(String::as_str) == Some(path)) {
        f.nodes.push(Node {
            native_semantic: None,
            id: id.clone(),
            kind: "component".into(),
            name: root.into(),
            qualname: root.into(),
            path: root.into(),
            line: 1,
            end_line: 1,
            is_test: false,
            language: "workspace".into(),
            generated: false,
            details: serde_json::Value::Null,
        });
    }
    f.edges.push(Edge {
        src: id,
        dst: format!("module:{path}"),
        kind: "contains".into(),
        path: path.to_owned(),
        line: 1,
        evidence: "component ownership".into(),
        confidence: "exact".into(),
    });
}

fn assign_components(facts: &mut BTreeMap<String, TypedFacts>, owners: &BTreeMap<String, String>) {
    for (path, f) in facts {
        assign_file_component(path, f, Some(owners));
    }
}

/// Validates that a code map database output target does not point to or overwrite
/// a task tracking database.
///
/// # Errors
/// Returns an error if:
/// - The target filename matches known task database names (e.g. `tasks.sqlite`, `tasks.db`).
/// - The target matches the configured `tasks_db` path in `acdd.yaml`.
/// - The target file exists and contains task tracking tables (e.g. `tasks`, `task_gates`, etc.).
pub fn validate_code_map_target(root: &Path, output: &Path) -> Result<()> {
    // Resolve output path to absolute matching atomic_build (cwd-based for relative paths)
    let output_abs = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| root.to_path_buf())
            .join(output)
    };
    let norm_output = acdd_core::fs::normalize_path(&output_abs);

    let file_name = norm_output
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();

    // 1. Rejection by filename pattern (case-insensitive and covering common variants)
    let file_name_lower = file_name.to_ascii_lowercase();
    if is_reserved_state_or_task_filename(&file_name_lower) {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to use task/state database path '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }

    // 2. Rejection by configured state_db (and legacy tasks_db if pre-migration) paths
    let base = acdd_core::git::primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    for candidate_root in [root, base.as_path()] {
        let cfg_path = candidate_root.join("acdd.yaml");
        if cfg_path.is_file() {
            if let Ok(raw) = fs::read_to_string(&cfg_path) {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&raw) {
                    if let Some(legacy_tasks_db) = yaml.get("tasks_db").and_then(|v| v.as_str()) {
                        let task_db_path = if Path::new(legacy_tasks_db).is_absolute() {
                            PathBuf::from(legacy_tasks_db)
                        } else {
                            base.join(legacy_tasks_db)
                        };
                        if acdd_core::fs::same_filesystem_path(&output_abs, &task_db_path) {
                            bail!(
                                "TASK_DATABASE_COLLISION: Refusing to use configured tasks database '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
                                output.display()
                            );
                        }
                    }
                }
            }
        }
    }
    let state_adapter = if root.join("acdd.yaml").exists() {
        Some(load_adapter(root, None))
    } else if base != root && base.join("acdd.yaml").exists() {
        Some(load_adapter(&base, None))
    } else {
        None
    };
    let state_db_path = match state_adapter {
        Some(Ok(adapter)) => resolve_state_db_path(&base, &adapter),
        Some(Err(error)) => {
            bail!(
                "TASK_DATABASE_COLLISION: Cannot validate configured state_db because acdd.yaml is invalid ({error}). Refusing code map output '{}'.",
                output.display()
            );
        }
        None => resolve_state_db_path(&base, &Adapter::default()),
    };
    if acdd_core::fs::same_filesystem_path(&output_abs, &state_db_path)
        || (base != root
            && !Path::new(&state_db_path).is_absolute()
            && acdd_core::fs::same_filesystem_path(&output_abs, &root.join(&state_db_path)))
    {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to use configured state database '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }

    // 3. Rejection by existing task/state table schema (with fail-closed validation)
    match fs::symlink_metadata(&output_abs) {
        Ok(meta) => {
            if !meta.is_file() {
                bail!(
                    "code map target must be a regular file: '{}'",
                    output.display()
                );
            }
            if meta.len() == 0 {
                return Ok(());
            }
            let header = db_reader::validate_database_header(&output_abs);
            if let Err(error) = &header {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() != std::io::ErrorKind::UnexpectedEof)
                {
                    bail!(
                        "TASK_DATABASE_COLLISION: Cannot read database at '{}' ({error}). Refusing to overwrite potentially active task database.",
                        output.display()
                    );
                }
            }
            if header.is_ok() {
                let conn = db_reader::open_raw_confined_readonly(&output_abs);
                match conn {
                    Ok(conn) => {
                        let is_task_db = conn.query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM sqlite_schema
                                WHERE type='table' AND name IN (
                                    'tasks', 'task_gates', 'task_claims', 'task_subtasks',
                                    'task_blackboard', 'task_findings', 'task_submissions',
                                    'task_revisions', 'task_store_metadata',
                                    'runs', 'gate_nodes', 'task_dependencies', 'subtasks',
                                    'gate_claims', 'gate_traces', 'gate_artifacts',
                                    'blackboard', 'run_tombstones'
                                )
                            )",
                            [],
                            |r| r.get::<_, bool>(0),
                        );
                        match is_task_db {
                            Ok(true) => {
                                bail!(
                                    "TASK_DATABASE_COLLISION: Refusing to overwrite task database at '{}' with code map index. The target database contains active task tracking tables. The code map database must be separate (default: '.acdd/code-map.sqlite').",
                                    output.display()
                                );
                            }
                            Ok(false) => {}
                            Err(err) => {
                                bail!(
                                    "TASK_DATABASE_COLLISION: Cannot inspect SQLite database schema at '{}' ({}). Refusing to overwrite potentially active task database.",
                                    output.display(),
                                    err
                                );
                            }
                        }
                    }
                    Err(err) => {
                        bail!(
                            "TASK_DATABASE_COLLISION: Cannot inspect SQLite database at '{}' ({}). Refusing to overwrite potentially active task database.",
                            output.display(),
                            err
                        );
                    }
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            bail!(
                "cannot access code map target '{}': {err}",
                output.display()
            );
        }
    }

    Ok(())
}

fn is_reserved_state_or_task_filename(file_name_lower: &str) -> bool {
    matches!(
        file_name_lower,
        "tasks.sqlite"
            | "tasks.db"
            | "tasks.sqlite3"
            | "state.sqlite"
            | "state.db"
            | "state.sqlite3"
    ) || file_name_lower.starts_with("tasks.sqlite")
        || file_name_lower.ends_with(".tasks.sqlite")
        || file_name_lower.starts_with("state.sqlite")
        || file_name_lower.ends_with(".state.sqlite")
}

fn ensure_no_publication_sidecars(output: &Path, allow_files: bool) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = output.as_os_str().to_owned();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match fs::symlink_metadata(&sidecar) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("inspect sidecar {}", sidecar.display()))
            }
            Ok(metadata) if allow_files && metadata.is_file() => {}
            Ok(_) => bail!(
                "cannot replace database with existing sidecar {}",
                sidecar.display()
            ),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Incremental Delta Indexing
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "lang-rust"))]
mod tests;
