//! Shared data structures for roadmap synthesis, discovery, graph resolution, and wave computation.

use crate::deps::ParsedDepSyntax;
use crate::overlay::RuntimeActiveRun;
use crate::spec::SpecDocument;
use crate::types::{RoadmapDepEntry, RoadmapWorktreeCopy};
use acdd_core::config::SpecConfig;
use acdd_core::ids::RunId;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Workspace inputs required for [`super::build_roadmap`].
#[derive(Debug, Clone, Copy)]
pub struct RoadmapConfig<'a> {
    /// Primary workspace root directory.
    pub workspace_root: &'a Path,
    /// Current worktree checkout directory.
    pub worktree: &'a Path,
    /// Configured spec collections keyed by collection name.
    pub spec_collections: &'a BTreeMap<String, SpecConfig>,
    /// Configured linked workspace roots keyed by alias.
    pub linked_workspaces: &'a BTreeMap<String, PathBuf>,
}

pub(super) type ActiveRunMap = BTreeMap<String, Vec<RuntimeActiveRun>>;
pub(super) type TaskDepsMap =
    BTreeMap<(String, String), Vec<(Option<ParsedDepSyntax>, RoadmapDepEntry)>>;
pub(super) type SpecWavesOutput = (Vec<(usize, Vec<String>)>, Vec<Vec<String>>);
pub(super) type CommittedArchives = Vec<(String, String, SpecDocument)>;

pub(super) struct SpecInfo {
    pub(super) spec_id: String,
    pub(super) effective_spec: String,
    pub(super) roadmap_visible: bool,
    pub(super) path: String,
    pub(super) doc: SpecDocument,
    pub(super) checkout: PathBuf,
    pub(super) effective_globs: Vec<String>,
    pub(super) is_archived: bool,
    pub(super) source_worktree: Option<String>,
    pub(super) divergence: Vec<RoadmapWorktreeCopy>,
}

#[derive(Clone)]
pub(super) struct DiscoveredCopy {
    pub(super) wt_label: String,
    pub(super) wt_path: PathBuf,
    pub(super) effective_spec: String,
    pub(super) roadmap_visible: bool,
    pub(super) rel_path: String,
    pub(super) doc: SpecDocument,
    pub(super) is_archived: bool,
}

pub(super) struct CheckoutSpecs {
    pub(super) collections: BTreeMap<String, SpecConfig>,
    pub(super) active_docs: Vec<SpecDocument>,
    /// `(collection, rel_path)` aligned with `active_docs`.
    pub(super) files: Vec<(String, String)>,
}

#[derive(Default)]
pub(super) struct LinkedWsCache {
    pub(super) workspaces: BTreeMap<PathBuf, CheckoutSpecs>,
    pub(super) outcomes: BTreeMap<(PathBuf, String, Option<String>), (&'static str, bool)>,
    pub(super) aliases: BTreeMap<(PathBuf, String, PathBuf, String), Option<PathBuf>>,
    pub(super) linked_archives: BTreeMap<PathBuf, BTreeMap<String, (String, SpecDocument)>>,
}

pub(super) struct DiscoveredWorkspace {
    pub(super) specs_by_id: BTreeMap<String, SpecInfo>,
    pub(super) archived_by_id: BTreeMap<String, SpecInfo>,
    pub(super) checkout_archives: BTreeMap<PathBuf, BTreeMap<String, (String, SpecDocument)>>,
    pub(super) specs_by_run: BTreeMap<RunId, SpecInfo>,
    pub(super) owned_worktrees: Vec<PathBuf>,
    pub(super) worktree_branches: BTreeMap<PathBuf, Option<String>>,
    pub(super) checkout_specs: BTreeMap<PathBuf, CheckoutSpecs>,
    pub(super) sibling_candidates: BTreeMap<String, Vec<PathBuf>>,
    pub(super) warnings: Vec<String>,
    pub(super) divergence_warnings: Vec<(String, String)>,
}

/// Canonical checkout-identity predicate matching a recorded run `(worktree, branch)`
/// against a discovered checkout `target_wt`.
pub(super) fn checkout_matches_run(
    worktree: Option<&str>,
    branch: Option<&str>,
    target_wt: &Path,
    worktree_branches: &BTreeMap<PathBuf, Option<String>>,
) -> bool {
    let Some(raw_w) = worktree else {
        return false;
    };
    let raw_path = Path::new(raw_w);
    if std::fs::symlink_metadata(raw_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    let Ok(wt_canon) = acdd_core::fs::canonical_root(raw_path) else {
        return false;
    };
    if wt_canon != target_wt {
        return false;
    }
    if !worktree_branches.is_empty() {
        let Some(discovered_branch) = worktree_branches.get(target_wt) else {
            return false;
        };
        if let Some(expected_branch) = branch {
            return discovered_branch.as_deref() == Some(expected_branch);
        }
    }
    true
}

impl DiscoveredWorkspace {
    pub(super) fn owning_checkout_for_run(&self, run: &RuntimeActiveRun) -> Option<PathBuf> {
        let raw_w = run.worktree.as_deref()?;
        let raw_path = Path::new(raw_w);
        if std::fs::symlink_metadata(raw_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return None;
        }
        let wt_canon = acdd_core::fs::canonical_root(raw_path).ok()?;
        checkout_matches_run(
            Some(raw_w),
            run.branch.as_deref(),
            &wt_canon,
            &self.worktree_branches,
        )
        .then_some(wt_canon)
    }

    fn resolve_run_spec(
        &self,
        run_id: &str,
        worktree: Option<&str>,
        branch: Option<&str>,
        spec_id: &str,
    ) -> Option<&SpecInfo> {
        if let Some(info) = self.specs_by_run.get(run_id) {
            return Some(info);
        }
        let fallback = self.specs_by_id.get(spec_id)?;
        match worktree {
            Some(_) => checkout_matches_run(
                worktree,
                branch,
                &fallback.checkout,
                &self.worktree_branches,
            )
            .then_some(fallback),
            None => Some(fallback),
        }
    }

    pub(super) fn active_run_spec(
        &self,
        run: &RuntimeActiveRun,
        spec_id: &str,
    ) -> Option<&SpecInfo> {
        self.resolve_run_spec(
            run.run_id.as_str(),
            run.worktree.as_deref(),
            run.branch.as_deref(),
            spec_id,
        )
    }

    pub(super) fn has_resolved_active_run(&self, spec_id: &str, runs: &[RuntimeActiveRun]) -> bool {
        runs.iter()
            .any(|run| self.active_run_spec(run, spec_id).is_some())
    }

    pub(super) fn entry_spec_info(
        &self,
        entry: &crate::types::RoadmapSpecEntry,
        active_runs_by_spec: &ActiveRunMap,
        include_archived: bool,
    ) -> Option<&SpecInfo> {
        if let Some(run_id) = entry.run_id.as_deref() {
            if let Some(run) = active_runs_by_spec
                .get(&entry.spec_id)
                .and_then(|runs| runs.iter().find(|r| r.run_id.as_str() == run_id))
            {
                return self.active_run_spec(run, &entry.spec_id);
            }
            return self.resolve_run_spec(run_id, entry.worktree.as_deref(), None, &entry.spec_id);
        }
        self.specs_by_id.get(&entry.spec_id).or_else(|| {
            include_archived
                .then(|| self.archived_by_id.get(&entry.spec_id))
                .flatten()
        })
    }
}

pub(super) struct DependencyGraphData {
    pub(super) spec_deps_resolved: BTreeMap<String, Vec<RoadmapDepEntry>>,
    pub(super) active_spec_deps_resolved: BTreeMap<RunId, Vec<RoadmapDepEntry>>,
    pub(super) task_deps_resolved: TaskDepsMap,
    pub(super) forward_spec_edges: BTreeMap<String, BTreeSet<String>>,
    pub(super) unsatisfied_blockers: BTreeMap<String, BTreeSet<String>>,
    pub(super) has_unsatisfied_external_blocker: BTreeMap<String, bool>,
    pub(super) spec_blocks: BTreeMap<String, BTreeSet<String>>,
    pub(super) active_spec_blocks: BTreeMap<RunId, BTreeSet<String>>,
    pub(super) task_blocks: BTreeMap<(String, String), BTreeSet<String>>,
    pub(super) active_task_blocks: BTreeMap<(RunId, acdd_core::ids::TaskRef), BTreeSet<String>>,
    /// Unstarted checkout copies of a spec that is active only in another checkout.
    pub(super) scoped_planned: BTreeSet<(PathBuf, String)>,
    pub(super) scoped_spec_blocks: BTreeMap<(PathBuf, String), BTreeSet<String>>,
    pub(super) scoped_task_blocks: BTreeMap<(PathBuf, String, String), BTreeSet<String>>,
}
