//! Host `DocsDomainProvider` (`domain_id = "docs"`) implementation.

use super::{
    budget_items, budget_items_with_fields, combine_file_hashes, open_code_db_readonly,
};
use crate::format::file_uri_for_path;
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::{checked_child, is_doc_path};
use acdd_core::path_policy::{
    expand_glob_braces, glob_matches, normalize_rel_path, scopes_overlap, WorkspacePathPolicy,
};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
};
use acdd_core::schema::SchemaDef;
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

/// Host `docs` domain provider exposing `docs.links`, `docs.adrs`, `docs.instructions`, and `docs.invariants`.
pub struct DocsDomainProvider {
    root: PathBuf,
    config: Adapter,
}

impl DocsDomainProvider {
    /// Construct a `DocsDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self { root, config }
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
            LinksScope::FullRepo => crate::cli::docs_check::collect_link_errors(&req.worktree, None)?,
            LinksScope::Scoped(patterns) => crate::cli::docs_check::collect_link_errors(
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

        if let Some(conn) = open_code_db_readonly(&self.root, &req.worktree) {
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
        Ok(items)
    }

    fn provide_adrs(&self, req: &DomainContextReq) -> Result<Value> {
        let items = self
            .collect_adr_records(req)?
            .into_iter()
            .map(|(path, title, doc_type)| {
                json!({
                    "path": path,
                    "title": title,
                    "doc_type": doc_type,
                })
            })
            .collect();
        Ok(budget_items("adrs", items, req.max_bytes))
    }

    fn provide_instructions(&self, req: &DomainContextReq) -> Result<Value> {
        let mut ordered_paths: Vec<(String, &'static str, String)> = Vec::new();
        let mut seen = BTreeSet::new();

        // 1. Root AGENTS.md (if present).
        if checked_child(&req.worktree, Path::new("AGENTS.md")).is_ok_and(|p| p.is_file())
            && seen.insert("AGENTS.md".to_owned())
        {
            ordered_paths.push((
                "AGENTS.md".to_owned(),
                "instructions_root",
                "AGENTS.md".to_owned(),
            ));
        }

        // 2. TESTS.md (when `tests/**` or `TESTS.md` overlaps `req.admitted_scope`).
        let tests_patterns = ["tests/**".to_owned(), "TESTS.md".to_owned()];
        if !req.admitted_scope.is_empty()
            && scopes_overlap(&req.admitted_scope, &tests_patterns)
            && checked_child(&req.worktree, Path::new("TESTS.md")).is_ok_and(|p| p.is_file())
            && seen.insert("TESTS.md".to_owned())
        {
            ordered_paths.push(("TESTS.md".to_owned(), "tests_guide", "TESTS.md".to_owned()));
        }

        // 3. Subtree `<dir>/AGENTS.md` along ancestor & matched directories of `req.admitted_scope`.
        for rel in collect_scoped_agents_md(&req.worktree, &req.admitted_scope, &req.path_policy) {
            if seen.insert(rel.clone()) {
                ordered_paths.push((rel.clone(), "instructions_subtree", rel));
            }
        }

        // 4. Relevant `docs/adr/*.md` files (prioritizing scope-relevant ADRs before general ADRs).
        let scope_tokens = extract_scope_tokens(&req.admitted_scope, req.target.as_deref());
        let mut matched_adrs = Vec::new();
        let mut other_adrs = Vec::new();
        for (rel, title, _doc_type) in self.collect_adr_records(req)? {
            if !seen.insert(rel.clone()) {
                continue;
            }
            if !checked_child(&req.worktree, Path::new(&rel)).is_ok_and(|p| p.is_file()) {
                continue;
            }
            let lower_rel = rel.to_ascii_lowercase();
            let lower_title = title.to_ascii_lowercase();
            let is_match = scope_tokens
                .iter()
                .any(|tok| lower_rel.contains(tok) || lower_title.contains(tok));
            if is_match {
                matched_adrs.push((rel, "adr", title));
            } else {
                other_adrs.push((rel, "adr", title));
            }
        }
        ordered_paths.extend(matched_adrs);
        ordered_paths.extend(other_adrs);

        let mut items = Vec::with_capacity(ordered_paths.len());
        for (rel, kind, fallback_title) in ordered_paths {
            let Ok(abs) = checked_child(&req.worktree, Path::new(&rel)) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&abs) else {
                continue;
            };
            let title = frontmatter_field(&text, "title")
                .or_else(|| first_markdown_heading(&text))
                .unwrap_or(fallback_title);
            let max_headings = if kind == "adr" { 6 } else { 10 };
            let headings = extract_markdown_headings(&text, max_headings);
            let uri = abs
                .canonicalize()
                .ok()
                .and_then(|canon| file_uri_for_path(&canon, None));
            let mut obj = serde_json::Map::new();
            obj.insert("path".to_owned(), json!(rel));
            if let Some(uri) = uri {
                obj.insert("uri".to_owned(), json!(uri));
            }
            obj.insert("kind".to_owned(), json!(kind));
            obj.insert("title".to_owned(), json!(title));
            if !headings.is_empty() {
                obj.insert("headings".to_owned(), json!(headings));
            }
            items.push(Value::Object(obj));
        }

        let mut extra = serde_json::Map::new();
        extra.insert(
            "mandatory_directives".to_owned(),
            json!([
                "Read all listed AGENTS.md and ADRs before edits, contracts, or reviews; reuse INV-SINGLE-OWNER owners before adding code.",
                "Prioritize ACDD MCP tools (code_map_search, code_map_inspect, code_map_impact, code_map_query, acdd_read, ast_grep_search) before shell grep/rg."
            ]),
        );
        // Reserve room for the outer `"docs.instructions":` map key inside `ClaimResp` Tier 6.
        let effective_max = if req.max_bytes >= 512 {
            req.max_bytes.saturating_sub(48)
        } else {
            req.max_bytes
        };
        let mut budgeted = budget_items_with_fields(
            "instructions",
            items.clone(),
            effective_max,
            extra.clone(),
        );
        if acdd_core::format::estimate_json_bytes(&budgeted) > req.max_bytes {
            for item in &mut items {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("headings");
                }
            }
            budgeted = budget_items_with_fields("instructions", items, effective_max, extra);
        }
        Ok(budgeted)
    }

    fn provide_invariants(&self, req: &DomainContextReq) -> Result<Value> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();

        if let Some(conn) = open_code_db_readonly(&self.root, &req.worktree) {
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
                                if path.is_empty() || is_contract_path_or_doc(&conn, &path, &self.config) {
                                    continue;
                                }
                                if seen.insert((path.clone(), id.clone())) {
                                    items.push(json!({
                                        "id": id,
                                        "path": path,
                                        "line": entry["source"]["line"],
                                        "rule": entry["rule"],
                                        "scope_match": true,
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            let mut stmt = conn.prepare(
                "SELECT path, section_title, invariants
                 FROM doc_sections
                 WHERE is_invariant = 1 AND doc_type != 'contract'
                 ORDER BY path, section_title
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
                let (path, section_title, inv_raw) = row;
                if is_contract_spec_path(&path, &self.config) {
                    continue;
                }
                let id = inv_raw.unwrap_or_else(|| section_title.clone());
                if seen.insert((path.clone(), id.clone())) {
                    items.push(json!({
                        "id": id,
                        "path": path,
                        "section_title": section_title,
                        "scope_match": false,
                    }));
                }
            }
        }

        if items.is_empty() {
            let docs_dir = req.worktree.join("docs");
            if docs_dir.is_dir() {
                for entry in WalkDir::new(&docs_dir)
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
                    if rel.contains("/archive/") || is_contract_spec_path(&rel, &self.config) {
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
                                items.push(json!({
                                    "id": inv_id,
                                    "path": rel,
                                    "line": idx + 1,
                                    "rule": line.trim(),
                                }));
                            }
                        }
                    }
                }
            }
        }

        Ok(budget_items("invariants", items, req.max_bytes))
    }
}

impl DomainProvider for DocsDomainProvider {
    fn domain_id(&self) -> &str {
        "docs"
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: Vec::new(),
            checks: vec![CheckCapability {
                name: "links".to_owned(),
                input_schema: SchemaDef::object(),
            }],
            context_lenses: BTreeSet::from([
                "adrs".to_owned(),
                "instructions".to_owned(),
                "invariants".to_owned(),
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
        Err(StateError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("docs domain does not expose action '{action}'"),
        )
        .into())
    }

    fn run_check(
        &self,
        check: &str,
        _params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "links" => self.run_links_check(req),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown docs check 'docs.{check}'"),
            )
            .into()),
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
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown docs check 'docs.{check}'"),
            )
            .into()),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "adrs" => self.provide_adrs(req),
            "instructions" => self.provide_instructions(req),
            "invariants" => self.provide_invariants(req),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown docs context lens 'docs.{lens}'"),
            )
            .into()),
        }
    }
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
) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
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
                    found.insert(cand);
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
                found.insert(rel);
            }
        }
    }

    found
}

fn extract_scope_tokens(admitted_scope: &[String], target: Option<&str>) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    for raw in admitted_scope.iter().map(String::as_str).chain(target) {
        for part in raw
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| s.len() >= 3)
        {
            let lower = part.to_ascii_lowercase();
            if !matches!(
                lower.as_str(),
                "src" | "tests" | "test" | "docs" | "crates" | "mod" | "lib" | "main"
            ) {
                tokens.insert(lower);
            }
        }
    }
    tokens.into_iter().collect()
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

#[cfg(test)]
mod tests {
    use super::DocsDomainProvider;
    use acdd_core::config::Adapter;
    use acdd_core::ids::RunId;
    use acdd_core::ports::{
        DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "acdd-ut-{tag}-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn docs_domain_checks_links_and_filters_contracts_from_lenses() {
        let dir = TempDir::new("docs-domain");
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
        let check = provider
            .run_check("links", &serde_json::Value::Null, &check_req)
            .unwrap();
        let observed = provider
            .observe_freshness("links", &serde_json::Value::Null, &check_req)
            .unwrap();
        match check {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => {
                assert!(execution_digest_salt
                    .as_deref()
                    .is_some_and(|s| s.starts_with("docs_live=")));
                assert_eq!(
                    observed,
                    FreshnessObservation::Passed {
                        index_generation: None,
                        execution_digest_salt,
                    }
                );
            }
            other => panic!("expected docs.links pass, got {other:?}"),
        }

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
            max_bytes: 8192,
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
            vec!["AGENTS.md", "src/AGENTS.md", "docs/adr/ADR-0001-test.md"]
        );
        assert!(src_inst["items"][0]["uri"].as_str().is_some_and(|u| u.starts_with("file://")));
        assert_eq!(src_inst["items"][0]["headings"], serde_json::json!(["Root Agent Rules", "Routes"]));
        let directives = src_inst["mandatory_directives"].to_string();
        for tool in [
            "AGENTS.md",
            "ADRs",
            "code_map_search",
            "code_map_inspect",
            "code_map_impact",
            "code_map_query",
            "acdd_read",
            "ast_grep_search",
            "grep/rg",
        ] {
            assert!(directives.contains(tool), "missing directive {tool}: {directives}");
        }

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
                "docs/adr/ADR-0001-test.md",
            ]
        );

        let tight_inst = provider
            .provide_context(
                "instructions",
                &DomainContextReq {
                    max_bytes: 700,
                    ..tests_ctx
                },
            )
            .unwrap();
        assert_eq!(tight_inst["truncated"], true);
        assert!(acdd_core::format::estimate_json_bytes(&tight_inst) <= 700);

        let adrs = provider.provide_context("adrs", &ctx_req).unwrap();
        assert_eq!(adrs["items"].as_array().map(|a| a.len()), Some(1));

        let invs = provider.provide_context("invariants", &ctx_req).unwrap();
        let inv_text = serde_json::to_string(&invs).unwrap();
        assert!(inv_text.contains("INV-TEST-1"), "{inv_text}");
        assert!(!inv_text.contains("INV-CONTRACT-ONLY"), "{inv_text}");

        // With indexed `code-map.sqlite` containing archived/README ADRs, both `docs.instructions`
        // and `docs.adrs` must exclude archived/README rows and still surface live unindexed ADRs.
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
                vec![
                    "AGENTS.md",
                    "src/AGENTS.md",
                    "docs/adr/ADR-0001-test.md",
                    "docs/adr/ADR-0002-live.md",
                ],
                vec!["docs/adr/archive/0000-old.md", "docs/adr/README.md"],
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

        fs::write(
            root.join("docs/adr/ADR-0001-test.md"),
            "# Broken\n[nope](../architecture/missing.md)\n",
        )
        .unwrap();
        let failed = provider
            .run_check("links", &serde_json::Value::Null, &check_req)
            .unwrap();
        assert!(matches!(failed, DomainCheckOutcome::Failed { .. }));
    }
}
