//! Cross-worktree spec and archive discovery for roadmap synthesis.

use super::types::{
    checkout_matches_run, ActiveRunMap, CheckoutSpecs, CommittedArchives, DiscoveredCopy,
    DiscoveredWorkspace, SpecInfo,
};
use crate::discovery::{
    collect_archived_specs, collect_workspace_specs_for_roadmap, effective_spec_collections,
    load_ws_adapter, CollectedWorkspaceSpec,
};
use crate::spec::{SpecDocument, SpecStatus};
use crate::types::RoadmapWorktreeCopy;
use acdd_core::config::{load_sibling_checkout_adapter, Adapter, SpecConfig, ACDD_CONFIG_FILE};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::format::sha256_serde_json_hex;
use acdd_core::fs::canonical_root;
use acdd_core::git::{
    git_object_ids_at_paths_batch, git_paths_clean_against_head, OwnedWorktreeInfo,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type ArchiveFingerprint = Vec<Option<String>>;
type ArchiveFingerprintMap = BTreeMap<String, Vec<Option<String>>>;

pub(super) fn load_checkout_adapter_for_roadmap(
    checkout: &Path,
    current_root: &Path,
) -> Result<Adapter, AcddError> {
    let checkout_canon = canonical_root(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let current_canon = canonical_root(current_root).unwrap_or_else(|_| current_root.to_path_buf());
    if checkout_canon != current_canon {
        load_sibling_checkout_adapter(&checkout_canon)
    } else {
        load_ws_adapter(&checkout_canon)
    }
}

fn worktree_label(wt_path: &Path, primary_root: &Path) -> String {
    if wt_path == primary_root {
        return "main".to_owned();
    }
    if let Ok(rel) = wt_path.strip_prefix(primary_root) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    wt_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| wt_path.display().to_string())
}

fn enumerate_worktree_roots(
    current_root: &Path,
    primary_root: &Path,
    current_only: bool,
    owned_worktrees: &[PathBuf],
) -> Vec<(String, PathBuf)> {
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    let cur_canon = canonical_root(current_root).unwrap_or_else(|_| current_root.to_path_buf());
    let pri_canon = canonical_root(primary_root).unwrap_or_else(|_| primary_root.to_path_buf());

    if cur_canon.is_dir() && seen.insert(cur_canon.clone()) {
        roots.push(cur_canon.clone());
    }
    if !current_only {
        for canon in owned_worktrees {
            if seen.insert(canon.clone()) {
                roots.push(canon.clone());
            }
        }
        if pri_canon.is_dir() && seen.insert(pri_canon.clone()) {
            roots.push(pri_canon.clone());
        }
    }

    let provisional: Vec<String> = roots
        .iter()
        .map(|wt| worktree_label(wt, &pri_canon))
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for lbl in &provisional {
        *counts.entry(lbl.clone()).or_default() += 1;
    }
    let mut used_labels = BTreeSet::new();
    roots
        .into_iter()
        .zip(provisional)
        .map(|(wt, lbl)| {
            let mut final_lbl = if counts.get(lbl.as_str()).copied().unwrap_or(0) > 1 {
                if let Ok(rel) = wt.strip_prefix(&pri_canon) {
                    let rel_s = rel.to_string_lossy().replace('\\', "/");
                    if rel_s.is_empty() {
                        lbl
                    } else {
                        rel_s
                    }
                } else {
                    wt.display().to_string()
                }
            } else {
                lbl
            };
            if !used_labels.insert(final_lbl.clone()) {
                let base = final_lbl.clone();
                let mut n = 2usize;
                while !used_labels.insert(format!("{base}#{n}")) {
                    n += 1;
                }
                final_lbl = format!("{base}#{n}");
            }
            (final_lbl, wt)
        })
        .collect()
}

fn copy_matches_active_run(
    spec_id: &str,
    wt_path: &Path,
    active_runs_by_spec: &ActiveRunMap,
    worktree_branches: &BTreeMap<PathBuf, Option<String>>,
) -> bool {
    active_runs_by_spec.get(spec_id).is_some_and(|runs| {
        runs.iter().any(|r| {
            checkout_matches_run(
                r.worktree.as_deref(),
                r.branch.as_deref(),
                wt_path,
                worktree_branches,
            )
        })
    })
}

fn copy_status_label(
    copy: &DiscoveredCopy,
    active_runs_by_spec: &ActiveRunMap,
    worktree_branches: &BTreeMap<PathBuf, Option<String>>,
) -> String {
    if copy.is_archived {
        "archived".to_owned()
    } else if copy_matches_active_run(
        &copy.doc.id,
        &copy.wt_path,
        active_runs_by_spec,
        worktree_branches,
    ) {
        "active".to_owned()
    } else {
        match copy.doc.status {
            SpecStatus::Completed => "completed".to_owned(),
            SpecStatus::Cancelled => "cancelled".to_owned(),
            SpecStatus::Active => "active".to_owned(),
            SpecStatus::Planned => "planned".to_owned(),
        }
    }
}

fn copy_priority_key(
    copy: &DiscoveredCopy,
    current_canon: &Path,
    primary_canon: &Path,
    active_runs_by_spec: &ActiveRunMap,
    worktree_branches: &BTreeMap<PathBuf, Option<String>>,
) -> (u8, u8, u8, u8, std::cmp::Reverse<String>) {
    let matches_active_run = !copy.is_archived
        && copy_matches_active_run(
            &copy.doc.id,
            &copy.wt_path,
            active_runs_by_spec,
            worktree_branches,
        );
    let lifecycle_rank = if copy.is_archived || copy.doc.status == SpecStatus::Completed {
        3
    } else if matches_active_run || copy.doc.status == SpecStatus::Active {
        2
    } else if copy.doc.status == SpecStatus::Cancelled {
        0
    } else {
        1
    };
    (
        u8::from(matches_active_run),
        lifecycle_rank,
        u8::from(copy.wt_path == current_canon),
        u8::from(copy.wt_path == primary_canon),
        std::cmp::Reverse(copy.wt_label.clone()),
    )
}

fn literal_archive_dirs(
    collections: &BTreeMap<String, SpecConfig>,
) -> Option<Vec<String>> {
    let pats: Vec<_> = collections
        .values()
        .flat_map(|col| col.patterns())
        .collect();
    pats.iter()
        .all(|p| !p.contains(['*', '?', '{', '[']))
        .then(|| {
            pats.iter()
                .map(|p| format!("{}/archive", p.trim_end_matches('/')))
                .collect()
        })
}

fn collection_patterns_key(cols: &BTreeMap<String, SpecConfig>) -> Vec<String> {
    cols.iter()
        .flat_map(|(name, col)| {
            col.patterns()
                .into_iter()
                .map(move |pat| format!("{name}:{pat}"))
        })
        .collect()
}

fn preload_archive_fingerprints_and_cleanliness(
    primary_canon: &Path,
    worktree_info: &[OwnedWorktreeInfo],
    worktrees: &[(String, PathBuf)],
    literal_dirs: Option<&Vec<String>>,
) -> (ArchiveFingerprintMap, BTreeMap<PathBuf, bool>) {
    let mut fingerprint_by_head: ArchiveFingerprintMap = BTreeMap::new();
    if let Some(dirs) = literal_dirs {
        let queries: Vec<_> = worktree_info
            .iter()
            .filter_map(|info| info.head.as_deref())
            .flat_map(|head| dirs.iter().map(move |path| (head, path.as_str())))
            .collect();
        if let Ok(ids) = git_object_ids_at_paths_batch(primary_canon, &queries) {
            for (info, chunk) in worktree_info
                .iter()
                .filter(|info| info.head.is_some())
                .zip(ids.chunks(dirs.len().max(1)))
            {
                if let Some(ref h) = info.head {
                    fingerprint_by_head.insert(h.clone(), chunk.to_vec());
                }
            }
        }
    }
    let mut preloaded_clean: BTreeMap<PathBuf, bool> = BTreeMap::new();
    if let Some(dirs) = literal_dirs.filter(|_| worktrees.len() > 1) {
        let archive_dir_refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        let sibling_wts: Vec<PathBuf> = worktrees
            .iter()
            .filter(|(_, wt)| {
                *wt != *primary_canon
                    && worktree_info
                        .iter()
                        .any(|info| &info.path == wt && info.head.is_some())
            })
            .map(|(_, wt)| wt.clone())
            .collect();
        let mut clean_results: Vec<Option<(PathBuf, bool)>> = vec![None; sibling_wts.len()];
        std::thread::scope(|s| {
            for (wt, slot) in sibling_wts.iter().zip(clean_results.iter_mut()) {
                let refs = &archive_dir_refs;
                s.spawn(move || {
                    *slot = Some((
                        wt.clone(),
                        git_paths_clean_against_head(wt, refs).unwrap_or(false),
                    ));
                });
            }
        });
        preloaded_clean.extend(clean_results.into_iter().flatten());
    }
    (fingerprint_by_head, preloaded_clean)
}

fn resolve_spec_copies_and_divergence(
    copies_by_id: BTreeMap<String, Vec<DiscoveredCopy>>,
    current_root: &Path,
    primary_canon: &Path,
    active_runs_by_spec: &ActiveRunMap,
    worktree_branches: &BTreeMap<PathBuf, Option<String>>,
    divergence_warnings: &mut Vec<(String, String)>,
) -> (
    BTreeMap<String, SpecInfo>,
    BTreeMap<String, SpecInfo>,
    BTreeMap<acdd_core::ids::RunId, SpecInfo>,
) {
    let mut specs_by_id = BTreeMap::new();
    let mut archived_by_id = BTreeMap::new();
    let mut specs_by_run = BTreeMap::new();
    for (spec_id, mut copies) in copies_by_id {
        if copies.is_empty() {
            continue;
        }
        let present_in_current = copies.iter().any(|c| c.wt_path == current_root);
        let first_digest = copies[0].doc.digest.clone();
        let first_archived = copies[0].is_archived;
        let all_identical = copies
            .iter()
            .all(|c| c.doc.digest == first_digest && c.is_archived == first_archived);
        let best_idx = copies
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| {
                copy_priority_key(
                    c,
                    current_root,
                    primary_canon,
                    active_runs_by_spec,
                    worktree_branches,
                )
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let divergence = if all_identical {
            Vec::new()
        } else {
            let canonical_wt = &copies[best_idx].wt_path;
            let mut seen_wt = BTreeSet::new();
            let mut parts = Vec::new();
            let mut div = Vec::new();
            for c in &copies {
                if !seen_wt.insert(c.wt_path.clone()) {
                    continue;
                }
                let st = copy_status_label(c, active_runs_by_spec, worktree_branches);
                parts.push(format!("{}: {st}", c.wt_label));
                div.push(RoadmapWorktreeCopy {
                    worktree: c.wt_label.clone(),
                    status: st,
                    canonical: &c.wt_path == canonical_wt,
                });
            }
            if div.len() > 1 {
                divergence_warnings.push((
                    spec_id.clone(),
                    format!(
                        "spec '{spec_id}' diverges across worktrees: {}",
                        parts.join(", ")
                    ),
                ));
                div
            } else {
                Vec::new()
            }
        };
        let to_info = |chosen: DiscoveredCopy, source_worktree| SpecInfo {
            spec_id: spec_id.clone(),
            effective_spec: chosen.effective_spec,
            roadmap_visible: chosen.roadmap_visible,
            path: chosen.rel_path,
            checkout: chosen.wt_path,
            effective_globs: effective_spec_globs(&chosen.doc),
            doc: chosen.doc,
            is_archived: chosen.is_archived,
            source_worktree,
            divergence: divergence.clone(),
        };
        if let Some(runs) = active_runs_by_spec.get(&spec_id) {
            for run in runs {
                if let Some(copy) = copies
                    .iter()
                    .filter(|copy| {
                        !copy.is_archived
                            && checkout_matches_run(
                                run.worktree.as_deref(),
                                run.branch.as_deref(),
                                &copy.wt_path,
                                worktree_branches,
                            )
                    })
                    .max_by_key(|copy| run.spec_path.as_deref() == Some(copy.rel_path.as_str()))
                {
                    let source = (copy.wt_path != current_root).then(|| copy.wt_label.clone());
                    specs_by_run.insert(run.run_id.clone(), to_info(copy.clone(), source));
                }
            }
        }
        let chosen = copies.swap_remove(best_idx);
        let source = (!present_in_current).then(|| chosen.wt_label.clone());
        let info = to_info(chosen, source);
        if info.is_archived {
            archived_by_id.insert(spec_id, info);
        } else {
            specs_by_id.insert(spec_id, info);
        }
    }
    (specs_by_id, archived_by_id, specs_by_run)
}

pub(super) fn discover_specs_across_worktrees(
    current_root: &Path,
    primary_canon: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    worktree_info: &[OwnedWorktreeInfo],
    owned_worktrees: Vec<PathBuf>,
    active_runs_by_spec: &ActiveRunMap,
    current_only: bool,
) -> Result<DiscoveredWorkspace, AcddError> {
    let worktree_branches: BTreeMap<PathBuf, Option<String>> = worktree_info
        .iter()
        .map(|info| (info.path.clone(), info.branch.clone()))
        .collect();
    let literal_dirs = literal_archive_dirs(collections);
    let worktrees =
        enumerate_worktree_roots(current_root, primary_canon, current_only, &owned_worktrees);
    let effective_patterns = collection_patterns_key(collections);
    let (fingerprint_by_head, preloaded_clean) = preload_archive_fingerprints_and_cleanliness(
        primary_canon,
        worktree_info,
        &worktrees,
        literal_dirs.as_ref(),
    );
    let mut copies_by_id: BTreeMap<String, Vec<DiscoveredCopy>> = BTreeMap::new();
    let mut checkout_specs = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut spec_cache = BTreeMap::new();
    let mut checkout_archives = BTreeMap::new();
    let mut committed_cache: BTreeMap<(ArchiveFingerprint, String), CommittedArchives> =
        BTreeMap::new();

    let mut collect_archives =
        |wt_path: &Path,
         collections: &BTreeMap<String, SpecConfig>|
         -> Result<(CommittedArchives, Vec<String>, CommittedArchives), AcddError> {
            let disk_only = wt_path != primary_canon;
            let head_entry = worktree_info.iter().find(|info| info.path == wt_path);
            let unborn_git_head = head_entry.is_some_and(|info| info.head.is_none());
            let head = head_entry.and_then(|info| info.head.as_ref());
            let wt_patterns = collection_patterns_key(collections);
            let same_patterns = wt_patterns == effective_patterns;
            let schema_digest =
                sha256_serde_json_hex(&effective_spec_collections(collections)).map_err(|err| {
                    AcddError::new(ErrorCode::WorkflowSchemaInvalid, err.to_string())
                })?;
            let disk_matches_head = disk_only
                && head.is_some()
                && preloaded_clean
                    .get(wt_path)
                    .copied()
                    .filter(|_| same_patterns)
                    .unwrap_or_else(|| {
                        literal_archive_dirs(collections).is_some_and(|dirs| {
                            let refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
                            git_paths_clean_against_head(wt_path, &refs).unwrap_or(false)
                        })
                    });
            let head_key = head.map(|h| {
                if same_patterns {
                    if let Some(fp) = fingerprint_by_head.get(h) {
                        return fp.clone();
                    }
                }
                vec![Some(h.to_owned())]
            });
            let cached_committed = head_key.as_ref().and_then(|k| {
                committed_cache
                    .get(&(k.clone(), schema_digest.clone()))
                    .cloned()
            });
            if disk_matches_head {
                if let Some(archived) = cached_committed {
                    return Ok((archived.clone(), Vec::new(), archived));
                }
                let (archived, warn) =
                    collect_archived_specs(wt_path, collections, false, head.map(String::as_str))?;
                if let Some(k) = head_key {
                    committed_cache.insert((k, schema_digest), archived.clone());
                }
                return Ok((archived.clone(), warn, archived));
            }
            let (arch, arch_warnings) = if unborn_git_head && !disk_only {
                (Vec::new(), Vec::new())
            } else {
                collect_archived_specs(wt_path, collections, disk_only, head.map(String::as_str))?
            };
            let committed = if unborn_git_head {
                Vec::new()
            } else if let Some(archived) = cached_committed {
                archived
            } else {
                let archived = if disk_only {
                    collect_archived_specs(wt_path, collections, false, head.map(String::as_str))?.0
                } else {
                    arch.clone()
                };
                if let Some(k) = head_key {
                    committed_cache.insert((k, schema_digest), archived.clone());
                }
                archived
            };
            Ok((arch, arch_warnings, committed))
        };
    for (wt_label, wt_path) in &worktrees {
        let worktree_adapter = if wt_path != current_root
            && wt_path.join(ACDD_CONFIG_FILE).try_exists().map_err(|err| {
                AcddError::new(ErrorCode::WorkflowSchemaInvalid, err.to_string())
            })? {
            Some(load_checkout_adapter_for_roadmap(wt_path, current_root)?)
        } else {
            None
        };
        let collections = worktree_adapter
            .as_ref()
            .map_or(collections, |adapter| &adapter.specs);
        let specs = scan_checkout_specs(
            wt_path,
            wt_label,
            collections,
            &mut warnings,
            &mut spec_cache,
        )?;
        if wt_path == primary_canon || owned_worktrees.contains(wt_path) {
            checkout_specs.insert(wt_path.clone(), checkout_specs_from(collections, &specs));
        }
        let is_primary_or_current = wt_path == primary_canon || wt_path == current_root;
        for item in specs {
            let is_active = copy_matches_active_run(
                &item.doc.id,
                wt_path,
                active_runs_by_spec,
                &worktree_branches,
            ) || item.doc.status == SpecStatus::Active;
            if !is_primary_or_current && !is_active {
                continue;
            }
            let collection_name = resolve_collection_name(collections, &item.rel_path);
            let (effective_spec, roadmap_visible) =
                resolve_effective_spec_meta(&item.doc, &collection_name, collections);
            copies_by_id
                .entry(item.doc.id.clone())
                .or_default()
                .push(DiscoveredCopy {
                    wt_label: wt_label.clone(),
                    wt_path: wt_path.clone(),
                    effective_spec,
                    roadmap_visible,
                    rel_path: item.rel_path,
                    doc: item.doc,
                    is_archived: false,
                });
        }

        let (arch, arch_warnings, committed) = collect_archives(wt_path, collections)?;
        let mut checkout_archive = BTreeMap::new();
        for (_, path, doc) in committed {
            checkout_archive
                .entry(doc.id.clone())
                .or_insert((path, doc));
        }
        checkout_archives.insert(wt_path.clone(), checkout_archive);
        for w in arch_warnings {
            warnings.push(format!("{wt_label}: {w}"));
        }
        for (col_name, rel_path, doc) in arch {
            let (effective_spec, roadmap_visible) =
                resolve_effective_spec_meta(&doc, &col_name, collections);
            copies_by_id
                .entry(doc.id.clone())
                .or_default()
                .push(DiscoveredCopy {
                    wt_label: wt_label.clone(),
                    wt_path: wt_path.clone(),
                    effective_spec,
                    roadmap_visible,
                    rel_path,
                    doc,
                    is_archived: true,
                });
        }
    }

    let mut divergence_warnings = Vec::new();
    let (specs_by_id, archived_by_id, specs_by_run) = resolve_spec_copies_and_divergence(
        copies_by_id,
        current_root,
        primary_canon,
        active_runs_by_spec,
        &worktree_branches,
        &mut divergence_warnings,
    );
    let mut sibling_candidates: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (checkout, scoped) in &checkout_specs {
        let archives = checkout_archives
            .get(checkout)
            .into_iter()
            .flat_map(|docs| docs.keys());
        for id in scoped.active_docs.iter().map(|d| &d.id).chain(archives) {
            let candidates = sibling_candidates.entry(id.clone()).or_default();
            if candidates.len() < 2 && candidates.last() != Some(checkout) {
                candidates.push(checkout.clone());
            }
        }
    }
    Ok(DiscoveredWorkspace {
        specs_by_id,
        archived_by_id,
        checkout_archives,
        specs_by_run,
        owned_worktrees,
        worktree_branches,
        checkout_specs,
        sibling_candidates,
        warnings,
        divergence_warnings,
    })
}

pub(super) fn scan_checkout_specs(
    checkout: &Path,
    label: &str,
    collections: &BTreeMap<String, SpecConfig>,
    warnings: &mut Vec<String>,
    parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, AcddError>>,
) -> Result<Vec<CollectedWorkspaceSpec>, AcddError> {
    let scan = collect_workspace_specs_for_roadmap(checkout, collections, parsed);
    for err in scan.errors {
        if err.message.contains("confinement") || err.message.contains("symlink") {
            return Err(AcddError::new(err.code, err.message));
        }
        let missing_optional_dir =
            err.diag_code == "VALIDATION_IO" && !checkout.join(&err.path).exists();
        if !missing_optional_dir {
            warnings.push(format!("{label}: {}: {}", err.path, err.message));
        }
    }
    Ok(scan.specs)
}

pub(super) fn checkout_specs_from(
    collections: &BTreeMap<String, SpecConfig>,
    specs: &[CollectedWorkspaceSpec],
) -> CheckoutSpecs {
    let mut active_docs = Vec::with_capacity(specs.len());
    let mut files = Vec::with_capacity(specs.len());
    for item in specs {
        files.push((
            resolve_collection_name(collections, &item.rel_path),
            item.rel_path.clone(),
        ));
        active_docs.push(item.doc.clone());
    }
    CheckoutSpecs {
        collections: collections.clone(),
        active_docs,
        files,
    }
}

pub(super) fn checkout_spec_id(
    discovered: &DiscoveredWorkspace,
    checkout: &Path,
    selector: &str,
) -> Option<String> {
    discovered
        .checkout_specs
        .get(checkout)?
        .active_docs
        .iter()
        .find(|doc| doc.id == selector)
        .map(|doc| doc.id.clone())
}

pub(super) fn materialize_checkout_spec(
    discovered: &DiscoveredWorkspace,
    checkout: &Path,
    spec_id: &str,
) -> Option<SpecInfo> {
    let scoped = discovered.checkout_specs.get(checkout)?;
    let (idx, doc) = scoped
        .active_docs
        .iter()
        .enumerate()
        .find(|(_, doc)| doc.id == spec_id)?;
    let (collection, path) = scoped.files.get(idx)?.clone();
    let (effective_spec, roadmap_visible) =
        resolve_effective_spec_meta(doc, &collection, &scoped.collections);
    Some(SpecInfo {
        spec_id: doc.id.clone(),
        effective_spec,
        roadmap_visible,
        path,
        effective_globs: effective_spec_globs(doc),
        doc: doc.clone(),
        checkout: checkout.to_path_buf(),
        is_archived: false,
        source_worktree: checkout
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        divergence: Vec::new(),
    })
}

fn resolve_effective_spec_meta(
    doc: &SpecDocument,
    collection: &str,
    collections: &BTreeMap<String, SpecConfig>,
) -> (String, bool) {
    let owning_col = collections.get(collection).map(|cfg| (collection, cfg));
    match crate::schema::resolve_spec_schema(doc.spec.as_deref(), owning_col, collections) {
        Ok(schema) => {
            let name = if owning_col.is_none() && doc.spec.is_none() {
                collection.to_owned()
            } else {
                schema.name
            };
            (name, schema.roadmap)
        }
        Err(_) => (collection.to_owned(), true),
    }
}

fn resolve_collection_name(
    collections: &BTreeMap<String, SpecConfig>,
    rel_path: &str,
) -> String {
    crate::discovery::match_spec_collection(collections, rel_path)
        .unwrap_or_else(|| "specs".to_owned())
}

pub(super) fn effective_spec_globs(doc: &SpecDocument) -> Vec<String> {
    let mut globs = BTreeSet::new();
    for task in &doc.tasks {
        for g in &task.scope {
            globs.insert(g.clone());
        }
        for ext in &task.extended_scope {
            globs.insert(ext.path.clone());
        }
    }
    if globs.is_empty() {
        for owner in &doc.owners {
            globs.insert(owner.clone());
        }
    }
    globs.into_iter().collect()
}
