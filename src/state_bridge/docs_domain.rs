//! Host `DocsDomainProvider` (`domain_id = "docs"`) implementation.

use super::{budget_items, combine_file_hashes, open_code_db_readonly};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, sha256_hex};
use acdd_core::fs::{checked_child, is_doc_path};
use acdd_core::path_policy::{
    expand_glob_braces, glob_matches, normalize_rel_path, scopes_overlap, WorkspacePathPolicy,
};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
};
use acdd_core::schema::SchemaDef;
use acdd_core::subsystems::SubsystemId;
use acdd_index::db::reader;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

enum LinksScope {
    NoDocsInScope,
    FullRepo,
    Scoped(Vec<String>),
}

/// Host `docs` domain provider exposing `docs.links`, `docs.specs`, `docs.adrs`, `docs.instructions`, `docs.invariants`, and `docs.workspace_config`.
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
            LinksScope::FullRepo => crate::engine::docs_check::collect_link_errors(&req.worktree, None)?,
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

    fn run_specs_check(&self, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let worktree_adapter =
            super::resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let live_digest = compute_specs_digest(&req.worktree, &worktree_adapter);
        let providers = super::build_domain_providers(
            &req.worktree,
            &worktree_adapter,
            self.code_db.as_deref(),
            Vec::new(),
        )?;
        let resp =
            acdd_state::validate::validate_workspace(&req.worktree, None, None, &providers)?;
        if resp.valid {
            Ok(DomainCheckOutcome::Passed {
                summary: format!(
                    "docs.specs validated {} document(s)",
                    resp.summary.documents_validated
                ),
                execution_digest_salt: Some(format!("specs_live={live_digest}")),
            })
        } else {
            let diagnostics: Vec<String> = resp
                .diagnostics
                .iter()
                .filter(|d| d.status == "fail")
                .map(|d| format!("[{}] {}:{} {}", d.code, d.path, d.line, d.message))
                .collect();
            Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "docs.specs found {} validation failure(s)",
                    diagnostics.len()
                ),
                diagnostics,
            })
        }
    }

    fn collect_adr_records(&self, req: &DomainContextReq) -> Result<Vec<(String, String, String)>> {
        let mut items: Vec<(String, String, String)> = Vec::new();
        let mut seen_paths = BTreeSet::new();

        // Always scan live worktree `docs/adr` so unindexed/edited ADRs appear immediately
        // and stale `code_db` rows for existing files cannot override live frontmatter/doc_type.
        let adr_dir = req.worktree.join("docs/adr");
        if adr_dir.is_dir() {
            for entry in WalkDir::new(&adr_dir)
                .max_depth(2)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry.file_type().is_dir()
                        || entry.file_name().to_str() != Some("archive")
                })
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if !is_doc_path(&path.to_string_lossy()) {
                    continue;
                }
                let rel = path
                    .strip_prefix(&req.worktree)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel.starts_with("archive/")
                    || rel.contains("/archive/")
                    || rel == "docs/adr/README.md"
                {
                    continue;
                }
                seen_paths.insert(rel.clone());
                let Ok(text) = fs::read_to_string(path) else {
                    continue;
                };
                let doc_type = frontmatter_field(&text, "doc_type")
                    .unwrap_or_else(|| "adr".to_owned());
                if doc_type == "contract" {
                    continue;
                }
                let title = frontmatter_field(&text, "title")
                    .or_else(|| first_markdown_heading(&text))
                    .unwrap_or_else(|| rel.clone());
                items.push((rel, title, doc_type));
            }
        }

        if let Some(conn) = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref()) {
            let mut stmt = conn.prepare(
                "SELECT path, MIN(section_title) AS title, doc_type
                 FROM doc_sections
                 WHERE doc_type = 'adr'
                   AND doc_type != 'contract'
                   AND path NOT LIKE 'archive/%'
                   AND path NOT LIKE '%/archive/%'
                   AND path != 'docs/adr/README.md'
                 GROUP BY path, doc_type
                 ORDER BY path",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows.flatten() {
                if !checked_child(&req.worktree, Path::new(&row.0)).is_ok_and(|p| p.is_file()) {
                    continue;
                }
                if seen_paths.insert(row.0.clone()) {
                    items.push(row);
                }
            }
        }

        items.sort_by(|a, b| a.0.cmp(&b.0));

        let docs_root = ["docs/**".to_owned()];
        let filter_by_scope = !req.admitted_scope.is_empty()
            && !req.admitted_scope.iter().any(|s| s.trim() == "**")
            && !scopes_overlap(&req.admitted_scope, &docs_root);
        if filter_by_scope {
            if let Some(conn) =
                open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref())
            {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT p_src.path, p_dst.path
                     FROM edges e
                     JOIN nodes n_src ON n_src.node_hash = e.src_hash
                     JOIN path_dictionary p_src ON p_src.path_id = n_src.path_id
                     JOIN nodes n_dst ON n_dst.node_hash = e.dst_hash
                     JOIN path_dictionary p_dst ON p_dst.path_id = n_dst.path_id
                     WHERE e.kind IN ('documents','references_doc','cites_doc','governed_by','governs')
                       AND (p_src.path LIKE 'docs/adr/%' OR p_dst.path LIKE 'docs/adr/%')",
                )?;
                let mut workspace_has_adr_links = false;
                let mut relevant_adrs = BTreeSet::new();
                let link_rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                for (src_path, dst_path) in link_rows.flatten() {
                    let (adr_path, code_path) = if src_path.starts_with("docs/adr/") {
                        (src_path, dst_path)
                    } else {
                        (dst_path, src_path)
                    };
                    if !code_path.starts_with("docs/") {
                        workspace_has_adr_links = true;
                        if req.path_policy.is_scope_admitted(&code_path, &req.admitted_scope) {
                            relevant_adrs.insert(adr_path);
                        }
                    }
                }
                if let Some(target_text) = req.target.as_deref() {
                    for (adr_path, title, _) in &items {
                        let stem = Path::new(adr_path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default();
                        let num_prefix = stem.split('-').next().unwrap_or_default();
                        if (!stem.is_empty() && target_text.contains(stem))
                            || (num_prefix.len() >= 4
                                && num_prefix.bytes().all(|b| b.is_ascii_digit())
                                && target_text.contains(num_prefix))
                            || target_text.contains(title.as_str())
                        {
                            relevant_adrs.insert(adr_path.clone());
                        }
                    }
                }
                if workspace_has_adr_links {
                    items.retain(|(path, _, _)| relevant_adrs.contains(path));
                }
            }
        }

        Ok(items)
    }

    fn provide_adrs(&self, req: &DomainContextReq) -> Result<Value> {
        let items = self
            .collect_adr_records(req)?
            .into_iter()
            .map(|(path, title, _doc_type)| {
                json!({
                    "title": title,
                    "path": path,
                })
            })
            .collect();
        Ok(budget_items("adrs", items, req.max_bytes))
    }

    fn provide_instructions(&self, req: &DomainContextReq) -> Result<Value> {
        let mut ordered_paths = vec![("AGENTS.md".to_owned(), true)];

        let tests_patterns = ["tests/**".to_owned(), "TESTS.md".to_owned()];
        if !req.admitted_scope.is_empty()
            && scopes_overlap(&req.admitted_scope, &tests_patterns)
        {
            ordered_paths.push(("TESTS.md".to_owned(), false));
        }
        ordered_paths.extend(collect_scoped_agents_md(
            &req.worktree,
            &req.admitted_scope,
            &req.path_policy,
        ));
        ordered_paths.sort_by_key(|(_, is_owner)| !is_owner);

        let mut items = Vec::with_capacity(ordered_paths.len());
        for (rel, _) in ordered_paths {
            let Ok(abs) = checked_child(&req.worktree, Path::new(&rel)) else {
                continue;
            };
            if !fs::symlink_metadata(&abs).is_ok_and(|metadata| metadata.is_file()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&abs) else {
                continue;
            };
            let headings = extract_markdown_headings(&text, super::DOCS_INSTRUCTION_MAX_HEADINGS);
            let mut item = json!({ "path": rel });
            if !headings.is_empty() {
                item["headings"] = json!(headings);
            }
            items.push(item);
        }

        let directive = super::DOCS_INSTRUCTIONS_DIRECTIVE;
        let directive_bytes = ",\"directive\":".len() + estimate_json_bytes(&directive);
        let item_budget = req.max_bytes.saturating_sub(directive_bytes);
        let mut budgeted = budget_items("instructions", items, item_budget);
        if let Some(arr) = budgeted.get_mut("items").and_then(Value::as_array_mut) {
            arr.sort_by(|a, b| {
                let order = |item: &Value| match item["path"].as_str() {
                    Some("AGENTS.md") => 0,
                    Some("TESTS.md") => 1,
                    _ => 2,
                };
                (order(a), a["path"].as_str()).cmp(&(order(b), b["path"].as_str()))
            });
        }
        budgeted["directive"] = json!(directive);
        Ok(budgeted)
    }

    fn provide_invariants(&self, req: &DomainContextReq) -> Result<Value> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let worktree_adapter =
            super::resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let mut enforced_ids: BTreeSet<String> = BTreeSet::new();

        if let Some(conn) = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref()) {
            enforced_ids = acdd_index::db::lint::enforced_invariant_ids(
                &conn,
                &worktree_adapter.architecture.rules,
            )
            .unwrap_or_default();
            if !req.admitted_scope.is_empty() {
                if let Ok(scoped) =
                    reader::governing_invariants_for_scope(&conn, &req.admitted_scope)
                {
                    for bucket in ["direct", "omitted_sources"] {
                        if let Some(arr) = scoped.get(bucket).and_then(Value::as_array) {
                            for entry in arr {
                                let id = entry["id"].as_str().unwrap_or_default().to_owned();
                                let path = entry["source"]["path"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned();
                                if id.is_empty()
                                    || path.is_empty()
                                    || is_contract_path_or_doc(&conn, &path, &worktree_adapter)
                                {
                                    continue;
                                }
                                let raw_rule = strip_invariant_prefix(
                                    entry["rule"].as_str().unwrap_or_default(),
                                    &id,
                                );
                                let rule = super::truncate_single_line(
                                    raw_rule,
                                    super::DOCS_INVARIANT_MAX_RULE_CHARS,
                                );
                                if seen.insert((path.clone(), id.clone())) {
                                    let enforced = enforced_ids.contains(&id);
                                    items.push(build_invariant_item(
                                        id,
                                        path,
                                        Some(entry["source"]["line"].clone()),
                                        rule,
                                        Some(true),
                                        enforced,
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            if req.admitted_scope.is_empty() || items.is_empty() {
                let mut stmt = conn.prepare(
                    "SELECT path, section_title, invariants
                     FROM doc_sections
                     WHERE is_invariant = 1 AND doc_type != 'contract'
                     ORDER BY CASE WHEN path LIKE 'docs/adr/%' OR path LIKE 'docs/architecture/%' THEN 0 ELSE 1 END,
                              path,
                              section_title
                     LIMIT 64",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?;
                for row in rows.flatten() {
                    if items.len() >= super::DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
                        break;
                    }
                    let (path, section_title, inv_raw) = row;
                    if path.contains("/archive/") || is_contract_spec_path(&path, &worktree_adapter) {
                        continue;
                    }
                    let parsed_entries = parse_doc_section_invariants(
                        inv_raw.as_deref(),
                        &section_title,
                        super::DOCS_INVARIANT_MAX_RULE_CHARS,
                    );
                    for (id, rule) in parsed_entries {
                        if items.len() >= super::DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
                            break;
                        }
                        if seen.insert((path.clone(), id.clone())) {
                            let enforced = enforced_ids.contains(&id);
                            items.push(build_invariant_item(
                                id,
                                path.clone(),
                                None,
                                rule,
                                Some(false),
                                enforced,
                            ));
                        }
                    }
                }
            }
        } else {
            for spec in &worktree_adapter.architecture.rules {
                if let Some(inv) = spec
                    .invariant
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or_else(|| spec.id.starts_with("INV-").then_some(spec.id.as_str()))
                {
                    enforced_ids.insert(inv.to_owned());
                }
            }
        }

        if items.is_empty() {
            let docs_dir = req.worktree.join("docs");
            if docs_dir.is_dir() {
                'walk: for entry in WalkDir::new(&docs_dir)
                    .max_depth(4)
                    .into_iter()
                    .filter_map(Result::ok)
                {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let path = entry.path();
                    if !is_doc_path(&path.to_string_lossy()) {
                        continue;
                    }
                    let rel = path
                        .strip_prefix(&req.worktree)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    if rel.contains("/archive/") || is_contract_spec_path(&rel, &worktree_adapter) {
                        continue;
                    }
                    let Ok(text) = fs::read_to_string(path) else {
                        continue;
                    };
                    if frontmatter_field(&text, "doc_type").as_deref() == Some("contract") {
                        continue;
                    }
                    for (idx, line) in text.lines().enumerate() {
                        if let Some(inv_id) = extract_invariant_id(line) {
                            if seen.insert((rel.clone(), inv_id.clone())) {
                                let enforced = enforced_ids.contains(&inv_id);
                                items.push(build_invariant_item(
                                    inv_id,
                                    rel.clone(),
                                    Some(json!(idx + 1)),
                                    super::truncate_single_line(
                                        line.trim(),
                                        super::DOCS_INVARIANT_MAX_RULE_CHARS,
                                    ),
                                    None,
                                    enforced,
                                ));
                                if items.len() >= super::DOCS_INVARIANT_FALLBACK_ITEM_LIMIT {
                                    break 'walk;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(budget_items("invariants", items, req.max_bytes))
    }

    fn provide_workspace_config(&self, req: &DomainContextReq) -> Result<Value> {
        use acdd_state::store::sqlite::SqliteStateReader;
        use acdd_state::store::traits::StateReader;
        use acdd_state::usecases::lifecycle::next_spec_id_for_prefix;

        let worktree_adapter =
            super::resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let state_db_path =
            acdd_state::usecases::deps::try_resolve_effective_state_db(&req.worktree, None)
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
                    "patterns": col_cfg.patterns(),
                    "default_draft_dir": col_cfg.default_draft_dir(),
                    "prefixes": col_cfg.prefixes,
                    "digits": col_cfg.digits,
                    "profile": col_cfg.profile,
                    "next_ids": next_ids,
                }),
            );
        }

        let mut available_profiles: BTreeSet<String> =
            ["base", "code", "audit", "documentation", "spec_planning"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        for p in collect_profile_dir_files(&req.worktree, &worktree_adapter.profiles_dirs, 2) {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "yaml" | "yml") {
                if let Ok(rel) = p.strip_prefix(&req.worktree) {
                    available_profiles.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }

        let mut payload = json!({
            "specs": specs_json,
            "docs_roots": worktree_adapter.docs,
            "profiles_dirs": worktree_adapter.profiles_dirs,
            "available_profiles": available_profiles,
            "architecture_zones": worktree_adapter.architecture.zones,
            "limits": {
                "max_metadata_bytes": worktree_adapter.limits.max_metadata_bytes,
            },
            "modules": {
                "profile": worktree_adapter.modules.profile,
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
                    input_schema: SchemaDef::object(),
                },
            ],
            context_lenses: BTreeSet::from([
                "adrs".to_owned(),
                "instructions".to_owned(),
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
        super::unsupported_domain_action(SubsystemId::Docs.as_str(), action)
    }

    fn run_check(
        &self,
        check: &str,
        _params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "links" => self.run_links_check(req),
            "specs" => self.run_specs_check(req),
            _ => Err(unknown_docs_check(check)),
        }
    }

    fn observe_freshness(
        &self,
        check: &str,
        _params: &Value,
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
                let worktree_adapter =
                    super::resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
                let digest = compute_specs_digest(&req.worktree, &worktree_adapter);
                Ok(FreshnessObservation::Passed {
                    index_generation: None,
                    execution_digest_salt: Some(format!("specs_live={digest}")),
                })
            }
            _ => Err(unknown_docs_check(check)),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "adrs" => self.provide_adrs(req),
            "instructions" => self.provide_instructions(req),
            "invariants" => self.provide_invariants(req),
            "workspace_config" => self.provide_workspace_config(req),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown docs context lens 'docs.{lens}'"),
            )
            .into()),
        }
    }
}

fn unknown_docs_check(check: &str) -> anyhow::Error {
    StateError::new(
        ErrorCode::DomainCapabilityUnavailable,
        format!("unknown docs check 'docs.{check}'"),
    )
    .into()
}

fn collect_profile_dir_files(
    worktree: &Path,
    profiles_dirs: &[String],
    max_depth: usize,
) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in profiles_dirs {
        let Ok(abs_dir) = checked_child(worktree, Path::new(dir)) else {
            continue;
        };
        if !abs_dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&abs_dir)
            .max_depth(max_depth)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() {
                out.push(entry.into_path());
            }
        }
    }
    out
}

fn compute_specs_digest(worktree: &Path, adapter: &Adapter) -> String {
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
    record_file(acdd_core::config::ACDD_CONFIG_FILE);
    record_file(".acdd/profile.yaml");
    for path in collect_profile_dir_files(worktree, &adapter.profiles_dirs, 3) {
        if let Ok(rel) = path.strip_prefix(worktree) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            record_file(&rel_str);
        }
    }
    for col in adapter.specs.values() {
        let Ok(dirs) = col.resolve_dirs(worktree) else {
            continue;
        };
        for dir in dirs {
            for entry in WalkDir::new(&dir)
                .max_depth(2)
                .into_iter()
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file()
                    && entry.path().extension().and_then(|e| e.to_str()) == Some("md")
                {
                    if let Ok(rel) = entry.path().strip_prefix(worktree) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        record_file(&rel_str);
                    }
                }
            }
        }
    }
    combine_file_hashes(&digests)
}

fn compute_docs_digest(
    root: &Path,
    scope: Option<&[String]>,
    policy: &WorkspacePathPolicy,
) -> Result<String> {
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | ".git" | ".worktrees")
                )
        })
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() || !is_doc_path(&entry.path().to_string_lossy()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(scope_list) = scope {
            if !scope_list.is_empty()
                && !scope_list.iter().any(|pat| glob_matches(pat, &rel))
                && !policy.is_scope_admitted(&rel, scope_list)
            {
                continue;
            }
        }
        if let Ok(bytes) = fs::read(entry.path()) {
            digests.insert(rel, sha256_hex(&bytes));
        }
    }
    Ok(combine_file_hashes(&digests))
}

fn build_invariant_item(
    id: String,
    path: String,
    line: Option<Value>,
    rule: String,
    scope_match: Option<bool>,
    enforced: bool,
) -> Value {
    let mut item = serde_json::Map::new();
    item.insert("id".to_owned(), json!(id));
    item.insert("path".to_owned(), json!(path));
    if let Some(line) = line.filter(|v| !v.is_null()) {
        item.insert("line".to_owned(), line);
    }
    item.insert("rule".to_owned(), json!(rule));
    if let Some(scope_match) = scope_match {
        item.insert("scope_match".to_owned(), json!(scope_match));
    }
    if enforced {
        item.insert("enforced".to_owned(), json!(true));
    }
    Value::Object(item)
}

fn is_contract_spec_path(rel_path: &str, config: &Adapter) -> bool {
    let norm = rel_path.trim_start_matches("./");
    config.spec_paths().iter().any(|prefix| {
        let p = prefix.trim_end_matches('/');
        norm == p || norm.starts_with(&format!("{p}/"))
    })
}

fn is_contract_path_or_doc(conn: &Connection, rel_path: &str, config: &Adapter) -> bool {
    if is_contract_spec_path(rel_path, config) {
        return true;
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM doc_sections WHERE path = ?1 AND doc_type = 'contract')",
        params![rel_path],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn frontmatter_field(markdown: &str, key: &str) -> Option<String> {
    let rest = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))?;
    let (header, _) = rest.split_once("\n---")?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(header).ok()?;
    yaml.get(key)
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn first_markdown_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn collect_scoped_agents_md(
    worktree: &Path,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> BTreeMap<String, bool> {
    let mut found = BTreeMap::new();
    if admitted_scope.is_empty() || !worktree.exists() {
        return found;
    }

    let mut walk_roots: BTreeSet<PathBuf> = BTreeSet::new();
    for pattern in admitted_scope {
        for variant in expand_glob_braces(pattern) {
            let trimmed = variant.trim().trim_start_matches("./");
            if trimmed.is_empty() {
                continue;
            }
            let replaced = trimmed.replace('\\', "/");
            let mut literal_segs: Vec<&str> = Vec::new();
            let mut has_wildcard = false;
            for seg in replaced.split('/') {
                if seg.is_empty() || seg == "." {
                    continue;
                }
                if seg == ".." {
                    literal_segs.clear();
                    has_wildcard = false;
                    break;
                }
                if seg.contains('*') || seg.contains('?') || seg.contains('[') {
                    has_wildcard = true;
                    break;
                }
                literal_segs.push(seg);
            }

            let mut prefix = String::new();
            for seg in &literal_segs {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(seg);
                let cand = format!("{prefix}/AGENTS.md");
                if checked_child(worktree, Path::new(&cand)).is_ok_and(|p| p.is_file()) {
                    found.insert(cand, true);
                }
            }

            if has_wildcard {
                if prefix.is_empty() {
                    walk_roots.insert(worktree.to_path_buf());
                } else if let Ok(abs) = checked_child(worktree, Path::new(&prefix)) {
                    if abs.is_dir() {
                        walk_roots.insert(abs);
                    }
                }
            } else if !prefix.is_empty() {
                if let Ok(abs) = checked_child(worktree, Path::new(&prefix)) {
                    if abs.is_dir() {
                        walk_roots.insert(abs);
                    }
                }
            }
        }
    }

    for root_dir in walk_roots {
        for entry in WalkDir::new(&root_dir)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || !e.file_type().is_dir()
                    || !matches!(
                        e.file_name().to_str(),
                        Some(".git" | ".worktrees" | "target" | "node_modules" | ".venv" | ".acdd")
                    )
            })
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || entry.file_name() != "AGENTS.md" {
                continue;
            }
            let Ok(rel_raw) = entry.path().strip_prefix(worktree) else {
                continue;
            };
            let Some(rel) = normalize_rel_path(&rel_raw.to_string_lossy()) else {
                continue;
            };
            if rel == "AGENTS.md" {
                continue;
            }
            let Some(dir_rel) = rel.rsplit_once('/').map(|(d, _)| d.to_owned()) else {
                continue;
            };
            if (scopes_overlap(admitted_scope, &[dir_rel])
                || policy.is_scope_admitted(&rel, admitted_scope))
                && checked_child(worktree, Path::new(&rel)).is_ok_and(|p| p.is_file())
            {
                found.entry(rel).or_insert(false);
            }
        }
    }

    found
}

fn extract_markdown_headings(markdown: &str, max_headings: usize) -> Vec<String> {
    let mut headings = Vec::new();
    let mut lines = markdown.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }
    let mut in_fence = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            let text = trimmed.trim_start_matches('#').trim();
            if !text.is_empty() {
                headings.push(text.to_owned());
                if headings.len() >= max_headings {
                    break;
                }
            }
        }
    }
    headings
}

fn extract_invariant_id(line: &str) -> Option<String> {
    let idx = line.find("INV-")?;
    let tail = &line[idx..];
    let end = tail
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(tail.len());
    let candidate = &tail[..end];
    (candidate.len() > 4).then(|| candidate.to_owned())
}

fn strip_invariant_prefix<'a>(text: &'a str, id: &str) -> &'a str {
    let trimmed = text.trim();
    let without_id = trimmed
        .strip_prefix(id)
        .or_else(|| trimmed.strip_prefix("Invariant"))
        .unwrap_or(trimmed);
    without_id
        .trim_start_matches([':', '-', '—', '`', '*', ' '])
        .trim()
}

fn parse_doc_section_invariants(
    inv_raw: Option<&str>,
    section_title: &str,
    max_rule_chars: usize,
) -> Vec<(String, String)> {
    let texts: Vec<String> = match inv_raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) if raw.starts_with('[') => serde_json::from_str::<Vec<String>>(raw)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect(),
        Some(raw) => vec![raw.to_owned()],
        None => Vec::new(),
    };
    if texts.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for block in texts {
        let mut cursor = block.as_str();
        let mut found_id = false;
        while let Some(idx) = cursor.find("INV-") {
            if let Some(id) = extract_invariant_id(&cursor[idx..]) {
                let after_id = &cursor[idx + id.len()..];
                let next_inv = after_id.find("INV-").unwrap_or(after_id.len());
                let raw_rule = strip_invariant_prefix(&after_id[..next_inv], &id);
                let rule = super::truncate_single_line(raw_rule, max_rule_chars);
                out.push((id, rule));
                found_id = true;
                cursor = &after_id[next_inv..];
            } else {
                cursor = &cursor[idx + 4..];
            }
        }
        if !found_id {
            let clean = strip_invariant_prefix(
                block.trim_start_matches("[!IMPORTANT]").trim(),
                section_title.trim(),
            );
            let id = if section_title.trim().is_empty() {
                "INV-SECTION".to_owned()
            } else {
                section_title.trim().to_owned()
            };
            out.push((id, super::truncate_single_line(clean, max_rule_chars)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::DocsDomainProvider;
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::ids::RunId;
    use acdd_core::ports::{
        DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
    };
    use std::fs;

    fn assert_check_matches_freshness(
        provider: &DocsDomainProvider,
        check_name: &str,
        expected_prefix: &str,
        req: &DomainCheckReq,
    ) {
        let check = provider
            .run_check(check_name, &serde_json::Value::Null, req)
            .unwrap();
        let observed = provider
            .observe_freshness(check_name, &serde_json::Value::Null, req)
            .unwrap();
        match check {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => {
                assert!(execution_digest_salt
                    .as_deref()
                    .is_some_and(|s| s.starts_with(expected_prefix)));
                assert_eq!(
                    observed,
                    FreshnessObservation::Passed {
                        index_generation: None,
                        execution_digest_salt,
                    }
                );
            }
            other => panic!("expected docs.{check_name} pass, got {other:?}"),
        }
    }

    #[test]
    fn docs_domain_checks_links_and_filters_contracts_from_lenses() {
        let dir = ScopedTempDir::create("acdd-ut-docs-domain").unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/adr")).unwrap();
        fs::create_dir_all(root.join("docs/milestones")).unwrap();
        fs::create_dir_all(root.join("docs/architecture")).unwrap();
        fs::write(
            root.join("docs/adr/ADR-0001-test.md"),
            "---\ntitle: Test ADR\n---\n# Test ADR\nSee [arch](../architecture/overview.md).\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/architecture/overview.md"),
            "# Architecture\n- INV-TEST-1: All invariants stay non-contract.\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/milestones/m1.md"),
            "# Spec M1\n- INV-CONTRACT-ONLY: Must not leak into docs.invariants.\n",
        )
        .unwrap();

        let provider = DocsDomainProvider::new(root.to_path_buf(), Adapter::default());
        let check_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.docs".parse().unwrap(),
            gate_id: "docs".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["docs/**".into()],
            changed_paths: vec![],
            test_target: None,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
        };
        assert_check_matches_freshness(&provider, "links", "docs_live=", &check_req);

        // Non-empty non-markdown scope must pass without scanning unrelated broken docs in the repo.
        fs::write(root.join("docs/unrelated_broken.md"), "[missing](./missing.md)\n").unwrap();
        let code_only_req = DomainCheckReq {
            admitted_scope: vec!["src/lib.rs".into()],
            ..check_req.clone()
        };
        let code_only_check = provider
            .run_check("links", &serde_json::Value::Null, &code_only_req)
            .unwrap();
        let code_only_observed = provider
            .observe_freshness("links", &serde_json::Value::Null, &code_only_req)
            .unwrap();
        match code_only_check {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => assert_eq!(
                code_only_observed,
                FreshnessObservation::Passed {
                    index_generation: None,
                    execution_digest_salt,
                }
            ),
            other => panic!("expected code-only scope docs.links pass, got {other:?}"),
        }

        // Brace-glob doc scope `docs/{a.md,b.md}` MUST inspect both files via canonical matcher.
        fs::write(root.join("docs/a.md"), "# Valid A\n").unwrap();
        fs::write(root.join("docs/b.md"), "[broken](./does_not_exist.md)\n").unwrap();
        let brace_doc_req = DomainCheckReq {
            admitted_scope: vec!["docs/{a.md,b.md}".into()],
            ..check_req.clone()
        };
        let brace_doc_outcome = provider
            .run_check("links", &serde_json::Value::Null, &brace_doc_req)
            .unwrap();
        assert!(
            matches!(brace_doc_outcome, DomainCheckOutcome::Failed { .. }),
            "docs/{{a.md,b.md}} must check both files and catch broken link in b.md: {brace_doc_outcome:?}"
        );

        let ctx_req = DomainContextReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.docs".parse().unwrap(),
            gate_id: "docs".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["docs/**".into()],
            target: None,
            max_bytes: crate::state_bridge::TEST_DEFAULT_CONTEXT_MAX_BYTES,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
        };
        assert!(provider.capabilities().context_lenses.contains("instructions"));
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("tests/acdd")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Root Agent Rules\n## Routes\n").unwrap();
        fs::write(root.join("TESTS.md"), "# Required Tests\n## Matrix\n").unwrap();
        fs::write(root.join("src/AGENTS.md"), "# Src Rules\n## Ownership\n").unwrap();
        fs::write(root.join("tests/AGENTS.md"), "# Test Rules\n").unwrap();
        fs::write(root.join("tests/acdd/AGENTS.md"), "# Acdd Test Rules\n").unwrap();
        fs::write(root.join("other/AGENTS.md"), "# Unrelated\n").unwrap();

        let src_only_ctx = DomainContextReq {
            admitted_scope: vec!["src/sub/lib.rs".into()],
            ..ctx_req.clone()
        };
        let src_inst = provider.provide_context("instructions", &src_only_ctx).unwrap();
        let src_paths: Vec<&str> = src_inst["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["path"].as_str())
            .collect();
        assert_eq!(
            src_paths,
            vec!["AGENTS.md", "src/AGENTS.md"]
        );
        assert!(src_inst["items"][0].get("uri").is_none());
        assert!(src_inst["items"][0].get("title").is_none());
        assert!(src_inst["items"][0].get("content").is_none());
        assert_eq!(src_inst["items"][0]["headings"], serde_json::json!(["Root Agent Rules", "Routes"]));
        assert_eq!(
            src_inst["directive"],
            "MANDATORY: read repository rules for respective scope."
        );
        assert!(src_inst.get("mandatory_directives").is_none());

        let tests_ctx = DomainContextReq {
            admitted_scope: vec!["tests/**".into()],
            ..ctx_req.clone()
        };
        let tests_inst = provider.provide_context("instructions", &tests_ctx).unwrap();
        let tests_paths: Vec<&str> = tests_inst["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["path"].as_str())
            .collect();
        assert_eq!(
            tests_paths,
            vec![
                "AGENTS.md",
                "TESTS.md",
                "tests/AGENTS.md",
                "tests/acdd/AGENTS.md",
            ]
        );

        let tight_inst = provider
            .provide_context(
                "instructions",
                &DomainContextReq {
                    max_bytes: crate::state_bridge::TEST_TIGHT_INSTRUCTIONS_MAX_BYTES,
                    ..tests_ctx
                },
            )
            .unwrap();
        assert_eq!(tight_inst["truncated"], true);
        assert!(
            acdd_core::format::estimate_json_bytes(&tight_inst)
                <= crate::state_bridge::TEST_TIGHT_INSTRUCTIONS_MAX_BYTES
        );

        let adrs = provider.provide_context("adrs", &ctx_req).unwrap();
        assert_eq!(adrs["items"].as_array().map(|a| a.len()), Some(1));
        assert!(adrs["items"][0].get("doc_type").is_none());

        let invs = provider.provide_context("invariants", &ctx_req).unwrap();
        let inv_text = serde_json::to_string(&invs).unwrap();
        assert!(inv_text.contains("INV-TEST-1"), "{inv_text}");
        assert!(!inv_text.contains("INV-CONTRACT-ONLY"), "{inv_text}");

        // With indexed `code-map.sqlite` containing archived/README ADRs, `docs.adrs` must
        // exclude archived/README rows and still surface live unindexed ADRs, while
        // `docs.instructions` surfaces AGENTS.md only without repeating ADRs.
        fs::create_dir_all(root.join("docs/adr/archive")).unwrap();
        fs::write(
            root.join("docs/adr/archive/0000-old.md"),
            "---\ntitle: Old Archived ADR\ndoc_type: adr\n---\n# Old Archived ADR\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/adr/README.md"),
            "---\ntitle: ADR Index\ndoc_type: adr\n---\n# ADR Index\n",
        )
        .unwrap();
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        fs::write(
            root.join("docs/adr/ADR-0002-live.md"),
            "---\ntitle: Live Unindexed ADR\ndoc_type: adr\n---\n# Live Unindexed ADR\n",
        )
        .unwrap();
        for (lens, expected_paths, excluded_paths) in [
            (
                "instructions",
                vec!["AGENTS.md", "src/AGENTS.md"],
                vec![
                    "docs/adr/ADR-0001-test.md",
                    "docs/adr/ADR-0002-live.md",
                    "docs/adr/archive/0000-old.md",
                    "docs/adr/README.md",
                ],
            ),
            (
                "adrs",
                vec!["docs/adr/ADR-0001-test.md", "docs/adr/ADR-0002-live.md"],
                vec!["docs/adr/archive/0000-old.md", "docs/adr/README.md"],
            ),
        ] {
            let res = provider.provide_context(lens, &src_only_ctx).unwrap();
            let paths: Vec<&str> = res["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["path"].as_str())
                .collect();
            for expected in expected_paths {
                assert!(paths.contains(&expected), "{lens} missing {expected}: {paths:?}");
            }
            for excluded in excluded_paths {
                assert!(!paths.contains(&excluded), "{lens} leaked {excluded}: {paths:?}");
            }
        }
        let indexed_invs = provider.provide_context("invariants", &src_only_ctx).unwrap();
        let indexed_inv_text = serde_json::to_string(&indexed_invs).unwrap();
        assert!(indexed_inv_text.contains("INV-TEST-1"), "{indexed_inv_text}");
        assert!(
            !indexed_inv_text.contains("\"id\":\"[]\"") && !indexed_inv_text.contains("0000-old.md"),
            "indexed fallback must skip empty JSON arrays and archived docs: {indexed_inv_text}"
        );

        fs::write(
            root.join("docs/adr/ADR-0001-test.md"),
            "# Broken\n[nope](../architecture/missing.md)\n",
        )
        .unwrap();
        let failed = provider
            .run_check("links", &serde_json::Value::Null, &check_req)
            .unwrap();
        assert!(matches!(failed, DomainCheckOutcome::Failed { .. }));

        // `docs.workspace_config` context lens and `docs.specs` validation + freshness check.
        fs::create_dir_all(root.join(".acdd")).unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\ninherit: spec_planning\n",
        )
        .unwrap();
        fs::write(
            root.join("acdd.yaml"),
            "docs:\n  - docs\nprofiles_dirs:\n  - .acdd/profiles\nlimits:\n  max_metadata_bytes: 1024\nmodules:\n  code: true\n  docs: true\n  state: true\nspecs:\n  milestones:\n    path: docs/milestones\n    prefixes: [AC]\n    digits: 3\narchitecture:\n  zones:\n    core:\n      - \"crates/core/**\"\n",
        )
        .unwrap();
        let adapter_cfg = acdd_core::config::load_adapter_report(root, None)
            .unwrap()
            .adapter;
        let cfg_provider = DocsDomainProvider::new(root.to_path_buf(), adapter_cfg);
        let ws_cfg_lens = cfg_provider
            .provide_context("workspace_config", &ctx_req)
            .unwrap();
        assert_eq!(ws_cfg_lens["specs"]["milestones"]["next_ids"]["AC"], "AC-001");
        assert_eq!(ws_cfg_lens["docs_roots"], serde_json::json!(["docs"]));
        assert_eq!(
            ws_cfg_lens["profiles_dirs"],
            serde_json::json!([".acdd/profiles"])
        );
        let avail_profiles = ws_cfg_lens["available_profiles"].as_array().unwrap();
        assert!(
            avail_profiles.iter().any(|p| p == "spec_planning"),
            "available_profiles should include embedded spec_planning: {ws_cfg_lens}"
        );
        assert_eq!(ws_cfg_lens["limits"]["max_metadata_bytes"], 1024);
        assert!(ws_cfg_lens["architecture_zones"].get("core").is_some());
        let mut lenses = serde_json::Map::new();
        lenses.insert("docs.workspace_config".to_owned(), ws_cfg_lens);
        let claim = acdd_state::context::ClaimResp {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "synthesize".parse().unwrap(),
            claim_revision: 1,
            contract_revision: 1,
            worker_id: "w1".parse().unwrap(),
            worktree: ".".to_owned(),
            candidate_tree: acdd_core::ids::TreeOid::new("0".repeat(40)).unwrap(),
            inspect_cmd: "acdd show synthesize --run r1".to_owned(),
            context_reset_required: false,
            contract: serde_json::json!({}),
            guidance: Vec::new(),
            unresolved_findings: None,
            pre_task_slice: None,
            dependency_receipts: Vec::new(),
            blackboard: Vec::new(),
            context_lenses: lenses,
            omitted_sections: Vec::new(),
            total_bytes: 0,
        };
        let rendered_cfg = acdd_state::context::render_claim_prompt_markdown(&claim, None);
        assert!(
            rendered_cfg.contains("- spec `milestones`: `docs/milestones` (prefixes: [AC], digits: 3, next: AC=AC-001)"),
            "expected live spec rendering in Markdown:\n{rendered_cfg}"
        );
        assert!(
            rendered_cfg.contains("- zone `core`: [crates/core/**]"),
            "expected live zone rendering in Markdown:\n{rendered_cfg}"
        );
        assert!(!rendered_cfg.contains("``"), "{rendered_cfg}");

        // Valid planned spec passes `docs.specs` and matches `observe_freshness("specs", ...)`.
        fs::write(
            root.join("docs/milestones/m1.md"),
            "---\nid: AC-001\ntitle: Valid Spec\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: build thing\nscope: [src/lib.rs]\n```\n",
        )
        .unwrap();
        assert_check_matches_freshness(&cfg_provider, "specs", "specs_live=", &check_req);
        // Invalid spec fails `docs.specs`.
        fs::write(
            root.join("docs/milestones/BAD-999.md"),
            "---\nid: BAD-999\ntitle: Invalid prefix\nstatus: planned\n---\n",
        )
        .unwrap();
        let specs_fail = cfg_provider
            .run_check("specs", &serde_json::Value::Null, &check_req)
            .unwrap();
        assert!(
            matches!(specs_fail, DomainCheckOutcome::Failed { .. }),
            "invalid spec should fail docs.specs: {specs_fail:?}"
        );
    }
}
