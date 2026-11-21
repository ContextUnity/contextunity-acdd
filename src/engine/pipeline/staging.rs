use super::*;

pub(super) fn consume_pipeline_results(
    mut connection: Connection,
    result_rx: Receiver<std::result::Result<PipelineItem, String>>,
    payload_spool_path: &Path,
    ram_budget: u64,
    control: &PipelineControl,
    counters: &PipelineCounters,
    test_hooks: Option<&PipelineTestHooks>,
) -> Result<PipelineConsumerOutput> {
    let mut payload_store = PayloadStore::new(OpenOptions::new().write(true).open(payload_spool_path)?, ram_budget);
    let transaction = connection.transaction()?;
    let mut statement =
        transaction.prepare_cached("INSERT INTO source_inventory VALUES(?1,'indexed',?2,?3)")?;
    let mut entries = Vec::new();
    let mut catalog = SymbolCatalog::default();
    let mut staged_payloads = BTreeMap::new();
    while let Ok(result) = result_rx.recv() {
        let item = match result {
            Ok(item) => item,
            Err(error) => {
                control.cancel(error);
                continue;
            }
        };
        if control.is_cancelled() {
            drop(item);
            continue;
        }
        if counters.ingested_files.load(Ordering::Acquire) == 0
            && test_hooks.is_some_and(|hooks| hooks.overlap)
        {
            let hooks = test_hooks.expect("checked above");
            hooks.overlap_participants.fetch_add(1, Ordering::AcqRel);
            hooks.overlap_barrier.wait();
        }
        let ingestion_started = Instant::now();
        let PipelineItem {
            file,
            catalog: file_catalog,
            encoded_facts,
            storage_rows,
            permit,
        } = item;
        let inserted =
            statement.execute(params![file.path, file.digest, i64::try_from(file.bytes)?]);
        counters.ingestion_nanos.fetch_add(
            ingestion_started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::AcqRel,
        );
        match inserted {
            Ok(_) => {
                let path = file.path.clone();
                let encoded_facts_ref = payload_store.stage(encoded_facts)?;
                let storage_rows_ref = payload_store.stage(storage_rows)?;
                entries.push(file);
                catalog.extend(file_catalog);
                staged_payloads.insert(
                    path,
                    StagedFilePayload {
                        encoded_facts: encoded_facts_ref,
                        storage_rows: Some(storage_rows_ref),
                    },
                );
                counters.ingested_files.fetch_add(1, Ordering::AcqRel);
            }
            Err(error) => control.cancel(error),
        }
        drop(permit);
    }
    payload_store.flush()?;
    drop(statement);
    if let Some(failure) = control.failure() {
        transaction.rollback()?;
        bail!("{failure}");
    }
    transaction.commit()?;
    Ok(PipelineConsumerOutput {
        connection,
        entries,
        catalog,
        staged_payloads,
    })
}

