use super::super::common::{
    combine_file_hashes, local_adapter_path_for_worktree, resolve_worktree_adapter,
    worktree_code_db_candidate,
};
use super::common::{collect_scoped_worktree_files, for_each_candidate_file, CodeDomainProvider};
use acdd_core::config::{Adapter, ACDD_CONFIG_FILE};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::{checked_child, is_doc_path};
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::path_policy::normalize_rel_path;
use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, FreshnessObservation};
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_index::db::{lint as db_lint, reader};
use acdd_index::languages::{self, dependency_registry::DependencyRegistry};
use acdd_index::scanner::{self, ScanReport};
use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LintCheckParams {
    categories: Option<Vec<LintCategory>>,
    severity: Option<LintSeverity>,
    rules: Option<Vec<String>>,
    target: Option<String>,
}

fn parse_lint_check_params(params: &Value) -> Result<LintCheckParams> {
    if params.is_null() {
        return Ok(LintCheckParams::default());
    }
    serde_json::from_value(params.clone()).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid code.lint parameters: {err}"),
        )
        .into()
    })
}

fn resolve_lint_scope_paths(req: &DomainCheckReq) -> Option<Vec<String>> {
    if req.admitted_scope.is_empty() {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for entry in req.admitted_scope.iter().chain(&req.changed_paths) {
        let trimmed = entry.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_owned()) {
            paths.push(trimmed.to_owned());
        }
    }
    (!paths.is_empty()).then_some(paths)
}

fn hash_file_or_marker(path: &Path, missing_marker: &str) -> String {
    match fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => missing_marker.to_owned(),
    }
}

fn compute_lint_live_digest(
    root: &Path,
    req: &DomainCheckReq,
    worktree_adapter: &Adapter,
    scoped_paths: Option<&[String]>,
) -> Result<String> {
    let mut file_hashes: BTreeMap<String, String> = BTreeMap::new();

    if let Ok(cfg_path) = checked_child(&req.worktree, Path::new(ACDD_CONFIG_FILE)) {
        file_hashes.insert(
            format!("__adapter__:{ACDD_CONFIG_FILE}"),
            hash_file_or_marker(&cfg_path, "<missing>"),
        );
    }
    if let Some(explicit) = local_adapter_path_for_worktree(root, &req.worktree, worktree_adapter) {
        file_hashes.insert(
            format!("__adapter_explicit__:{}", explicit.display()),
            hash_file_or_marker(&explicit, "<missing>"),
        );
    }

    let include_all_sources = scoped_paths.is_none();
    for_each_candidate_file(&req.worktree, &req.path_policy, true, |abs, rel| {
        if is_doc_path(&rel) {
            if !rel.starts_with("archive/") && !rel.contains("/archive/") {
                file_hashes.insert(rel, hash_file_or_marker(abs, "<missing>"));
            }
        } else if include_all_sources && languages::for_path(Path::new(&rel)).is_some() {
            file_hashes.insert(rel, hash_file_or_marker(abs, "<missing>"));
        }
    });

    if let Some(scope) = scoped_paths {
        for rel in collect_scoped_worktree_files(&req.worktree, scope, &req.path_policy) {
            if is_doc_path(&rel) || languages::for_path(Path::new(&rel)).is_none() {
                continue;
            }
            if let Ok(abs) = checked_child(&req.worktree, Path::new(&rel)) {
                file_hashes.insert(rel, hash_file_or_marker(&abs, "<deleted>"));
            }
        }
        for rel in &req.changed_paths {
            let Some(norm) = normalize_rel_path(rel) else {
                continue;
            };
            if is_doc_path(&norm) || languages::for_path(Path::new(&norm)).is_none() {
                continue;
            }
            let hash = checked_child(&req.worktree, Path::new(&norm))
                .ok()
                .filter(|abs| abs.is_file())
                .map_or_else(
                    || "<deleted>".to_owned(),
                    |abs| hash_file_or_marker(&abs, "<deleted>"),
                );
            file_hashes.insert(norm, hash);
        }
    }

    Ok(combine_file_hashes(&file_hashes))
}

fn rebuild_index(
    worktree: &Path,
    db: &Path,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<()> {
    match cache_dir {
        Some(dir) => {
            crate::engine::pipeline::build_with_cache_dir(worktree, db, adapter_path, dir)?;
        }
        None => {
            crate::engine::pipeline::build(worktree, db, adapter_path)?;
        }
    }
    Ok(())
}

/// Ensures `db` reflects the live workspace files and adapter configuration, using delta updates when valid.
pub(crate) fn ensure_fresh_worktree_index(
    worktree: &Path,
    db: &Path,
    adapter: &Adapter,
    adapter_path: Option<&Path>,
    cache_dir: Option<&Path>,
    pre_scan: Option<&ScanReport>,
) -> Result<()> {
    crate::engine::pipeline::validate_code_map_target(worktree, db)?;
    let conn = match reader::open_locked(db, worktree) {
        Ok(conn) => conn,
        Err(err)
            if err.downcast_ref::<reader::IndexRebuildRequired>().is_some() || !db.exists() =>
        {
            return rebuild_index(worktree, db, adapter_path, cache_dir);
        }
        Err(err) => return Err(err),
    };

    let persisted = reader::read_policy(worktree, &conn)?;
    if persisted.digest != adapter.digest
        || persisted.adapter_path != adapter.adapter_path
        || persisted.linked_workspaces != adapter.linked_workspaces
    {
        drop(conn);
        return rebuild_index(worktree, db, adapter_path, cache_dir);
    }

    let previous = reader::inventory_snapshot(&conn)?;
    let owned_scan;
    let scan = match pre_scan {
        Some(s) => s,
        None => {
            let watched: BTreeSet<String> = previous.iter().map(|e| e.path.clone()).collect();
            owned_scan = scanner::scan_reusing_watching(worktree, adapter, &previous, &watched)?;
            &owned_scan
        }
    };

    let manifests = DependencyRegistry::try_collect_with_paths(
        worktree,
        Some(adapter),
        &scan.dependency_manifests,
    )?;
    let previous_manifest_digest: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='manifest_digest'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if previous_manifest_digest.as_deref() != Some(manifests.digest()) {
        drop(conn);
        return rebuild_index(worktree, db, adapter_path, cache_dir);
    }

    let prev_map: BTreeMap<&str, _> = previous.iter().map(|e| (e.path.as_str(), e)).collect();
    let curr_map: BTreeMap<&str, _> = scan.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let changed: BTreeSet<&str> = prev_map
        .keys()
        .chain(curr_map.keys())
        .filter(|path| match (prev_map.get(**path), curr_map.get(**path)) {
            (Some(old), Some(new)) => {
                old.digest != new.digest || old.language != new.language || old.is_doc != new.is_doc
            }
            _ => true,
        })
        .copied()
        .collect();
    drop(conn);

    if changed.is_empty() {
        return Ok(());
    }

    let has_excluded = changed
        .iter()
        .any(|p| !curr_map.contains_key(p) && scan.matched_excluded.contains(*p));
    let exact_unresolved = scanner::exact_paths_exist(
        worktree,
        adapter,
        changed
            .iter()
            .copied()
            .filter(|p| !curr_map.contains_key(p) && !scan.matched_excluded.contains(*p)),
    )?;
    if has_excluded || !exact_unresolved.is_empty() {
        return rebuild_index(worktree, db, adapter_path, cache_dir);
    }

    let modified: Vec<PathBuf> = changed.into_iter().map(PathBuf::from).collect();
    match crate::engine::pipeline::delta_with_selection(
        worktree,
        db,
        &modified,
        adapter_path,
        cache_dir,
    ) {
        Ok(_) => Ok(()),
        Err(err) if err.is::<crate::engine::pipeline::SourceSnapshotMismatch>() => {
            rebuild_index(worktree, db, adapter_path, cache_dir)
        }
        Err(err) => Err(err),
    }
}

impl CodeDomainProvider {
    pub(super) fn observe_lint_freshness(
        &self,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        let _ = parse_lint_check_params(params)?;
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let scoped_paths = resolve_lint_scope_paths(req);
        let (wt_db, _) =
            worktree_code_db_candidate(&self.root, &req.worktree, self.code_db.as_deref());
        let index_gen = if wt_db.is_file() {
            let conn = reader::open_raw_confined_readonly(&wt_db)?;
            Self::index_generation(Some(&conn))
        } else {
            "no-index".to_owned()
        };
        let live_digest =
            compute_lint_live_digest(&self.root, req, &worktree_adapter, scoped_paths.as_deref())?;
        Ok(FreshnessObservation::Passed {
            index_generation: Some(index_gen.clone()),
            execution_digest_salt: Some(format!(
                "index_generation={index_gen};live_files_digest={live_digest}"
            )),
        })
    }

    pub(super) fn run_lint_check(
        &self,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        let parsed = parse_lint_check_params(params)?;
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let local_adapter_path =
            local_adapter_path_for_worktree(&self.root, &req.worktree, &self.config);
        let scoped_paths = resolve_lint_scope_paths(req);
        let live_digest =
            compute_lint_live_digest(&self.root, req, &worktree_adapter, scoped_paths.as_deref())?;

        let (wt_db, _) =
            worktree_code_db_candidate(&self.root, &req.worktree, self.code_db.as_deref());
        let pre_scan = if !wt_db.is_file() {
            let scan = scanner::scan_with_adapter(&req.worktree, &worktree_adapter)?;
            if scan.entries.is_empty() {
                return Ok(DomainCheckOutcome::Passed {
                    summary: "code.lint: no indexable files in worktree".to_owned(),
                    execution_digest_salt: Some(format!(
                        "index_generation=no-index;live_files_digest={live_digest}"
                    )),
                });
            }
            Some(scan)
        } else {
            None
        };
        ensure_fresh_worktree_index(
            &req.worktree,
            &wt_db,
            &worktree_adapter,
            local_adapter_path.as_deref(),
            None,
            pre_scan.as_ref(),
        )?;

        let conn = reader::open_locked(&wt_db, &req.worktree)?;
        let index_gen = Self::index_generation(Some(&conn));
        let page = QueryOptions::resolve(
            &ResponsePolicy::default(),
            Some(100),
            0,
            Some(Detail::Full),
            None,
        )?;
        let target = parsed
            .target
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let rule_ids = parsed.rules.as_deref().filter(|r| !r.is_empty());
        let lint_req = db_lint::LintRequest {
            target,
            paths: scoped_paths.as_deref(),
            categories: parsed.categories.as_deref(),
            rule_ids,
            minimum_severity: parsed.severity,
            architecture: Some(&worktree_adapter.architecture),
            workspace_root: Some(&req.worktree),
            page: &page,
        };
        let report = db_lint::run_lint(&conn, &lint_req)?;
        let errors = report
            .pointer("/summary/errors")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let warnings = report
            .pointer("/summary/warnings")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let rules_evaluated = report
            .pointer("/summary/rules_evaluated")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        if errors == 0 {
            return Ok(DomainCheckOutcome::Passed {
                summary: format!(
                    "code.lint passed ({warnings} warning(s), {rules_evaluated} rule(s) evaluated)"
                ),
                execution_digest_salt: Some(format!(
                    "index_generation={index_gen};live_files_digest={live_digest}"
                )),
            });
        }

        let diagnostics: Vec<String> = report
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("severity").and_then(Value::as_str) == Some("error"))
                    .map(|item| {
                        let rule_id = item.get("rule_id").and_then(Value::as_str).unwrap_or("");
                        let path = item.get("path").and_then(Value::as_str).unwrap_or("");
                        let line = item
                            .get("line")
                            .and_then(Value::as_u64)
                            .map(|l| format!(":{l}"))
                            .unwrap_or_default();
                        let message = item.get("message").and_then(Value::as_str).unwrap_or("");
                        format!("[{rule_id}] {path}{line}: {message}")
                    })
                    .collect()
            })
            .unwrap_or_default();

        let detail = diagnostics
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let summary = if detail.is_empty() {
            format!("code.lint failed with {errors} error(s) and {warnings} warning(s)")
        } else {
            format!("code.lint failed with {errors} error(s) and {warnings} warning(s): {detail}")
        };
        Ok(DomainCheckOutcome::Failed {
            summary,
            diagnostics,
        })
    }
}
