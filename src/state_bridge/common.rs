//! Shared leaf constants, path/adapter resolution, SQLite read helpers, and state execution helpers for `state_bridge`.

use acdd_core::config::{Adapter, SpecConfig, ACDD_CONFIG_FILE};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::git::{git_common_dir, git_command, primary_worktree_root};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use acdd_core::ports::{CompositeDomainProvider, DomainActionResult, DomainCheckReq, DomainProvider};
use acdd_core::subsystems::SubsystemId;
use acdd_index::db::reader::open_raw_confined_readonly;
use acdd_state::context::budget::pack_json_lens;
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateEngineConfig, StateQuery, StateQueryEngine,
    StateQueryOutput,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Explicit CLI/MCP invocation settings replacing legacy `acdd_tasks::tasks::configuration::InvocationSettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationSettings {
    /// Primary workspace root.
    pub root: PathBuf,
    /// Explicit `--config` / `acdd.yaml` path override.
    pub config: Option<PathBuf>,
    /// Explicit `--workflow` override.
    pub workflow: Option<PathBuf>,
    /// Explicit `--state-db` override.
    pub state_db: Option<PathBuf>,
    /// Resolved `code-map.sqlite` path.
    pub code_db: PathBuf,
}

/// Mandatory instruction banner emitted in `paths.instructions` to direct workers to scoped `AGENTS.md` files.
pub(super) const DOCS_INSTRUCTIONS_DIRECTIVE: &str =
    "MANDATORY: read repository rules for respective scope.";
/// Maximum fallback invariants returned when `code-map.sqlite` has no scope-matched `governs` edges or `admitted_scope` is empty.
pub(super) const DOCS_INVARIANT_FALLBACK_ITEM_LIMIT: usize = 8;
/// Maximum rule summary characters kept per entry in `docs.invariants` so full rules stay readable without mid-word clipping.
pub(super) const DOCS_INVARIANT_MAX_RULE_CHARS: usize = 400;
/// Maximum declarations per file below which private helpers remain visible alongside public symbols in `code.symbols`.
pub(super) const CODE_SYMBOLS_SMALL_FILE_PRIVATE_THRESHOLD: usize = 5;
/// Maximum scoped symbol entries retained in `code.symbols` after per-file round-robin ranking.
pub(super) const CODE_SYMBOLS_MAX_ITEMS: usize = 20;
/// Maximum single-line signature character length kept per entry in `code.symbols`.
pub(super) const CODE_SYMBOLS_MAX_SIG_CHARS: usize = 120;
/// Maximum scoped files queried against the indexed test graph when building `code.tests`.
pub(super) const CODE_TESTS_LENS_SCOPED_FILE_LIMIT: usize = 10;
/// Maximum inbound test entries fetched per scoped file from the indexed test graph in `code.tests`.
pub(super) const CODE_TESTS_LENS_PAGE_SIZE: usize = 25;
/// Maximum inbound graph test entries retained per scoped file before interleaving sibling files in `code.tests`.
pub(super) const CODE_TESTS_PER_FILE_GRAPH_LIMIT: usize = 8;
/// Maximum total unique-path test entries retained in `code.tests` per claim context.
pub(super) const CODE_TESTS_MAX_ITEMS: usize = 8;
/// Maximum external-caller symbol entries (and callers per entry) retained in `code.external_callers` per claim context.
pub(super) const CODE_EXTERNAL_CALLERS_MAX_ITEMS: usize = 5;
/// Maximum candidate call edges rendered in `code.seam` reachability failure diagnostics.
pub(super) const CODE_SEAM_DIAGNOSTIC_EDGE_LIMIT: usize = 10;

/// Default non-tight byte budget used across domain-provider context unit tests.
#[cfg(test)]
pub(super) const TEST_DEFAULT_CONTEXT_MAX_BYTES: usize = 8_192;
/// Tight byte budget asserting deterministic `truncated: true` capping on `paths.instructions`.
#[cfg(test)]
pub(super) const TEST_TIGHT_INSTRUCTIONS_MAX_BYTES: usize = 145;
/// Full 32 KiB context budget used in `code_domain` external-caller unit tests.
#[cfg(test)]
pub(super) const TEST_LARGE_CONTEXT_MAX_BYTES: usize = 32_768;

/// Collapse whitespace into a single line and truncate cleanly at a UTF-8 word boundary within `max_chars`.
pub(super) fn truncate_single_line(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let trimmed = prefix
            .rfind(char::is_whitespace)
            .filter(|&idx| idx * 2 >= prefix.len())
            .map_or(prefix.as_str(), |idx| prefix[..idx].trim_end());
        format!("{trimmed}...")
    } else {
        prefix
    }
}

/// Construct a [`CompositeDomainProvider`] applying `config.modules` enablement flags to `providers`.
pub(super) fn build_composite_providers(
    config: &Adapter,
    providers: Vec<Arc<dyn DomainProvider>>,
) -> Result<Arc<CompositeDomainProvider>, AcddError> {
    let mut flags = BTreeMap::new();
    for provider in &providers {
        let id = provider.domain_id();
        let enabled = if id == SubsystemId::Code.as_str()
            || id == SubsystemId::Docs.as_str()
            || id == SubsystemId::Spec.as_str()
        {
            config.modules.is_enabled(id)
        } else {
            config.modules.is_domain_enabled(id)
        };
        flags.insert(id.to_owned(), enabled);
    }
    let composite = CompositeDomainProvider::with_module_flags(providers, flags)?;
    Ok(Arc::new(composite))
}

/// Resolve the effective `state.sqlite` path for `root`, falling back to the loaded worktree
/// `config.state_db` only when a secondary worktree's primary checkout fails with
/// `WorkflowSchemaInvalid` due to cross-branch `acdd.yaml` schema drift.
pub(crate) fn resolve_effective_state_db_with_fallback(
    root: &Path,
    state_db: Option<&Path>,
    config: &Adapter,
) -> Result<PathBuf, AcddError> {
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    if let Some(path) = acdd_core::config::resolve_custom_db_path(&primary, state_db) {
        return Ok(path);
    }
    match acdd_state::usecases::deps::try_resolve_effective_state_db(root, None) {
        Ok(path) => Ok(path),
        Err(err) if err.code == ErrorCode::WorkflowSchemaInvalid => {
            let root_canon = acdd_core::fs::canonicalize_macos_var(root);
            let primary_canon = acdd_core::fs::canonicalize_macos_var(&primary);
            if primary_canon != root_canon {
                Ok(acdd_core::config::resolve_effective_state_db_path(
                    &primary, None, config,
                ))
            } else {
                Err(err)
            }
        }
        Err(err) => Err(err),
    }
}

/// Construct a [`StateEngineConfig`] from workspace root, active worktree, optional `--state-db` / `--db` overrides, and [`Adapter`].
pub(crate) fn state_engine_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
) -> Result<StateEngineConfig, AcddError> {
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let default_state_db = primary.join(".acdd/state.sqlite");
    let configured = resolve_effective_state_db_with_fallback(root, state_db, config)?;
    let state_db_override = (configured != default_state_db).then_some(configured);
    let linked_workspaces = config
        .linked_workspaces
        .iter()
        .map(|lw| (lw.name.clone(), lw.path.clone()))
        .collect();
    let code_db_path = (config.modules.is_subsystem_enabled(SubsystemId::Code)
        || config.modules.is_subsystem_enabled(SubsystemId::Docs))
    .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    let allowed_binary_extensions = config.limits.normalized_allowed_binary_extensions()?;
    Ok(StateEngineConfig {
        workspace_root: primary,
        worktree: worktree.to_path_buf(),
        state_db_override,
        code_db_path,
        spec_collections: config.specs.clone(),
        workflows_dirs: config.workflows_dirs.clone(),
        linked_workspaces,
        modules: config.modules.clone(),
        response_policy: config.response.clone(),
        retain_traces: config.retain_traces,
        max_snapshot_file_bytes: config.limits.max_snapshot_file_bytes,
        allowed_binary_extensions,
    })
}

pub(crate) fn unsupported_domain_action(
    domain: &str,
    action: &str,
) -> anyhow::Result<DomainActionResult> {
    Err(AcddError::new(
        ErrorCode::DomainCapabilityUnavailable,
        format!("{domain} domain does not expose action '{action}'"),
    )
    .into())
}

/// Execute a mutating [`StateCommand`] against the workspace state engine using caller-supplied
/// [`CompositeDomainProvider`] and optional `--state-db` / `--db` overrides.
pub fn execute_state_command(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
) -> Result<StateCommandOutput, AcddError> {
    execute_state_command_with_config(
        root, worktree, state_db, code_db, config, providers, cmd, false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_state_command_with_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
    explicit_config: bool,
) -> Result<StateCommandOutput, AcddError> {
    if let StateCommand::Snapshot(req) = &cmd {
        acdd_core::fs::check_git_entry_no_symlinks(root)
            .map_err(|e| AcddError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
        let wt = req.worktree.as_deref().unwrap_or(worktree);
        acdd_core::fs::check_git_entry_no_symlinks(wt)
            .map_err(|e| AcddError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    }
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    if let StateCommand::Snapshot(req) = cmd {
        return acdd_state::usecases::execute_snapshot_readonly(&engine_cfg, req);
    }
    let engine = StateEngine::open(engine_cfg, providers)?;
    if explicit_config {
        engine.execute_with_source_config(cmd, config)
    } else {
        engine.execute(cmd)
    }
}

/// Execute a read-only [`StateQuery`] against the workspace state query engine (`SQLITE_OPEN_READ_ONLY`)
/// using caller-supplied [`CompositeDomainProvider`] and optional `--state-db` / `--db` overrides.
pub fn execute_state_query(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    query: StateQuery,
) -> Result<StateQueryOutput, AcddError> {
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    let engine = StateQueryEngine::open_read_only(engine_cfg, providers)?;
    engine.query(query)
}

/// Resolve the configured [`acdd_state::workflow::ProofDef`] for a gate node (`SQLITE_OPEN_READ_ONLY`).
pub(crate) fn resolve_gate_proof(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
    run: Option<&str>,
    node: &str,
) -> Result<acdd_state::workflow::ProofDef, AcddError> {
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    let engine =
        StateQueryEngine::open_read_only(engine_cfg, Arc::new(CompositeDomainProvider::default()))?;
    engine.resolve_gate_proof(run, node)
}

pub(super) fn worktree_code_db_candidate(
    root: &Path,
    worktree: &Path,
    code_db: Option<&Path>,
) -> (PathBuf, PathBuf) {
    let primary = primary_worktree_root(worktree)
        .or_else(|| primary_worktree_root(root))
        .unwrap_or_else(|| root.to_path_buf());
    let wt_override: Option<PathBuf> = code_db.filter(|p| p.is_absolute()).and_then(|p| {
        let canon = acdd_core::fs::canonicalize_macos_var(p);
        canon
            .strip_prefix(acdd_core::fs::canonicalize_macos_var(&primary))
            .or_else(|_| canon.strip_prefix(acdd_core::fs::canonicalize_macos_var(root)))
            .ok()
            .map(Path::to_path_buf)
    });
    let wt_candidate =
        acdd_core::config::resolve_code_db_path(worktree, wt_override.as_deref().or(code_db));
    let primary_candidate = acdd_core::config::resolve_code_db_path(&primary, code_db);
    (wt_candidate, primary_candidate)
}

pub(super) fn open_code_db_readonly(
    root: &Path,
    worktree: &Path,
    code_db: Option<&Path>,
) -> Option<Connection> {
    let (wt_candidate, primary_candidate) = worktree_code_db_candidate(root, worktree, code_db);
    for candidate in [wt_candidate, primary_candidate] {
        if candidate.is_file() {
            if let Ok(conn) = open_raw_confined_readonly(&candidate) {
                return Some(conn);
            }
        }
    }
    None
}

pub(super) fn local_adapter_path_for_worktree(
    root: &Path,
    worktree: &Path,
    fallback: &Adapter,
) -> Option<PathBuf> {
    let abs = fallback.adapter_path.as_deref()?;
    let canon = acdd_core::fs::canonicalize_macos_var(abs);
    let primary = primary_worktree_root(worktree).or_else(|| primary_worktree_root(root));
    let rel = canon
        .strip_prefix(acdd_core::fs::canonicalize_macos_var(worktree))
        .ok()
        .or_else(|| {
            canon
                .strip_prefix(acdd_core::fs::canonicalize_macos_var(root))
                .ok()
        })
        .or_else(|| {
            primary.as_deref().and_then(|p| {
                canon
                    .strip_prefix(acdd_core::fs::canonicalize_macos_var(p))
                    .ok()
            })
        })?;
    acdd_core::fs::checked_child(worktree, rel)
        .ok()
        .filter(|p| p.is_file())
}

pub(crate) fn resolve_worktree_adapter(
    root: &Path,
    worktree: &Path,
    fallback: &Adapter,
) -> anyhow::Result<Adapter> {
    let local_path = local_adapter_path_for_worktree(root, worktree, fallback);
    let has_default = acdd_core::fs::checked_child(worktree, Path::new(ACDD_CONFIG_FILE))
        .is_ok_and(|p| p.is_file());
    if local_path.is_some() || has_default {
        return acdd_core::config::load_adapter(worktree, local_path.as_deref()).map_err(|err| {
            anyhow::anyhow!(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("failed to load worktree adapter: {err}"),
            ))
        });
    }
    Ok(fallback.clone())
}

pub(super) fn combine_file_hashes(file_hashes: &BTreeMap<String, String>) -> String {
    let mut buf = String::new();
    for (path, digest) in file_hashes {
        buf.push_str(path);
        buf.push(':');
        buf.push_str(digest);
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

pub(super) fn collect_spec_selectors<'a>(
    spec: &'a Option<String>,
    specs: &'a Option<Vec<String>>,
) -> Vec<&'a str> {
    let mut selectors = Vec::new();
    if let Some(ref single) = spec {
        selectors.push(single.trim());
    }
    if let Some(ref many) = specs {
        for s in many {
            selectors.push(s.trim());
        }
    }
    selectors
}

pub(super) fn collect_profile_dir_files(
    worktree: &Path,
    dirs: &[String],
    max_depth: usize,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(abs_dir) = checked_child(worktree, Path::new(dir)) else {
            continue;
        };
        if !abs_dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&abs_dir)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|e| !e.file_type().is_symlink())
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() {
                out.push(entry.into_path());
            }
        }
    }
    out
}

pub(super) fn effective_baseline_ref(baseline_ref: Option<&str>) -> &str {
    baseline_ref
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .unwrap_or("HEAD")
}

pub(super) fn collect_git_diff_paths(
    worktree: &Path,
    policy: &WorkspacePathPolicy,
    baseline: &str,
    diff_filter: &str,
    predicate: impl Fn(&str) -> bool,
) -> Vec<String> {
    collect_git_diff_paths_raw(worktree, baseline, diff_filter, |norm| {
        policy.is_candidate_included(norm) && predicate(norm)
    })
}

fn collect_git_diff_paths_raw(
    worktree: &Path,
    baseline: &str,
    diff_filter: &str,
    predicate: impl Fn(&str) -> bool,
) -> Vec<String> {
    if baseline.starts_with('-')
        || (!worktree.join(".git").exists() && git_common_dir(worktree).is_err())
    {
        return Vec::new();
    }
    let Ok(out) = git_command(
        worktree,
        &[
            "diff",
            "--name-only",
            diff_filter,
            "--no-renames",
            "-z",
            baseline,
            "--",
        ],
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut paths = BTreeSet::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let rel = String::from_utf8_lossy(raw);
        if let Some(norm) = normalize_rel_path(&rel) {
            if predicate(&norm) {
                paths.insert(norm);
            }
        }
    }
    paths.into_iter().collect()
}

/// Collect workspace-relative tracked changes against `baseline_ref` (or `HEAD`) plus untracked
/// non-ignored files in `worktree`. Works uniformly for code, docs, workflows, and instructions.
pub(super) fn collect_active_diff_paths(
    worktree: &Path,
    baseline_ref: Option<&str>,
) -> BTreeSet<String> {
    let baseline = effective_baseline_ref(baseline_ref);
    let mut paths: BTreeSet<String> = collect_git_diff_paths_raw(
        worktree,
        baseline,
        "--diff-filter=ACMR",
        |norm| !norm.starts_with(".git/") && !norm.starts_with(".acdd/"),
    )
    .into_iter()
    .collect();
    if worktree.join(".git").exists() || git_common_dir(worktree).is_ok() {
        if let Ok(out) = git_command(worktree, &["ls-files", "--others", "--exclude-standard", "-z"])
        {
            if out.status.success() {
                for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
                    let rel = String::from_utf8_lossy(raw);
                    if let Some(norm) = normalize_rel_path(&rel) {
                        if !norm.starts_with(".git/") && !norm.starts_with(".acdd/") {
                            paths.insert(norm);
                        }
                    }
                }
            }
        }
    }
    paths
}

pub(super) fn is_active_collection_spec_rel(rel: &str, col_cfg: &SpecConfig) -> bool {
    let Some(norm) = normalize_rel_path(rel) else {
        return false;
    };
    norm.ends_with(".md") && col_cfg.matches_rel_path(&norm) && !col_cfg.is_ignored_rel_path(&norm)
}

pub(super) fn compute_specs_digest(
    worktree: &Path,
    adapter: &Adapter,
    params: &Value,
    req: &DomainCheckReq,
    meta_key: &str,
) -> String {
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let mut record_file = |rel: &str| {
        if let Ok(abs) = checked_child(worktree, Path::new(rel)) {
            if fs::symlink_metadata(&abs).is_ok_and(|m| m.is_file()) {
                if let Ok(bytes) = fs::read(&abs) {
                    digests.insert(rel.to_owned(), sha256_hex(&bytes));
                }
            }
        }
    };
    record_file(ACDD_CONFIG_FILE);
    record_file(".acdd/workflows/default.yaml");
    for path in collect_profile_dir_files(worktree, &adapter.workflows_dirs, 3)
        .into_iter()
        .chain(collect_profile_dir_files(worktree, &adapter.specs_dirs, 3))
    {
        if let Ok(rel) = path.strip_prefix(worktree) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            record_file(&rel_str);
        }
    }
    let effective_cols = acdd_spec::effective_spec_collections(&adapter.specs);
    for col in effective_cols.values() {
        let Ok(dirs) = col.resolve_dirs(worktree) else {
            continue;
        };
        for dir in dirs {
            for entry in WalkDir::new(&dir)
                .into_iter()
                .filter_entry(|e| {
                    !e.file_type().is_symlink()
                        && !(e.depth() > 0
                            && e.file_type().is_dir()
                            && e.file_name()
                                .to_str()
                                .is_some_and(|name| col.is_ignored_dir_name(name)))
                })
                .filter_map(Result::ok)
            {
                let Some(name) = entry.file_name().to_str() else {
                    continue;
                };
                if entry.file_type().is_file()
                    && entry.path().extension().and_then(|e| e.to_str()) == Some("md")
                    && !col.is_ignored_entry_name(name)
                {
                    if let Ok(rel) = entry.path().strip_prefix(worktree) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if is_active_collection_spec_rel(&rel_str, col) {
                            record_file(&rel_str);
                        }
                    }
                }
            }
        }
    }
    let meta = json!({
        "params": params,
        "changed_paths": req.changed_paths,
        "admitted_scope": req.admitted_scope,
    });
    digests.insert(
        meta_key.to_owned(),
        sha256_hex(meta.to_string().as_bytes()),
    );
    combine_file_hashes(&digests)
}

pub(super) fn budget_scored_items(kind: &str, items: Vec<(u64, Value)>, max_bytes: usize) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("kind".to_owned(), json!(kind));
    pack_json_lens(envelope, items.into_iter(), max_bytes).0
}

#[cfg(test)]
#[test]
fn budget_items_packs_ordered_candidates_within_budget() {
    use acdd_core::format::estimate_json_bytes;

    let scored_of = |items: Vec<Value>| -> Vec<(u64, Value)> {
        let total = items.len();
        items
            .into_iter()
            .enumerate()
            .map(|(index, item)| (u64::try_from(total - index).unwrap_or(u64::MAX), item))
            .collect()
    };
    let items = vec![json!("a".repeat(100)), json!("é"), json!("b")];
    let expected = json!({"kind":"symbols", "items":["é", "b"], "total":3, "truncated":true});
    let budget = estimate_json_bytes(&expected);
    let result = budget_scored_items("symbols", scored_of(items), budget);
    assert_eq!(result, expected);
    assert_eq!(estimate_json_bytes(&result), budget);
    let complete = json!({"kind":"symbols", "items":["b", "c"], "total":2, "truncated":false});
    let complete_size = estimate_json_bytes(&complete);
    for (budget, expected) in [
        (complete_size, complete),
        (
            complete_size - 1,
            json!({"kind":"symbols", "items":["b"], "total":2, "truncated":true}),
        ),
    ] {
        assert_eq!(
            budget_scored_items(
                "symbols",
                scored_of(vec![json!("b"), json!("c")]),
                budget
            ),
            expected
        );
    }
    let scored = budget_scored_items(
        "symbols",
        vec![
            (10, json!("low-value-wide")),
            (500, json!("hi-1")),
            (500, json!("hi-2")),
        ],
        estimate_json_bytes(
            &json!({"kind":"symbols","items":["hi-1","hi-2"],"total":3,"truncated":true}),
        ),
    );
    assert_eq!(scored["items"], json!(["hi-1", "hi-2"]));
    assert_eq!(scored["truncated"], true);
    let empty = budget_scored_items("symbols", scored_of(vec![json!("oversized")]), 0);
    assert_eq!(empty["items"], json!([]));
    assert_eq!(empty["truncated"], true);
}
