use crate::models::Edge;
#[cfg(test)]
use crate::models::Node;
#[cfg(test)]
use crate::models::Reference;
#[cfg(test)]
use crate::typed_facts::FlowState;
use crate::typed_facts::TypedFacts;
use acdd_core::format::{sha256_bytes, sha256_serde_json_hex};
use anyhow::Context;
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const MAGIC_V4: &[u8] = b"ACDD-EXTRACTION-FACTS\0\x04";
const MAGIC_V5: &[u8] = b"ACDD-EXTRACTION-FACTS\0\x05";
const LOCAL_MAGIC: &[u8] = b"ACDD-LOCAL-FACTS\0\x03";
const CHECKSUM_BYTES: usize = 32;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static EXTRACTION_CACHE_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct PreparedCatalogStorage {
    pub(super) facts: TypedFacts,
    pub(super) search_texts: Vec<String>,
    pub(super) reference_keys: Vec<String>,
    pub(super) leaves: BTreeMap<String, BTreeMap<String, String>>,
}

crate::struct_codecs! {
    PreparedCatalogStorage {
        facts,
        search_texts,
        reference_keys,
        leaves
    };
}

const RAW_PREPARED_STORAGE_MAGIC: &[u8] = b"ACDD-RAW-STORAGE\0\x01";

pub(super) struct CatalogStorageView<'a> {
    pub(super) facts: &'a TypedFacts,
    pub(super) search_texts: &'a [String],
    pub(super) reference_keys: &'a [String],
    pub(super) leaves: &'a BTreeMap<crate::commitments::CommitmentDomain, BTreeMap<String, String>>,
}

impl CatalogStorageView<'_> {
    pub(super) fn encode(&self) -> Vec<u8> {
        let Self {
            facts,
            search_texts,
            reference_keys,
            leaves,
        } = self;
        let mut raw = Vec::with_capacity(512);
        raw.extend_from_slice(RAW_PREPARED_STORAGE_MAGIC);
        (facts.nodes.len() as u32).encode(&mut raw);
        for node in &facts.nodes {
            node.id.encode(&mut raw);
            node.kind.encode(&mut raw);
            node.name.encode(&mut raw);
            node.qualname.encode(&mut raw);
            node.path.encode(&mut raw);
            node.line.encode(&mut raw);
            node.end_line.encode(&mut raw);
            node.is_test.encode(&mut raw);
            node.language.encode(&mut raw);
            node.generated.encode(&mut raw);
            super::retained_node_details(node).encode(&mut raw);
            Option::<crate::typed_facts::NodeSemanticEntry>::None.encode(&mut raw);
        }
        0u32.encode(&mut raw);
        0u32.encode(&mut raw);
        facts.docs.encode(&mut raw);
        facts.errors.encode(&mut raw);
        for _ in 0..5 {
            0u32.encode(&mut raw);
        }
        search_texts.encode(&mut raw);
        reference_keys.encode(&mut raw);
        (leaves.len() as u32).encode(&mut raw);
        let ordered: BTreeMap<_, _> = leaves
            .iter()
            .map(|(domain, leaves)| (domain.as_str(), leaves))
            .collect();
        for (domain, leaves) in ordered {
            domain.encode(&mut raw);
            leaves.encode(&mut raw);
        }
        raw
    }
}

pub(super) fn decode_prepared_catalog_storage_payload(
    bytes: &[u8],
) -> anyhow::Result<PreparedCatalogStorage> {
    use crate::typed_facts::binary::BinaryDecode;
    use std::io::Read;

    if let Some(raw) = bytes.strip_prefix(RAW_PREPARED_STORAGE_MAGIC) {
        let mut cursor = 0;
        let prepared = PreparedCatalogStorage::decode(raw, &mut cursor)
            .context("corrupted prepared catalog storage")?;
        anyhow::ensure!(cursor == raw.len(), "corrupted prepared catalog storage");
        return Ok(prepared);
    }

    thread_local! {
        static DECODER: RefCell<(zstd::zstd_safe::DCtx<'static>, Vec<u8>)> =
            RefCell::new((zstd::zstd_safe::DCtx::create(), Vec::new()));
    }
    DECODER.with(|slot| {
        let mut scratch = slot.borrow_mut();
        let (context, decoded) = &mut *scratch;
        context
            .init()
            .map_err(|code| std::io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
        decoded.clear();
        zstd::stream::read::Decoder::with_context(bytes, context).read_to_end(decoded)?;
        let mut cursor = 0;
        let prepared = PreparedCatalogStorage::decode(decoded, &mut cursor)
            .context("corrupted prepared catalog storage")?;
        anyhow::ensure!(
            cursor == decoded.len(),
            "corrupted prepared catalog storage"
        );
        Ok(prepared)
    })
}

#[derive(Serialize)]
struct CacheIdentity<'a> {
    source_digest: &'a str,
    relative_path: &'a str,
    grammar_id: &'a str,
    extractor_version: &'a str,
    effective_ruleset_hash: &'a str,
}

/// Computes cache key for file extraction identity.
pub fn key(
    source_digest: &str,
    relative_path: &str,
    grammar_id: &str,
    extractor_version: &str,
    effective_ruleset_hash: &str,
) -> String {
    let identity = CacheIdentity {
        source_digest,
        relative_path,
        grammar_id,
        extractor_version,
        effective_ruleset_hash,
    };
    sha256_serde_json_hex(&identity).expect("cache identity is JSON serializable")
}

/// Computes combined effective ruleset hash from adapter and dependency digests.
pub fn effective_ruleset_hash(adapter_digest: &str, dependency_digest: &str) -> String {
    let mut input = Vec::with_capacity(adapter_digest.len() + dependency_digest.len() + 32);
    input.extend_from_slice(b"acdd-effective-ruleset-v1\0");
    input.extend_from_slice(&(adapter_digest.len() as u64).to_le_bytes());
    input.extend_from_slice(adapter_digest.as_bytes());
    input.extend_from_slice(&(dependency_digest.len() as u64).to_le_bytes());
    input.extend_from_slice(dependency_digest.as_bytes());
    acdd_core::format::sha256_hex(&input)
}

/// File-backed persistent extraction cache.
pub struct ExtractionCache {
    root: Option<PathBuf>,
}

/// Decoded extraction facts, encoded bytes, and the prior component edges.
pub type BuildCacheEntry = (TypedFacts, Option<Vec<u8>>, Option<Vec<Edge>>);

impl ExtractionCache {
    /// Creates a new extraction cache backed by an optional root path.
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    /// Loads cached typed facts if valid and within payload byte limit.
    pub fn load(&self, key: &str, max_payload_bytes: u64) -> Option<TypedFacts> {
        self.load_with_encoded(key, max_payload_bytes)
            .map(|(facts, _)| facts)
    }

    /// Loads facts and, for V5 entries, their canonical encoded payload.
    pub fn load_with_encoded(
        &self,
        key: &str,
        max_payload_bytes: u64,
    ) -> Option<(TypedFacts, Option<Vec<u8>>)> {
        self.load_for_build(key, max_payload_bytes)
            .map(|(facts, encoded, _)| (facts, encoded))
    }

    /// Loads clean facts and the component edges present in the encoded payload.
    pub fn load_for_build(&self, key: &str, max_payload_bytes: u64) -> Option<BuildCacheEntry> {
        let path = self.entry_path(key)?;
        let file = open_regular_file(&path).ok()?;
        let max_entry_bytes = max_payload_bytes.saturating_add(256);
        if file.metadata().ok()?.len() > max_entry_bytes {
            return None;
        }
        EXTRACTION_CACHE_BUFFER.with(|buffer| {
            let mut bytes = buffer.borrow_mut();
            bytes.clear();
            file.take(max_entry_bytes.saturating_add(1))
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() as u64 > max_entry_bytes {
                return None;
            }
            decode_entry_for_build(&bytes, key, max_payload_bytes)
        })
    }

    /// Publishes a complete entry atomically. Cache failures never affect extraction.
    pub fn store(&self, key: &str, facts: &TypedFacts) -> bool {
        self.store_entry(key, |bytes| encode_entry_into(key, facts, bytes).is_ok())
    }

    /// Publishes a canonical compressed local-facts payload.
    pub fn store_encoded(&self, key: &str, encoded_facts: &[u8], uncompressed_len: u64) -> bool {
        self.store_entry(key, |bytes| {
            bytes.clear();
            bytes.extend_from_slice(MAGIC_V5);
            key.encode(bytes);
            bytes.extend_from_slice(&(encoded_facts.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&uncompressed_len.to_le_bytes());
            bytes.extend_from_slice(&sha256_bytes(encoded_facts));
            bytes.extend_from_slice(encoded_facts);
            true
        })
    }

    fn store_entry(&self, key: &str, encode: impl FnOnce(&mut Vec<u8>) -> bool) -> bool {
        let Some(path) = self.entry_path(key) else {
            return false;
        };
        let Some(parent) = path.parent() else {
            return false;
        };
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
        let temporary = parent.join(format!(
            ".cache-{}-{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            key
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = EXTRACTION_CACHE_BUFFER.with(|buffer| {
            let mut bytes = buffer.borrow_mut();
            if !encode(&mut bytes) {
                return false;
            }
            options
                .open(&temporary)
                .and_then(|mut file| file.write_all(&bytes))
                .and_then(|()| fs::rename(&temporary, &path))
                .is_ok()
        });
        if !result {
            let _ = fs::remove_file(&temporary);
            return false;
        }
        true
    }

    fn entry_path(&self, key: &str) -> Option<PathBuf> {
        if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(
            self.root
                .as_ref()?
                .join(&key[..2])
                .join(format!("{key}.facts")),
        )
    }
}

/// Resolves the default root directory for the extraction cache.
pub fn default_cache_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ACDD_EXTRACTION_CACHE_DIR") {
        return Some(PathBuf::from(path));
    }
    #[cfg(test)]
    return Some(std::env::temp_dir().join(format!(
        "contextunity-acdd-cache-tests-{}",
        std::process::id()
    )));
    #[cfg(not(test))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|base| base.join("contextunity-acdd").join("extractions"))
    }
}

fn open_regular_file(path: &std::path::Path) -> std::io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "extraction cache entry is not a regular file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}
use crate::typed_facts::binary::{BinaryDecode, BinaryEncode};

#[cfg(test)]
fn encode_entry(key: &str, facts: &TypedFacts) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(4096);
    encode_entry_into(key, facts, &mut bytes)?;
    Ok(bytes)
}

fn encode_entry_into(key: &str, facts: &TypedFacts, bytes: &mut Vec<u8>) -> anyhow::Result<()> {
    anyhow::ensure!(
        !facts.flows.has_invalid_named_uncertainty_carriers(),
        "malformed named-definition uncertainty carrier"
    );
    bytes.clear();
    bytes.extend_from_slice(MAGIC_V4);
    key.encode(bytes);
    let payload_len_offset = bytes.len();
    bytes.extend_from_slice(&[0; 8]);
    let checksum_offset = bytes.len();
    bytes.extend_from_slice(&[0; CHECKSUM_BYTES]);
    let payload_offset = bytes.len();
    facts.encode(bytes);
    let payload_len = bytes.len() - payload_offset;
    let checksum = sha256_bytes(&bytes[payload_offset..]);
    bytes[payload_len_offset..payload_len_offset + 8]
        .copy_from_slice(&(payload_len as u64).to_le_bytes());
    bytes[checksum_offset..checksum_offset + CHECKSUM_BYTES].copy_from_slice(&checksum);
    Ok(())
}

#[cfg(test)]
fn decode_entry(bytes: &[u8], expected_key: &str, max_payload_bytes: u64) -> Option<TypedFacts> {
    decode_entry_with_encoded(bytes, expected_key, max_payload_bytes).map(|(facts, _)| facts)
}

#[cfg(test)]
fn decode_entry_with_encoded(
    bytes: &[u8],
    expected_key: &str,
    max_payload_bytes: u64,
) -> Option<(TypedFacts, Option<Vec<u8>>)> {
    decode_entry_for_build(bytes, expected_key, max_payload_bytes)
        .map(|(facts, encoded, _)| (facts, encoded))
}

fn decode_entry_for_build(
    bytes: &[u8],
    expected_key: &str,
    max_payload_bytes: u64,
) -> Option<BuildCacheEntry> {
    let v5 = bytes.starts_with(MAGIC_V5);
    if !v5 && !bytes.starts_with(MAGIC_V4) {
        return None;
    }
    let mut cursor = MAGIC_V4.len();
    let key_len = usize::try_from(u32::decode(bytes, &mut cursor)?).ok()?;
    let stored_key = bytes.get(cursor..cursor.checked_add(key_len)?)?;
    cursor += key_len;
    if stored_key != expected_key.as_bytes() {
        return None;
    }
    let payload_len = u64::decode(bytes, &mut cursor)?;
    if payload_len > max_payload_bytes {
        return None;
    }
    let uncompressed_len = if v5 {
        let length = u64::decode(bytes, &mut cursor)?;
        if length > max_payload_bytes {
            return None;
        }
        Some(length)
    } else {
        None
    };
    let checksum_end = cursor.checked_add(CHECKSUM_BYTES)?;
    let stored_checksum = bytes.get(cursor..checksum_end)?;
    cursor = checksum_end;
    let payload_len = usize::try_from(payload_len).ok()?;
    let payload_end = cursor.checked_add(payload_len)?;
    let payload = bytes.get(cursor..payload_end)?;
    if payload_end != bytes.len() || sha256_bytes(payload).as_slice() != stored_checksum {
        return None;
    }
    let mut raw = Vec::new();
    let decoded = if let Some(length) = uncompressed_len {
        let compressed = payload.strip_prefix(LOCAL_MAGIC)?;
        let decoder = zstd::stream::read::Decoder::new(compressed).ok()?;
        decoder
            .take(max_payload_bytes.saturating_add(1))
            .read_to_end(&mut raw)
            .ok()?;
        if raw.len() as u64 > max_payload_bytes || raw.len() as u64 != length {
            return None;
        }
        raw.as_slice()
    } else {
        payload
    };
    let mut payload_cursor = 0;
    let mut facts = if v5 {
        TypedFacts::decode_with_string_table(decoded, &mut payload_cursor)?
    } else {
        TypedFacts::decode(decoded, &mut payload_cursor)?
    };
    if payload_cursor != decoded.len() || facts.flows.has_invalid_named_uncertainty_carriers() {
        return None;
    }
    let mut components = Vec::new();
    facts.edges.retain(|edge| {
        if edge.src.starts_with("component:") {
            components.push(edge.clone());
            false
        } else {
            true
        }
    });
    let has_component_node = facts.nodes.iter().any(|node| node.kind == "component");
    facts.facts.retain_nodes(|node| node.kind != "component");
    Some((
        facts,
        v5.then(|| payload.to_vec()),
        (!has_component_node).then_some(components),
    ))
}

pub use crate::typed_facts::binary::{decode_references, encode_references};

#[cfg(test)]
mod tests;
