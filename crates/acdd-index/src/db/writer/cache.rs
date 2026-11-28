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
const MAGIC_V6: &[u8] = b"ACDD-EXTRACTION-FACTS\0\x06";
const CHECKSUM_BYTES: usize = 32;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static EXTRACTION_CACHE_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Contiguous packed string table backed by one string buffer and end offsets.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PackedStrings {
    pub(super) data: String,
    pub(super) ends: Vec<u32>,
}

impl PackedStrings {
    pub(super) fn append_with(&mut self, write: impl FnOnce(&mut String)) -> anyhow::Result<()> {
        write(&mut self.data);
        let end = u32::try_from(self.data.len()).context("packed string buffer exceeds u32")?;
        self.ends.push(end);
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.ends.len()
    }

    pub(super) fn get(&self, index: usize) -> Option<&str> {
        let end = *self.ends.get(index)? as usize;
        let start = if index == 0 {
            0
        } else {
            *self.ends.get(index - 1)? as usize
        };
        self.data.get(start..end)
    }

    pub(super) fn is_valid(&self) -> bool {
        let mut prev = 0usize;
        for &end in &self.ends {
            let end = end as usize;
            if end < prev || end > self.data.len() || !self.data.is_char_boundary(end) {
                return false;
            }
            prev = end;
        }
        true
    }

    pub(super) fn memory_bytes(&self) -> usize {
        self.data.capacity().saturating_add(
            self.ends
                .capacity()
                .saturating_mul(std::mem::size_of::<u32>()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NodeDocumentation {
    pub(super) index: u32,
    pub(super) doc: Option<serde_json::Value>,
    pub(super) docstring: Option<serde_json::Value>,
}

#[derive(Debug)]
pub struct PreparedCatalogStorage {
    pub(super) node_docs: Vec<NodeDocumentation>,
    pub(super) doc_bodies: Vec<(String, Vec<String>)>,
    pub(super) search_texts: PackedStrings,
    pub(super) reference_keys: Vec<String>,
    pub(super) leaves: BTreeMap<String, BTreeMap<String, String>>,
}

crate::struct_codecs! {
    PackedStrings { data, ends };
    NodeDocumentation { index, doc, docstring };
    PreparedCatalogStorage {
        node_docs,
        doc_bodies,
        search_texts,
        reference_keys,
        leaves
    };
}

const RAW_PREPARED_STORAGE_MAGIC: &[u8] = b"ACDD-RAW-STORAGE\0\x02";

impl PreparedCatalogStorage {
    pub(super) fn memory_bytes(&self) -> u64 {
        fn value_bytes(value: &serde_json::Value) -> usize {
            match value {
                serde_json::Value::String(s) => s.capacity(),
                serde_json::Value::Array(items) => items
                    .capacity()
                    .saturating_mul(std::mem::size_of::<serde_json::Value>())
                    .saturating_add(items.iter().map(value_bytes).sum::<usize>()),
                serde_json::Value::Object(items) => 1024usize.saturating_add(
                    items
                        .iter()
                        .map(|(k, v)| {
                            256usize
                                .saturating_add(k.capacity())
                                .saturating_add(value_bytes(v))
                        })
                        .sum::<usize>(),
                ),
                _ => 0,
            }
        }
        fn strings(items: &Vec<String>) -> usize {
            items
                .capacity()
                .saturating_mul(std::mem::size_of::<String>())
                .saturating_add(items.iter().map(String::capacity).sum::<usize>())
        }
        let node_docs = self
            .node_docs
            .capacity()
            .saturating_mul(std::mem::size_of::<NodeDocumentation>())
            .saturating_add(
                self.node_docs
                    .iter()
                    .map(|entry| {
                        entry.doc.as_ref().map_or(0, value_bytes)
                            + entry.docstring.as_ref().map_or(0, value_bytes)
                    })
                    .sum::<usize>(),
            );
        let doc_bodies = self
            .doc_bodies
            .capacity()
            .saturating_mul(std::mem::size_of::<(String, Vec<String>)>())
            .saturating_add(
                self.doc_bodies
                    .iter()
                    .map(|(content, invariants)| content.capacity() + strings(invariants))
                    .sum::<usize>(),
            );
        let leaves = 1024usize.saturating_add(
            self.leaves
                .iter()
                .map(|(domain, leaves)| {
                    1280usize
                        + domain.capacity()
                        + leaves
                            .iter()
                            .map(|(k, v)| 256usize + k.capacity() + v.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>(),
        );
        let bytes = std::mem::size_of::<Self>()
            .saturating_add(node_docs)
            .saturating_add(doc_bodies)
            .saturating_add(self.search_texts.memory_bytes())
            .saturating_add(strings(&self.reference_keys))
            .saturating_add(leaves);
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    pub(super) fn encode_spill(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(512);
        raw.extend_from_slice(RAW_PREPARED_STORAGE_MAGIC);
        self.encode(&mut raw);
        raw
    }
}

pub(super) fn decode_prepared_catalog_storage_payload(
    bytes: &[u8],
) -> anyhow::Result<PreparedCatalogStorage> {
    use crate::typed_facts::binary::BinaryDecode;
    let raw = bytes
        .strip_prefix(RAW_PREPARED_STORAGE_MAGIC)
        .context("unsupported prepared catalog storage")?;
    let mut cursor = 0;
    let prepared = PreparedCatalogStorage::decode(raw, &mut cursor)
        .context("corrupted prepared catalog storage")?;
    anyhow::ensure!(
        cursor == raw.len()
            && prepared.search_texts.is_valid()
            && prepared
                .node_docs
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index),
        "corrupted prepared catalog storage"
    );
    Ok(prepared)
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

/// Decoded extraction facts, encoded bytes, prior component edges, and optional skeleton blob.
pub type BuildCacheEntry = (
    TypedFacts,
    Option<Vec<u8>>,
    Option<Vec<Edge>>,
    Option<Vec<u8>>,
);

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

    /// Loads facts and, for V5/V6 entries, their canonical encoded payload.
    pub fn load_with_encoded(
        &self,
        key: &str,
        max_payload_bytes: u64,
    ) -> Option<(TypedFacts, Option<Vec<u8>>)> {
        self.load_for_build(key, max_payload_bytes)
            .map(|(facts, encoded, _, _)| (facts, encoded))
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

    /// Publishes a canonical compressed local-facts payload alongside optional statement skeletons.
    pub fn store_encoded_with_skeletons(
        &self,
        key: &str,
        encoded_facts: &[u8],
        uncompressed_len: u64,
        skeleton_blob: Option<&[u8]>,
    ) -> bool {
        self.store_entry(key, |bytes| {
            bytes.clear();
            bytes.extend_from_slice(MAGIC_V6);
            key.encode(bytes);
            bytes.extend_from_slice(&(encoded_facts.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&uncompressed_len.to_le_bytes());
            bytes.extend_from_slice(&sha256_bytes(encoded_facts));
            bytes.extend_from_slice(encoded_facts);
            match skeleton_blob {
                Some(skeletons) if !skeletons.is_empty() => {
                    bytes.extend_from_slice(&(skeletons.len() as u64).to_le_bytes());
                    bytes.extend_from_slice(&sha256_bytes(skeletons));
                    bytes.extend_from_slice(skeletons);
                }
                _ => {
                    bytes.extend_from_slice(&0u64.to_le_bytes());
                }
            }
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
    return Some(acdd_core::fs::temp_dir_root().join(format!(
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
        .map(|(facts, encoded, _, _)| (facts, encoded))
}

fn decode_checked_payload<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    payload_len: u64,
) -> Option<&'a [u8]> {
    let checksum_end = cursor.checked_add(CHECKSUM_BYTES)?;
    let stored_checksum = bytes.get(*cursor..checksum_end)?;
    *cursor = checksum_end;
    let len = usize::try_from(payload_len).ok()?;
    let payload_end = cursor.checked_add(len)?;
    let payload = bytes.get(*cursor..payload_end)?;
    if sha256_bytes(payload).as_slice() != stored_checksum {
        return None;
    }
    *cursor = payload_end;
    Some(payload)
}

fn decode_entry_for_build(
    bytes: &[u8],
    expected_key: &str,
    max_payload_bytes: u64,
) -> Option<BuildCacheEntry> {
    let v6 = bytes.starts_with(MAGIC_V6);
    let v5 = bytes.starts_with(MAGIC_V5);
    let compressed_format = v5 || v6;
    if !compressed_format && !bytes.starts_with(MAGIC_V4) {
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
    let uncompressed_len = if compressed_format {
        let length = u64::decode(bytes, &mut cursor)?;
        if length > max_payload_bytes {
            return None;
        }
        Some(length)
    } else {
        None
    };
    let payload = decode_checked_payload(bytes, &mut cursor, payload_len)?;
    let skeleton_blob = if v6 {
        let skeleton_len = u64::decode(bytes, &mut cursor)?;
        if skeleton_len > max_payload_bytes {
            return None;
        }
        if skeleton_len > 0 {
            Some(decode_checked_payload(bytes, &mut cursor, skeleton_len)?.to_vec())
        } else {
            None
        }
    } else {
        None
    };
    if cursor != bytes.len() {
        return None;
    }
    let mut raw = Vec::new();
    let decoded = if let Some(length) = uncompressed_len {
        let compressed = crate::typed_facts::compressed_facts_payload(payload)?;
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
    let mut facts = if compressed_format {
        TypedFacts::decode_with_string_table(decoded, &mut payload_cursor)?
    } else {
        TypedFacts::decode(decoded, &mut payload_cursor)?
    };
    if payload_cursor != decoded.len() || facts.flows.has_invalid_named_uncertainty_carriers() {
        return None;
    }
    if let Some(ref skel) = skeleton_blob {
        if !crate::typed_facts::skeletons::hydrate_statement_skeletons(&mut facts.facts, skel) {
            return None;
        }
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
    let has_component_node = facts
        .nodes
        .iter()
        .any(|node| node.kind == crate::models::tags::NodeKind::Component);
    facts
        .facts
        .retain_nodes(|node| node.kind != crate::models::tags::NodeKind::Component);
    Some((
        facts,
        compressed_format.then(|| payload.to_vec()),
        (!has_component_node).then_some(components),
        skeleton_blob,
    ))
}

#[cfg(test)]
pub(crate) use crate::typed_facts::binary::encode_references;

#[cfg(test)]
mod tests;
