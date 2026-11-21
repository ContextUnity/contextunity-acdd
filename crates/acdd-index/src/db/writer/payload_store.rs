//! Bounded in-memory staging with positioned spool overflow.
use anyhow::{bail, Context, Result};
use std::fs::File;

/// A byte range in the stage-private spool.
#[derive(Debug, Clone, Copy)]
pub struct SpoolSlice {
    pub offset: u64,
    pub length: u64,
}

/// An owned memory payload or a location in the spool.
#[derive(Debug)]
pub enum StagedPayload {
    InMemory(Vec<u8>),
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

    pub fn stage(&mut self, mut bytes: Vec<u8>) -> Result<StagedPayload> {
        let length = u64::try_from(bytes.len()).context("staged payload length overflow")?;
        if length <= self.ram_budget.saturating_sub(self.ram_bytes) {
            bytes.shrink_to_fit();
            if bytes.capacity() as u64 <= self.ram_budget.saturating_sub(self.ram_bytes) {
                self.ram_bytes += bytes.capacity() as u64;
                return Ok(StagedPayload::InMemory(bytes));
            }
        }
        let offset = self.next_offset;
        self.next_offset = offset
            .checked_add(length)
            .context("staged payload spool offset overflow")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut position = offset;
            let mut remaining = bytes.as_slice();
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
            self.file.write_all(&bytes)?;
        }
        Ok(StagedPayload::Spilled(SpoolSlice { offset, length }))
    }

    pub fn flush(&mut self) -> Result<()> {
        std::io::Write::flush(&mut self.file)?;
        Ok(())
    }
}

pub(super) fn take_staged_payload(file: &File, payload: StagedPayload) -> Result<Vec<u8>> {
    match payload {
        StagedPayload::InMemory(bytes) => Ok(bytes),
        StagedPayload::Spilled(slice) => read_spool(file, slice),
    }
}

pub(super) fn read_staged_payload(file: &File, payload: &StagedPayload) -> Result<Vec<u8>> {
    match payload {
        StagedPayload::InMemory(bytes) => Ok(bytes.clone()),
        StagedPayload::Spilled(slice) => read_spool(file, *slice),
    }
}

fn read_spool(file: &File, slice: SpoolSlice) -> Result<Vec<u8>> {
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
