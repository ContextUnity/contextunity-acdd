//! Read-only roadmap builder: cross-collection & cross-worktree spec DAG,
//! topological waves, critical-path unblocks priority, scope-overlap detection,
//! and task drill-down.

use crate::machine::schedule::is_dep_satisfied_against_statuses;
use crate::spec::{SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{
    GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, RunStatus,
    RunTombstoneRecord, ValidatedObservations,
};
use crate::usecases::deps::{
    checkout_scoped_target_selector, collect_archived_specs,
    collect_external_satisfied_deps_cached, collect_external_satisfied_deps_with_owned,
    collect_workspace_specs_cached,
    effective_active_run_config, load_dep_snapshot, load_task_status_map, load_tombstone_archives,
    load_ws_adapter, parse_dep_syntax, resolve_dep_target, resolve_linked_workspace_root,
    resolve_spec_repositories, resolve_ws_context_canon, run_in_checkout, run_matches_selector,
    CollectedWorkspaceSpec, DepSnapshot, DepTargetResolution, ParsedDepSyntax,
    PreloadedDepContext, TombstoneArchiveError,
};
use crate::usecases::dto::{
    RoadmapActiveGateEntry, RoadmapDepEntry, RoadmapReq, RoadmapResp, RoadmapSpecEntry,
    RoadmapTaskEntry, RoadmapWave, RoadmapWorktreeCopy,
};
use crate::usecases::lifecycle::{
    effective_spec_collections, list_all_runs, list_all_tombstones, primary_workspace_root,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::canonical_root;
use acdd_core::git::{
    git_object_ids_at_paths_batch, git_paths_clean_against_head, list_owned_worktrees_with_heads,
};
use acdd_core::ids::{RunId, TaskRef};
use acdd_core::path_policy::scopes_overlap;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

type ActiveRunMap = BTreeMap<String, Vec<(RunRecord, Option<RunGraphSnapshot>)>>;
type TaskDepsMap = BTreeMap<(String, String), Vec<(Option<ParsedDepSyntax>, RoadmapDepEntry)>>;
type SpecWavesOutput = (Vec<(usize, Vec<String>)>, Vec<Vec<String>>);
type CommittedArchives = Vec<(String, String, SpecDocument)>;

struct SpecInfo {
    spec_id: String,
    collection: String,
    path: String,
    doc: SpecDocument,
    checkout: PathBuf,
    effective_globs: Vec<String>,
    is_archived: bool,
    source_worktree: Option<String>,
    divergence: Vec<RoadmapWorktreeCopy>,
}

#[derive(Clone)]
struct DiscoveredCopy {
    wt_label: String,
    wt_path: PathBuf,
    collection: String,
    rel_path: String,
    doc: SpecDocument,
    is_archived: bool,
}

struct CheckoutSpecs {
    collections: BTreeMap<String, SpecCollectionConfig>,
    active_docs: Vec<SpecDocument>,
    /// `(collection, rel_path)` aligned with `active_docs`.
    files: Vec<(String, String)>,
}

#[derive(Default)]
struct LinkedWsCache {
    workspaces: BTreeMap<PathBuf, (CheckoutSpecs, Option<SqliteStateReader>)>,
    outcomes: BTreeMap<(PathBuf, String, Option<String>), (&'static str, bool)>,
    aliases: BTreeMap<(PathBuf, String, PathBuf, String), Option<PathBuf>>,
    runs: Vec<RunRecord>,
    task_statuses: BTreeMap<(RunId, String), bool>,
    tombstones: Vec<RunTombstoneRecord>,
    tombstone_archives: BTreeMap<RunId, Result<SpecDocument, TombstoneArchiveError>>,
    linked_deps: BTreeMap<PathBuf, DepSnapshot>,
}

struct DiscoveredWorkspace {
    specs_by_id: BTreeMap<String, SpecInfo>,
    archived_by_id: BTreeMap<String, SpecInfo>,
    checkout_archives: BTreeMap<PathBuf, BTreeMap<String, (String, SpecDocument)>>,
    specs_by_run: BTreeMap<RunId, SpecInfo>,
    owned_worktrees: Vec<PathBuf>,
    checkout_specs: BTreeMap<PathBuf, CheckoutSpecs>,
    sibling_candidates: BTreeMap<String, Vec<PathBuf>>,
    warnings: Vec<String>,
}

struct DependencyGraphData {
    spec_deps_resolved: BTreeMap<String, Vec<RoadmapDepEntry>>,
    active_spec_deps_resolved: BTreeMap<RunId, Vec<RoadmapDepEntry>>,
    task_deps_resolved: TaskDepsMap,
    forward_spec_edges: BTreeMap<String, BTreeSet<String>>,
    unsatisfied_blockers: BTreeMap<String, BTreeSet<String>>,
    has_unsatisfied_external_blocker: BTreeMap<String, bool>,
    spec_blocks: BTreeMap<String, BTreeSet<String>>,
    active_spec_blocks: BTreeMap<RunId, BTreeSet<String>>,
    task_blocks: BTreeMap<(String, String), BTreeSet<String>>,
    active_task_blocks: BTreeMap<(RunId, TaskRef), BTreeSet<String>>,
    /// Unstarted checkout copies of a spec that is active only in another checkout.
    scoped_planned: BTreeSet<(PathBuf, String)>,
    scoped_spec_blocks: BTreeMap<(PathBuf, String), BTreeSet<String>>,
    scoped_task_blocks: BTreeMap<(PathBuf, String, String), BTreeSet<String>>,
    active_ext_deps: BTreeMap<RunId, Vec<(String, RunId, TaskRef)>>,
}

/// Execute `StateQuery::Roadmap` strictly over `dyn StateReader` without creating `state.sqlite`.
pub(crate) fn handle_roadmap(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: RoadmapReq,
) -> Result<RoadmapResp, StateError> {
    let root = primary_workspace_root(config);
    let current_root = canonical_root(&config.worktree).unwrap_or_else(|_| config.worktree.clone());
    let primary_canon = canonical_root(&root).unwrap_or_else(|_| root.clone());
    let collections = effective_spec_collections(config);

    let active_runs_by_spec = load_active_runs(reader)?;
    let mut discovered = discover_specs_across_worktrees(
        &current_root,
        &primary_canon,
        &collections,
        &active_runs_by_spec,
        req.current_only,
    )?;

    let mut graph_warnings = Vec::new();
    let graph = build_dependency_graph(
        &discovered,
        &primary_canon,
        &collections,
        config,
        reader,
        &active_runs_by_spec,
        req.archived
            || req
                .spec_id
                .as_ref()
                .is_some_and(|id| discovered.archived_by_id.contains_key(id)),
        &mut graph_warnings,
    )?;
    discovered.warnings.extend(graph_warnings);

    let unblocks_counts =
        compute_unblocks_counts(&discovered.specs_by_id, &graph.forward_spec_edges);
    assemble_roadmap_output(
        config,
        &discovered,
        &graph,
        &active_runs_by_spec,
        &unblocks_counts,
        &req,
    )
}

fn load_active_runs(reader: &dyn StateReader) -> Result<ActiveRunMap, StateError> {
    let active_runs = list_all_runs(reader, Some(RunStatus::Active))?;
    let mut active_runs_by_spec: ActiveRunMap = BTreeMap::new();
    for run in active_runs {
        let graph = reader.load_run_graph(&run.run_id)?;
        if let Some(ref spec_id) = run.spec_id {
            active_runs_by_spec
                .entry(spec_id.clone())
                .or_default()
                .push((run, graph));
        }
    }
    for runs in active_runs_by_spec.values_mut() {
        runs.sort_by(|a, b| a.0.run_id.as_str().cmp(b.0.run_id.as_str()));
    }
    Ok(active_runs_by_spec)
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
) -> bool {
    active_runs_by_spec.get(spec_id).is_some_and(|runs| {
        runs.iter().any(|(r, _)| {
            r.worktree
                .as_deref()
                .is_some_and(|w| canonical_root(Path::new(w)).ok().as_deref() == Some(wt_path))
        })
    })
}

fn copy_status_label(copy: &DiscoveredCopy, active_runs_by_spec: &ActiveRunMap) -> String {
    if copy.is_archived {
        "archived".to_owned()
    } else if copy_matches_active_run(&copy.doc.id, &copy.wt_path, active_runs_by_spec) {
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
) -> (u8, u8, u8, u8, std::cmp::Reverse<String>) {
    let matches_active_run = !copy.is_archived
        && copy_matches_active_run(&copy.doc.id, &copy.wt_path, active_runs_by_spec);
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

type ArchiveFingerprint = Vec<Option<String>>;

fn literal_archive_dirs(collections: &BTreeMap<String, SpecCollectionConfig>) -> Option<Vec<String>> {
    let pats: Vec<_> = collections.values().flat_map(|col| col.patterns()).collect();
    pats.iter()
        .all(|p| !p.contains(['*', '?', '{', '[']))
        .then(|| {
            pats.iter()
                .map(|p| format!("{}/archive", p.trim_end_matches('/')))
                .collect()
        })
}

fn discover_specs_across_worktrees(
    current_root: &Path,
    primary_canon: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    active_runs_by_spec: &ActiveRunMap,
    current_only: bool,
) -> Result<DiscoveredWorkspace, StateError> {
    let worktree_heads = list_owned_worktrees_with_heads(primary_canon).unwrap_or_default();
    let owned_worktrees: Vec<_> = worktree_heads.iter().map(|(path, _)| path.clone()).collect();
    let literal_dirs = literal_archive_dirs(collections);
    let mut fingerprint_by_head: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
    if let Some(dirs) = &literal_dirs {
        let queries: Vec<_> = worktree_heads
            .iter()
            .filter_map(|(_, head)| head.as_deref())
            .flat_map(|head| dirs.iter().map(move |path| (head, path.as_str())))
            .collect();
        if let Ok(ids) = git_object_ids_at_paths_batch(primary_canon, &queries) {
            for ((_, head), chunk) in worktree_heads
                .iter()
                .filter(|(_, h)| h.is_some())
                .zip(ids.chunks(dirs.len().max(1)))
            {
                if let Some(h) = head {
                    fingerprint_by_head.insert(h.clone(), chunk.to_vec());
                }
            }
        }
    }
    let worktrees =
        enumerate_worktree_roots(current_root, primary_canon, current_only, &owned_worktrees);
    let collection_patterns = |cols: &BTreeMap<String, SpecCollectionConfig>| -> Vec<String> {
        cols.iter()
            .flat_map(|(name, col)| {
                col.patterns()
                    .into_iter()
                    .map(move |pat| format!("{name}:{pat}"))
            })
            .collect()
    };
    let effective_patterns = collection_patterns(collections);
    let mut preloaded_clean: BTreeMap<PathBuf, bool> = BTreeMap::new();
    let archive_key = |head: &str, same_pats: bool| -> ArchiveFingerprint {
        if same_pats {
            if let Some(fp) = fingerprint_by_head.get(head) {
                return fp.clone();
            }
        }
        vec![Some(head.to_owned())]
    };
    if let Some(dirs) = literal_dirs.as_ref().filter(|_| worktrees.len() > 1) {
        let archive_dir_refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        let sibling_wts: Vec<PathBuf> = worktrees
            .iter()
            .filter(|(_, wt)| {
                *wt != *primary_canon
                    && worktree_heads.iter().any(|(p, h)| p == wt && h.is_some())
            })
            .map(|(_, wt)| wt.clone())
            .collect();
        let mut clean_results: Vec<Option<(PathBuf, bool)>> = vec![None; sibling_wts.len()];
        std::thread::scope(|s| {
            for (wt, slot) in sibling_wts.iter().zip(clean_results.iter_mut()) {
                let refs = &archive_dir_refs;
                s.spawn(move || {
                    *slot = Some((wt.clone(), git_paths_clean_against_head(wt, refs).unwrap_or(false)));
                });
            }
        });
        preloaded_clean.extend(clean_results.into_iter().flatten());
    }
    let mut copies_by_id: BTreeMap<String, Vec<DiscoveredCopy>> = BTreeMap::new();
    let mut checkout_specs = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut spec_cache = BTreeMap::new();
    let mut checkout_archives = BTreeMap::new();
    let mut committed_cache: BTreeMap<(ArchiveFingerprint, Vec<String>), CommittedArchives> =
        BTreeMap::new();

    for (wt_label, wt_path) in &worktrees {
        let worktree_adapter = (wt_path != current_root
            && wt_path.join("acdd.yaml").try_exists().map_err(|err| {
                StateError::new(ErrorCode::ProfileSchemaInvalid, err.to_string())
            })?)
        .then(|| load_ws_adapter(wt_path))
        .transpose()?;
        let collections = worktree_adapter
            .as_ref()
            .map_or(collections, |adapter| &adapter.specs);
        let specs = scan_checkout_specs(wt_path, wt_label, collections, &mut warnings, &mut spec_cache)?;
        if wt_path == primary_canon || owned_worktrees.contains(wt_path) {
            checkout_specs.insert(wt_path.clone(), checkout_specs_from(collections, &specs));
        }
        let is_primary_or_current = wt_path == primary_canon || wt_path == current_root;
        for item in specs {
            let is_active = copy_matches_active_run(&item.doc.id, wt_path, active_runs_by_spec)
                || item.doc.status == SpecStatus::Active;
            if !is_primary_or_current && !is_active {
                continue;
            }
            let collection_name = resolve_collection_name(collections, &item.rel_path);
            copies_by_id
                .entry(item.doc.id.clone())
                .or_default()
                .push(DiscoveredCopy {
                    wt_label: wt_label.clone(),
                    wt_path: wt_path.clone(),
                    collection: collection_name,
                    rel_path: item.rel_path,
                    doc: item.doc,
                    is_archived: false,
                });
        }

        let disk_only = *wt_path != *primary_canon;
        let head_entry = worktree_heads.iter().find(|(path, _)| path == wt_path);
        let unborn_git_head = head_entry.is_some_and(|(_, head)| head.is_none());
        let head = head_entry.and_then(|(_, head)| head.as_ref());
        let wt_patterns = collection_patterns(collections);
        let same_patterns = wt_patterns == effective_patterns;
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
        let head_key = head.map(|h| archive_key(h, same_patterns));
        let cached_committed = head_key
            .as_ref()
            .and_then(|k| committed_cache.get(&(k.clone(), wt_patterns.clone())).cloned());
        let (arch, arch_warnings, committed) = if disk_matches_head {
            if let Some(archived) = cached_committed {
                (archived.clone(), Vec::new(), archived)
            } else {
                let (archived, warn) =
                    collect_archived_specs(wt_path, collections, false, head.map(String::as_str))?;
                if let Some(k) = head_key {
                    committed_cache.insert((k, wt_patterns), archived.clone());
                }
                (archived.clone(), warn, archived)
            }
        } else {
            let (arch, arch_warnings) = if unborn_git_head && !disk_only {
                (Vec::new(), Vec::new())
            } else {
                collect_archived_specs(
                    wt_path,
                    collections,
                    disk_only,
                    head.map(String::as_str),
                )?
            };
            let committed = if unborn_git_head {
                Vec::new()
            } else if let Some(archived) = cached_committed {
                archived
            } else {
                let archived = if disk_only {
                    collect_archived_specs(
                        wt_path,
                        collections,
                        false,
                        head.map(String::as_str),
                    )?
                    .0
                } else {
                    arch.clone()
                };
                if let Some(k) = head_key {
                    committed_cache.insert((k, wt_patterns), archived.clone());
                }
                archived
            };
            (arch, arch_warnings, committed)
        };
        let mut checkout_archive = BTreeMap::new();
        for (_, path, doc) in committed {
            checkout_archive.entry(doc.id.clone()).or_insert((path, doc));
        }
        checkout_archives.insert(wt_path.clone(), checkout_archive);
        for w in arch_warnings {
            warnings.push(format!("{wt_label}: {w}"));
        }
        for (col_name, rel_path, doc) in arch {
            copies_by_id
                .entry(doc.id.clone())
                .or_default()
                .push(DiscoveredCopy {
                    wt_label: wt_label.clone(),
                    wt_path: wt_path.clone(),
                    collection: col_name,
                    rel_path,
                    doc,
                    is_archived: true,
                });
        }
    }

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
                copy_priority_key(c, current_root, primary_canon, active_runs_by_spec)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);

        let mut divergence = Vec::new();
        if !all_identical {
            let canonical_wt = &copies[best_idx].wt_path;
            let mut seen_wt = BTreeSet::new();
            let mut parts = Vec::new();
            for c in &copies {
                if !seen_wt.insert(c.wt_path.clone()) {
                    continue;
                }
                let st = copy_status_label(c, active_runs_by_spec);
                parts.push(format!("{}: {st}", c.wt_label));
                divergence.push(RoadmapWorktreeCopy {
                    worktree: c.wt_label.clone(),
                    status: st,
                    canonical: &c.wt_path == canonical_wt,
                });
            }
            if divergence.len() > 1 {
                warnings.push(format!(
                    "spec '{spec_id}' diverges across worktrees: {}",
                    parts.join(", ")
                ));
            } else {
                divergence.clear();
            }
        }

        let to_info = |chosen: DiscoveredCopy, source_worktree| SpecInfo {
            spec_id: spec_id.clone(),
            collection: chosen.collection,
            path: chosen.rel_path,
            checkout: chosen.wt_path,
            effective_globs: effective_spec_globs(&chosen.doc),
            doc: chosen.doc,
            is_archived: chosen.is_archived,
            source_worktree,
            divergence: divergence.clone(),
        };
        if let Some(runs) = active_runs_by_spec.get(&spec_id) {
            for (run, _) in runs {
                let checkout = run
                    .worktree
                    .as_deref()
                    .and_then(|path| canonical_root(Path::new(path)).ok());
                if let Some(copy) = copies
                    .iter()
                    .filter(|copy| !copy.is_archived && checkout.as_ref() == Some(&copy.wt_path))
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

    let mut sibling_candidates: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (checkout, scoped) in &checkout_specs {
        let archives = checkout_archives.get(checkout).into_iter().flat_map(|docs| docs.keys());
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
        checkout_specs,
        sibling_candidates,
        warnings,
    })
}

fn scan_checkout_specs(
    checkout: &Path,
    label: &str,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    warnings: &mut Vec<String>,
    parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, StateError>>,
) -> Result<Vec<CollectedWorkspaceSpec>, StateError> {
    let scan = collect_workspace_specs_cached(checkout, collections, parsed);
    for err in scan.errors {
        if err.message.contains("confinement") || err.message.contains("symlink") {
            return Err(StateError::new(err.code, err.message));
        }
        let missing_optional_dir =
            err.diag_code == "VALIDATION_IO" && !checkout.join(&err.path).exists();
        if !missing_optional_dir {
            warnings.push(format!("{label}: {}: {}", err.path, err.message));
        }
    }
    Ok(scan.specs)
}

#[allow(clippy::too_many_arguments)]
fn process_dep_edge(
    raw: &str,
    source: &SpecInfo,
    source_task_ref: Option<&str>,
    local_refs: &BTreeSet<&str>,
    annotation_run: Option<&RunId>,
    discovered: &DiscoveredWorkspace,
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    linked_cache: &mut LinkedWsCache,
    graph: &mut DependencyGraphData,
    warnings: &mut Vec<String>,
) -> Result<(Option<ParsedDepSyntax>, RoadmapDepEntry), StateError> {
    let source_spec_id = &source.spec_id;
    let record_in_graph = !source.is_archived && annotation_run.is_none();
    let mut parsed = match parse_dep_syntax(raw, local_refs) {
        Ok(p) => p,
        Err(err) => {
            let ctx = match source_task_ref {
                Some(t) => format!("spec '{source_spec_id}' task '{t}'"),
                None => format!("spec '{source_spec_id}'"),
            };
            warnings.push(format!("{ctx} malformed depends_on '{raw}': {err}"));
            if record_in_graph && source_task_ref.is_none() {
                graph
                    .has_unsatisfied_external_blocker
                    .insert(source_spec_id.to_owned(), true);
            }
            return Ok((
                None,
                RoadmapDepEntry {
                    raw: raw.to_owned(),
                    status: "invalid".to_owned(),
                    satisfied: false,
                },
            ));
        }
    };

    let resolution = resolve_dep_entry(
        raw,
        &mut parsed,
        source,
        root,
        annotation_run.is_some(),
        collections,
        config,
        reader,
        discovered,
        linked_cache,
        warnings,
    );
    let (entry, active_target) = match (resolution, annotation_run) {
        (Ok(resolved), _) => resolved,
        (Err(err), Some(run_id)) => {
            warnings.push(format!(
                "spec '{source_spec_id}' run '{run_id}' depends_on '{raw}': {err}"
            ));
            return Ok((
                None,
                RoadmapDepEntry {
                    raw: raw.to_owned(),
                    status: "invalid".to_owned(),
                    satisfied: false,
                },
            ));
        }
        (Err(err), None) => return Err(err),
    };

    if record_in_graph {
        record_reverse_block(
            &parsed,
            source_spec_id,
            source_task_ref,
            &mut graph.spec_blocks,
            &mut graph.task_blocks,
        );
        if let Some(target_run) = &active_target {
            let label = source_task_ref.map_or_else(
                || source_spec_id.to_owned(),
                |source_task| format!("{source_spec_id}:{source_task}"),
            );
            graph
                .active_spec_blocks
                .entry(target_run.clone())
                .or_default()
                .insert(label);
        }
        if let (
            Some(target_run),
            ParsedDepSyntax::LocalSpec {
                task_ref: Some(task_ref),
                ..
            }
            | ParsedDepSyntax::LinkedSpec {
                task_ref: Some(task_ref),
                ..
            },
        ) = (active_target, &parsed)
        {
            let label = source_task_ref.map_or_else(
                || source_spec_id.to_owned(),
                |source_task| format!("{source_spec_id}:{source_task}"),
            );
            graph
                .active_task_blocks
                .entry((target_run, TaskRef::new(task_ref)?))
                .or_default()
                .insert(label);
        }
        if !entry.satisfied && !matches!(parsed, ParsedDepSyntax::LocalTask(_)) {
            match &parsed {
                ParsedDepSyntax::LocalSpec {
                    spec_id: target_spec,
                    ..
                } if discovered.specs_by_id.contains_key(target_spec)
                    && (target_spec != source_spec_id || source_task_ref.is_none()) =>
                {
                    graph
                        .forward_spec_edges
                        .entry(target_spec.clone())
                        .or_default()
                        .insert(source_spec_id.to_owned());
                    if source_task_ref.is_none() {
                        graph
                            .unsatisfied_blockers
                            .entry(source_spec_id.to_owned())
                            .or_default()
                            .insert(target_spec.clone());
                    }
                }
                _ => {
                    if source_task_ref.is_none() {
                        graph
                            .has_unsatisfied_external_blocker
                            .insert(source_spec_id.to_owned(), true);
                    }
                }
            }
        }
    }

    Ok((Some(parsed), entry))
}

#[allow(clippy::too_many_arguments)]
fn build_dependency_graph(
    discovered: &DiscoveredWorkspace,
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    active_runs_by_spec: &ActiveRunMap,
    include_archived: bool,
    warnings: &mut Vec<String>,
) -> Result<DependencyGraphData, StateError> {
    let mut graph = DependencyGraphData {
        spec_deps_resolved: BTreeMap::new(),
        active_spec_deps_resolved: BTreeMap::new(),
        task_deps_resolved: BTreeMap::new(),
        forward_spec_edges: BTreeMap::new(),
        unsatisfied_blockers: BTreeMap::new(),
        has_unsatisfied_external_blocker: BTreeMap::new(),
        spec_blocks: BTreeMap::new(),
        active_spec_blocks: BTreeMap::new(),
        task_blocks: BTreeMap::new(),
        active_task_blocks: BTreeMap::new(),
        scoped_planned: BTreeSet::new(),
        scoped_spec_blocks: BTreeMap::new(),
        scoped_task_blocks: BTreeMap::new(),
        active_ext_deps: BTreeMap::new(),
    };
    let mut linked_cache = LinkedWsCache {
        runs: list_all_runs(reader, None)?,
        tombstones: list_all_tombstones(reader)?,
        ..Default::default()
    };
    let has_task_deps = discovered
        .specs_by_id
        .values()
        .chain(discovered.archived_by_id.values().filter(|_| include_archived))
        .chain(discovered.specs_by_run.values())
        .any(|info| {
            info.doc.depends_on.iter().any(|d| d.contains(':'))
                || info
                    .doc
                    .tasks
                    .iter()
                    .any(|t| t.depends_on.iter().any(|d| d.contains(':')))
        });
    linked_cache.task_statuses = load_task_status_map(reader)?;
    if has_task_deps {
        linked_cache.tombstone_archives = load_tombstone_archives(root, &linked_cache.tombstones);
    }
    let empty_refs = BTreeSet::new();

    let planned = discovered
        .specs_by_id
        .values()
        .filter(|info| !active_runs_by_spec.contains_key(&info.spec_id))
        .chain(
            discovered
                .archived_by_id
                .values()
                .filter(|_| include_archived),
        )
        .map(|info| (info, None));
    let active = active_runs_by_spec.iter().flat_map(|(id, runs)| {
        runs.iter().filter_map(move |(run, _)| {
            let info = discovered
                .specs_by_run
                .get(&run.run_id)
                .or_else(|| discovered.specs_by_id.get(id))?;
            Some((info, Some(run)))
        })
    });
    for (info, run) in planned.chain(active) {
        let effective = run
            .filter(|_| !info.doc.depends_on.is_empty())
            .map(|run| {
                effective_active_run_config(config, run, root, Some(&discovered.owned_worktrees))
            })
            .transpose()?;
        let spec_config = effective.as_deref().unwrap_or(config);
        let spec_collections = effective
            .as_ref()
            .map(|cfg| effective_spec_collections(cfg));
        let mut deps_out = Vec::with_capacity(info.doc.depends_on.len());
        for raw in &info.doc.depends_on {
            let (_, entry) = process_dep_edge(
                raw,
                info,
                None,
                &empty_refs,
                run.map(|run| &run.run_id),
                discovered,
                root,
                spec_collections.as_ref().unwrap_or(collections),
                spec_config,
                reader,
                &mut linked_cache,
                &mut graph,
                warnings,
            )?;
            deps_out.push(entry);
        }
        if let Some(run) = run {
            graph
                .active_spec_deps_resolved
                .insert(run.run_id.clone(), deps_out);
            continue;
        }
        graph
            .spec_deps_resolved
            .insert(info.spec_id.clone(), deps_out);

        let local_refs: BTreeSet<&str> =
            info.doc.tasks.iter().map(|t| t.task_ref.as_str()).collect();
        for task in &info.doc.tasks {
            let t_ref = task.task_ref.as_str();
            let mut t_entries = Vec::with_capacity(task.depends_on.len());
            for raw in &task.depends_on {
                let (parsed_opt, entry) = process_dep_edge(
                    raw,
                    info,
                    Some(t_ref),
                    &local_refs,
                    None,
                    discovered,
                    root,
                    collections,
                    config,
                    reader,
                    &mut linked_cache,
                    &mut graph,
                    warnings,
                )?;
                t_entries.push((parsed_opt, entry));
            }
            graph
                .task_deps_resolved
                .insert((info.spec_id.clone(), t_ref.to_owned()), t_entries);
        }
    }

    record_active_blocks(config, discovered, root, active_runs_by_spec, &mut graph)?;
    Ok(graph)
}

fn record_active_blocks(
    config: &StateEngineConfig,
    discovered: &DiscoveredWorkspace,
    root: &Path,
    runs: &ActiveRunMap,
    graph: &mut DependencyGraphData,
) -> Result<(), StateError> {
    let snapshots: BTreeMap<_, _> = runs
        .iter()
        .flat_map(|(spec_id, runs)| {
            runs.iter().filter_map(move |(run, graph)| {
                graph.as_ref().map(|graph| (&run.run_id, (spec_id, graph)))
            })
        })
        .collect();
    let mut external_cache = BTreeMap::new();
    for (&source_run, &(source_spec, source)) in &snapshots {
        let ext_deps = collect_external_satisfied_deps_cached(
            config,
            source,
            Some(&discovered.owned_worktrees),
            &mut external_cache,
        )?;
        graph
            .active_ext_deps
            .insert(source_run.clone(), ext_deps.clone());
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            ..ValidatedObservations::default()
        };
        let completed_refs: BTreeSet<TaskRef> = source
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Task && node.status == NodeStatus::Completed)
            .filter_map(|node| node.task_ref.clone())
            .collect();
        for dep in &source.dependencies {
            let key = (dep.depends_on_run_id.clone(), dep.depends_on_ref.clone());
            if &dep.depends_on_run_id == source_run {
                graph
                    .active_task_blocks
                    .entry(key)
                    .or_default()
                    .insert(dep.task_ref.to_string());
                continue;
            }
            let checkout_raw = PathBuf::from(&dep.depends_on_workspace);
            let checkout = if checkout_raw.as_path() == Path::new(".") {
                checkout_raw
            } else {
                canonical_root(&checkout_raw).unwrap_or(checkout_raw)
            };
            if checkout.as_path() != Path::new(".")
                && checkout.as_path() != root
                && !discovered
                    .owned_worktrees
                    .iter()
                    .any(|owned| owned == &checkout)
            {
                continue;
            }
            let label = format!("{source_spec}:{}", dep.task_ref);
            let persisted = snapshots.get(&dep.depends_on_run_id).copied();
            let scoped = (checkout.as_path() != Path::new(".")).then_some(checkout.as_path());
            let selector = checkout_scoped_target_selector(
                persisted.map(|(_, target)| &target.run),
                dep.depends_on_run_id.as_str(),
                scoped,
            );
            let in_scope = |target: &RunGraphSnapshot| {
                scoped.is_none_or(|checkout| run_in_checkout(&target.run, checkout))
            };
            let target = persisted
                .filter(|(_, target)| in_scope(target))
                .or_else(|| {
                    snapshots.values().copied().find(|(spec_id, target)| {
                        (spec_id.as_str() == selector
                            || run_matches_selector(&target.run, selector))
                            && in_scope(target)
                    })
                });
            let edge_satisfied = is_dep_satisfied_against_statuses(
                &source.run.run_id,
                dep,
                |task_ref| completed_refs.contains(task_ref),
                &obs,
            );
            let (target_spec, completed) = if let Some((target_spec, target)) = target {
                if !dep.depends_on_ref.is_whole_spec() {
                    graph
                        .active_task_blocks
                        .entry((target.run.run_id.clone(), dep.depends_on_ref.clone()))
                        .or_default()
                        .insert(label.clone());
                }
                let completed = target.nodes.iter().any(|node| {
                    node.kind == NodeKind::Task
                        && node.status == NodeStatus::Completed
                        && node.task_ref.as_ref() == Some(&dep.depends_on_ref)
                });
                graph
                    .active_spec_blocks
                    .entry(target.run.run_id.clone())
                    .or_default()
                    .insert(label.clone());
                (target_spec.clone(), completed)
            } else if let Some((target_spec, _)) = discovered
                .specs_by_id
                .get_key_value(selector)
                .filter(|(_, info)| {
                    checkout.as_path() == Path::new(".") || info.checkout == checkout
                })
            {
                if !dep.depends_on_ref.is_whole_spec() {
                    graph
                        .task_blocks
                        .entry((target_spec.clone(), dep.depends_on_ref.to_string()))
                        .or_default()
                        .insert(label.clone());
                }
                graph
                    .spec_blocks
                    .entry(target_spec.clone())
                    .or_default()
                    .insert(label.clone());
                (target_spec.clone(), false)
            } else if let Some(checkout) =
                scoped.filter(|checkout| checkout_spec_id(discovered, checkout, selector).is_some())
            {
                let spec_id = checkout_spec_id(discovered, checkout, selector)
                    .expect("planned checkout spec");
                record_scoped_planned_blocks(
                    graph,
                    checkout,
                    &spec_id,
                    &dep.depends_on_ref,
                    &label,
                );
                (spec_id, false)
            } else {
                continue;
            };
            if !edge_satisfied && !completed && *source_spec != target_spec {
                graph
                    .forward_spec_edges
                    .entry(target_spec.clone())
                    .or_default()
                    .insert(source_spec.clone());
            }
        }
    }
    Ok(())
}

fn checkout_specs_from(
    collections: &BTreeMap<String, SpecCollectionConfig>,
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

fn checkout_spec_id(
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

fn record_scoped_planned_blocks(
    graph: &mut DependencyGraphData,
    checkout: &Path,
    spec_id: &str,
    task_ref: &TaskRef,
    label: &str,
) {
    let key = (checkout.to_path_buf(), spec_id.to_owned());
    graph.scoped_planned.insert(key.clone());
    if !task_ref.is_whole_spec() {
        graph
            .scoped_task_blocks
            .entry((key.0.clone(), key.1.clone(), task_ref.to_string()))
            .or_default()
            .insert(label.to_owned());
    }
    graph
        .scoped_spec_blocks
        .entry(key)
        .or_default()
        .insert(label.to_owned());
}

fn materialize_checkout_spec(
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
    Some(SpecInfo {
        spec_id: doc.id.clone(),
        collection,
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

fn compute_unblocks_counts(
    specs_by_id: &BTreeMap<String, SpecInfo>,
    forward_spec_edges: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, (usize, usize)> {
    let mut unblocks_counts = BTreeMap::new();
    let enqueue_children =
        |node: &str, visited: &mut BTreeSet<String>, queue: &mut VecDeque<String>| {
            if let Some(children) = forward_spec_edges.get(node) {
                for c in children {
                    if visited.insert(c.clone()) {
                        queue.push_back(c.clone());
                    }
                }
            }
        };
    for spec_id in specs_by_id.keys() {
        let mut visited = BTreeSet::new();
        visited.insert(spec_id.clone());
        let mut queue = VecDeque::new();
        enqueue_children(spec_id, &mut visited, &mut queue);
        while let Some(curr) = queue.pop_front() {
            enqueue_children(&curr, &mut visited, &mut queue);
        }
        let spec_count = visited.len().saturating_sub(1);
        let task_count: usize = visited
            .iter()
            .filter(|id| id.as_str() != spec_id.as_str())
            .filter_map(|id| specs_by_id.get(id))
            .map(|s| s.doc.tasks.len())
            .sum();
        unblocks_counts.insert(spec_id.clone(), (spec_count, task_count));
    }
    unblocks_counts
}

fn assemble_roadmap_output(
    config: &StateEngineConfig,
    discovered: &DiscoveredWorkspace,
    graph: &DependencyGraphData,
    active_runs_by_spec: &ActiveRunMap,
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    req: &RoadmapReq,
) -> Result<RoadmapResp, StateError> {
    let mut in_progress_ids = Vec::new();
    let mut planned_ids = Vec::new();
    for (spec_id, info) in &discovered.specs_by_id {
        if active_runs_by_spec.contains_key(spec_id) {
            in_progress_ids.push(spec_id.clone());
        } else if info.doc.status != SpecStatus::Cancelled
            && info.doc.status != SpecStatus::Completed
        {
            planned_ids.push(spec_id.clone());
        }
    }

    let (wave_layers, cycles) = compute_spec_waves(
        &planned_ids,
        &graph.unsatisfied_blockers,
        &graph.has_unsatisfied_external_blocker,
    );
    let want_tasks = |spec_id: &str| req.include_tasks || req.spec_id.as_deref() == Some(spec_id);

    let mut in_progress_entries: Vec<RoadmapSpecEntry> = Vec::new();
    for spec_id in &in_progress_ids {
        if let Some(runs) = active_runs_by_spec.get(spec_id) {
            for active_run in runs {
                in_progress_entries.push(build_spec_entry(
                    config,
                    discovered
                        .specs_by_run
                        .get(&active_run.0.run_id)
                        .unwrap_or(&discovered.specs_by_id[spec_id]),
                    "in_progress",
                    Some(active_run),
                    graph,
                    unblocks_counts,
                    want_tasks(spec_id),
                )?);
            }
        }
    }
    sort_spec_entries(&mut in_progress_entries);
    let active_info = |entry: &RoadmapSpecEntry| {
        entry
            .run_id
            .as_deref()
            .and_then(|run_id| discovered.specs_by_run.get(run_id))
            .unwrap_or(&discovered.specs_by_id[&entry.spec_id])
    };
    apply_spec_overlaps(&mut in_progress_entries, active_info);
    in_progress_entries.retain(|e| passes_display_filter(active_info(e), req));

    let mut waves_out = Vec::with_capacity(wave_layers.len());
    for (wave_idx, layer_ids) in wave_layers {
        let status = if wave_idx == 0 { "ready" } else { "planned" };
        let mut entries: Vec<RoadmapSpecEntry> = layer_ids
            .iter()
            .map(|spec_id| {
                build_spec_entry(
                    config,
                    &discovered.specs_by_id[spec_id],
                    status,
                    None,
                    graph,
                    unblocks_counts,
                    want_tasks(spec_id),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_spec_entries(&mut entries);
        apply_spec_overlaps(&mut entries, |e| &discovered.specs_by_id[&e.spec_id]);
        entries.retain(|e| passes_display_filter(&discovered.specs_by_id[&e.spec_id], req));
        if !entries.is_empty() {
            waves_out.push(RoadmapWave {
                wave: wave_idx,
                specs: entries,
            });
        }
    }

    let mut scoped_entries = Vec::new();
    for (checkout, spec_id) in &graph.scoped_planned {
        let active_here = active_runs_by_spec
            .get(spec_id)
            .is_some_and(|runs| runs.iter().any(|(run, _)| run_in_checkout(run, checkout)));
        if active_here {
            continue;
        }
        let Some(info) = materialize_checkout_spec(discovered, checkout, spec_id) else {
            continue;
        };
        if !passes_display_filter(&info, req) {
            continue;
        }
        scoped_entries.push(build_spec_entry(
            config,
            &info,
            "planned",
            None,
            graph,
            unblocks_counts,
            want_tasks(spec_id),
        )?);
    }
    if !scoped_entries.is_empty() {
        sort_spec_entries(&mut scoped_entries);
        let wave = waves_out
            .last()
            .map(|item| item.wave.saturating_add(1))
            .unwrap_or(0);
        waves_out.push(RoadmapWave {
            wave,
            specs: scoped_entries,
        });
    }

    let mut archived_entries = Vec::new();
    if req.archived
        || req
            .spec_id
            .as_deref()
            .is_some_and(|id| discovered.archived_by_id.contains_key(id))
    {
        let empty_counts = BTreeMap::new();
        for (spec_id, info) in &discovered.archived_by_id {
            if !passes_display_filter(info, req) {
                continue;
            }
            archived_entries.push(build_spec_entry(
                config,
                info,
                "archived",
                None,
                graph,
                &empty_counts,
                want_tasks(spec_id),
            )?);
        }
        archived_entries.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
    }

    if req.ready_only {
        in_progress_entries.clear();
        waves_out.retain(|w| w.wave == 0);
        archived_entries.clear();
    } else if req.active_only {
        waves_out.clear();
        archived_entries.clear();
    }

    Ok(RoadmapResp {
        in_progress: in_progress_entries,
        waves: waves_out,
        archived: archived_entries,
        archived_count: discovered.archived_by_id.len(),
        cycles,
        warnings: discovered.warnings.clone(),
    })
}

fn passes_display_filter(info: &SpecInfo, req: &RoadmapReq) -> bool {
    if let Some(ref target_id) = req.spec_id {
        if info.spec_id != *target_id {
            return false;
        }
    }
    if let Some(ref target_col) = req.collection {
        if info.collection != *target_col {
            return false;
        }
    }
    true
}

fn resolve_collection_name(
    collections: &BTreeMap<String, SpecCollectionConfig>,
    rel_path: &str,
) -> String {
    for (name, col) in collections {
        if col.matches_rel_path(rel_path) {
            return name.clone();
        }
    }
    "specs".to_owned()
}

fn effective_spec_globs(doc: &SpecDocument) -> Vec<String> {
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

fn sort_spec_entries(entries: &mut [RoadmapSpecEntry]) {
    entries.sort_by(|a, b| {
        b.unblocks_specs
            .cmp(&a.unblocks_specs)
            .then_with(|| b.unblocks_tasks.cmp(&a.unblocks_tasks))
            .then_with(|| b.spec_id.cmp(&a.spec_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
}

fn apply_spec_overlaps<'a>(
    entries: &mut [RoadmapSpecEntry],
    info: impl Fn(&RoadmapSpecEntry) -> &'a SpecInfo,
) {
    let n = entries.len();
    for i in 0..n {
        let mut overlaps = BTreeSet::new();
        let globs_i = &info(&entries[i]).effective_globs;
        for (j, other) in entries.iter().enumerate() {
            if i == j {
                continue;
            }
            let globs_j = &info(other).effective_globs;
            if !globs_i.is_empty() && !globs_j.is_empty() && scopes_overlap(globs_i, globs_j) {
                overlaps.insert(other.spec_id.clone());
            }
        }
        entries[i].parallel_safe = overlaps.is_empty();
        entries[i].scope_overlaps = overlaps.into_iter().collect();
    }
}

type KahnWavesResult = (
    BTreeMap<usize, Vec<String>>,
    BTreeMap<String, usize>,
    BTreeSet<String>,
);

fn kahn_propagate_waves(
    ids: &[String],
    mut in_degree: BTreeMap<String, usize>,
    adj: &BTreeMap<String, Vec<String>>,
    mut node_wave: BTreeMap<String, usize>,
) -> KahnWavesResult {
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut waves_by_idx: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut processed = BTreeSet::new();

    while let Some(u) = queue.pop_front() {
        processed.insert(u.clone());
        let w = node_wave[&u];
        waves_by_idx.entry(w).or_default().push(u.clone());
        if let Some(nexts) = adj.get(&u) {
            for v in nexts {
                let next_w = node_wave[v].max(w + 1);
                node_wave.insert(v.clone(), next_w);
                if let Some(d) = in_degree.get_mut(v) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        queue.push_back(v.clone());
                    }
                }
            }
        }
    }

    let remaining: BTreeSet<String> = ids
        .iter()
        .filter(|id| !processed.contains(id.as_str()))
        .cloned()
        .collect();
    (waves_by_idx, node_wave, remaining)
}

fn compute_spec_waves(
    planned_ids: &[String],
    unsatisfied_blockers: &BTreeMap<String, BTreeSet<String>>,
    has_unsatisfied_external_blocker: &BTreeMap<String, bool>,
) -> SpecWavesOutput {
    let planned_set: BTreeSet<&str> = planned_ids.iter().map(String::as_str).collect();
    let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut base_wave: BTreeMap<String, usize> = BTreeMap::new();

    for id in planned_ids {
        let mut deg = 0usize;
        let mut min_wave = usize::from(
            has_unsatisfied_external_blocker
                .get(id)
                .copied()
                .unwrap_or(false),
        );
        if let Some(blockers) = unsatisfied_blockers.get(id) {
            for b in blockers {
                if planned_set.contains(b.as_str()) {
                    deg += 1;
                    adj.entry(b.clone()).or_default().push(id.clone());
                } else {
                    // Any unsatisfied blocker outside `planned_ids` (in_progress, cancelled, etc.) keeps `id` out of Wave 0.
                    min_wave = min_wave.max(1);
                }
            }
        }
        in_degree.insert(id.clone(), deg);
        base_wave.insert(id.clone(), min_wave);
    }

    let (mut waves_by_idx, final_wave, remaining) =
        kahn_propagate_waves(planned_ids, in_degree, &adj, base_wave);
    let cycles = extract_true_cycles(&remaining, &adj);
    let cycle_members: BTreeSet<&str> = cycles.iter().flatten().map(String::as_str).collect();
    let non_cyclic: Vec<String> = remaining
        .iter()
        .filter(|id| !cycle_members.contains(id.as_str()))
        .cloned()
        .collect();
    if !non_cyclic.is_empty() {
        let non_cyclic_set: BTreeSet<&str> = non_cyclic.iter().map(String::as_str).collect();
        let mut res_in_deg: BTreeMap<String, usize> =
            non_cyclic.iter().map(|id| (id.clone(), 0usize)).collect();
        let mut res_adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let res_base_wave: BTreeMap<String, usize> = non_cyclic
            .iter()
            .map(|id| {
                let lb = final_wave.get(id).copied().unwrap_or(1).max(1);
                (id.clone(), lb)
            })
            .collect();
        for src in &non_cyclic {
            if let Some(nexts) = adj.get(src) {
                for dst in nexts {
                    if non_cyclic_set.contains(dst.as_str()) {
                        *res_in_deg.entry(dst.clone()).or_default() += 1;
                        res_adj.entry(src.clone()).or_default().push(dst.clone());
                    }
                }
            }
        }
        let (res_waves, _, _) =
            kahn_propagate_waves(&non_cyclic, res_in_deg, &res_adj, res_base_wave);
        for (w, ids) in res_waves {
            waves_by_idx.entry(w).or_default().extend(ids);
        }
    }

    (waves_by_idx.into_iter().collect(), cycles)
}

fn extract_true_cycles(
    remaining: &BTreeSet<String>,
    adj: &BTreeMap<String, Vec<String>>,
) -> Vec<Vec<String>> {
    if remaining.is_empty() {
        return Vec::new();
    }
    struct Tarjan<'a> {
        remaining: &'a BTreeSet<String>,
        adj: &'a BTreeMap<String, Vec<String>>,
        index: usize,
        indices: BTreeMap<String, usize>,
        lowlink: BTreeMap<String, usize>,
        stack: Vec<String>,
        on_stack: BTreeSet<String>,
        cycles: Vec<Vec<String>>,
    }
    impl<'a> Tarjan<'a> {
        fn strongconnect(&mut self, v: &str) {
            let v_idx = self.index;
            self.index += 1;
            self.indices.insert(v.to_owned(), v_idx);
            self.lowlink.insert(v.to_owned(), v_idx);
            self.stack.push(v.to_owned());
            self.on_stack.insert(v.to_owned());

            let mut self_loop = false;
            if let Some(neighbors) = self.adj.get(v) {
                for w in neighbors {
                    if !self.remaining.contains(w) {
                        continue;
                    }
                    if w == v {
                        self_loop = true;
                    }
                    if !self.indices.contains_key(w) {
                        self.strongconnect(w);
                        let low_w = self.lowlink[w];
                        if let Some(low_v) = self.lowlink.get_mut(v) {
                            *low_v = (*low_v).min(low_w);
                        }
                    } else if self.on_stack.contains(w) {
                        let idx_w = self.indices[w];
                        if let Some(low_v) = self.lowlink.get_mut(v) {
                            *low_v = (*low_v).min(idx_w);
                        }
                    }
                }
            }

            if self.lowlink[v] == v_idx {
                let mut comp = Vec::new();
                while let Some(w) = self.stack.pop() {
                    self.on_stack.remove(&w);
                    let done = w == v;
                    comp.push(w);
                    if done {
                        break;
                    }
                }
                if comp.len() > 1 || self_loop {
                    comp.sort();
                    self.cycles.push(comp);
                }
            }
        }
    }

    let mut tarjan = Tarjan {
        remaining,
        adj,
        index: 0,
        indices: BTreeMap::new(),
        lowlink: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        cycles: Vec::new(),
    };
    for v in remaining {
        if !tarjan.indices.contains_key(v) {
            tarjan.strongconnect(v);
        }
    }
    tarjan.cycles.sort();
    tarjan.cycles
}

fn record_reverse_block(
    parsed: &ParsedDepSyntax,
    source_spec_id: &str,
    source_task_ref: Option<&str>,
    spec_blocks: &mut BTreeMap<String, BTreeSet<String>>,
    task_blocks: &mut BTreeMap<(String, String), BTreeSet<String>>,
) {
    match parsed {
        ParsedDepSyntax::LocalTask(target_task) => {
            if let Some(t_ref) = source_task_ref {
                task_blocks
                    .entry((source_spec_id.to_owned(), target_task.clone()))
                    .or_default()
                    .insert(t_ref.to_owned());
            }
        }
        ParsedDepSyntax::LocalSpec { spec_id, task_ref } => {
            let label = match source_task_ref {
                Some(t_ref) => format!("{source_spec_id}:{t_ref}"),
                None => source_spec_id.to_owned(),
            };
            if let Some(target_task) = task_ref {
                task_blocks
                    .entry((spec_id.clone(), target_task.clone()))
                    .or_default()
                    .insert(label.clone());
            }
            spec_blocks
                .entry(spec_id.clone())
                .or_default()
                .insert(label);
        }
        ParsedDepSyntax::LinkedSpec { .. } => {}
    }
}

fn map_dep_resolution(resolution: DepTargetResolution) -> (&'static str, bool) {
    match resolution {
        DepTargetResolution::ActiveSpec {
            cancelled,
            task_exists,
        } => {
            if !task_exists {
                ("missing", false)
            } else if cancelled {
                ("cancelled", false)
            } else {
                ("planned", false)
            }
        }
        DepTargetResolution::ActiveRun {
            task_exists,
            task_completed,
            ..
        } => {
            if !task_exists {
                ("missing", false)
            } else if task_completed {
                ("satisfied", true)
            } else {
                ("active", false)
            }
        }
        DepTargetResolution::Tombstone {
            cleared,
            task_outcome,
        } => {
            if cleared {
                ("cleared", false)
            } else if task_outcome.is_ok() {
                ("satisfied", true)
            } else {
                ("missing", false)
            }
        }
        DepTargetResolution::ArchivedSpec {
            completed_doc,
            task_exists,
            task_completed,
            ..
        } => {
            if !task_exists {
                ("missing", false)
            } else if completed_doc && task_completed {
                ("archived", true)
            } else {
                ("cancelled", false)
            }
        }
        DepTargetResolution::NotFound => ("missing", false),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_dep_entry(
    raw: &str,
    parsed: &mut ParsedDepSyntax,
    source: &SpecInfo,
    root: &Path,
    active_annotation: bool,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    discovered: &DiscoveredWorkspace,
    linked_cache: &mut LinkedWsCache,
    warnings: &mut Vec<String>,
) -> Result<(RoadmapDepEntry, Option<RunId>), StateError> {
    let to_entry = |(status, satisfied): (&str, bool)| RoadmapDepEntry {
        raw: raw.to_owned(),
        status: status.to_owned(),
        satisfied,
    };
    let local_checkout = if active_annotation {
        &config.worktree
    } else {
        root
    };
    let mut qualified = None;
    let (spec_id, task_ref) = match parsed {
        ParsedDepSyntax::LocalTask(t_ref) => {
            let completed = source.doc.tasks.iter().any(|task| {
                task.task_ref.as_str() == t_ref && task.status == SpecStatus::Completed
            });
            return Ok((
                to_entry((if completed { "satisfied" } else { "planned" }, completed)),
                None,
            ));
        }
        ParsedDepSyntax::LocalSpec { spec_id, task_ref } => (spec_id, task_ref),
        ParsedDepSyntax::LinkedSpec {
            ws_alias,
            spec_id,
            task_ref,
        } => {
            let key = (
                source.checkout.clone(),
                source.spec_id.clone(),
                config.worktree.clone(),
                ws_alias.clone(),
            );
            if !linked_cache.aliases.contains_key(&key) {
                let spec_repos = resolve_spec_repositories(&source.checkout, &source.doc);
                let checkout = if config.linked_workspaces.contains_key(ws_alias)
                    || spec_repos.contains_key(ws_alias)
                {
                    Some(resolve_linked_workspace_root(
                        ws_alias,
                        raw,
                        &config.linked_workspaces,
                        &spec_repos,
                    )?)
                } else {
                    None
                };
                linked_cache.aliases.insert(key.clone(), checkout);
            }
            let Some(checkout) = linked_cache.aliases[&key].clone() else {
                return Ok((to_entry(("missing", false)), None));
            };
            if checkout != root && !discovered.owned_worktrees.contains(&checkout) {
                let outcome = resolve_linked_dep(
                    raw,
                    ws_alias,
                    spec_id,
                    task_ref.as_deref(),
                    checkout,
                    linked_cache,
                    warnings,
                )?;
                return Ok((to_entry(outcome), None));
            }
            qualified = Some((checkout, ws_alias.as_str()));
            (spec_id, task_ref)
        }
    };
    if let Some((checkout, alias)) = &qualified {
        if !discovered.checkout_specs.contains_key(checkout) {
            linked_cache.workspace(
                checkout,
                alias,
                Some((local_checkout, collections)),
                warnings,
            )?;
        }
    }
    let (checkout, target_collections, active_docs): (&Path, &BTreeMap<String, SpecCollectionConfig>, &[SpecDocument]) =
        if let Some((checkout, _)) = &qualified {
            let scoped = match discovered.checkout_specs.get(checkout) {
                Some(scoped) => scoped,
                None => &linked_cache.workspaces[checkout].0,
            };
            (checkout.as_path(), &scoped.collections, &scoped.active_docs)
        } else {
            (
                local_checkout,
                collections,
                discovered
                    .checkout_specs
                    .get(local_checkout)
                    .map_or(&[][..], |scoped| scoped.active_docs.as_slice()),
            )
        };
    let empty_archives = BTreeMap::new();
    let archived_specs = discovered.checkout_archives.get(checkout).unwrap_or(&empty_archives);
    let preloaded = PreloadedDepContext {
        workspace: root,
        runs: &linked_cache.runs,
        task_statuses: &linked_cache.task_statuses,
        tombstones: &linked_cache.tombstones,
        tombstone_archives: &linked_cache.tombstone_archives,
        archived_specs,
    };
    let mut resolved = resolve_dep_target(
        checkout,
        target_collections,
        Some(reader),
        Some(active_docs),
        spec_id,
        task_ref.as_deref(),
        raw,
        qualified.as_ref().map(|(checkout, _)| checkout.as_path()),
        Some(&preloaded),
    )?;
    if qualified.is_none() && matches!(resolved, DepTargetResolution::NotFound) {
        if let Some(sibling) = discovered.sibling_candidates.get(spec_id)
            .and_then(|paths| paths.iter().find(|path| path.as_path() != checkout))
        {
            let scoped = &discovered.checkout_specs[sibling];
            let sibling_context = PreloadedDepContext {
                archived_specs: discovered.checkout_archives.get(sibling).unwrap_or(&empty_archives),
                ..preloaded
            };
            resolved = resolve_dep_target(
                sibling, &scoped.collections, None, Some(&scoped.active_docs), spec_id,
                task_ref.as_deref(), raw, None, Some(&sibling_context),
            )?;
        }
    }
    let mut active_target = None;
    let target_in_checkout = match &resolved {
        DepTargetResolution::ActiveRun {
            run_id,
            spec_id: resolved_spec,
            worktree: _,
            task_exists,
            ..
        } => {
            let matches_checkout = qualified.is_none() || *task_exists;
            if matches_checkout {
                active_target = Some(run_id.clone());
            }
            if let Some(resolved_spec) = resolved_spec {
                spec_id.clone_from(resolved_spec);
            }
            matches_checkout
        }
        DepTargetResolution::ActiveSpec { task_exists, .. } => *task_exists,
        _ => false,
    };
    let local_vertex = qualified.is_some()
        && target_in_checkout
        && discovered
            .specs_by_id
            .get(spec_id)
            .is_some_and(|info| info.checkout == checkout);
    if local_vertex {
        *parsed = ParsedDepSyntax::LocalSpec {
            spec_id: std::mem::take(spec_id),
            task_ref: task_ref.take(),
        };
    }
    Ok((to_entry(map_dep_resolution(resolved)), active_target))
}

fn resolve_linked_dep(
    raw: &str,
    ws_alias: &str,
    spec_id: &str,
    task_ref: Option<&str>,
    ws_path: PathBuf,
    linked_cache: &mut LinkedWsCache,
    warnings: &mut Vec<String>,
) -> Result<(&'static str, bool), StateError> {
    let cache_key = (
        ws_path.clone(),
        spec_id.to_owned(),
        task_ref.map(str::to_owned),
    );
    if let Some(&cached) = linked_cache.outcomes.get(&cache_key) {
        return Ok(cached);
    }

    if !linked_cache.linked_deps.contains_key(&ws_path) {
        let (scoped, ext_reader) = linked_cache.workspace(&ws_path, ws_alias, None, warnings)?;
        let reader = ext_reader.as_ref().expect("linked workspace has a reader");
        let (data, archive_warnings) = load_dep_snapshot(&ws_path, &scoped.collections, reader)?;
        warnings.extend(archive_warnings);
        linked_cache.linked_deps.insert(ws_path.clone(), data);
    }
    let (scoped, ext_reader) = &linked_cache.workspaces[&ws_path];
    let preloaded = linked_cache.linked_deps[&ws_path].context();
    let outcome = map_dep_resolution(resolve_dep_target(
        &ws_path,
        &scoped.collections,
        ext_reader.as_ref().map(|reader| reader as &dyn StateReader),
        Some(&scoped.active_docs),
        spec_id,
        task_ref,
        raw,
        None,
        Some(&preloaded),
    )?);
    linked_cache.outcomes.insert(cache_key, outcome);
    Ok(outcome)
}

impl LinkedWsCache {
    fn workspace(
        &mut self,
        checkout: &Path,
        alias: &str,
        local: Option<(&Path, &BTreeMap<String, SpecCollectionConfig>)>,
        warnings: &mut Vec<String>,
    ) -> Result<&(CheckoutSpecs, Option<SqliteStateReader>), StateError> {
        use std::collections::btree_map::Entry;
        match self.workspaces.entry(checkout.to_path_buf()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let (collections, reader) = match local {
                    Some((root, collections)) if root == checkout => (collections.clone(), None),
                    Some(_) => (load_ws_adapter(checkout)?.specs, None),
                    None => {
                        let (db_path, specs) = resolve_ws_context_canon(checkout, None)?;
                        (specs, Some(SqliteStateReader::new(db_path)))
                    }
                };
                let scanned = scan_checkout_specs(checkout, alias, &collections, warnings, &mut BTreeMap::new())?;
                Ok(entry.insert((checkout_specs_from(&collections, &scanned), reader)))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_spec_entry(
    config: &StateEngineConfig,
    info: &SpecInfo,
    status: &str,
    active_run: Option<&(RunRecord, Option<RunGraphSnapshot>)>,
    deps: &DependencyGraphData,
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    include_tasks: bool,
) -> Result<RoadmapSpecEntry, StateError> {
    let mut total_tasks = info.doc.tasks.len();
    let (run_id, worktree, completed_tasks, active_gates) = match active_run {
        Some((run, graph_opt)) => {
            let (completed, gates) = match graph_opt {
                Some(graph) => {
                    total_tasks = graph
                        .nodes
                        .iter()
                        .filter(|n| n.kind == NodeKind::Task)
                        .count();
                    let comp = graph
                        .nodes
                        .iter()
                        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
                        .count();
                    let active = graph
                        .nodes
                        .iter()
                        .filter(|n| {
                            matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                                && n.status == NodeStatus::InProgress
                        })
                        .map(to_active_gate_entry)
                        .collect();
                    (comp, active)
                }
                None => (0, Vec::new()),
            };
            (
                Some(run.run_id.as_str().to_owned()),
                run.worktree.clone(),
                completed,
                gates,
            )
        }
        None => {
            let comp = info
                .doc
                .tasks
                .iter()
                .filter(|t| t.status == SpecStatus::Completed)
                .count();
            (None, None, comp, Vec::new())
        }
    };

    let (unblocks_specs, unblocks_tasks) = unblocks_counts
        .get(&info.spec_id)
        .copied()
        .unwrap_or((0, 0));

    let tasks = if include_tasks {
        build_task_entries(config, info, active_run.and_then(|(_, g)| g.as_ref()), deps)?
    } else {
        Vec::new()
    };

    Ok(RoadmapSpecEntry {
        spec_id: info.spec_id.clone(),
        title: info.doc.title.clone(),
        collection: info.collection.clone(),
        path: info.path.clone(),
        doc_type: info.doc.doc_type.clone(),
        status: status.to_owned(),
        run_id,
        worktree,
        source_worktree: info.source_worktree.clone(),
        divergence: info.divergence.clone(),
        completed_tasks,
        total_tasks,
        active_gates,
        unblocks_specs,
        unblocks_tasks,
        parallel_safe: true,
        scope_overlaps: Vec::new(),
        depends_on: match active_run {
            Some((run, _)) => deps.active_spec_deps_resolved.get(&run.run_id),
            None => deps.spec_deps_resolved.get(&info.spec_id),
        }
        .cloned()
        .unwrap_or_default(),
        blocks: {
            let scoped_key = (info.checkout.clone(), info.spec_id.clone());
            let blocks = if let Some((run, _)) = active_run {
                deps.active_spec_blocks.get(&run.run_id)
            } else if deps.scoped_planned.contains(&scoped_key) {
                deps.scoped_spec_blocks.get(&scoped_key)
            } else {
                deps.spec_blocks.get(&info.spec_id)
            };
            blocks
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default()
        },
        tasks,
    })
}

fn to_active_gate_entry(node: &GateNodeRecord) -> RoadmapActiveGateEntry {
    let cursor = match (&node.kind, &node.task_ref) {
        (NodeKind::TaskGate, Some(t_ref)) => format!("{t_ref}.{}", node.gate_id),
        _ => node.gate_id.clone(),
    };
    RoadmapActiveGateEntry {
        cursor,
        status: node.status.as_str().to_owned(),
        worker_id: node.worker_id.as_ref().map(|w| w.as_str().to_owned()),
    }
}

fn compute_task_waves(
    task_ids: &[String],
    unsatisfied_local: &BTreeMap<String, BTreeSet<String>>,
    has_unsatisfied_ext: &BTreeMap<String, bool>,
) -> BTreeMap<String, usize> {
    let mut in_deg: BTreeMap<String, usize> = BTreeMap::new();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut base_wave: BTreeMap<String, usize> = BTreeMap::new();
    for id in task_ids {
        let blockers = unsatisfied_local.get(id);
        in_deg.insert(id.clone(), blockers.map_or(0, BTreeSet::len));
        base_wave.insert(
            id.clone(),
            usize::from(has_unsatisfied_ext.get(id).copied().unwrap_or(false)),
        );
        if let Some(bs) = blockers {
            for b in bs {
                adj.entry(b.clone()).or_default().push(id.clone());
            }
        }
    }
    let (_, task_wave, _) = kahn_propagate_waves(task_ids, in_deg, &adj, base_wave);
    task_wave
}

fn resolve_task_status_and_gates(
    info: &SpecInfo,
    t_ref: &str,
    task_status: SpecStatus,
    all_deps_ok: bool,
    graph_opt: Option<&RunGraphSnapshot>,
    claimed_scopes: &[(acdd_core::ids::TaskRef, Vec<String>)],
) -> (&'static str, Vec<RoadmapActiveGateEntry>) {
    if let Some(graph) = graph_opt {
        let task_node = graph.nodes.iter().find(|n| {
            n.kind == NodeKind::Task && n.task_ref.as_ref().map(|t| t.as_str()) == Some(t_ref)
        });
        let gates: Vec<RoadmapActiveGateEntry> = graph
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.task_ref.as_ref().map(|t| t.as_str()) == Some(t_ref)
                    && n.status == NodeStatus::InProgress
            })
            .map(to_active_gate_entry)
            .collect();
        let st = match task_node {
            Some(node) if node.status == NodeStatus::Completed => "completed",
            Some(node) if node.status == NodeStatus::Cancelled => "cancelled",
            Some(node) if node.status == NodeStatus::InProgress => "in_progress",
            Some(node)
                if crate::machine::schedule::is_task_schedule_eligible(
                    graph,
                    node,
                    claimed_scopes,
                    all_deps_ok,
                ) =>
            {
                "ready"
            }
            _ => "planned",
        };
        (st, gates)
    } else if task_status == SpecStatus::Cancelled {
        ("cancelled", Vec::new())
    } else if info.is_archived || task_status == SpecStatus::Completed {
        ("completed", Vec::new())
    } else if all_deps_ok {
        ("ready", Vec::new())
    } else {
        ("planned", Vec::new())
    }
}

fn apply_task_overlaps(entries: &mut [RoadmapTaskEntry]) {
    let n = entries.len();
    for i in 0..n {
        let mut overlaps = BTreeSet::new();
        for (j, other) in entries.iter().enumerate() {
            if i == j || entries[i].wave != other.wave {
                continue;
            }
            if !entries[i].scope.is_empty()
                && !other.scope.is_empty()
                && scopes_overlap(&entries[i].scope, &other.scope)
            {
                overlaps.insert(other.task_ref.clone());
            }
        }
        entries[i].scope_overlaps = overlaps.into_iter().collect();
    }
}

fn build_task_entries(
    config: &StateEngineConfig,
    info: &SpecInfo,
    graph_opt: Option<&RunGraphSnapshot>,
    projection: &DependencyGraphData,
) -> Result<Vec<RoadmapTaskEntry>, StateError> {
    if let Some(graph) = graph_opt {
        let ext_deps = match projection.active_ext_deps.get(&graph.run.run_id) {
            Some(cached) => cached.clone(),
            None => collect_external_satisfied_deps_with_owned(config, graph, None)?,
        };
        let obs = crate::types::ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            ..Default::default()
        };
        let task_nodes: Vec<&GateNodeRecord> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task && n.task_ref.is_some())
            .collect();
        let completed_refs: BTreeSet<acdd_core::ids::TaskRef> = task_nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .filter_map(|n| n.task_ref.clone())
            .collect();
        let task_ids: Vec<String> = task_nodes
            .iter()
            .filter_map(|n| n.task_ref.as_ref().map(|t| t.as_str().to_owned()))
            .collect();

        let mut deps_by_task: BTreeMap<String, Vec<RoadmapDepEntry>> = BTreeMap::new();
        let mut unsatisfied_local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut has_unsatisfied_ext: BTreeMap<String, bool> = BTreeMap::new();
        for node in &task_nodes {
            let Some(ref task_ref) = node.task_ref else {
                continue;
            };
            let t_ref = task_ref.as_str().to_owned();
            let mut entries = Vec::new();
            for dep in graph
                .dependencies
                .iter()
                .filter(|d| &d.task_ref == task_ref)
            {
                let satisfied = crate::machine::schedule::is_dep_satisfied_against_statuses(
                    &graph.run.run_id,
                    dep,
                    |r| completed_refs.contains(r),
                    &obs,
                );
                let raw = if dep.depends_on_run_id == graph.run.run_id {
                    dep.depends_on_ref.as_str().to_owned()
                } else if dep.depends_on_workspace == "." {
                    format!(
                        "{}:{}",
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str()
                    )
                } else {
                    format!(
                        "{}/{}:{}",
                        dep.depends_on_workspace,
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str()
                    )
                };
                if !satisfied {
                    if dep.depends_on_run_id == graph.run.run_id {
                        unsatisfied_local
                            .entry(t_ref.clone())
                            .or_default()
                            .insert(dep.depends_on_ref.as_str().to_owned());
                    } else {
                        has_unsatisfied_ext.insert(t_ref.clone(), true);
                    }
                }
                entries.push(RoadmapDepEntry {
                    raw,
                    status: if satisfied {
                        "satisfied".to_owned()
                    } else {
                        "planned".to_owned()
                    },
                    satisfied,
                });
            }
            deps_by_task.insert(t_ref, entries);
        }

        let task_wave = compute_task_waves(&task_ids, &unsatisfied_local, &has_unsatisfied_ext);
        let claimed_scopes = crate::machine::schedule::claimed_unfinished_scopes(graph);
        let mut out = Vec::with_capacity(task_nodes.len());
        for node in &task_nodes {
            let Some(ref task_ref) = node.task_ref else {
                continue;
            };
            let t_ref = task_ref.as_str().to_owned();
            let disk_task = info.doc.tasks.iter().find(|t| t.task_ref.as_str() == t_ref);
            let deps = deps_by_task.remove(&t_ref).unwrap_or_default();
            let all_deps_ok =
                crate::machine::schedule::task_dependencies_satisfied(graph, task_ref, &obs);
            let (status, active_gates) = resolve_task_status_and_gates(
                info,
                &t_ref,
                disk_task.map_or(SpecStatus::Planned, |t| t.status),
                all_deps_ok,
                Some(graph),
                &claimed_scopes,
            );

            let mut scope = crate::machine::graph::container_scope(graph, node);
            if scope.is_empty() {
                if let Some(dt) = disk_task {
                    scope.clone_from(&dt.scope);
                    for ext in &dt.extended_scope {
                        if !scope.contains(&ext.path) {
                            scope.push(ext.path.clone());
                        }
                    }
                }
            }
            out.push(RoadmapTaskEntry {
                task_ref: t_ref.clone(),
                target: disk_task.map(|t| t.target.clone()).unwrap_or_default(),
                status: status.to_owned(),
                wave: task_wave.get(&t_ref).copied().unwrap_or(0),
                depends_on: deps,
                blocks: projection
                    .active_task_blocks
                    .get(&(graph.run.run_id.clone(), task_ref.clone()))
                    .map(|blocks| blocks.iter().cloned().collect())
                    .unwrap_or_default(),
                scope,
                scope_overlaps: Vec::new(),
                active_gates,
            });
        }
        apply_task_overlaps(&mut out);
        return Ok(out);
    }

    let mut deps_by_task: BTreeMap<String, Vec<RoadmapDepEntry>> = BTreeMap::new();
    let mut unsatisfied_local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut has_unsatisfied_ext: BTreeMap<String, bool> = BTreeMap::new();

    for task in &info.doc.tasks {
        let t_ref = task.task_ref.as_str().to_owned();
        if let Some(resolved) = projection
            .task_deps_resolved
            .get(&(info.spec_id.clone(), t_ref.clone()))
        {
            let mut entries = Vec::with_capacity(resolved.len());
            for (parsed_opt, entry) in resolved {
                if !entry.satisfied {
                    if let Some(ParsedDepSyntax::LocalTask(blocker)) = parsed_opt {
                        unsatisfied_local
                            .entry(t_ref.clone())
                            .or_default()
                            .insert(blocker.clone());
                    } else {
                        has_unsatisfied_ext.insert(t_ref.clone(), true);
                    }
                }
                entries.push(entry.clone());
            }
            deps_by_task.insert(t_ref, entries);
        }
    }

    let task_ids: Vec<String> = info
        .doc
        .tasks
        .iter()
        .map(|t| t.task_ref.as_str().to_owned())
        .collect();
    let task_wave = compute_task_waves(&task_ids, &unsatisfied_local, &has_unsatisfied_ext);
    let mut out = Vec::with_capacity(info.doc.tasks.len());
    for task in &info.doc.tasks {
        let t_ref = task.task_ref.as_str().to_owned();
        let deps = deps_by_task.remove(&t_ref).unwrap_or_default();
        let all_deps_ok = deps.iter().all(|d| d.satisfied);
        let (status, active_gates) =
            resolve_task_status_and_gates(info, &t_ref, task.status, all_deps_ok, None, &[]);

        let mut scope: Vec<String> = task.scope.clone();
        for ext in &task.extended_scope {
            if !scope.contains(&ext.path) {
                scope.push(ext.path.clone());
            }
        }

        out.push(RoadmapTaskEntry {
            task_ref: t_ref.clone(),
            target: task.target.clone(),
            status: status.to_owned(),
            wave: task_wave.get(&t_ref).copied().unwrap_or(0),
            depends_on: deps,
            blocks: {
                let scoped_key = (info.checkout.clone(), info.spec_id.clone());
                let blocks = if projection.scoped_planned.contains(&scoped_key) {
                    projection.scoped_task_blocks.get(&(
                        info.checkout.clone(),
                        info.spec_id.clone(),
                        t_ref.clone(),
                    ))
                } else {
                    projection
                        .task_blocks
                        .get(&(info.spec_id.clone(), t_ref.clone()))
                };
                blocks
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default()
            },
            scope,
            scope_overlaps: Vec::new(),
            active_gates,
        });
    }

    apply_task_overlaps(&mut out);
    Ok(out)
}

#[cfg(test)]
mod roadmap_graph_tests {
    use super::*;

    fn blockers(rows: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        rows.iter()
            .map(|(id, deps)| {
                (
                    (*id).to_owned(),
                    deps.iter().map(|dep| (*dep).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn spec_info(id: &str, task_count: usize) -> SpecInfo {
        let tasks = (1..=task_count)
            .map(|index| {
                format!(
                    "### task: T-{index:02}\n```yaml\ntask_ref: T-{index:02}\nscope: [src/{id}/{index}/**]\nacceptance_criteria: [done]\n```\n"
                )
            })
            .collect::<String>();
        let doc = crate::spec::parse_spec(&format!(
            "---\nid: {id}\ntitle: {id}\nstatus: planned\n---\n{tasks}"
        ))
        .unwrap();
        SpecInfo {
            spec_id: id.to_owned(),
            collection: "milestones".to_owned(),
            path: format!("docs/milestones/{id}.md"),
            doc,
            checkout: PathBuf::new(),
            effective_globs: Vec::new(),
            is_archived: false,
            source_worktree: None,
            divergence: Vec::new(),
        }
    }

    #[test]
    fn pure_roadmap_graph_matrix() {
        struct Case {
            ids: Vec<&'static str>,
            blockers: Vec<(&'static str, Vec<&'static str>)>,
            external: Vec<&'static str>,
            waves: Vec<(usize, Vec<&'static str>)>,
            cycles: Vec<Vec<&'static str>>,
        }

        let cases = [
            Case {
                ids: vec!["A", "B", "C"],
                blockers: vec![("B", vec!["A"]), ("C", vec!["B"])],
                external: Vec::new(),
                waves: vec![(0, vec!["A"]), (1, vec!["B"]), (2, vec!["C"])],
                cycles: Vec::new(),
            },
            Case {
                ids: vec!["A", "B"],
                blockers: Vec::new(),
                external: vec!["B"],
                waves: vec![(0, vec!["A"]), (1, vec!["B"])],
                cycles: Vec::new(),
            },
            Case {
                ids: vec!["A", "C1", "C2", "D", "MIX-B", "MIX-1", "SELF"],
                blockers: vec![
                    ("C1", vec!["C2"]),
                    ("C2", vec!["C1"]),
                    ("D", vec!["C1"]),
                    ("MIX-B", vec!["A"]),
                    ("MIX-1", vec!["MIX-B", "C1"]),
                    ("SELF", vec!["SELF"]),
                ],
                external: Vec::new(),
                waves: vec![(0, vec!["A"]), (1, vec!["MIX-B", "D"]), (2, vec!["MIX-1"])],
                cycles: vec![vec!["C1", "C2"], vec!["SELF"]],
            },
        ];

        for case in cases {
            let ids: Vec<String> = case.ids.iter().map(|id| (*id).to_owned()).collect();
            let rows: Vec<(&str, &[&str])> = case
                .blockers
                .iter()
                .map(|(id, deps)| (*id, deps.as_slice()))
                .collect();
            let external = case
                .external
                .iter()
                .map(|id| ((*id).to_owned(), true))
                .collect();
            let (actual_waves, actual_cycles) =
                compute_spec_waves(&ids, &blockers(&rows), &external);
            let expected_waves: Vec<(usize, Vec<String>)> = case
                .waves
                .iter()
                .map(|(wave, ids)| (*wave, ids.iter().map(|id| (*id).to_owned()).collect()))
                .collect();
            let expected_cycles: Vec<Vec<String>> = case
                .cycles
                .iter()
                .map(|ids| ids.iter().map(|id| (*id).to_owned()).collect())
                .collect();
            assert_eq!(actual_waves, expected_waves);
            assert_eq!(actual_cycles, expected_cycles);
        }

        let task_waves = compute_task_waves(
            &["T-01".to_owned(), "T-02".to_owned(), "T-03".to_owned()],
            &blockers(&[("T-02", &["T-01"]), ("T-03", &["T-02"])]),
            &BTreeMap::new(),
        );
        assert_eq!(task_waves["T-01"], 0);
        assert_eq!(task_waves["T-02"], 1);
        assert_eq!(task_waves["T-03"], 2);

        let specs = [
            ("A".to_owned(), spec_info("A", 1)),
            ("B".to_owned(), spec_info("B", 2)),
            ("C".to_owned(), spec_info("C", 3)),
        ]
        .into_iter()
        .collect();
        let edges = BTreeMap::from([
            ("A".to_owned(), BTreeSet::from(["B".to_owned()])),
            ("B".to_owned(), BTreeSet::from(["C".to_owned()])),
        ]);
        assert_eq!(compute_unblocks_counts(&specs, &edges)["A"], (2, 5));
    }
}
