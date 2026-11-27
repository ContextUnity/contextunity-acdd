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
    pub(super) collection: String,
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
    pub(super) collection: String,
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
    pub(super) checkout_specs: BTreeMap<PathBuf, CheckoutSpecs>,
    pub(super) sibling_candidates: BTreeMap<String, Vec<PathBuf>>,
    pub(super) warnings: Vec<String>,
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
