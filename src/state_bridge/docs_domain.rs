//! Host `DocsDomainProvider` (`domain_id = "docs"`) implementation.

use super::{budget_items, combine_file_hashes, open_code_db_readonly};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::is_doc_path;
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider,
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

/// Host `docs` domain provider exposing `docs.links`, `docs.adrs`, and `docs.invariants`.
pub struct DocsDomainProvider {
    root: PathBuf,
    config: Adapter,
}

impl DocsDomainProvider {
    /// Construct a `DocsDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self { root, config }
    }

    fn run_links_check(&self, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let policy = WorkspacePathPolicy::default();
        let mut scoped_docs: Vec<String> = Vec::new();
        for rel in req.admitted_scope.iter().chain(&req.changed_paths) {
            if is_doc_path(rel) || rel.contains('*') || req.worktree.join(rel).is_dir() {
                scoped_docs.push(rel.clone());
            }
        }
        let scope_slice = (!scoped_docs.is_empty()).then_some(scoped_docs.as_slice());
        let (checked_count, errors) =
            crate::cli::docs_check::collect_link_errors(&req.worktree, scope_slice)?;

        let live_digest = compute_docs_digest(&req.worktree, scope_slice, &policy)?;
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

    fn provide_adrs(&self, req: &DomainContextReq) -> Result<Value> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen_paths = BTreeSet::new();

        if let Some(conn) = open_code_db_readonly(&self.root, &req.worktree) {
            let mut stmt = conn.prepare(
                "SELECT path, MIN(section_title) AS title, doc_type
                 FROM doc_sections
                 WHERE doc_type = 'adr' AND doc_type != 'contract'
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
                if seen_paths.insert(row.0.clone()) {
                    items.push(json!({
                        "path": row.0,
                        "title": row.1,
                        "doc_type": row.2,
                    }));
                }
            }
        }

        if items.is_empty() {
            let adr_dir = req.worktree.join("docs/adr");
            if adr_dir.is_dir() {
                for entry in WalkDir::new(&adr_dir)
                    .max_depth(2)
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
                    if rel.contains("/archive/") || rel == "docs/adr/README.md" {
                        continue;
                    }
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
                    if seen_paths.insert(rel.clone()) {
                        items.push(json!({
                            "path": rel,
                            "title": title,
                            "doc_type": doc_type,
                        }));
                    }
                }
            }
        }

        items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        Ok(budget_items("adrs", items, req.max_bytes))
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
            context_lenses: BTreeSet::from(["adrs".to_owned(), "invariants".to_owned()]),
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

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "adrs" => self.provide_adrs(req),
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
            if !scope_list.is_empty() && !policy.is_scope_admitted(&rel, scope_list) {
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
    use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider};
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
        };
        let check = provider
            .run_check("links", &serde_json::Value::Null, &check_req)
            .unwrap();
        match check {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => assert!(execution_digest_salt
                .as_deref()
                .is_some_and(|s| s.starts_with("docs_live="))),
            other => panic!("expected docs.links pass, got {other:?}"),
        }

        let ctx_req = DomainContextReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.docs".parse().unwrap(),
            gate_id: "docs".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["docs/**".into()],
            target: None,
            max_bytes: 8192,
        };
        let adrs = provider.provide_context("adrs", &ctx_req).unwrap();
        assert_eq!(adrs["items"].as_array().map(|a| a.len()), Some(1));

        let invs = provider.provide_context("invariants", &ctx_req).unwrap();
        let inv_text = serde_json::to_string(&invs).unwrap();
        assert!(inv_text.contains("INV-TEST-1"), "{inv_text}");
        assert!(!inv_text.contains("INV-CONTRACT-ONLY"), "{inv_text}");

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
