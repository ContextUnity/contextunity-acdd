use super::cold::validate_code_map_target;
use super::hooks::PipelineTestHooks;
use super::hydration::extract_typed_with_cache;
use super::invalidation::*;
use super::shared::{assign_components, BuildMode, SourceSnapshotMismatch};
use acdd_core::{
    config::{self, checked_child, load_adapter, Adapter, ENGINE_SCHEMA_VERSION},
    errors::{ErrorCode, StateError},
    fs::canonical_root,
};
use acdd_index::{
    ast,
    db::{
        cache, commitments as db_commitments, reader as db_reader,
        writer::{
            cache::{self as extraction_cache, ExtractionCache},
            delta_persist,
        },
    },
    languages::{self, dependency_registry::DependencyRegistry, module_name},
    linker,
    models::{stable_hash64, FileEntry, Node},
    scanner,
    typed_facts::{decode_facts, TypedFacts},
};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

fn reference_keys(facts: &TypedFacts) -> Vec<&str> {
    facts.collect_reference_keys(true)
}

/// Applies a delta with an explicit adapter selection, retaining legacy persisted-policy defaults.
pub fn delta_with_selection(
    root: &Path,
    db: &Path,
    modified: &[PathBuf],
    adapter: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<Value> {
    delta_inner(root, db, modified, adapter, cache_dir, None, None)
}

/// Applies a delta with an explicit adapter selection and pre-publish daemon guard.
pub(crate) fn delta_with_selection_and_guard(
    root: &Path,
    db: &Path,
    modified: &[PathBuf],
    adapter: Option<&Path>,
    cache_dir: Option<&Path>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    delta_inner(root, db, modified, adapter, cache_dir, None, before_publish)
}

/// Applies a delta with test synchronization around a persistence failure.
#[cfg(test)]
pub(super) fn delta_with_test_hooks(
    root: &Path,
    db: &Path,
    modified: &[PathBuf],
    cache_dir: &Path,
    hooks: &PipelineTestHooks,
) -> Result<Value> {
    delta_inner(root, db, modified, None, Some(cache_dir), Some(hooks), None)
}

fn rebuild_with_cache_dir(
    root: &Path,
    db: &Path,
    adapter: Option<&Path>,
    cache_dir: Option<&Path>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    super::cold::build_with_selection_and_guard(
        root,
        db,
        adapter,
        cache_dir,
        None,
        before_publish,
    )
}


struct DeltaAdmission {
    root: PathBuf,
    adapter: Adapter,
    dependencies: DependencyRegistry,
    scan: scanner::ScanReport,
    inventory: BTreeMap<String, FileEntry>,
    previous: Vec<FileEntry>,
    modified: BTreeSet<String>,
    scan_ms: f64,
    generation_lock: acdd_core::lock::FileLockGuard,
    idle_readers_guard: cache::IdleReadersGuard,
    admitted: Connection,
    admitted_identity: cache::Identity,
    expected: String,
}

enum AdmissionResult {
    RebuildCold(Option<PathBuf>),
    Admitted(Box<DeltaAdmission>),
}

fn admit_delta_generation(
    root: &Path,
    db: &Path,
    modified: &[PathBuf],
    selected_adapter: Option<&Path>,
    _cache_dir: Option<&Path>,
) -> Result<AdmissionResult> {
    if modified.is_empty() {
        bail!("delta requires at least one modified path");
    }
    let root = canonical_root(root)?;
    // Reject invalid explicit policy before opening or publishing an index generation.
    let selected_adapter = selected_adapter
        .map(|path| load_adapter(&root, Some(path)))
        .transpose()?;
    if selected_adapter
        .as_ref()
        .is_some_and(|adapter| !adapter.modules.is_enabled("code") && !adapter.modules.is_enabled("docs"))
    {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "FEATURE_DISABLED: both 'code' and 'docs' modules are disabled",
        )
        .into());
    }
    let generation_lock = cache::exclusive_lock(db)?;
    let _idle_readers_guard = cache::release_idle_readers(db)?;
    let (schema_compatible, previous_adapter_path) = {
        let probe = db_reader::open_raw_confined_readonly(db)?;
        let schema_compatible = probe
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .as_deref()
            == Some(ENGINE_SCHEMA_VERSION);
        let raw_policy: Option<String> = probe
            .query_row(
                "SELECT value FROM metadata WHERE key='adapter'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let adapter_path = raw_policy
            .map(|raw| {
                let value: Value = serde_json::from_str(&raw)?;
                config::persisted_adapter_path(&root, &value)
            })
            .transpose()?
            .flatten();
        (schema_compatible, adapter_path)
    };
    if !schema_compatible {
        drop(_idle_readers_guard);
        drop(generation_lock);
        let adapter_path = selected_adapter
            .as_ref()
            .and_then(|adapter| adapter.adapter_path.as_deref())
            .or(previous_adapter_path.as_deref())
            .map(Path::to_path_buf);
        return Ok(AdmissionResult::RebuildCold(adapter_path));
    }
    let admitted = db_reader::open(db, &root)?;
    let admitted_identity = cache::identity(db)?;
    let expected: String = admitted.query_row(
        "SELECT value FROM metadata WHERE key='output_root'",
        [],
        |r| r.get(0),
    )?;
    let adapter = db_reader::read_policy(&root, &admitted)?;
    let current_adapter = match selected_adapter {
        Some(adapter) => adapter,
        None => load_adapter(&root, adapter.adapter_path.as_deref())?,
    };
    if !current_adapter.modules.is_enabled("code") && !current_adapter.modules.is_enabled("docs") {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "FEATURE_DISABLED: both 'code' and 'docs' modules are disabled",
        )
        .into());
    }
    config::check_root_scope(&root, current_adapter.limits.allow_broad_root)?;
    for path in modified {
        checked_child(&root, path).with_context(|| format!("inspect {}", path.display()))?;
    }
    let manifests_changed = current_adapter.modules.is_enabled("code")
        && modified.iter().any(|path| {
            path.file_name().and_then(|name| name.to_str()).is_some_and(
                languages::dependency_registry::is_dependency_manifest_filename,
            )
        });
    if current_adapter.digest != adapter.digest
        || current_adapter.adapter_path != adapter.adapter_path
        || current_adapter.linked_workspaces != adapter.linked_workspaces
        || manifests_changed
    {
        drop(admitted);
        drop(_idle_readers_guard);
        drop(generation_lock);
        return Ok(AdmissionResult::RebuildCold(
            current_adapter.adapter_path,
        ));
    }
    let previous = db_reader::inventory_snapshot(&admitted)?;
    let watched: BTreeSet<String> = previous
        .iter()
        .map(|entry| entry.path.clone())
        .chain(
            modified
                .iter()
                .map(|path| path.to_string_lossy().replace('\\', "/")),
        )
        .collect();
    let scanning = Instant::now();
    let scan = scanner::scan_reusing_watching(&root, &adapter, &previous, &watched)?;
    let scan_ms = scanning.elapsed().as_secs_f64() * 1000.;
    let dependencies = DependencyRegistry::try_collect_with_paths(
        &root,
        Some(&adapter),
        &scan.dependency_manifests,
    )?;
    let previous_manifest_digest = admitted
        .query_row(
            "SELECT value FROM metadata WHERE key='manifest_digest'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if previous_manifest_digest.as_deref() != Some(dependencies.digest()) {
        drop(admitted);
        drop(_idle_readers_guard);
        drop(generation_lock);
        return Ok(AdmissionResult::RebuildCold(adapter.adapter_path));
    }
    let modified: BTreeSet<String> = modified
        .iter()
        .map(|p| {
            checked_child(&root, p).with_context(|| format!("inspect {}", p.display()))?;
            Ok(p.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Result<_>>()?;
    let inventory_owned: BTreeMap<String, FileEntry> =
        scan.entries.iter().map(|f| (f.path.clone(), f.clone())).collect();
    let previous_owned = previous;
    let inventory: BTreeMap<&str, &FileEntry> =
        inventory_owned.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let previous: BTreeMap<&str, &FileEntry> =
        previous_owned.iter().map(|f| (f.path.as_str(), f)).collect();
    for (path, old) in &previous {
        if !modified.contains(*path) && inventory.get(path).is_none_or(|f| f.digest != old.digest) {
            return Err(SourceSnapshotMismatch(format!("unlisted source change: {path}")).into());
        }
    }
    for path in inventory.keys() {
        if !previous.contains_key(path) && !modified.contains(*path) {
            return Err(SourceSnapshotMismatch(format!("unlisted source addition: {path}")).into());
        }
    }
    let existing = scanner::exact_paths_exist(
        &root,
        &adapter,
        modified
            .iter()
            .filter(|path| {
                !inventory.contains_key(path.as_str()) && !scan.matched_excluded.contains(*path)
            })
            .map(String::as_str),
    )?;
    for path in &modified {
        if !inventory.contains_key(path.as_str()) {
            if scan.matched_excluded.contains(path) || existing.contains(path) {
                bail!("modified path outside admitted inventory: {path}");
            }
            if !previous.contains_key(path.as_str()) {
                bail!("deletion was not in previous inventory: {path}");
            }
        }
    }

    Ok(AdmissionResult::Admitted(Box::new(DeltaAdmission {
        root,
        adapter,
        dependencies,
        scan,
        inventory: inventory_owned,
        previous: previous_owned,
        modified,
        scan_ms,
        generation_lock,
        idle_readers_guard: _idle_readers_guard,
        admitted,
        admitted_identity,
        expected,
    })))
}


struct DeltaFrontiers<'a> {
    frontier_tables: TempFrontierTables<'a>,
    facts: BTreeMap<String, TypedFacts>,
    changed_resolution: BTreeSet<String>,
    affected: BTreeSet<String>,
    named_candidates: BTreeSet<String>,
    replace_files: BTreeSet<String>,
    component_owners: BTreeMap<String, String>,
    load_owners: BTreeSet<String>,
    loaded_fact_files: usize,
    extract_ms: f64,
}

fn compute_delta_frontiers<'a>(
    admission: &'a DeltaAdmission,
    cache_dir: Option<&Path>,
) -> Result<DeltaFrontiers<'a>> {
    let inventory: BTreeMap<&str, &FileEntry> = admission
        .inventory
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let previous: BTreeMap<&str, &FileEntry> = admission
        .previous
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let frontier_tables = ensure_temp_frontier_tables(&admission.admitted)?;
    populate_temp_text_frontier(
        &admission.admitted,
        "delta_modified_paths",
        admission.modified.iter().map(String::as_str),
    )?;

    let extracting = Instant::now();
    let extracted =
        extract_modified_and_detect_resolution_changes(admission, cache_dir, &inventory, &previous)?;
    let mut facts = extracted.facts;
    let changed_resolution = extracted.changed_resolution;
    let old_modified_facts = extracted.old_modified_facts;
    let names = collect_changed_resolution_names(
        admission,
        &facts,
        &old_modified_facts,
        &changed_resolution,
        &inventory,
        &previous,
    )?;
    let extract_ms = extracting.elapsed().as_secs_f64() * 1000.;

    let mut affected = seed_initial_affected_owners(
        admission,
        &facts,
        &old_modified_facts,
        &changed_resolution,
        &names,
        &inventory,
        &previous,
    )?;
    drop(old_modified_facts);

    let loaded_fact_files = expand_import_frontier(
        &admission.admitted,
        &changed_resolution,
        &inventory,
        &previous,
        &mut facts,
        &mut affected,
    )?;

    let route_include_paths =
        route_include_closure(&admission.admitted, &facts, &admission.modified, &inventory)?;
    let mut replace_files = admission.modified.clone();
    replace_files.extend(route_include_paths.iter().cloned());
    affected.extend(route_include_paths);
    let component_owners = delta_components(
        &admission.admitted,
        &admission.modified,
        &facts,
        &mut replace_files,
    )?;
    affected.extend(replace_files.iter().cloned());

    let load_owners = expand_template_and_named_frontiers(
        admission,
        &facts,
        &changed_resolution,
        &inventory,
        &previous,
        &mut affected,
    )?;

    Ok(DeltaFrontiers {
        frontier_tables,
        facts,
        changed_resolution,
        affected,
        named_candidates: load_owners.named_candidates,
        replace_files,
        component_owners,
        load_owners: load_owners.load_owners,
        loaded_fact_files,
        extract_ms,
    })
}

struct ExtractedDeltaFacts {
    facts: BTreeMap<String, TypedFacts>,
    changed_resolution: BTreeSet<String>,
    old_modified_facts: BTreeMap<String, TypedFacts>,
}

struct TemplateFrontierOutputs {
    named_candidates: BTreeSet<String>,
    load_owners: BTreeSet<String>,
}

fn load_frontier_local_facts<'a>(
    admitted: &rusqlite::Connection,
    paths: impl Iterator<Item = &'a str>,
) -> Result<Vec<(String, TypedFacts)>> {
    populate_temp_text_frontier(admitted, "delta_frontier_text", paths)?;
    let mut st = admitted.prepare(
        "SELECT path,facts_blob FROM local_facts WHERE path IN(SELECT key FROM temp.delta_frontier_text)",
    )?;
    let raw_blobs = st
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw_blobs
        .into_par_iter()
        .map(|(path, blob)| Ok((path, decode_facts(&blob)?)))
        .collect()
}

fn extract_modified_and_detect_resolution_changes(
    admission: &DeltaAdmission,
    cache_dir: Option<&Path>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
) -> Result<ExtractedDeltaFacts> {
    let root = &admission.root;
    let adapter = &admission.adapter;
    let dependencies = &admission.dependencies;
    let modified = &admission.modified;
    let admitted = &admission.admitted;

    let extraction_cache = Arc::new(ExtractionCache::new(match cache_dir {
        Some(dir) => Some(dir.to_owned()),
        None => extraction_cache::default_cache_root()
            .map(|path| acdd_core::fs::resolve_workspace_path(root, &path)),
    }));
    let effective_ruleset_hash =
        extraction_cache::effective_ruleset_hash(&adapter.digest, dependencies.digest());
    let modified_files: Vec<&FileEntry> = modified
        .iter()
        .filter_map(|path| inventory.get(path.as_str()).copied())
        .collect();
    let extracted_pairs: Vec<(String, TypedFacts)> = modified_files
        .par_iter()
        .map_init(String::new, |source, &file| {
            let extracted = extract_typed_with_cache(
                root,
                file,
                adapter,
                dependencies,
                &extraction_cache,
                &effective_ruleset_hash,
                source,
            )?;
            Ok((file.path.clone(), extracted))
        })
        .collect::<Result<_>>()?;
    let facts: BTreeMap<String, TypedFacts> = extracted_pairs.into_iter().collect();
    let mut changed_resolution: BTreeSet<String> = modified
        .iter()
        .filter(|path| !inventory.contains_key(path.as_str()) || !previous.contains_key(path.as_str()))
        .cloned()
        .collect();
    let existing_modified: Vec<&str> = facts
        .keys()
        .filter(|path| previous.contains_key(path.as_str()))
        .map(String::as_str)
        .collect();
    let old_modified_facts: BTreeMap<String, TypedFacts> = if existing_modified.is_empty() {
        BTreeMap::new()
    } else {
        let map: BTreeMap<String, TypedFacts> =
            load_frontier_local_facts(admitted, existing_modified.iter().copied())?
                .into_iter()
                .collect();
        let identity_changed: Vec<String> = existing_modified
            .par_iter()
            .filter_map(|&path| {
                let new_facts = facts.get(path)?;
                let Some(old_facts) = map.get(path) else {
                    return Some(Ok(path.to_owned()));
                };
                match resolution_identity_changed_facts(old_facts, new_facts) {
                    Ok(true) => Some(Ok(path.to_owned())),
                    Ok(false) => (linker::needs_reference_identity(path, new_facts)
                        && linker::reference_identity_changed(path, old_facts, new_facts))
                    .then(|| Ok(path.to_owned())),
                    Err(err) => Some(Err(err)),
                }
            })
            .collect::<Result<_>>()?;
        changed_resolution.extend(identity_changed);
        map
    };
    Ok(ExtractedDeltaFacts {
        facts,
        changed_resolution,
        old_modified_facts,
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_changed_symbol_names(
    names: &mut BTreeSet<String>,
    name: &str,
    qual: &str,
    kind: &str,
    mod_name: &str,
    local_module: &str,
    src_module: Option<&str>,
) {
    names.insert(qual.to_owned());
    if kind == "route" {
        names.insert(name.to_owned());
    }
    if !matches!(kind, "field" | "method" | "route" | "document") {
        let is_top = qual == name
            || qual
                .strip_prefix(mod_name)
                .and_then(|s| s.strip_prefix('.'))
                == Some(name)
            || qual
                .strip_prefix(local_module)
                .and_then(|s| s.strip_prefix('.'))
                == Some(name)
            || src_module.is_some_and(|m| {
                qual.strip_prefix(m).and_then(|s| s.strip_prefix('.')) == Some(name)
            });
        if is_top {
            names.insert(name.to_owned());
        }
    }
}

fn collect_changed_resolution_names(
    admission: &DeltaAdmission,
    facts: &BTreeMap<String, TypedFacts>,
    old_modified_facts: &BTreeMap<String, TypedFacts>,
    changed_resolution: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
) -> Result<BTreeSet<String>> {
    let admitted = &admission.admitted;
    let dependencies = &admission.dependencies;
    let mut names = BTreeSet::new();
    let callable_providers = linker::configured_callable_provider_paths(
        changed_resolution,
        inventory
            .iter()
            .chain(previous.iter())
            .map(|(path, file)| (*path, file.language.as_str())),
        dependencies,
    );
    if !callable_providers.is_empty() {
        // Configuration-only changes seed old exports before unchanged consumer
        // selection. These indexed names grant no callable resolution authority.
        populate_temp_text_frontier(
            admitted,
            "delta_frontier_text",
            callable_providers.iter().map(String::as_str),
        )?;
        let mut st = admitted.prepare(
            "SELECT json_extract(binding.value,'$.name'),json_extract(binding.value,'$.local') FROM path_dictionary p JOIN nodes n INDEXED BY idx_nodes_path ON n.path_id=p.path_id JOIN json_each(n.details,'$.exports') binding WHERE p.path IN(SELECT key FROM temp.delta_frontier_text) AND n.kind='module'",
        )?;
        for row in st.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })? {
            let (public, local) = row?;
            names.extend(public.into_iter().chain(local));
        }
    }
    populate_temp_text_frontier(
        admitted,
        "delta_changed_resolution",
        changed_resolution.iter().map(String::as_str),
    )?;
    if !changed_resolution.is_empty() {
        let mut st = admitted.prepare(
            "SELECT owner.path,n.name,n.qualname,n.kind FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE owner.path IN(SELECT key FROM temp.delta_changed_resolution)",
        )?;
        for row in st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })? {
            let (owner, name, qual, kind) = row?;
            let mod_name = module_name(&owner);
            let (_, local_path) = config::workspace_path(&owner);
            let local_module = module_name(local_path);
            let src_module = local_path
                .split_once("/src/")
                .map(|(_, r)| module_name(r))
                .or_else(|| local_path.strip_prefix("src/").map(module_name));
            insert_changed_symbol_names(
                &mut names,
                &name,
                &qual,
                &kind,
                &mod_name,
                &local_module,
                src_module.as_deref(),
            );
        }
    }
    if !changed_resolution.is_empty() {
        let mut missing_old_resolution = Vec::new();
        for path in changed_resolution {
            if let Some(old) = old_modified_facts.get(path) {
                names.extend(linker::configured_result_names(old, dependencies));
            } else {
                missing_old_resolution.push(path.as_str());
            }
        }
        if !missing_old_resolution.is_empty() {
            populate_temp_text_frontier(
                admitted,
                "delta_frontier_text",
                missing_old_resolution,
            )?;
            let mut st = admitted.prepare(
                "SELECT facts_blob FROM local_facts WHERE path IN(SELECT key FROM temp.delta_frontier_text)",
            )?;
            let raw_blobs = st
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let decoded: Vec<TypedFacts> = raw_blobs
                .into_par_iter()
                .map(|blob| decode_facts(&blob))
                .collect::<Result<_>>()?;
            for old in &decoded {
                names.extend(linker::configured_result_names(old, dependencies));
            }
        }
    }
    for path in changed_resolution {
        let mod_name = module_name(path);
        names.insert(mod_name.clone());
        let (_, local_path) = config::workspace_path(path);
        let local_module = module_name(local_path);
        names.insert(local_module.clone());
        let src_module = local_path
            .split_once("/src/")
            .map(|(_, r)| module_name(r))
            .or_else(|| local_path.strip_prefix("src/").map(module_name));
        if let Some(src_mod) = &src_module {
            names.insert(src_mod.clone());
        }
        let mut stable_module = false;
        if let Some(f) = facts.get(path) {
            names.extend(linker::configured_result_names(f, dependencies));
            let previous_module: Option<(String, String, String)> = admitted
                .prepare_cached("SELECT n.id,n.qualname,n.language FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE owner.path=?1 AND n.kind='module'")?
                .query_row([path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            stable_module = previous_module
                .as_ref()
                .is_some_and(|(id, qualname, language)| {
                    f.nodes.iter().any(|n| {
                        n.kind == acdd_index::models::tags::NodeKind::Module
                            && &n.id == id
                            && &n.qualname == qualname
                            && n.language.as_str() == language
                    })
                });
            for n in &f.nodes {
                insert_changed_symbol_names(
                    &mut names,
                    &n.name,
                    &n.qualname,
                    n.kind.as_str(),
                    &mod_name,
                    &local_module,
                    src_module.as_deref(),
                );
            }
        }
        if !stable_module {
            if let Some((_, last)) = local_module.rsplit_once('.') {
                names.insert(last.to_owned());
            }
        }
    }
    Ok(names)
}

fn seed_initial_affected_owners(
    admission: &DeltaAdmission,
    facts: &BTreeMap<String, TypedFacts>,
    old_modified_facts: &BTreeMap<String, TypedFacts>,
    changed_resolution: &BTreeSet<String>,
    names: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
) -> Result<BTreeSet<String>> {
    let admitted = &admission.admitted;
    let modified = &admission.modified;
    let mut affected = modified.clone();
    if !changed_resolution.is_empty() {
        let mut st = admitted.prepare(
            "SELECT p.path FROM dependencies d JOIN path_dictionary p ON p.path_id=d.owner_id WHERE d.target_hash IN(SELECT path_hash FROM files WHERE path IN(SELECT key FROM temp.delta_changed_resolution))",
        )?;
        for owner in st.query_map([], |r| r.get::<_, String>(0))? {
            affected.insert(owner?);
        }
    }
    #[cfg(feature = "lang-html")]
    {
        let html_targets: BTreeMap<_, Vec<_>> = modified
            .iter()
            .filter(|path| {
                inventory
                    .get(path.as_str())
                    .or_else(|| previous.get(path.as_str()))
                    .is_some_and(|file| file.language == "html")
            })
            .fold(BTreeMap::new(), |mut targets, path| {
                targets
                    .entry(stable_hash64(path))
                    .or_insert_with(Vec::new)
                    .push(path.as_str());
                targets
            });
        if !html_targets.is_empty() {
            populate_temp_int_frontier(
                admitted,
                "delta_frontier_int",
                html_targets.keys().copied(),
            )?;
            let mut st = admitted.prepare(
                "SELECT p.path,d.symbol,d.target_hash FROM dependencies d JOIN path_dictionary p ON p.path_id=d.owner_id WHERE d.kind='renders' AND d.target_hash IN(SELECT key FROM temp.delta_frontier_int)",
            )?;
            let family = languages::LanguageFamily("python");
            for row in st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })? {
                let (owner, symbol, hash) = row?;
                let Some(target) = languages::html::local_template_target(
                    &owner,
                    &symbol,
                    &admission.dependencies.language_context_for_path(&owner),
                ) else {
                    continue;
                };
                if inventory
                    .get(owner.as_str())
                    .is_some_and(|file| file.language == "python")
                    && html_targets
                        .get(&hash)
                        .is_some_and(|paths| paths.contains(&target.as_str()))
                    && is_scoped_html_pair(&owner, &target, &admission.dependencies, family)
                {
                    affected.insert(owner);
                }
            }
        }
    }
    if !names.is_empty() {
        populate_temp_int_frontier(
            admitted,
            "delta_frontier_int",
            names.iter().map(|name| stable_hash64(name)),
        )?;
        populate_temp_text_frontier(
            admitted,
            "delta_frontier_text",
            names.iter().map(String::as_str),
        )?;
        let mut st = admitted.prepare("SELECT p.path FROM shared_owners s JOIN shared_keys k ON k.key_hash=s.key_hash LEFT JOIN nodes key_node ON typeof(k.key)='integer' AND key_node.node_id=k.key JOIN path_dictionary p ON p.path_id=s.owner_id WHERE s.kind_id=1 AND s.key_hash IN(SELECT key FROM temp.delta_frontier_int) AND (CASE WHEN typeof(k.key)='integer' THEN key_node.qualname ELSE k.key END) IN(SELECT key FROM temp.delta_frontier_text)")?;
        for owner in st.query_map([], |r| r.get::<_, String>(0))? {
            affected.insert(owner?);
        }
    }
    let changed_rust_paths: BTreeSet<_> = changed_resolution
        .iter()
        .filter(|path| {
            inventory
                .get(path.as_str())
                .or_else(|| previous.get(path.as_str()))
                .is_some_and(|file| file.language == "rust")
        })
        .cloned()
        .collect();
    let changed_impl_methods =
        changed_rust_impl_methods(admitted, &changed_rust_paths, facts, old_modified_facts)?;
    affected.extend(persisted_possible_call_owners(admitted, &changed_rust_paths)?);
    affected.extend(unchanged_trait_dispatch_owners(
        admitted,
        &changed_impl_methods,
    )?);
    Ok(affected)
}

fn expand_import_frontier(
    admitted: &Connection,
    changed_resolution: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
    facts: &mut BTreeMap<String, TypedFacts>,
    affected: &mut BTreeSet<String>,
) -> Result<usize> {
    let mut loaded_fact_files = 0;
    let is_supported = |lang: &str| {
        matches!(
            lang,
            "python" | "javascript" | "typescript" | "vue" | "rust"
        )
    };
    let mut import_frontier: BTreeSet<_> = changed_resolution
        .iter()
        .filter(|path| {
            inventory
                .get(path.as_str())
                .or_else(|| previous.get(path.as_str()))
                .is_some_and(|file| is_supported(&file.language))
        })
        .cloned()
        .collect();
    let mut visited_imports = import_frontier.clone();
    let mut expanded_imports = import_frontier.clone();
    while !import_frontier.is_empty() {
        populate_temp_text_frontier(
            admitted,
            "delta_frontier_text",
            import_frontier.iter().map(String::as_str),
        )?;
        let mut importing_owners = admitted.prepare_cached(
            "SELECT DISTINCT owner.path,target.path FROM dependencies d JOIN path_dictionary owner ON owner.path_id=d.owner_id JOIN files target ON target.path_hash=d.target_hash WHERE d.kind='imports' AND target.path IN(SELECT key FROM temp.delta_frontier_text)",
        )?;
        let importers: Vec<(String, String)> = importing_owners
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(importing_owners);
        let mut next = BTreeSet::new();
        for (owner, upstream) in importers {
            let supported = inventory
                .get(owner.as_str())
                .is_some_and(|file| is_supported(&file.language));
            if supported && visited_imports.insert(owner.clone()) {
                affected.insert(owner.clone());
            }
            if supported
                && module_exposes_provider_contract(
                    admitted,
                    &owner,
                    &upstream,
                    facts,
                    &mut loaded_fact_files,
                )?
                && expanded_imports.insert(owner.clone())
            {
                next.insert(owner);
            }
        }
        import_frontier = next;
    }
    Ok(loaded_fact_files)
}

#[cfg(feature = "lang-html")]
fn is_scoped_html_pair(
    owner: &str,
    target: &str,
    dependencies: &DependencyRegistry,
    family: languages::LanguageFamily,
) -> bool {
    config::workspace_path(owner).0 == config::workspace_path(target).0
        && dependencies.nearest_manifest_scope_for_path(family, owner)
            == dependencies.nearest_manifest_scope_for_path(family, target)
}

#[cfg(feature = "lang-html")]
fn initial_html_frontier(
    affected: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let frontier: BTreeSet<_> = affected
        .iter()
        .filter(|path| {
            inventory
                .get(path.as_str())
                .is_some_and(|file| file.language == "html")
        })
        .cloned()
        .collect();
    let visited = frontier.clone();
    (frontier, visited)
}

#[cfg(feature = "lang-html")]
fn expand_html_include_targets(
    admission: &DeltaAdmission,
    facts: &BTreeMap<String, TypedFacts>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
    affected: &mut BTreeSet<String>,
) -> Result<()> {
    let admitted = &admission.admitted;
    let dependencies = &admission.dependencies;
    let family = languages::LanguageFamily("python");
    let modified_python: Vec<_> = admission
        .modified
        .iter()
        .filter(|path| {
            inventory
                .get(path.as_str())
                .or_else(|| previous.get(path.as_str()))
                .is_some_and(|file| file.language == "python")
        })
        .collect();
    if !modified_python.is_empty() {
        populate_temp_text_frontier(
            admitted,
            "delta_frontier_text",
            modified_python.iter().map(|path| path.as_str()),
        )?;
        let mut old_targets = admitted.prepare(
            "SELECT DISTINCT f.path FROM dependencies d JOIN path_dictionary owner ON owner.path_id=d.owner_id JOIN files f ON f.path_hash=d.target_hash WHERE d.kind='renders' AND owner.path IN(SELECT key FROM temp.delta_frontier_text)",
        )?;
        for row in old_targets.query_map([], |row| row.get::<_, String>(0))? {
            let target = row?;
            if inventory
                .get(target.as_str())
                .is_some_and(|file| file.language == "html")
            {
                affected.insert(target);
            }
        }
        for provider in modified_python {
            let Some(provider_facts) = facts.get(provider.as_str()) else {
                continue;
            };
            let scope = dependencies.nearest_manifest_scope_for_path(family, provider);
            let workspace = config::workspace_path(provider).0;
            for node in provider_facts
                .nodes
                .iter()
                .filter(|node| node.language == acdd_index::models::tags::LanguageId::Python)
            {
                let Some(flow) = provider_facts.flows.get(node) else {
                    continue;
                };
                for context in flow.template_contexts() {
                    let Some(target) = context.target_path.clone() else {
                        continue;
                    };
                    if inventory
                        .get(target.as_str())
                        .is_some_and(|file| file.language == "html")
                        && config::workspace_path(&target).0 == workspace
                        && dependencies.nearest_manifest_scope_for_path(family, &target) == scope
                    {
                        affected.insert(target);
                    }
                }
            }
        }
    }
    let (mut frontier, mut visited) = initial_html_frontier(affected, inventory);
    for _ in 0..16 {
        if frontier.is_empty() {
            break;
        }
        populate_temp_text_frontier(
            admitted,
            "delta_frontier_text",
            frontier.iter().map(String::as_str),
        )?;
        let mut old_includes = admitted.prepare_cached(
            "SELECT owner.path,f.path FROM dependencies d JOIN path_dictionary owner ON owner.path_id=d.owner_id JOIN files f ON f.path_hash=d.target_hash WHERE d.kind='includes' AND owner.path IN(SELECT key FROM temp.delta_frontier_text)",
        )?;
        let rows: Vec<(String, String)> = old_includes
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(old_includes);
        let mut next = BTreeSet::new();
        let mut record_target = |owner: &str, target: String| {
            if inventory
                .get(target.as_str())
                .is_some_and(|file| file.language == "html")
                && is_scoped_html_pair(owner, &target, dependencies, family)
                && visited.insert(target.clone())
            {
                affected.insert(target.clone());
                next.insert(target);
            }
        };
        for (owner, target) in rows {
            record_target(&owner, target);
        }
        for owner in &frontier {
            let Some(file_facts) = facts.get(owner.as_str()) else {
                continue;
            };
            for reference in file_facts
                .references
                .iter()
                .filter(|reference| reference.kind == "includes")
            {
                let Some(target) = languages::html::local_template_target(
                    owner,
                    &reference.expression,
                    &dependencies.language_context_for_path(owner),
                ) else {
                    continue;
                };
                record_target(owner, target);
            }
        }
        frontier = next;
    }
    Ok(())
}

#[cfg(feature = "lang-html")]
fn expand_html_include_dependents(
    admission: &DeltaAdmission,
    inventory: &BTreeMap<&str, &FileEntry>,
    affected: &BTreeSet<String>,
    load_owners: &mut BTreeSet<String>,
) -> Result<()> {
    let admitted = &admission.admitted;
    let dependencies = &admission.dependencies;
    let modified = &admission.modified;
    let family = languages::LanguageFamily("javascript");
    let (mut frontier, mut visited) = initial_html_frontier(affected, inventory);
    for _ in 0..16 {
        if frontier.is_empty() {
            break;
        }
        let mut targets = BTreeMap::<_, Vec<&str>>::new();
        for path in &frontier {
            targets.entry(stable_hash64(path)).or_default().push(path);
        }
        populate_temp_int_frontier(admitted, "delta_frontier_int", targets.keys().copied())?;
        let mut incoming = admitted.prepare_cached(
            "SELECT DISTINCT d.target_hash, owner.path FROM dependencies d JOIN path_dictionary owner ON owner.path_id=d.owner_id WHERE d.kind='includes' AND d.target_hash IN(SELECT key FROM temp.delta_frontier_int)",
        )?;
        let rows: Vec<(i64, String)> = incoming
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(incoming);
        let mut next = BTreeSet::new();
        for (hash, owner) in rows {
            let Some(owner_file) = inventory.get(owner.as_str()) else {
                continue;
            };
            if owner_file.language != "html"
                || !targets.get(&hash).is_some_and(|paths| {
                    paths
                        .iter()
                        .any(|target| is_scoped_html_pair(&owner, target, dependencies, family))
                })
            {
                continue;
            }
            if visited.insert(owner.clone()) {
                if !modified.contains(&owner) {
                    load_owners.insert(owner.clone());
                }
                next.insert(owner);
            }
        }
        frontier = next;
    }
    Ok(())
}

fn expand_template_and_named_frontiers(
    admission: &DeltaAdmission,
    facts: &BTreeMap<String, TypedFacts>,
    changed_resolution: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
    previous: &BTreeMap<&str, &FileEntry>,
    affected: &mut BTreeSet<String>,
) -> Result<TemplateFrontierOutputs> {
    let admitted = &admission.admitted;
    let dependencies = &admission.dependencies;
    let modified = &admission.modified;
    let template_inputs_changed = modified.iter().any(|path| {
        inventory
            .get(path.as_str())
            .or_else(|| previous.get(path.as_str()))
            .is_some_and(|file| matches!(file.language.as_str(), "html" | "python"))
    });
    let old_loader_roots: Vec<(String, String)> = if template_inputs_changed {
        let mut st = admitted.prepare("SELECT p.path,n.name FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE p.path IN(SELECT key FROM temp.delta_modified_paths) AND n.kind='template_loader_root'")?;
        let rows = st.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    if template_inputs_changed {
        let family = languages::LanguageFamily("python");
        let mut changed_roots = Vec::new();
        for (provider, directory) in &old_loader_roots {
            if modified.contains(provider) {
                changed_roots.push((provider.as_str(), directory.as_str()));
            }
        }
        for (provider, file_facts) in facts {
            if !modified.contains(provider) || !inventory.contains_key(provider.as_str()) {
                continue;
            }
            for node in file_facts
                .nodes
                .iter()
                .filter(|node| node.kind == acdd_index::models::tags::NodeKind::TemplateLoaderRoot)
            {
                changed_roots.push((provider.as_str(), node.name.as_str()));
            }
        }
        for (provider, directory) in changed_roots {
            let Some(scope) = dependencies.nearest_manifest_scope_for_path(family, provider) else {
                continue;
            };
            for (path, file) in inventory {
                if file.language == "html"
                    && path
                        .strip_prefix(directory)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                    && dependencies.nearest_manifest_scope_for_path(family, path) == Some(scope)
                {
                    affected.insert((*path).to_owned());
                }
            }
        }
    }
    #[cfg(feature = "lang-html")]
    if template_inputs_changed {
        expand_html_include_targets(admission, facts, inventory, previous, affected)?;
    }
    affected.extend(semantic_fact_owners(admitted, facts, affected, inventory)?);
    let named_candidates = named_projection_candidates(
        admitted,
        facts,
        affected,
        inventory,
        dependencies,
        changed_resolution,
        Some(modified),
    )?;
    let previously_affected = affected.clone();
    affected.extend(named_candidates.iter().cloned());
    let mut load_owners: BTreeSet<_> = affected
        .difference(modified)
        .filter(|path| !named_candidates.contains(*path) || previously_affected.contains(*path))
        .filter(|path| inventory.contains_key(path.as_str()) && !facts.contains_key(path.as_str()))
        .cloned()
        .collect();
    if template_inputs_changed {
        let mut roots = BTreeSet::new();
        for path in affected.iter().filter(|path| {
            inventory
                .get(path.as_str())
                .is_some_and(|file| file.language == "html")
        }) {
            let mut ancestor = path.rsplit_once('/').map(|(directory, _)| directory);
            while let Some(directory) = ancestor {
                roots.insert(directory.to_owned());
                ancestor = directory.rsplit_once('/').map(|(parent, _)| parent);
            }
        }
        if !roots.is_empty() {
            populate_temp_text_frontier(
                admitted,
                "delta_frontier_text",
                roots.iter().map(String::as_str),
            )?;
            let mut st = admitted.prepare("SELECT DISTINCT p.path FROM nodes n INDEXED BY idx_nodes_name JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.name IN(SELECT key FROM temp.delta_frontier_text) AND n.kind='template_loader_root'")?;
            for row in st.query_map([], |row| row.get::<_, String>(0))? {
                let provider = row?;
                if inventory.contains_key(provider.as_str()) && !modified.contains(&provider) {
                    load_owners.insert(provider);
                }
            }
        }
    }
    #[cfg(feature = "lang-html")]
    if template_inputs_changed {
        expand_html_include_dependents(admission, inventory, affected, &mut load_owners)?;
    }
    Ok(TemplateFrontierOutputs {
        named_candidates,
        load_owners,
    })
}



struct DeltaHydrationAndLink {
    facts: BTreeMap<String, TypedFacts>,
    graph: acdd_index::models::compact_graph::CompactGraph,
    affected: BTreeSet<String>,
    replace_files: BTreeSet<String>,
    loaded_fact_files: usize,
    extract_ms: f64,
    hydrate_ms: f64,
    link_ms: f64,
}

fn hydrate_and_link_delta(
    admission: &DeltaAdmission,
    frontiers: DeltaFrontiers<'_>,
) -> Result<DeltaHydrationAndLink> {
    let DeltaFrontiers {
        frontier_tables,
        mut facts,
        changed_resolution,
        mut affected,
        mut named_candidates,
        mut replace_files,
        component_owners,
        load_owners,
        mut loaded_fact_files,
        extract_ms,
    } = frontiers;

    let root = &admission.root;
    let dependencies = &admission.dependencies;
    let scan = &admission.scan;
    let inventory: BTreeMap<&str, &FileEntry> =
        admission.inventory.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let modified = &admission.modified;
    let admitted = &admission.admitted;
    let hydrating = Instant::now();

    if !load_owners.is_empty() {
        for (path, f) in load_frontier_local_facts(admitted, load_owners.iter().map(String::as_str))? {
            facts.insert(path, f);
            loaded_fact_files += 1;
        }
    }
    {
        let catalog: Vec<_> = scan
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.language.as_str()))
            .collect();
        // Borrow compact route metadata for endpoint provider selection. Full
        // provider facts still enter through the canonical convergence below.
        let route_catalog = {
            let mut routes = admitted.prepare("SELECT n.id,n.name,n.qualname,p.path,n.line,n.end_line,n.is_test,n.language,n.generated,n.details FROM nodes n INDEXED BY idx_nodes_kind JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.kind='route'")?;
            let selected = routes
                .query_map([], |row| {
                    let details: String = row.get(9)?;
                    Ok(Node {
                        native_semantic: None,
                        id: row.get(0)?,
                        kind: acdd_index::models::tags::NodeKind::Route,
                        name: row.get(1)?,
                        qualname: row.get(2)?,
                        path: row.get(3)?,
                        line: row.get(4)?,
                        end_line: row.get(5)?,
                        is_test: row.get(6)?,
                        language: acdd_index::models::tags::LanguageId::parse(&row.get::<_, String>(7)?).ok_or(rusqlite::Error::InvalidQuery)?,
                        generated: row.get(8)?,
                        details: serde_json::from_str(&details).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                9,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    })
                })?
                .filter(|node| {
                    node.as_ref().map_or(true, |node| {
                        !modified.contains(&node.path) && inventory.contains_key(node.path.as_str())
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            selected
        };
        let mut attempted = BTreeSet::new();
        let mut named_frontier: BTreeSet<String> = facts.keys().cloned().collect();
        loop {
            // Cached providers can themselves reference another named definition.
            // Query each newly hydrated frontier through the same compact index.
            named_candidates.extend(named_projection_candidates(
                admitted,
                &facts,
                &named_frontier,
                &inventory,
                dependencies,
                &changed_resolution,
                None,
            )?);
            named_frontier.clear();
            let missing_paths: Vec<String> = linker::required_full_typed_facts(
                &facts,
                &affected,
                &catalog,
                dependencies,
                &named_candidates,
                &route_catalog,
            )
            .into_iter()
            .filter(|path| !facts.contains_key(path) && attempted.insert(path.clone()))
            .collect();
            if !missing_paths.is_empty() {
                populate_temp_text_frontier(
                    admitted,
                    "delta_frontier_text",
                    missing_paths.iter().map(String::as_str),
                )?;
                let mut st = admitted.prepare_cached(
                    "SELECT path,facts_blob FROM local_facts WHERE path IN(SELECT key FROM temp.delta_frontier_text)",
                )?;
                let raw_blobs = st
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(st);
                let decoded: Vec<(String, TypedFacts)> = raw_blobs
                    .into_par_iter()
                    .map(|(path, raw)| Ok((path, decode_facts(&raw)?)))
                    .collect::<Result<_>>()?;
                for (path, typed) in decoded {
                    facts.insert(path.clone(), typed);
                    named_frontier.insert(path);
                    loaded_fact_files += 1;
                }
            }
            if named_frontier.is_empty() {
                break;
            }
        }
    }
    let composed_routes = ast::route_includes::compose_route_includes(&mut facts);
    affected.extend(composed_routes.iter().cloned());
    replace_files.extend(composed_routes);
    assign_components(&mut facts, &component_owners);
    let mut tokens = BTreeSet::new();
    let mut doc_symbols = BTreeSet::new();
    for f in facts.values() {
        tokens.extend(reference_keys(f).into_iter().map(str::to_owned));
        doc_symbols.extend(
            f.docs
                .iter()
                .flat_map(|doc| doc.referenced_symbols.iter())
                .map(|symbol| symbol.replace("::", "."))
                .filter(|symbol| !symbol.is_empty()),
        );
    }
    let mut suffix_ids = BTreeSet::new();
    if !doc_symbols.is_empty() {
        let mut terms = BTreeSet::new();
        let mut fallback_symbols = BTreeSet::new();
        for symbol in &doc_symbols {
            let token = symbol
                .is_ascii()
                .then(|| symbol.rsplit(|c: char| !c.is_ascii_alphanumeric()).next())
                .flatten()
                .filter(|token| !token.is_empty() && token.len() <= 128);
            if let Some(token) = token {
                terms.insert(token.to_owned());
            } else {
                fallback_symbols.insert(symbol.as_str());
            }
        }
        let mut st = admitted.prepare("SELECT n.id,n.qualname FROM node_search s JOIN nodes n ON n.node_hash=s.rowid WHERE node_search MATCH ?1")?;
        for chunk in terms.into_iter().collect::<Vec<_>>().chunks(32) {
            let query = chunk
                .iter()
                .map(|term| format!("\"{term}\""))
                .collect::<Vec<_>>()
                .join(" OR ");
            for row in st.query_map([query], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (id, qualname) = row?;
                if qualname
                    .match_indices('.')
                    .any(|(at, _)| doc_symbols.contains(&qualname[at + 1..]))
                {
                    suffix_ids.insert(id);
                }
            }
        }
        if !fallback_symbols.is_empty() {
            let mut st =
                admitted.prepare("SELECT id,qualname FROM nodes WHERE instr(qualname,'.')>0")?;
            for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (id, qualname) = row?;
                if qualname
                    .match_indices('.')
                    .any(|(at, _)| fallback_symbols.contains(&qualname[at + 1..]))
                {
                    suffix_ids.insert(id);
                }
            }
        }
    }
    populate_temp_text_frontier(
        admitted,
        "delta_frontier_text",
        tokens.iter().map(String::as_str),
    )?;
    populate_temp_text_frontier(
        admitted,
        "delta_frontier_text_2",
        suffix_ids.iter().map(String::as_str),
    )?;
    {
        let sql = "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line,n.end_line,n.is_test,n.language,n.generated,n.details,EXISTS(SELECT 1 FROM shared_owners s JOIN shared_keys k ON k.key_hash=s.key_hash WHERE s.kind_id=2 AND s.key_hash=n.node_hash AND k.key=n.id) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id IN(SELECT id FROM nodes WHERE kind='module' UNION SELECT id FROM nodes WHERE name IN(SELECT key FROM temp.delta_frontier_text) UNION SELECT id FROM nodes WHERE qualname IN(SELECT key FROM temp.delta_frontier_text) UNION SELECT n2.id FROM shared_owners s JOIN shared_keys k ON k.key_hash=s.key_hash JOIN nodes n2 ON n2.node_hash=s.key_hash AND n2.id=k.key WHERE s.kind_id=2 UNION SELECT key FROM temp.delta_frontier_text_2)";
        let mut st = admitted.prepare(sql)?;
        let nodes = st.query_map([], |r| {
            Ok(Node {
                native_semantic: None,
                id: r.get(0)?,
                kind: acdd_index::models::tags::NodeKind::parse(&r.get::<_, String>(1)?).ok_or(rusqlite::Error::InvalidQuery)?,
                name: r.get(2)?,
                qualname: r.get(3)?,
                path: r.get(4)?,
                line: r.get(5)?,
                end_line: r.get(6)?,
                is_test: r.get(7)?,
                language: acdd_index::models::tags::LanguageId::parse(&r.get::<_, String>(8)?).ok_or(rusqlite::Error::InvalidQuery)?,
                generated: r.get(9)?,
                details: serde_json::from_str(&r.get::<_, String>(10)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            })
        })?;
        let complete: BTreeSet<_> = facts.keys().cloned().collect();
        for node in nodes {
            let node = node?;
            if complete.contains(&node.path) || modified.contains(&node.path) {
                continue;
            }
            facts
                .entry(node.path.clone())
                .or_default()
                .push_node(node);
        }
    }
    let hydrate_ms = hydrating.elapsed().as_secs_f64() * 1000.;
    let linking = Instant::now();
    let graph = linker::link_compact_with_typed_registry(
        &facts,
        Some(&affected),
        Some(root),
        dependencies,
    );
    let link_ms = linking.elapsed().as_secs_f64() * 1000.;
    drop(frontier_tables);
    Ok(DeltaHydrationAndLink {
        facts,
        graph,
        affected,
        replace_files,
        loaded_fact_files,
        extract_ms,
        hydrate_ms,
        link_ms,
    })
}



fn persist_and_report_delta(
    admission: DeltaAdmission,
    hydration: DeltaHydrationAndLink,
    db: &Path,
    started: Instant,
    test_hooks: Option<&PipelineTestHooks>,
) -> Result<Value> {
    let DeltaAdmission {
        root,
        adapter,
        dependencies,
        scan,
        inventory,
        previous: _,
        modified,
        scan_ms,
        generation_lock,
        idle_readers_guard: _idle_readers_guard,
        admitted,
        admitted_identity,
        expected,
    } = admission;

    let DeltaHydrationAndLink {
        facts,
        graph,
        affected,
        replace_files,
        loaded_fact_files,
        extract_ms,
        hydrate_ms,
        link_ms,
    } = hydration;


    drop(admitted);

    if let Some(hook) = test_hooks.and_then(|hooks| hooks.before_delta_persist.as_ref()) {
        hook(db);
    }

    // Only a receipt matching the admitted generation may be recovered after
    // SQLite has rolled back and closed its writer.
    let trusted_receipt =
        match cache::matches_with_reason(db, &root, &expected, &admitted_identity) {
            Ok(()) => true,
            Err(reason) => {
                eprintln!(
                    "delta receipt is not trusted for {}: {reason:#}",
                    db.display()
                );
                false
            }
        };

    let persist_result = delta_persist::persist_delta(
        delta_persist::DeltaPersistenceInputs {
            db,
            root: &root,
            expected: &expected,
            admitted_identity: admitted_identity.clone(),
            entries: &scan.entries,
            affected: &affected,
            replace_files: &replace_files,
            facts: &facts,
            graph: &graph,
            manifest_digest: dependencies.digest(),
        },
        |persist_ms, seal_ms, verify_ms, _| {
            let latest = scanner::scan_reusing(&root, &adapter, &scan.entries)?;
            if latest.entries != scan.entries {
                return Err(SourceSnapshotMismatch(format!(
                    "workspace changed during delta; transaction rolled back (scan={scan_ms:.0}ms extract={extract_ms:.0}ms hydrate={hydrate_ms:.0}ms link={link_ms:.0}ms persist={persist_ms:.0}ms seal={seal_ms:.0}ms verify={verify_ms:.0}ms)"
                ))
                .into());
            }
            Ok(())
        },
    );
    let persist_result = match persist_result {
        Ok(result) => result,
        Err(error) => {
            if let Some(hook) = test_hooks.and_then(|hooks| hooks.after_delta_failure.as_ref()) {
                hook(db);
            }
            if trusted_receipt {
                let recovery = (|| -> anyhow::Result<()> {
                    let after = cache::identity(db)?;
                    if after == admitted_identity {
                        return cache::matches_with_reason(db, &root, &expected, &after);
                    }
                    cache::matches_with_reason(
                        db,
                        &root,
                        &expected,
                        &admitted_identity,
                    )?;
                    let conn = db_reader::open_raw_confined_readonly(db)?;
                    let seal: String = conn.query_row(
                        "SELECT value FROM metadata WHERE key='output_root'",
                        [],
                        |row| row.get(0),
                    )?;
                    anyhow::ensure!(seal == expected, "rollback output root changed");
                    db_commitments::verify(&conn)?;
                    drop(conn);
                    if let Some(hook) =
                        test_hooks.and_then(|hooks| hooks.during_delta_verification.as_ref())
                    {
                        hook(db);
                    }
                    let stable = cache::identity(db)?;
                    anyhow::ensure!(
                        after == stable,
                        "database changed during rollback verification ({})",
                        cache::describe_diff(&after, &stable)
                    );
                    if let Some(hook) =
                        test_hooks.and_then(|hooks| hooks.before_delta_receipt_publish.as_ref())
                    {
                        hook(db);
                    }
                    anyhow::ensure!(
                        cache::publish_verified_identity(db, &root, &expected, &stable)?,
                        "rollback receipt publication refused"
                    );
                    Ok(())
                })();
                if let Err(recovery_error) = recovery {
                    if let Err(invalidation_error) = cache::invalidate_checked(db) {
                        eprintln!(
                            "delta receipt invalidation failed for {}: {invalidation_error:#}",
                            db.display()
                        );
                    }
                    eprintln!(
                        "delta receipt recovery failed for {}: {recovery_error:#}",
                        db.display()
                    );
                }
            }
            return Err(error);
        }
    };

    drop(_idle_readers_guard);
    drop(generation_lock);

    let mut report = json!({
        "files": scan.files,
        "changed_files": modified.len(),
        "affected_owners": affected.len(),
        "reparsed_files": modified.iter().filter(|p| inventory.contains_key(p.as_str())).count(),
        "loaded_fact_files": loaded_fact_files,
        "rewritten_files": persist_result.rewritten_files,
        "verification_cache": persist_result.verification_cache,
        "output_root": persist_result.output_root,
        "build_mode": BuildMode::IncrementalDelta.as_str(),
        "scan_ms": scan_ms,
        "extract_ms": extract_ms,
        "hydrate_ms": hydrate_ms,
        "link_ms": link_ms,
        "persist_ms": persist_result.persist_ms,
        "delete_ms": persist_result.delete_ms,
        "files_ms": persist_result.files_ms,
        "graph_ms": persist_result.graph_ms,
        "aggregate_ms": persist_result.aggregate_ms,
        "seal_ms": persist_result.seal_ms,
        "verify_ms": persist_result.verify_ms,
        "commit_ms": persist_result.commit_ms,
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.,
    });
    report["framework_rule_diagnostics"] = json!(dependencies.framework_rule_diagnostics(
        scan.entries
            .iter()
            .map(|file| (file.path.as_str(), file.language.as_str()))
    ));
    report["output"] = json!(db);
    Ok(report)
}


pub(super) fn delta_inner(
    root: &Path,
    db: &Path,
    modified: &[PathBuf],
    selected_adapter: Option<&Path>,
    cache_dir: Option<&Path>,
    test_hooks: Option<&PipelineTestHooks>,
    before_publish: Option<&(dyn Fn() -> Result<()> + Sync)>,
) -> Result<Value> {
    validate_code_map_target(root, db)?;
    let started = Instant::now();

    let admission = match admit_delta_generation(root, db, modified, selected_adapter, cache_dir)? {
        AdmissionResult::RebuildCold(adapter_path) => {
            if let Some(guard) = before_publish {
                guard()?;
            }
            return rebuild_with_cache_dir(
                root,
                db,
                adapter_path.as_deref(),
                cache_dir,
                before_publish,
            );
        }
        AdmissionResult::Admitted(admission) => *admission,
    };

    let frontiers = compute_delta_frontiers(&admission, cache_dir)?;
    let hydration = hydrate_and_link_delta(&admission, frontiers)?;
    if let Some(guard) = before_publish {
        guard()?;
    }
    persist_and_report_delta(admission, hydration, db, started, test_hooks)
}
