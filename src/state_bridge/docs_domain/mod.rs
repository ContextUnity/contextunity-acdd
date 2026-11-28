//! Host `DocsDomainProvider` (`domain_id = "docs"`) implementation.

mod invariants;
mod ranking;
mod specs_check;
#[cfg(test)]
mod tests;
mod walk;

use self::invariants::{
    collect_configured_enforced_ids, collect_fallback_db_invariants, collect_filesystem_invariants,
    collect_scoped_db_invariants,
};
use self::ranking::{
    any_glob_matches, build_doc_id_tokens, classify_path_candidate, first_markdown_heading,
    is_archived_doc_rel, is_global_wildcard, parse_frontmatter_metadata, push_json_scope_values,
    rank_and_budget_candidates, RankedDocCandidate,
};
use self::specs_check::{
    check_required_specs, docs_specs_input_schema, parse_docs_specs_params,
    resolve_target_spec_collections,
};
use self::walk::{collect_scoped_agents_md, compute_docs_digest};
use super::common::{
    budget_scored_items, collect_profile_dir_files, combine_file_hashes, compute_specs_digest,
    open_code_db_readonly, resolve_worktree_adapter, unsupported_domain_action,
    DOCS_INSTRUCTIONS_DIRECTIVE,
};
use acdd_core::config::Adapter;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::format::estimate_json_bytes;
use acdd_core::fs::is_doc_path;
use acdd_core::path_policy::expand_glob_braces;
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
};
use acdd_core::schema::SchemaDef;
use acdd_core::subsystems::SubsystemId;
use acdd_spec::SpecStatus;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

enum LinksScope {
    NoDocsInScope,
    FullRepo,
    Scoped(Vec<String>),
}

/// Host `docs` domain provider exposing `paths.<name>`, `specs.<name>`, `docs.links`, `docs.specs`, `docs.invariants`, and `docs.workspace_config`.
pub struct DocsDomainProvider {
    root: PathBuf,
    config: Adapter,
    code_db: Option<PathBuf>,
}

impl DocsDomainProvider {
    /// Construct a `DocsDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self::with_code_db(root, config, None)
    }

    /// Construct a `DocsDomainProvider` bound to `root`, workspace `config`, and an optional explicit `code_db` path.
    pub fn with_code_db(root: PathBuf, config: Adapter, code_db: Option<PathBuf>) -> Self {
        Self {
            root,
            config,
            code_db,
        }
    }

    fn resolve_links_scope(&self, req: &DomainCheckReq) -> LinksScope {
        let mut scoped_docs: Vec<String> = Vec::new();
        for rel in req.admitted_scope.iter().chain(&req.changed_paths) {
            let expanded = expand_glob_braces(rel);
            let matches_docs = expanded.iter().any(|v| {
                is_doc_path(v)
                    || v.contains('*')
                    || v.contains('?')
                    || v.contains('[')
                    || req.worktree.join(v).is_dir()
            });
            if matches_docs {
                scoped_docs.push(rel.clone());
            }
        }
        if !req.admitted_scope.is_empty() && scoped_docs.is_empty() {
            LinksScope::NoDocsInScope
        } else if scoped_docs.is_empty() {
            LinksScope::FullRepo
        } else {
            LinksScope::Scoped(scoped_docs)
        }
    }

    fn compute_scoped_docs_digest(
        &self,
        req: &DomainCheckReq,
        scope: &LinksScope,
    ) -> Result<String> {
        match scope {
            LinksScope::NoDocsInScope => Ok(combine_file_hashes(&BTreeMap::new())),
            LinksScope::FullRepo => compute_docs_digest(&req.worktree, None, &req.path_policy),
            LinksScope::Scoped(patterns) => {
                compute_docs_digest(&req.worktree, Some(patterns.as_slice()), &req.path_policy)
            }
        }
    }

    fn run_links_check(&self, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let scope = self.resolve_links_scope(req);
        let live_digest = self.compute_scoped_docs_digest(req, &scope)?;
        let (checked_count, errors) = match &scope {
            LinksScope::NoDocsInScope => {
                return Ok(DomainCheckOutcome::Passed {
                    summary: "docs.links: no markdown files in admitted_scope".to_owned(),
                    execution_digest_salt: Some(format!("docs_live={live_digest}")),
                });
            }
            LinksScope::FullRepo => {
                crate::engine::docs_check::collect_link_errors(&req.worktree, None)?
            }
            LinksScope::Scoped(patterns) => crate::engine::docs_check::collect_link_errors(
                &req.worktree,
                Some(patterns.as_slice()),
            )?,
        };

        if errors.is_empty() {
            Ok(DomainCheckOutcome::Passed {
                summary: format!("docs.links verified {checked_count} markdown file(s)"),
                execution_digest_salt: Some(format!("docs_live={live_digest}")),
            })
        } else {
            Ok(DomainCheckOutcome::Failed {
                summary: format!("docs.links found {} broken link/anchor(s)", errors.len()),
                diagnostics: errors,
            })
        }
    }

    fn run_specs_check(&self, params: &Value, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let parsed = parse_docs_specs_params(params)?;
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let effective_cols = acdd_spec::effective_spec_collections(&worktree_adapter.specs);
        let target_cols = resolve_target_spec_collections(&parsed, &effective_cols)?;
        let live_digest = compute_specs_digest(
            &req.worktree,
            &worktree_adapter,
            params,
            req,
            "__docs_specs_meta__",
        );
        let mut diagnostics = Vec::new();
        let (scanned, _scan_failed) = acdd_spec::validate::scan_collection_specs(
            &req.worktree,
            &worktree_adapter,
            &mut diagnostics,
        );
        let active_docs: Vec<acdd_spec::SpecDocument> =
            scanned.iter().map(|s| s.doc.clone()).collect();
        let mut warnings = Vec::new();
        for item in &scanned {
            acdd_spec::validate::validate_spec_static_checks(
                &req.worktree,
                &worktree_adapter,
                &acdd_spec::overlay::NoopRuntimeOverlay,
                item.col.as_ref(),
                &item.rel_path,
                &item.doc,
                &active_docs,
                true,
                &mut diagnostics,
                &mut warnings,
            );
        }
        let failures: Vec<String> = diagnostics
            .iter()
            .filter(|d| d.status == "fail")
            .map(|d| format!("[{}] {}: {}", d.code, d.path, d.message))
            .collect();
        if !failures.is_empty() {
            return Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "docs.specs found {} validation failure(s)",
                    failures.len()
                ),
                diagnostics: failures,
            });
        }
        if parsed.required {
            if let Some(diag) =
                check_required_specs(&req.worktree, req, &target_cols, &effective_cols)
            {
                return Ok(DomainCheckOutcome::Failed {
                    summary: diag.clone(),
                    diagnostics: vec![diag],
                });
            }
        }
        Ok(DomainCheckOutcome::Passed {
            summary: format!("docs.specs validated {} document(s)", scanned.len()),
            execution_digest_salt: Some(format!("specs_live={live_digest}")),
        })
    }

    fn provide_paths_lens(&self, name: &str, req: &DomainContextReq) -> Result<Value> {
        let owner_agents = if name == "instructions" {
            collect_scoped_agents_md(&req.worktree, &req.admitted_scope, &req.path_policy)
        } else {
            BTreeMap::new()
        };
        let matched = walk::collect_matching_worktree_files_for_globs(
            &req.worktree,
            &req.lens_globs,
            |rel| {
                !is_archived_doc_rel(rel)
                    && (name != "adrs" || rel != "docs/adr/README.md")
                    && any_glob_matches(&req.lens_globs, rel)
            },
        );
        let mut candidates = Vec::with_capacity(matched.len());
        for (_, rel) in matched {
            let Ok(text) =
                acdd_core::fs::read_confined_utf8(&req.worktree, std::path::Path::new(&rel))
            else {
                continue;
            };
            let meta = parse_frontmatter_metadata(&text);
            let title = meta
                .title
                .clone()
                .or_else(|| first_markdown_heading(&text))
                .unwrap_or_else(|| rel.clone());
            let (explicit_scopes, is_global_fallback, priority_bonus) =
                classify_path_candidate(&rel, &meta.scopes, &owner_agents);
            let id_tokens = build_doc_id_tokens(&rel, meta.id.as_deref());
            let payload = if name == "instructions" {
                json!({ "path": rel })
            } else {
                json!({ "title": title, "path": rel })
            };
            candidates.push(RankedDocCandidate {
                rel_path: rel,
                explicit_scopes,
                is_global_fallback,
                id_tokens,
                title: (name != "instructions").then_some(title),
                priority_bonus,
                payload,
            });
        }
        let directive_bytes = if name == "instructions" {
            ",\"directive\":".len() + estimate_json_bytes(&DOCS_INSTRUCTIONS_DIRECTIVE)
        } else {
            0
        };
        let item_budget = req.max_bytes.saturating_sub(directive_bytes);
        let mut budgeted = rank_and_budget_candidates(
            &self.root,
            self.code_db.as_deref(),
            name,
            candidates,
            req,
            item_budget,
        )?;
        if name == "instructions" {
            budgeted["directive"] = json!(DOCS_INSTRUCTIONS_DIRECTIVE);
        }
        Ok(budgeted)
    }

    fn provide_specs_lens(&self, name: &str, req: &DomainContextReq) -> Result<Value> {
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let effective_cols = acdd_spec::effective_spec_collections(&worktree_adapter.specs);
        if !effective_cols.contains_key(name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("unknown spec collection 'specs.{name}'"),
            )
            .into());
        }
        let col_globs: Vec<String> = effective_cols
            .values()
            .flat_map(|cfg| cfg.patterns().into_iter().map(str::to_owned))
            .collect();
        let matched = walk::collect_matching_worktree_files_for_globs(
            &req.worktree,
            &col_globs,
            |rel| {
                is_doc_path(rel)
                    && !is_archived_doc_rel(rel)
                    && effective_cols
                        .values()
                        .any(|cfg| super::common::is_active_collection_spec_rel(rel, cfg))
            },
        );
        let mut candidates = Vec::with_capacity(matched.len());
        for (_, rel) in matched {
            let Ok(text) =
                acdd_core::fs::read_confined_utf8(&req.worktree, std::path::Path::new(&rel))
            else {
                continue;
            };
            let owning_col_key = acdd_spec::match_spec_collection(&effective_cols, &rel);
            let owning_col = owning_col_key
                .as_deref()
                .and_then(|k| effective_cols.get(k).map(|cfg| (k, cfg)));
            let Ok(doc) = acdd_spec::parse_spec_with_collections(&text, owning_col, &effective_cols)
            else {
                continue;
            };
            let belongs = acdd_spec::schema::spec_matches_selector(
                doc.spec.as_deref(),
                owning_col_key.as_deref(),
                name,
                &effective_cols,
            );
            if !belongs || doc.status == SpecStatus::Cancelled {
                continue;
            }
            let meta = parse_frontmatter_metadata(&text);
            let mut doc_scopes = meta.scopes;
            for owner in &doc.owners {
                if !doc_scopes.contains(owner) {
                    doc_scopes.push(owner.clone());
                }
            }
            for task in &doc.tasks {
                for scope in &task.scope {
                    if !doc_scopes.contains(scope) {
                        doc_scopes.push(scope.clone());
                    }
                }
            }
            let title = if doc.title.trim().is_empty() {
                rel.clone()
            } else {
                doc.title.clone()
            };
            if doc.items.is_empty() {
                let is_global_fallback =
                    doc_scopes.is_empty() || doc_scopes.iter().any(|s| is_global_wildcard(s));
                let id_tokens = build_doc_id_tokens(&rel, Some(&doc.id));
                candidates.push(RankedDocCandidate {
                    rel_path: rel.clone(),
                    explicit_scopes: doc_scopes,
                    is_global_fallback,
                    id_tokens,
                    title: Some(title.clone()),
                    priority_bonus: 0,
                    payload: json!({
                        "spec_id": doc.id,
                        "id": doc.id,
                        "title": title,
                        "path": rel,
                        "status": doc.status,
                    }),
                });
            } else {
                for items_vec in doc.items.values() {
                    for spec_item in items_vec {
                        if spec_item.status == SpecStatus::Cancelled {
                            continue;
                        }
                        let mut item_scopes = Vec::new();
                        push_json_scope_values(
                            spec_item.fields.get("scope"),
                            &mut item_scopes,
                        );
                        push_json_scope_values(
                            spec_item.fields.get("owners"),
                            &mut item_scopes,
                        );
                        let explicit_scopes = if item_scopes.is_empty() {
                            doc_scopes.clone()
                        } else {
                            item_scopes
                        };
                        let is_global_fallback = explicit_scopes.is_empty()
                            || explicit_scopes.iter().any(|s| is_global_wildcard(s));
                        let item_ref = spec_item.item_ref.as_str();
                        let full_ref = format!("{}#{item_ref}", doc.id);
                        let mut id_tokens = build_doc_id_tokens(&rel, Some(&doc.id));
                        if !id_tokens.iter().any(|t| t == &full_ref) {
                            id_tokens.push(full_ref);
                        }
                        if !id_tokens.iter().any(|t| t == item_ref) {
                            id_tokens.push(item_ref.to_owned());
                        }
                        let summary = spec_item
                            .fields
                            .get("statement")
                            .or_else(|| spec_item.fields.get("target"))
                            .or_else(|| spec_item.fields.get("title"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(title.as_str())
                            .to_owned();
                        candidates.push(RankedDocCandidate {
                            rel_path: rel.clone(),
                            explicit_scopes,
                            is_global_fallback,
                            id_tokens,
                            title: Some(summary.clone()),
                            priority_bonus: 0,
                            payload: json!({
                                "spec_id": doc.id,
                                "id": doc.id,
                                "ref": item_ref,
                                "item": spec_item.item,
                                "title": title,
                                "statement": summary,
                                "path": rel,
                                "status": spec_item.status,
                            }),
                        });
                    }
                }
            }
        }
        rank_and_budget_candidates(
            &self.root,
            self.code_db.as_deref(),
            name,
            candidates,
            req,
            req.max_bytes,
        )
    }

    fn provide_invariants(&self, req: &DomainContextReq) -> Result<Value> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let effective_cols = acdd_spec::effective_spec_collections(&worktree_adapter.specs);
        let default_docs_globs = [
            "docs/**".to_owned(),
            "AGENTS.md".to_owned(),
            "README.md".to_owned(),
        ];
        let docs_globs: &[String] = if req.lens_globs.is_empty() {
            &default_docs_globs
        } else {
            &req.lens_globs
        };
        let mut enforced_ids: BTreeSet<String> = BTreeSet::new();

        if let Some(conn) =
            open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref())
        {
            enforced_ids = acdd_index::db::lint::enforced_invariant_ids(
                &conn,
                &worktree_adapter.architecture.rules,
            )
            .unwrap_or_default();
            if !req.admitted_scope.is_empty() {
                collect_scoped_db_invariants(
                    &conn,
                    &req.admitted_scope,
                    docs_globs,
                    &effective_cols,
                    &enforced_ids,
                    &mut seen,
                    &mut items,
                );
            }
            if req.admitted_scope.is_empty() || items.is_empty() {
                collect_fallback_db_invariants(
                    &conn,
                    docs_globs,
                    &effective_cols,
                    &enforced_ids,
                    &mut seen,
                    &mut items,
                )?;
            }
        } else {
            collect_configured_enforced_ids(&worktree_adapter, &mut enforced_ids);
        }

        if items.is_empty() {
            collect_filesystem_invariants(
                &req.worktree,
                docs_globs,
                &effective_cols,
                &enforced_ids,
                &mut seen,
                &mut items,
            );
        }

        let total = items.len();
        let scored = items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| {
                let scoped =
                    u64::from(item.get("scope_match").and_then(Value::as_bool) == Some(true))
                        * 4_000;
                let enforced =
                    u64::from(item.get("enforced").and_then(Value::as_bool) == Some(true)) * 2_000;
                (
                    500 + scoped + enforced + u64::try_from(total - idx).unwrap_or(1),
                    item,
                )
            })
            .collect();
        Ok(budget_scored_items("invariants", scored, req.max_bytes))
    }

    fn provide_workspace_config(&self, req: &DomainContextReq) -> Result<Value> {
        use acdd_state::store::sqlite::SqliteStateReader;
        use acdd_state::store::traits::StateReader;
        use acdd_state::usecases::lifecycle::next_spec_id_for_prefix;

        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let state_db_path =
            super::common::resolve_effective_state_db_with_fallback(&req.worktree, None, &worktree_adapter)
                .ok()
                .filter(|p| p.is_file());
        let reader = state_db_path.as_deref().map(SqliteStateReader::new);

        let mut specs_json = serde_json::Map::new();
        for (col_name, col_cfg) in &worktree_adapter.specs {
            let mut next_ids = serde_json::Map::new();
            for prefix in &col_cfg.prefixes {
                let db_max = reader
                    .as_ref()
                    .and_then(|r| r.max_spec_number(col_name, prefix).ok().flatten());
                if let Ok(next_id) =
                    next_spec_id_for_prefix(&req.worktree, col_name, col_cfg, prefix, db_max)
                {
                    next_ids.insert(prefix.clone(), json!(next_id));
                }
            }
            specs_json.insert(
                col_name.clone(),
                json!({
                    "base": col_cfg.effective_builtin_base(Some(col_name)),
                    "roadmap": col_cfg.effective_roadmap(Some(col_name)),
                    "patterns": col_cfg.patterns(),
                    "default_draft_dir": col_cfg.default_draft_dir(),
                    "prefixes": col_cfg.prefixes,
                    "digits": col_cfg.digits,
                    "workflow": col_cfg.workflow,
                    "next_ids": next_ids,
                }),
            );
        }

        let mut available_workflows: BTreeSet<String> = ["base", "code", "documentation"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for p in collect_profile_dir_files(&req.worktree, &worktree_adapter.workflows_dirs, 2) {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "yaml" | "yml") {
                if let Ok(rel) = p.strip_prefix(&req.worktree) {
                    available_workflows.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }

        let mut payload = json!({
            "specs": specs_json,
            "docs_roots": worktree_adapter.docs,
            "workflows_dirs": worktree_adapter.workflows_dirs,
            "available_workflows": available_workflows,
            "architecture_zones": worktree_adapter.architecture.zones,
            "limits": {
                "max_metadata_bytes": worktree_adapter.limits.max_metadata_bytes,
            },
            "modules": {
                "preset": worktree_adapter.modules.preset,
                "enabled": worktree_adapter.modules.enabled,
                "disabled": worktree_adapter.modules.disabled,
                "domains": worktree_adapter.modules.domains,
            },
        });
        if estimate_json_bytes(&payload) > req.max_bytes {
            if let Some(obj) = payload.as_object_mut() {
                obj.remove("architecture_zones");
            }
        }
        Ok(payload)
    }
}

impl DomainProvider for DocsDomainProvider {
    fn domain_id(&self) -> &str {
        SubsystemId::Docs.as_str()
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: Vec::new(),
            checks: vec![
                CheckCapability {
                    name: "links".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "specs".to_owned(),
                    input_schema: docs_specs_input_schema(),
                },
            ],
            context_lenses: BTreeSet::from([
                "invariants".to_owned(),
                "workspace_config".to_owned(),
            ]),
            tools: Vec::new(),
        }
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        unsupported_domain_action(SubsystemId::Docs.as_str(), action)
    }

    fn run_check(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "links" => self.run_links_check(req),
            "specs" => self.run_specs_check(params, req),
            _ => Err(unknown_docs_check(check)),
        }
    }

    fn observe_freshness(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        match check {
            "links" => {
                let scope = self.resolve_links_scope(req);
                let docs_digest = self.compute_scoped_docs_digest(req, &scope)?;
                Ok(FreshnessObservation::Passed {
                    index_generation: None,
                    execution_digest_salt: Some(format!("docs_live={docs_digest}")),
                })
            }
            "specs" => {
                let parsed = parse_docs_specs_params(params)?;
                let worktree_adapter =
                    resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
                let effective_cols =
                    acdd_spec::effective_spec_collections(&worktree_adapter.specs);
                let _ = resolve_target_spec_collections(&parsed, &effective_cols)?;
                let digest = compute_specs_digest(
                    &req.worktree,
                    &worktree_adapter,
                    params,
                    req,
                    "__docs_specs_meta__",
                );
                Ok(FreshnessObservation::Passed {
                    index_generation: None,
                    execution_digest_salt: Some(format!("specs_live={digest}")),
                })
            }
            _ => Err(unknown_docs_check(check)),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        if let Some(name) = lens.strip_prefix("paths.") {
            return self.provide_paths_lens(name, req);
        }
        if let Some(name) = lens.strip_prefix("specs.") {
            return self.provide_specs_lens(name, req);
        }
        match lens {
            "invariants" => self.provide_invariants(req),
            "workspace_config" => self.provide_workspace_config(req),
            "instructions" => Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "legacy context lens 'docs.instructions' was removed; use 'paths.instructions'",
            )
            .into()),
            "adrs" => Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "legacy context lens 'docs.adrs' was removed; use 'paths.adrs'",
            )
            .into()),
            _ => Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown docs context lens 'docs.{lens}'"),
            )
            .into()),
        }
    }
}

fn unknown_docs_check(check: &str) -> anyhow::Error {
    AcddError::new(
        ErrorCode::DomainCapabilityUnavailable,
        format!("unknown docs check 'docs.{check}'"),
    )
    .into()
}
