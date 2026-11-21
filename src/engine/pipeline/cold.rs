use super::*;

/// Extracts one scanned file and rejects changes to its source snapshot.
pub fn extract(root: &Path, file: &FileEntry, adapter: &Adapter) -> Result<Facts> {
    let dependencies = DependencyRegistry::try_collect_with_scan_config(root, Some(adapter))?;
    Ok(extract_typed(root, file, adapter, &dependencies)?.into_public())
}

pub(super) fn extract_typed(
    root: &Path,
    file: &FileEntry,
    adapter: &Adapter,
    dependencies: &DependencyRegistry,
) -> Result<TypedFacts> {
    let path = resolve_file_path(root, adapter, &file.path)?;
    let source = fs::read_to_string(&path)?;
    extract_typed_source(file, &source, dependencies, &adapter.spec_paths())
}

pub(super) fn extract_typed_source(
    file: &FileEntry,
    source: &str,
    dependencies: &DependencyRegistry,
    spec_paths: &[String],
) -> Result<TypedFacts> {
    let source_digest = commitments::hash(source.as_bytes());
    extract_typed_source_with_digest(file, source, &source_digest, dependencies, spec_paths)
}

pub(super) fn extract_typed_source_with_digest(
    file: &FileEntry,
    source: &str,
    source_digest: &str,
    dependencies: &DependencyRegistry,
    spec_paths: &[String],
) -> Result<TypedFacts> {
    if source_digest != file.digest {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            file.path
        ))
        .into());
    }
    let mut typed = if file.is_doc {
        let parsed =
            docs::extract_with_details(&file.path, source, file.mtime_ns as f64 / 1e9, spec_paths)?;
        let sections = parsed
            .iter()
            .map(|(doc, _)| doc.clone())
            .collect::<Vec<_>>();
        let mut nodes = Vec::new();
        let mut errors = Vec::new();
        if let Some(err_msg) = parsed
            .first()
            .and_then(|(_, metadata)| metadata.get("frontmatter_error"))
            .and_then(Value::as_str)
        {
            errors.push(acdd_index::models::Diagnostic {
                path: file.path.clone(),
                line: 1,
                message: format!("invalid frontmatter: {err_msg}"),
            });
        }
        for (doc, metadata) in &parsed {
            let line = metadata["line"].as_u64().unwrap_or(1) as usize;
            let end_line = metadata["end_line"].as_u64().unwrap_or(line as u64) as usize;
            let mut details = json!({
                "doc_type":doc.doc_type,
                "is_invariant":doc.is_invariant,
                "status":metadata["status"],
                "citations":metadata["citations"],
                "symbol_claims":metadata["symbol_claims"],
                "declared_invariants":metadata["declared_invariants"],
            });
            if metadata["status"].is_null() {
                details.as_object_mut().unwrap().remove("status");
            }
            nodes.push(Node {
                native_semantic: None,
                id: doc.doc_id.clone(),
                kind: "document".into(),
                name: doc.section_title.clone(),
                qualname: doc.doc_id.clone(),
                path: doc.path.clone(),
                line,
                end_line,
                is_test: false,
                language: "markdown".into(),
                generated: false,
                details,
            });
            if let Some(invariants) = metadata["declared_invariants"].as_array() {
                for invariant in invariants {
                    let Some(name) = invariant["id"].as_str() else {
                        continue;
                    };
                    let invariant_line = invariant["line"].as_u64().unwrap_or(line as u64) as usize;
                    nodes.push(Node {
                        native_semantic: None,
                        id: format!("invariant:{}:{invariant_line}:{name}", doc.path),
                        kind: "invariant".into(),
                        name: name.to_owned(),
                        qualname: format!("{}::{name}", doc.doc_id),
                        path: doc.path.clone(),
                        line: invariant_line,
                        end_line: invariant_line,
                        is_test: false,
                        language: "markdown".into(),
                        generated: false,
                        details: json!({
                            "document_id":doc.doc_id,
                            "rule":invariant["rule"],
                            "status":metadata["status"],
                        }),
                    });
                }
            }
        }
        TypedFacts::from_public(Facts {
            docs: sections,
            nodes,
            errors,
            ..Facts::default()
        })
    } else {
        ast::extract_typed_with_registry(&file.path, &file.language, source, dependencies)?
    };
    sanitize_extracted_facts(&mut typed);
    Ok(typed)
}

pub(super) fn sanitize_extracted_facts(facts: &mut TypedFacts) {
    for node in &mut facts.facts.nodes {
        if node.details.get("classic_global") == Some(&serde_json::Value::Bool(true)) {
            if facts.flows.state(node).is_some() {
                if let Some(details) = node.details.as_object_mut() {
                    details.remove("value_flow");
                }
            } else if let Some(details) = node.details.as_object_mut() {
                details.remove("value_flow");
            }
        }
    }
}

type CandidateExtraction = (
    FileEntry,
    TypedFacts,
    bool,
    Option<Vec<u8>>,
    Option<Vec<Edge>>,
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
    let path = resolve_file_path(root, adapter, &candidate.path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut source_file = options.open(&path)?;
    if !candidate.matches_metadata(&source_file.metadata()?) {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            candidate.path
        ))
        .into());
    }
    let source_capacity =
        usize::try_from(candidate.bytes).context("source size exceeds addressable memory")?;
    source.clear();
    source.try_reserve_exact(source_capacity)?;
    source_file
        .by_ref()
        .take(adapter.limits.max_file_bytes.saturating_add(1))
        .read_to_string(source)?;
    if source.len() as u64 > adapter.limits.max_file_bytes || source.len() as u64 != candidate.bytes
    {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            candidate.path
        ))
        .into());
    }
    if !candidate.matches_metadata(&source_file.metadata()?) {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            candidate.path
        ))
        .into());
    }
    let source_digest = commitments::hash(source.as_bytes());
    let file = candidate.into_entry(source_digest);
    let (key, cache_limit) = hydration::extraction_cache_key_and_limit(
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
        hydration::touch_cached_facts(&mut facts, file.mtime_ns);
        return Ok((file, facts, true, encoded, components, key));
    }
    let spec_paths = adapter.spec_paths();
    let facts =
        extract_typed_source_with_digest(&file, source, &file.digest, dependencies, &spec_paths)?;
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
    // SAFETY: glibc's allocator entry point is thread-safe and takes no pointers.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
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
            let reused = cached.is_some();
            let encoded_facts = match cached {
                Some(encoded) => encoded,
                None => acdd_index::typed_facts::encode_facts(&facts)?,
            };
            if !reused {
                if let Ok(Some(length)) = zstd::zstd_safe::get_frame_content_size(
                    &encoded_facts[b"ACDD-LOCAL-FACTS\0\x03".len()..],
                ) {
                    extraction_cache.store_encoded(&key, &encoded_facts, length);
                }
            }
            let mut catalog = SymbolCatalog::default();
            catalog.insert_typed(file.path.clone(), facts, false);
            Ok((file, catalog, encoded_facts, storage_rows, cache_hit))
        });
        counters.extraction_nanos.fetch_add(
            extraction_started
                .elapsed()
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            Ordering::AcqRel,
        );
        let (file, catalog, encoded_facts, storage_rows, cache_hit) = match extracted {
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

        // Linker caches have been dropped before storage payloads are restored.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            // SAFETY: no pointers are passed; glibc synchronizes allocator arenas.
            libc::malloc_trim(0);
        }
        let facts = streamed.catalog.into_files();
        let staged_disk_bytes = fs::metadata(&payload_spool_path)?.len();

        let mut report = db_writer::populate_owned_with_store(
            &mut streamed.connection,
            root,
            adapter,
            &streamed.entries,
            facts,
            &graph,
            db_writer::BuildInputs {
                manifest_digest: dependencies.digest(),
                memory_budget,
                schema_ready: true,
            },
            &mut streamed.staged_payloads,
            Some(&payload_spool_path),
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
        if ensure_no_publication_sidecars(&output, false).is_err() {
            ensure_no_publication_sidecars(&output, true)?;
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
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut name = output.as_os_str().to_owned();
                name.push(suffix);
                let sidecar = PathBuf::from(name);
                match fs::symlink_metadata(&sidecar) {
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("inspect sidecar {}", sidecar.display()));
                    }
                    Ok(metadata) if metadata.is_file() => {
                        fs::remove_file(&sidecar)
                            .with_context(|| format!("remove sidecar {}", sidecar.display()))?;
                    }
                    Ok(_) => bail!(
                        "cannot replace database with existing sidecar {}",
                        sidecar.display()
                    ),
                }
            }
        }
        ensure_no_publication_sidecars(&output, false)?;
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
    execute_with_mode(BuildMode::Cold, root, output, None, adapter_path, None)
}

/// Builds index with test synchronization and fault-injection hooks.
pub fn build_with_test_hooks(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    test_hooks: Arc<PipelineTestHooks>,
) -> Result<Value> {
    build_inner(root, output, adapter_path, None, Some(test_hooks))
}

pub(super) fn build_inner(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
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
    )
}

/// Builds index overriding default extraction cache directory.
pub fn build_with_cache_dir(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    cache_dir: &Path,
) -> Result<Value> {
    execute_with_mode(
        BuildMode::Cold,
        root,
        output,
        None,
        adapter_path,
        Some(cache_dir),
    )
}

fn build_inner_with_cache(
    root: &Path,
    output: &Path,
    adapter_path: Option<&Path>,
    extraction_cache: Arc<ExtractionCache>,
    test_hooks: Option<Arc<PipelineTestHooks>>,
) -> Result<Value> {
    let resolved_output = if output.as_os_str().is_empty() {
        resolve_code_db_path(root, None)
    } else {
        output.to_path_buf()
    };
    let output = resolved_output.as_path();
    ensure_no_publication_sidecars(output, true)?;
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
    )?;
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["output"] = json!(output);
    Ok(report)
}

