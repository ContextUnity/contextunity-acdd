//! Bounded in-memory staging with positioned spool overflow.
use anyhow::{bail, Context, Result};
use std::fs::File;

/// A byte range in the stage-private spool.
#[derive(Debug, Clone, Copy)]
pub struct SpoolSlice {
    pub offset: u64,
    pub length: u64,
}

/// Prepared catalog rows held in memory or encoded in the spool.
#[derive(Debug)]
pub enum StagedPreparedStorage {
    InMemory(Box<super::cache::PreparedCatalogStorage>),
    Spilled(SpoolSlice),
}

/// Tracks the bounded RAM allowance and next spool position.
pub struct PayloadStore {
    file: File,
    next_offset: u64,
    ram_bytes: u64,
    ram_budget: u64,
}

impl PayloadStore {
    pub fn new(file: File, ram_budget: u64) -> Self {
        Self {
            file,
            next_offset: 0,
            ram_bytes: 0,
            ram_budget,
        }
    }

    pub fn stage_prepared(
        &mut self,
        storage: super::cache::PreparedCatalogStorage,
    ) -> Result<StagedPreparedStorage> {
        let size = storage.memory_bytes();
        if size <= self.ram_budget.saturating_sub(self.ram_bytes) {
            self.ram_bytes += size;
            Ok(StagedPreparedStorage::InMemory(Box::new(storage)))
        } else {
            Ok(StagedPreparedStorage::Spilled(
                self.spill(&storage.encode_spill())?,
            ))
        }
    }

    fn spill(&mut self, bytes: &[u8]) -> Result<SpoolSlice> {
        let length = u64::try_from(bytes.len()).context("staged payload length overflow")?;
        let offset = self.next_offset;
        self.next_offset = offset
            .checked_add(length)
            .context("staged payload spool offset overflow")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut position = offset;
            let mut remaining = bytes;
            while !remaining.is_empty() {
                match self.file.write_at(remaining, position) {
                    Ok(0) => bail!("failed to write staged payload blob"),
                    Ok(n) => {
                        position += n as u64;
                        remaining = &remaining[n..];
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom, Write};
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.write_all(bytes)?;
        }
        Ok(SpoolSlice { offset, length })
    }

    pub fn flush(&mut self) -> Result<()> {
        std::io::Write::flush(&mut self.file)?;
        Ok(())
    }
}

pub(super) fn read_spool(file: &File, slice: SpoolSlice) -> Result<Vec<u8>> {
    let length = usize::try_from(slice.length).context("staged payload length overflow")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .context("cannot reserve memory for staged payload")?;
    bytes.resize(length, 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut offset = slice.offset;
        let mut remaining = bytes.as_mut_slice();
        while !remaining.is_empty() {
            match file.read_at(remaining, offset) {
                Ok(0) => {
                    bail!(
                        "unexpected EOF reading staged payload at offset {} (remaining {} bytes)",
                        offset,
                        remaining.len()
                    );
                }
                Ok(read_bytes) => {
                    remaining = &mut remaining[read_bytes..];
                    offset = offset
                        .checked_add(read_bytes as u64)
                        .context("staged payload offset overflow")?;
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        static SPOOL_READ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = SPOOL_READ_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut reader = file;
        reader.seek(SeekFrom::Start(slice.offset))?;
        reader.read_exact(&mut bytes)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::writer::{self, BuildInputs, StagedFilePayload},
        models::{compact_graph::CompactGraph, FileEntry},
        typed_facts::{encode_facts, TypedFacts},
    };
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use rusqlite::Connection;
    use std::{collections::BTreeMap, fs::OpenOptions, io::Write};

    #[test]
    fn corrupt_staged_spool_offset_fails_closed_in_populate_owned() {
        let workspace = ScopedTempDir::create("acdd-corrupt-spool").unwrap();
        let acdd_dir = workspace.path().join(".acdd");
        std::fs::create_dir_all(&acdd_dir).unwrap();
        let spool_file = acdd_dir.join("corrupt-spool.bin");
        let valid_blob = encode_facts(&TypedFacts::default()).unwrap();
        let mut handle = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&spool_file)
            .unwrap();
        handle.write_all(&valid_blob).unwrap();
        drop(handle);

        let mut stage_conn = Connection::open_in_memory().unwrap();
        let entry = FileEntry {
            path: "src/unit.rs".to_owned(),
            digest: "00".repeat(32),
            bytes: 10,
            mtime_ns: 1,
            identity: [0, 0, 0],
            language: "rust".to_owned(),
            is_doc: false,
        };
        let mut staged_payloads = BTreeMap::from([(
            entry.path.clone(),
            StagedFilePayload {
                storage_rows: Some(StagedPreparedStorage::Spilled(SpoolSlice {
                    offset: 999_999,
                    length: valid_blob.len() as u64,
                })),
                local_facts_leaf: Some((String::new(), String::new())),
            },
        )]);
        let err = writer::populate_owned(
            &mut stage_conn,
            writer::PopulateOwnedParams {
                root: workspace.path(),
                adapter: &Adapter::default(),
                entries: std::slice::from_ref(&entry),
                facts: BTreeMap::from([(entry.path.clone(), TypedFacts::default())]),
                graph: &CompactGraph::default(),
                build_inputs: BuildInputs {
                    manifest_digest: "test",
                    memory_budget: 8 * 1024 * 1024,
                    schema_ready: false,
                },
                staged_payloads: &mut staged_payloads,
                payload_spool_path: Some(&spool_file),
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("EOF") || err.to_string().contains("staged"),
            "corrupt spool offset must fail closed: {err:#}"
        );
    }
}
