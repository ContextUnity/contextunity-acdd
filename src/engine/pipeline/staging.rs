use super::hooks::PipelineTestHooks;
use super::runtime::{PipelineConsumerOutput, PipelineControl, PipelineCounters, PipelineItem};
use acdd_index::{
    db::writer::{self as db_writer, PayloadStore, StagedFilePayload},
    typed_facts::SymbolCatalog,
};
use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::Path,
    sync::{atomic::Ordering, mpsc::Receiver},
    time::Instant,
};

pub(super) fn consume_pipeline_results(
    mut connection: Connection,
    result_rx: Receiver<std::result::Result<PipelineItem, String>>,
    payload_spool_path: &Path,
    ram_budget: u64,
    control: &PipelineControl,
    counters: &PipelineCounters,
    test_hooks: Option<&PipelineTestHooks>,
) -> Result<PipelineConsumerOutput> {
    let mut payload_store = PayloadStore::new(
        OpenOptions::new().write(true).open(payload_spool_path)?,
        ram_budget,
    );
    let transaction = connection.transaction()?;
    let mut statement =
        transaction.prepare_cached("INSERT INTO source_inventory VALUES(?1,'indexed',?2,?3)")?;
    let mut local_facts_writer = db_writer::LocalFactsBatchWriter::new(&transaction)?;
    let mut skeletons_writer = db_writer::StatementSkeletonsBatchWriter::new(&transaction)?;
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
            skeleton_blob,
            local_facts_leaf,
            storage_rows,
            permit,
        } = item;
        let path = file.path.clone();
        local_facts_writer.push(&file, encoded_facts)?;
        if let Some(skeleton_blob) = skeleton_blob {
            skeletons_writer.push(&path, skeleton_blob)?;
        }
        let storage_rows_ref = payload_store.stage_prepared(storage_rows)?;
        entries.push(file);
        catalog.extend(file_catalog);
        staged_payloads.insert(
            path,
            StagedFilePayload {
                storage_rows: Some(storage_rows_ref),
                local_facts_leaf: Some(local_facts_leaf),
            },
        );
        counters.ingestion_nanos.fetch_add(
            ingestion_started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::AcqRel,
        );
        counters.ingested_files.fetch_add(1, Ordering::AcqRel);
        drop(permit);
    }
    if let Some(failure) = control.failure() {
        drop(skeletons_writer);
        drop(local_facts_writer);
        drop(statement);
        transaction.rollback()?;
        bail!("{failure}");
    }
    local_facts_writer.flush()?;
    drop(local_facts_writer);
    skeletons_writer.flush()?;
    drop(skeletons_writer);
    payload_store.flush()?;
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    for file in &entries {
        statement.execute(params![file.path, file.digest, i64::try_from(file.bytes)?])?;
    }
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
