use super::*;

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
    let path = resolve_file_path(root, adapter, &file.path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut source_file = options.open(&path)?;
    let source_capacity =
        usize::try_from(file.bytes).context("source size exceeds addressable memory")?;
    source.clear();
    source.try_reserve_exact(source_capacity)?;
    source_file
        .by_ref()
        .take(adapter.limits.max_file_bytes.saturating_add(1))
        .read_to_string(source)?;
    if source.len() as u64 > adapter.limits.max_file_bytes {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            file.path
        ))
        .into());
    }
    let source_digest = commitments::hash(source.as_bytes());
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
