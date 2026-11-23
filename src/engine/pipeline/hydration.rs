use super::shared::{
    extract_typed_source_with_digest, hash_source_bytes, open_and_read_verified_source,
    sanitize_extracted_facts, SourceSnapshotMismatch,
};
use acdd_core::config::Adapter;
use acdd_index::{
    db::writer::cache::{self as extraction_cache, ExtractionCache},
    languages::dependency_registry::DependencyRegistry,
    models::FileEntry,
    typed_facts::TypedFacts,
    INDEX_SEMANTICS_VERSION,
};
use anyhow::Result;
use std::path::Path;

pub(super) fn extraction_cache_key_and_limit(
    digest: &str,
    path: &str,
    language: &str,
    effective_ruleset_hash: &str,
    bytes: u64,
    max_file_bytes: u64,
) -> (String, u64) {
    let key = extraction_cache::key(
        digest,
        path,
        language,
        INDEX_SEMANTICS_VERSION,
        effective_ruleset_hash,
    );
    let cache_limit = bytes
        .saturating_mul(64)
        .max(512 * 1024)
        .min(max_file_bytes.saturating_mul(16));
    (key, cache_limit)
}

pub(super) fn touch_cached_facts(facts: &mut TypedFacts, mtime_ns: u128) {
    let mtime = mtime_ns as f64 / 1e9;
    for section in &mut facts.docs {
        section.mtime = mtime;
    }
    sanitize_extracted_facts(facts);
}

pub(super) fn extract_typed_with_cache(
    root: &Path,
    file: &FileEntry,
    adapter: &Adapter,
    dependencies: &DependencyRegistry,
    extraction_cache: &ExtractionCache,
    effective_ruleset_hash: &str,
    source: &mut String,
) -> Result<TypedFacts> {
    let _source_file = open_and_read_verified_source(
        root,
        adapter,
        &file.path,
        file.bytes,
        None,
        source,
    )?;
    let source_digest = hash_source_bytes(source);
    if source_digest != file.digest {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            file.path
        ))
        .into());
    }
    let (key, cache_limit) = extraction_cache_key_and_limit(
        &source_digest,
        &file.path,
        &file.language,
        effective_ruleset_hash,
        file.bytes,
        adapter.limits.max_file_bytes,
    );
    if let Some(mut facts) = extraction_cache.load(&key, cache_limit) {
        touch_cached_facts(&mut facts, file.mtime_ns);
        return Ok(facts);
    }
    let spec_paths = adapter.spec_paths();
    let zones = adapter.architecture.zone_names();
    let facts = extract_typed_source_with_digest(
        file,
        source,
        &source_digest,
        dependencies,
        &spec_paths,
        Some(&zones),
    )?;
    extraction_cache.store(&key, &facts);
    Ok(facts)
}
