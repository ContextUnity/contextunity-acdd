use super::budget::ByteBudget;
use super::hooks::PipelineTestHooks;
use super::hydration::{extraction_cache_key_and_limit, touch_cached_facts};
use super::runtime::{
    ColdPipelineInputs, ExtractionWorkerContext, PipelineConsumerOutput, PipelineControl,
    PipelineCounters, PipelineItem, PipelineMetrics, PipelineWork,
};
use super::shared::{
    assign_file_component, components, extract_typed_source_with_digest, hash_source_bytes,
    manage_publication_sidecars, open_and_read_verified_source, BuildMode, SidecarDisposition,
    SourceSnapshotMismatch, EXTRACTION_MEMORY_MULTIPLIER, NONCE,
};
use super::staging::consume_pipeline_results;
use acdd_core::{
    config::{
        self, load_adapter, resolve_code_db_path, resolve_memory_budget_bytes,
        resolve_state_db_path, Adapter,
    },
    errors::{ErrorCode, StateError},
    fs::canonical_root,
};
use acdd_index::{
    ast,
    db::{
        cache, reader as db_reader,
        writer::{
            self as db_writer,
            cache::{self as extraction_cache, ExtractionCache},
        },
    },
    languages::dependency_registry::DependencyRegistry,
    linker,
    models::FileEntry,
    scanner::{self, BuildCandidate},
    typed_facts::{SymbolCatalog, TypedFacts},
};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
    thread,
    time::Instant,
};

type CandidateExtraction = (
    FileEntry,
    TypedFacts,
    bool,
    Option<Vec<u8>>,
    Option<Vec<acdd_index::models::Edge>>,
    String,
);

pub(super) fn extract_candidate(
    root: &Path,
    candidate: BuildCandidate,
    adapter: &Adapter,
    dependencies: &DependencyRegistry,
    extraction_cache: &ExtractionCache,
    effective_ruleset_hash: &str,
    source: &mut String,
) -> Result<CandidateExtraction> {
    let source_file = open_and_read_verified_source(
        root,
        adapter,
        &candidate.path,
        candidate.bytes,
        Some(candidate.bytes),
        source,
    )?;
    if !candidate.matches_metadata(&source_file.metadata()?) {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            candidate.path
        ))
        .into());
    }
    let source_digest = hash_source_bytes(source);
    let file = candidate.into_entry(source_digest);
    let (key, cache_limit) = extraction_cache_key_and_limit(
        &file.digest,
        &file.path,
        &file.language,
        effective_ruleset_hash,
        file.bytes,
        adapter.limits.max_file_bytes,
    );
    if let Some((mut facts, encoded, components)) =
        extraction_cache.load_for_build(&key, cache_limit)
    {
        touch_cached_facts(&mut facts, file.mtime_ns);
        return Ok((file, facts, true, encoded, components, key));
    }
    let spec_paths = adapter.spec_paths();
    let zones = adapter.architecture.zone_names();
    let facts = extract_typed_source_with_digest(
        &file,
        source,
        &file.digest,
        dependencies,
        &spec_paths,
        Some(&zones),
    )?;
    Ok((file, facts, false, None, None, key))
}

pub(super) fn run_cold_pipeline(
    inputs: ColdPipelineInputs<'_>,
) -> Result<(PipelineConsumerOutput, PipelineMetrics)> {
    let ColdPipelineInputs {
        root,
        payload_spool_path,
        adapter,
        dependencies,
        extraction_cache,
        effective_ruleset_hash,
        connection,
        budget_bytes,
        candidates,
        scan,
        scan_ms,
        test_hooks,
    } = inputs;
    let worker_count = rayon::current_num_threads().max(1);
    let channel_capacity = worker_count.saturating_mul(2).max(1);
    let byte_budget = ByteBudget::new(budget_bytes);
    let control = Arc::new(PipelineControl::new(Arc::clone(&byte_budget)));
    let counters = Arc::new(PipelineCounters::default());
    let (work_tx, work_rx) = mpsc::sync_channel::<PipelineWork>(channel_capacity);
    let work_rx = Arc::new(Mutex::new(work_rx));
    let (result_tx, result_rx) =
        mpsc::sync_channel::<std::result::Result<PipelineItem, String>>(channel_capacity);

    let consumer_control = Arc::clone(&control);
    let consumer_counters = Arc::clone(&counters);
    let consumer_hooks = test_hooks.clone();
    let consumer_payload_spool_path = payload_spool_path.to_path_buf();
    let consumer = thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            consume_pipeline_results(
                connection,
                result_rx,
                &consumer_payload_spool_path,
                (budget_bytes / 16).min(128 * 1024 * 1024),
                consumer_control.as_ref(),
                consumer_counters.as_ref(),
                consumer_hooks.as_deref(),
            )
        }));
        match outcome {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => {
                consumer_control.cancel(format!("cold pipeline SQLite consumer failed: {error}"));
                Err(error)
            }
            Err(_) => {
                consumer_control.cancel("cold pipeline SQLite consumer panicked");
                Err(anyhow::anyhow!("cold pipeline SQLite consumer panicked"))
            }
        }
    });

    let root = root.to_path_buf();
    let adapter = Arc::new(adapter.clone());
    let mut workers = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let worker_root = root.clone();
        let worker_adapter = Arc::clone(&adapter);
        let worker_dependencies = Arc::clone(&dependencies);
        let worker_extraction_cache = Arc::clone(&extraction_cache);
        let worker_ruleset_hash = Arc::clone(&effective_ruleset_hash);
        let worker_rx = Arc::clone(&work_rx);
        let worker_tx = result_tx.clone();
        let worker_control = Arc::clone(&control);
        let panic_control = Arc::clone(&worker_control);
        let worker_counters = Arc::clone(&counters);
        let worker_hooks = test_hooks.clone();
        workers.push(thread::spawn(move || {
            let context = ExtractionWorkerContext {
                root: worker_root,
                adapter: worker_adapter,
                dependencies: worker_dependencies,
                extraction_cache: worker_extraction_cache,
                effective_ruleset_hash: worker_ruleset_hash,
                work_rx: worker_rx,
                result_tx: worker_tx,
                control: worker_control,
                counters: worker_counters,
                test_hooks: worker_hooks,
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| extraction_worker(context)));
            if outcome.is_err() {
                panic_control.cancel("cold pipeline extraction worker panicked");
            }
        }));
    }
    drop(result_tx);
    drop(work_rx);

    let producer_control = Arc::clone(&control);
    let producer_budget = Arc::clone(&byte_budget);
    let producer_hooks = test_hooks.clone();
    let producer = thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            for (current_sequence, candidate) in candidates.into_iter().enumerate() {
                let estimate = candidate
                    .bytes
                    .saturating_mul(EXTRACTION_MEMORY_MULTIPLIER)
                    .max(1);
                let permit = producer_budget
                    .acquire(estimate, &producer_control.cancelled)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                work_tx
                    .send(PipelineWork {
                        sequence: current_sequence,
                        candidate,
                        permit,
                    })
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "cold pipeline worker channel closed: {error}"
                        ))
                    })?;
                if producer_hooks
                    .as_deref()
                    .is_some_and(|hooks| hooks.overlap && current_sequence == 2)
                {
                    let hooks = producer_hooks.as_deref().expect("checked above");
                    hooks.overlap_participants.fetch_add(1, Ordering::AcqRel);
                    hooks.overlap_barrier.wait();
                }
            }
            Ok::<_, std::io::Error>((scan, scan_ms))
        }));
        match outcome {
            Ok(Ok(report)) => Ok(report),
            Ok(Err(error)) => {
                producer_control.cancel(&error);
                Err(error)
            }
            Err(_) => {
                producer_control.cancel("cold pipeline scanner panicked");
                Err(std::io::Error::other("cold pipeline scanner panicked"))
            }
        }
    });

    let scan_result = match producer.join() {
        Ok(result) => result,
        Err(_) => {
            control.cancel("cold pipeline scanner thread panicked");
            Err(std::io::Error::other(
                "cold pipeline scanner thread panicked",
            ))
        }
    };
    if let Err(error) = &scan_result {
        control.cancel(error);
    }
    for worker in workers {
        if worker.join().is_err() {
            control.cancel("cold pipeline extraction worker thread panicked");
        }
    }
    let consumer_result = match consumer.join() {
        Ok(result) => result,
        Err(_) => {
            control.cancel("cold pipeline SQLite consumer thread panicked");
            Err(anyhow::anyhow!(
                "cold pipeline SQLite consumer thread panicked"
            ))
        }
    };
    // Extraction threads have exited and released their parser/source buffers.
    acdd_core::sys::trim_allocator();
    if let Some(failure) = control.failure() {
        bail!("{failure}");
    }
    let (scan, scan_ms) = scan_result.context("cold pipeline scan failed")?;
    let output = consumer_result?;
    let byte_state = byte_budget.snapshot();
    let metrics = PipelineMetrics {
        budget_bytes,
        peak_in_flight_bytes: byte_state.peak_in_flight_bytes,
        max_item_bytes: byte_state.max_item_bytes,
        oversized_admissions: byte_state.oversized_admissions,
        extracted_files: counters.extracted_files.load(Ordering::Acquire),
        ingested_files: counters.ingested_files.load(Ordering::Acquire),
        extraction_cache_hits: counters.extraction_cache_hits.load(Ordering::Acquire),
        scan_ms,
        extraction_ms: counters.extraction_nanos.load(Ordering::Acquire) as f64 / 1_000_000.,
        sqlite_ingest_ms: counters.ingestion_nanos.load(Ordering::Acquire) as f64 / 1_000_000.,
        scanned_files: scan.files,
        scanned_bytes: scan.bytes,
    };
    Ok((output, metrics))
}

pub(super) fn extraction_worker(context: ExtractionWorkerContext) {
    let ExtractionWorkerContext {
        root,
        adapter,
        dependencies,
        extraction_cache,
        effective_ruleset_hash,
        work_rx,
        result_tx,
        control,
        counters,
        test_hooks,
    } = context;
    let mut source = String::new();
    loop {
        let next = {
            let receiver = work_rx.lock().unwrap_or_else(|error| error.into_inner());
            receiver.recv()
        };
        let Ok(work) = next else {
            break;
        };
        let PipelineWork {
            sequence,
            candidate,
            permit,
        } = work;
        if control.is_cancelled() {
            drop(permit);
            continue;
        }
        if test_hooks
            .as_deref()
            .is_some_and(|hooks| hooks.overlap && sequence == 1)
        {
            let hooks = test_hooks.as_deref().expect("checked above");
            hooks.overlap_participants.fetch_add(1, Ordering::AcqRel);
            hooks.overlap_barrier.wait();
        }
        let extraction_started = Instant::now();
        let extracted = extract_candidate(
            &root,
            candidate,
            &adapter,
            &dependencies,
            &extraction_cache,
            &effective_ruleset_hash,
            &mut source,
        )
        .and_then(|(file, mut facts, cache_hit, cached, components, key)| {
            assign_file_component(&file.path, &mut facts, None);
            let storage_rows = db_writer::prepare_catalog_storage_with_facts(&file, &facts)?;
            let cached = cached.filter(|_| {
                if file.is_doc || !facts.docs.is_empty() {
                    return false;
                }
                let Some(previous) = &components else {
                    return false;
                };
                let current = facts
                    .edges
                    .iter()
                    .filter(|edge| edge.src.starts_with("component:"));
                previous.len() == current.clone().count()
                    && previous.iter().zip(current).all(|(a, b)| {
                        (
                            &a.src,
                            &a.dst,
                            &a.kind,
                            &a.path,
                            a.line,
                            &a.evidence,
                            &a.confidence,
                        ) == (
                            &b.src,
                            &b.dst,
                            &b.kind,
                            &b.path,
                            b.line,
                            &b.evidence,
                            &b.confidence,
                        )
                    })
            });
            let encoded_facts = match cached {
                Some(encoded) => encoded,
                None => {
                    let (encoded, length) = acdd_index::typed_facts::encode_facts_with_len(&facts)?;
                    extraction_cache.store_encoded(&key, &encoded, length);
                    encoded
                }
            };
            let local_facts_leaf = db_writer::local_facts_leaf(&file, &encoded_facts)?;
            let mut catalog = SymbolCatalog::default();
            catalog.insert_typed(file.path.clone(), facts);
            Ok((
                file,
                catalog,
                encoded_facts,
                local_facts_leaf,
                storage_rows,
                cache_hit,
            ))
        });
        counters.extraction_nanos.fetch_add(
            extraction_started
                .elapsed()
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            Ordering::AcqRel,
        );
        let (file, catalog, encoded_facts, local_facts_leaf, storage_rows, cache_hit) =
            match extracted {
                Ok(value) => value,
                Err(error) => {
                    let message = error.to_string();
                    control.cancel(&message);
                    let _ = result_tx.send(Err(message));
                    drop(permit);
                    continue;
                }
            };
        if cache_hit {
            counters
                .extraction_cache_hits
                .fetch_add(1, Ordering::AcqRel);
        }
        counters.extracted_files.fetch_add(1, Ordering::AcqRel);
        let item = PipelineItem {
            file,
            catalog,
            encoded_facts,
            local_facts_leaf,
            storage_rows,
            permit,
        };
        if let Err(error) = result_tx.send(Ok(item)) {
            control.cancel(format!("cold pipeline SQLite consumer stopped: {error}"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn atomic_build(
    root: &Path,
    output: &Path,
    adapter: &Adapter,
    dependencies: Arc<DependencyRegistry>,
    extraction_cache: Arc<ExtractionCache>,
    memory_budget: u64,
    candidates: Vec<BuildCandidate>,
    scan: scanner::BuildStreamReport,
    scan_ms: f64,
    test_hooks: Option<Arc<PipelineTestHooks>>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    validate_code_map_target(root, output)?;
    let output = if output.is_absolute() {
        output.to_owned()
    } else {
        std::env::current_dir()?.join(output)
    };
    let parent = output.parent().context("output needs a parent")?;
    let mut ancestor = PathBuf::new();
    for part in parent.components() {
        ancestor.push(part);
        if !acdd_core::fs::is_system_root_symlink(&ancestor)
            && fs::symlink_metadata(&ancestor).is_ok_and(|m| m.file_type().is_symlink())
        {
            bail!("output parent must not traverse a symlink");
        }
    }
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    if fs::symlink_metadata(&output).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("output must not be a symlink");
    }
    let stage = parent.join(format!(
        ".acdd-stage-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&stage)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(&stage)?;
    }
    let temporary = stage.join("generation.sqlite");
    let payload_spool_path = stage.join("payloads.bin");
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&temporary)?;
        let mut payload_options = OpenOptions::new();
        payload_options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            payload_options.mode(0o600);
        }
        payload_options.open(&payload_spool_path)?;
        let mut conn = Connection::open_with_flags(
            &temporary,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.execute_batch("PRAGMA page_size=32768;")?;
        db_writer::prepare_pipeline_schema(
            &mut conn,
            memory_budget,
            test_hooks.as_deref().is_some_and(|h| h.fail_consumer),
        )?;
        let (mut streamed, pipeline_metrics) = run_cold_pipeline(ColdPipelineInputs {
            root,
            payload_spool_path: &payload_spool_path,
            adapter,
            dependencies: Arc::clone(&dependencies),
            extraction_cache: Arc::clone(&extraction_cache),
            effective_ruleset_hash: Arc::<str>::from(extraction_cache::effective_ruleset_hash(
                &adapter.digest,
                dependencies.digest(),
            )),
            connection: conn,
            budget_bytes: memory_budget,
            candidates,
            scan,
            scan_ms,
            test_hooks: test_hooks.clone(),
        })?;
        streamed
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        let preparing_components = Instant::now();
        let mut changed_paths =
            ast::route_includes::compose_route_includes(&mut streamed.catalog.files);
        changed_paths.extend(components(&mut streamed.catalog.files));
        for path in changed_paths {
            if let Some(payload) = streamed.staged_payloads.get_mut(&path) {
                payload.storage_rows.take();
            }
        }
        let components_ms = preparing_components.elapsed().as_secs_f64() * 1000.;

        let linking = Instant::now();
        let graph = linker::link_compact_catalog_with_registry(
            &streamed.catalog,
            Some(root),
            &dependencies,
        );
        let link_ms = linking.elapsed().as_secs_f64() * 1000.;
        // Linker worker caches have been dropped; reclaim glibc arenas before SQLite persistence.
        acdd_core::sys::trim_allocator();

        let facts = streamed.catalog.into_files();
        let staged_disk_bytes = fs::metadata(&payload_spool_path)?.len();

        let mut report = db_writer::populate_owned(
            &mut streamed.connection,
            db_writer::PopulateOwnedParams {
                root,
                adapter,
                entries: &streamed.entries,
                facts,
                graph: &graph,
                build_inputs: db_writer::BuildInputs {
                    manifest_digest: dependencies.digest(),
                    memory_budget,
                    schema_ready: true,
                },
                staged_payloads: &mut streamed.staged_payloads,
                payload_spool_path: Some(&payload_spool_path),
            },
        )?;
        report["link_ms"] = json!(link_ms);
        report["staged_disk_bytes"] = json!(staged_disk_bytes);
        report["linking_facts_mode"] = json!("symbol-catalog");
        report["preencoded_facts_ms"] = json!(0.0);
        report["memory_budget_bytes"] = json!(memory_budget);
        report["scanned_files"] = json!(pipeline_metrics.scanned_files);
        report["scanned_bytes"] = json!(pipeline_metrics.scanned_bytes);
        report["scan_ms"] = json!(pipeline_metrics.scan_ms);
        report["extract_ms"] = json!(pipeline_metrics.extraction_ms);
        report["components_ms"] = json!(components_ms);
        report["pipeline"] = json!({
            "budget_bytes": pipeline_metrics.budget_bytes,
            "peak_in_flight_bytes": pipeline_metrics.peak_in_flight_bytes,
            "max_item_bytes": pipeline_metrics.max_item_bytes,
            "oversized_admissions": pipeline_metrics.oversized_admissions,
            "extracted_files": pipeline_metrics.extracted_files,
            "ingested_files": pipeline_metrics.ingested_files,
            "extraction_cache_hits": pipeline_metrics.extraction_cache_hits,
        });
        report["sqlite_ingest_ms"] = json!(pipeline_metrics.sqlite_ingest_ms);
        report["framework_rule_diagnostics"] = json!(dependencies.framework_rule_diagnostics(
            streamed
                .entries
                .iter()
                .map(|file| (file.path.as_str(), file.language.as_str()))
        ));
        cache::checkpoint(&streamed.connection, true)?;
        let candidate_mode: String =
            streamed
                .connection
                .query_row("PRAGMA journal_mode=DELETE;", [], |r| r.get(0))?;
        anyhow::ensure!(
            candidate_mode.eq_ignore_ascii_case("delete"),
            "candidate database failed to finalize in DELETE journal mode (got '{candidate_mode}')"
        );
        streamed.connection.close().map_err(|(_, e)| e)?;
        if let Some(action) = test_hooks
            .as_ref()
            .and_then(|hooks| hooks.before_candidate_verify.as_ref())
        {
            action(&temporary);
        }
        let verifying = Instant::now();
        let candidate = Connection::open_with_flags(
            &temporary,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("candidate database failed integrity check")?;
        let integrity: String = candidate
            .query_row("PRAGMA integrity_check(sqlite_schema)", [], |r| r.get(0))
            .context("candidate database failed integrity check")?;
        let page_count: i64 = candidate
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .context("candidate database failed integrity check")?;
        if integrity != "ok" || page_count <= 0 {
            bail!("candidate database failed integrity check: {integrity}");
        }
        candidate.close().map_err(|(_, e)| e)?;
        report["verify_ms"] = json!(verifying.elapsed().as_secs_f64() * 1000.);
        if let Some(action) = test_hooks
            .as_ref()
            .and_then(|hooks| hooks.before_generation_lock.as_ref())
        {
            action(&output);
        }
        let _generation_lock = cache::exclusive_lock(&output)?;
        let _idle_readers_guard = cache::release_idle_readers(&output)?;
        validate_code_map_target(root, &output)?;
        if let Some(guard) = before_publish {
            guard()?;
        }
        if manage_publication_sidecars(&output, SidecarDisposition::Reject).is_err() {
            manage_publication_sidecars(&output, SidecarDisposition::Allow)?;
            if output.exists() {
                let open_path = acdd_core::fs::confined_database_open_path(&output)?;
                let connection = Connection::open_with_flags(
                    open_path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )?;
                cache::checkpoint(&connection, true)?;
                let _ = connection.query_row("PRAGMA journal_mode=DELETE;", [], |_| Ok(()));
                connection.close().map_err(|(_, err)| err)?;
            }
            manage_publication_sidecars(&output, SidecarDisposition::Remove)?;
        }
        manage_publication_sidecars(&output, SidecarDisposition::Reject)?;
        fs::rename(&temporary, &output)?;
        let receipt = cache::publish_verified(
            &output,
            root,
            report["output_root"]
                .as_str()
                .context("missing output root")?,
        );
        report["verification_cache"] = json!(match receipt {
            Ok(true) => true,
            Ok(false) => {
                eprintln!(
                    "database published; verified receipt publication refused for {}",
                    output.display()
                );
                false
            }
            Err(err) => {
                eprintln!(
                    "database published; verified receipt publication failed for {}: {err:#}",
                    output.display()
                );
                false
            }
        });
        Ok(report)
    })();
    if let Some(action) = test_hooks
        .as_ref()
        .and_then(|hooks| hooks.before_stage_cleanup.as_ref())
    {
        action(&stage);
    }
    let mut cleanup_errors = Vec::new();
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = stage.join(format!("generation.sqlite{suffix}"));
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => {
                if let Err(err) = fs::remove_file(&sidecar) {
                    cleanup_errors.push(format!("remove sidecar {}: {err}", sidecar.display()));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                cleanup_errors.push(format!("inspect sidecar {}: {err}", sidecar.display()))
            }
        }
    }
    if let Err(err) = fs::remove_dir_all(&stage) {
        cleanup_errors.push(format!("remove stage {}: {err}", stage.display()));
    }
    let cleanup = if cleanup_errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(cleanup_errors.join("; ")))
    };
    match (result, cleanup) {
        (Err(err), Err(cleanup)) => {
            Err(err.context(format!("stage cleanup also failed: {cleanup:#}")))
        }
        (Err(err), Ok(())) => Err(err),
        (Ok(mut report), Err(err)) => {
            eprintln!("database published; stage cleanup failed: {err:#}");
            report["cleanup_error"] = json!(format!("{err:#}"));
            report["build_mode"] = json!(BuildMode::Cold.as_str());
            Ok(report)
        }
        (Ok(mut report), Ok(())) => {
            report["build_mode"] = json!(BuildMode::Cold.as_str());
            Ok(report)
        }
    }
}

/// Builds and publishes a sealed index for the workspace.
pub fn build(root: &Path, output: &Path, adapter_path: Option<&Path>) -> Result<Value> {
    build_inner(root, output, adapter_path, None, None, None)
}

/// Builds index with optional adapter selection, cache dir, test hooks, and pre-publication guard.
pub fn build_with_selection_and_guard(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    build_inner(
        root,
        output,
        adapter_path,
        cache_dir,
        test_hooks,
        before_publish,
    )
}

pub(super) fn build_inner(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    let cache_dir_opt = cache_dir.map(Path::to_path_buf).or_else(|| {
        test_hooks
            .as_ref()
            .and_then(|hooks| hooks.cache_dir_override.clone())
            .or_else(|| {
                extraction_cache::default_cache_root()
                    .map(|path| acdd_core::fs::resolve_workspace_path(root, &path))
            })
    });
    build_inner_with_cache(
        root,
        output,
        adapter_path,
        Arc::new(ExtractionCache::new(cache_dir_opt)),
        test_hooks,
        before_publish,
    )
}

/// Builds index overriding default extraction cache directory.
pub fn build_with_cache_dir(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    cache_dir: &Path,
) -> Result<Value> {
    build_inner(root, output, adapter_path, Some(cache_dir), None, None)
}

fn build_inner_with_cache(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    extraction_cache: Arc<ExtractionCache>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    let resolved_output = if output.as_os_str().is_empty() {
        resolve_code_db_path(root, None)
    } else {
        output.to_path_buf()
    };
    let output = resolved_output.as_path();
    manage_publication_sidecars(output, SidecarDisposition::Allow)?;
    validate_code_map_target(root, output)?;
    let started = Instant::now();
    let root = canonical_root(root)?;
    let adapter = load_adapter(&root, adapter_path)?;
    if !adapter.modules.is_enabled("code") && !adapter.modules.is_enabled("docs") {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "FEATURE_DISABLED: both 'code' and 'docs' modules are disabled",
        )
        .into());
    }
    config::check_root_scope(&root, adapter.limits.allow_broad_root)?;
    let scanning = Instant::now();
    let scanner::ColdScanResult {
        candidates,
        dependency_manifests,
        report: scan,
    } = if adapter.modules.is_enabled("code") {
        scanner::collect_for_build(&root, &adapter)?
    } else {
        let mut candidates = Vec::new();
        let scan = scanner::stream_for_build(&root, &adapter, |candidate| {
            candidates.push(candidate);
            Ok(())
        })?;
        scanner::ColdScanResult {
            candidates,
            dependency_manifests: Vec::new(),
            report: scan,
        }
    };
    let scan_ms = scanning.elapsed().as_secs_f64() * 1000.;
    let dependencies =
        DependencyRegistry::try_collect_with_paths(&root, Some(&adapter), &dependency_manifests)?;
    let budget = match test_hooks
        .as_ref()
        .and_then(|hooks| hooks.memory_budget_override)
    {
        Some(budget) => budget,
        None => resolve_memory_budget_bytes(&adapter.limits)?,
    };
    let dependencies = Arc::new(dependencies);
    let mut report = atomic_build(
        &root,
        output,
        &adapter,
        dependencies,
        extraction_cache,
        budget,
        candidates,
        scan,
        scan_ms,
        test_hooks,
        before_publish,
    )?;
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["output"] = json!(output);
    Ok(report)
}

fn check_legacy_tasks_db_collision(
    candidate_root: &Path,
    base: &Path,
    output: &Path,
    output_abs: &Path,
) -> Result<()> {
    let cfg_path = candidate_root.join("acdd.yaml");
    if !cfg_path.is_file() {
        return Ok(());
    }
    let Ok(raw) = fs::read_to_string(&cfg_path) else {
        return Ok(());
    };
    let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return Ok(());
    };
    let Some(legacy_tasks_db) = yaml.get("tasks_db").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let task_db_path = if Path::new(legacy_tasks_db).is_absolute() {
        PathBuf::from(legacy_tasks_db)
    } else {
        base.join(legacy_tasks_db)
    };
    if acdd_core::fs::same_filesystem_path(output_abs, &task_db_path) {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to use configured tasks database '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }
    Ok(())
}

fn check_configured_state_db_collision(
    root: &Path,
    base: &Path,
    output: &Path,
    output_abs: &Path,
) -> Result<()> {
    let state_adapter = if root.join("acdd.yaml").exists() {
        Some(load_adapter(root, None))
    } else if base != root && base.join("acdd.yaml").exists() {
        Some(load_adapter(base, None))
    } else {
        None
    };
    let state_db_path = match state_adapter {
        Some(Ok(adapter)) => resolve_state_db_path(base, &adapter),
        Some(Err(error)) => {
            bail!(
                "TASK_DATABASE_COLLISION: Cannot validate configured state_db because acdd.yaml is invalid ({error}). Refusing code map output '{}'.",
                output.display()
            );
        }
        None => resolve_state_db_path(base, &Adapter::default()),
    };
    if acdd_core::fs::same_filesystem_path(output_abs, &state_db_path)
        || (base != root
            && !Path::new(&state_db_path).is_absolute()
            && acdd_core::fs::same_filesystem_path(output_abs, &root.join(&state_db_path)))
    {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to use configured state database '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }
    Ok(())
}

fn check_existing_code_map_target(output: &Path, output_abs: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(output_abs) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => bail!(
            "cannot access code map target '{}': {err}",
            output.display()
        ),
    };
    if !meta.is_file() {
        bail!(
            "code map target must be a regular file: '{}'",
            output.display()
        );
    }
    if meta.len() == 0 {
        return Ok(());
    }
    let header = db_reader::validate_database_header(output_abs);
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
        return Ok(());
    }
    let conn = db_reader::open_raw_confined_readonly(output_abs).map_err(|err| {
        anyhow::anyhow!(
            "TASK_DATABASE_COLLISION: Cannot inspect SQLite database schema at '{}' ({}). Refusing to overwrite potentially active task database.",
            output.display(),
            err
        )
    })?;
    let is_task_db = conn
        .query_row(
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
        )
        .map_err(|err| {
            anyhow::anyhow!(
                "TASK_DATABASE_COLLISION: Cannot inspect SQLite database schema at '{}' ({}). Refusing to overwrite potentially active task database.",
                output.display(),
                err
            )
        })?;
    if is_task_db {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to overwrite task database at '{}' with code map index. The target database contains active task tracking tables. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }
    Ok(())
}

/// Validates that a code map database output target does not point to or overwrite
/// a task tracking database.
pub fn validate_code_map_target(root: &Path, output: &Path) -> Result<()> {
    let output_abs = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| root.to_path_buf())
            .join(output)
    };
    let norm_output = acdd_core::fs::normalize_path(&output_abs);
    let file_name_lower = norm_output
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_reserved_state_or_task_filename(&file_name_lower) {
        bail!(
            "TASK_DATABASE_COLLISION: Refusing to use task/state database path '{}' as code map output. The code map database must be separate (default: '.acdd/code-map.sqlite').",
            output.display()
        );
    }

    let base = acdd_core::git::primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    for candidate_root in [root, base.as_path()] {
        check_legacy_tasks_db_collision(candidate_root, &base, output, &output_abs)?;
    }
    check_configured_state_db_collision(root, &base, output, &output_abs)?;
    check_existing_code_map_target(output, &output_abs)?;
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
