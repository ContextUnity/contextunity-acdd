//! Candidate ranking, citation matching, and frontmatter metadata parsing for `paths.*` / `specs.*` lenses.

use super::super::common::{
    budget_scored_items, collect_active_diff_paths, open_code_db_readonly,
};
use acdd_core::path_policy::{
    expand_glob_braces, glob_matches, scopes_overlap, WorkspacePathPolicy,
};
use acdd_core::ports::DomainContextReq;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) struct RankedDocCandidate {
    pub(super) rel_path: String,
    pub(super) item_heading_ref: Option<String>,
    pub(super) explicit_scopes: Vec<String>,
    pub(super) explicit_symbols: Vec<String>,
    pub(super) is_global_fallback: bool,
    pub(super) id_tokens: Vec<String>,
    pub(super) title: Option<String>,
    pub(super) priority_bonus: u64,
    pub(super) payload: Value,
}

#[derive(Default)]
pub(super) struct FrontmatterMeta {
    pub(super) id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) scopes: Vec<String>,
}

pub(super) fn rank_and_budget_candidates(
    root: &Path,
    code_db: Option<&Path>,
    kind: &str,
    mut candidates: Vec<RankedDocCandidate>,
    req: &DomainContextReq,
    max_bytes: usize,
) -> Result<Value> {
    candidates.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let candidate_paths: BTreeSet<String> = candidates.iter().map(|c| c.rel_path.clone()).collect();
    let diff_paths = collect_active_diff_paths(&req.worktree, req.baseline_ref.as_deref());
    let diff_paths_vec: Vec<String> = diff_paths.iter().cloned().collect();

    let (scope_linked_paths, diff_linked_paths, diff_symbol_sections, diff_symbols) =
        if candidate_paths.is_empty() {
            (
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
            )
        } else if let Some(conn) = open_code_db_readonly(root, &req.worktree, code_db) {
            let (symbols, changed_hashes) = if !diff_paths.is_empty() {
                acdd_index::db::traversal::collect_changed_symbol_seeds(
                    &conn,
                    &req.worktree,
                    req.baseline_ref.as_deref(),
                    &diff_paths,
                )
                .unwrap_or_default()
            } else {
                (BTreeSet::new(), BTreeSet::new())
            };
            let (scope_linked, diff_linked, symbol_sections) = collect_code_map_doc_edge_paths(
                &conn,
                &candidate_paths,
                &req.admitted_scope,
                &diff_paths,
                &changed_hashes,
                &req.path_policy,
            )?;
            (scope_linked, diff_linked, symbol_sections, symbols)
        } else {
            (
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
            )
        };

    let mut citation_corpus = String::new();
    if let Some(target) = req.target.as_deref() {
        citation_corpus.push_str(target);
        citation_corpus.push('\n');
    }
    if let Some(citation_text) = req.citation_text.as_deref() {
        citation_corpus.push_str(citation_text);
    }
    let citation_lower = citation_corpus.to_ascii_lowercase();

    let total = candidates.len();
    let mut scored = Vec::with_capacity(total);
    for (idx, cand) in candidates.into_iter().enumerate() {
        let non_global_scopes: Vec<String> = cand
            .explicit_scopes
            .iter()
            .filter(|s| !is_global_wildcard(s))
            .cloned()
            .collect();
        let diff_symbol_hit = (!diff_symbols.is_empty()
            && candidate_matches_diff_symbols(&cand.explicit_symbols, &diff_symbols))
            || cand.item_heading_ref.as_ref().is_some_and(|heading| {
                diff_symbol_sections.contains(&(cand.rel_path.clone(), heading.clone()))
            });
        let diff_scope_hit = !diff_paths_vec.is_empty()
            && !non_global_scopes.is_empty()
            && scopes_overlap(&diff_paths_vec, &non_global_scopes);
        let diff_edge_hit = diff_linked_paths.contains(&cand.rel_path);

        let tier_base =
            if !citation_lower.trim().is_empty() && is_direct_citation(&cand, &citation_lower) {
                1_u64 << 56
            } else if diff_symbol_hit || diff_scope_hit || diff_edge_hit {
                let diff_subtier = if diff_symbol_hit {
                    1_u64 << 44
                } else if diff_scope_hit {
                    1_u64 << 43
                } else {
                    0
                };
                (1_u64 << 48) + diff_subtier
            } else if !req.admitted_scope.is_empty()
                && !non_global_scopes.is_empty()
                && scopes_overlap(&req.admitted_scope, &non_global_scopes)
            {
                1_u64 << 42
            } else if scope_linked_paths.contains(&cand.rel_path) {
                1_u64 << 28
            } else if cand.is_global_fallback {
                1_u64 << 14
            } else {
                continue;
            };
        let tie_break = u64::try_from(total - idx).unwrap_or(1);
        scored.push((tier_base + cand.priority_bonus + tie_break, cand.payload));
    }
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));

    Ok(budget_scored_items(kind, scored, max_bytes))
}

pub(super) fn parse_frontmatter_metadata(markdown: &str) -> FrontmatterMeta {
    let mut meta = FrontmatterMeta::default();
    let Some(rest) = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
    else {
        return meta;
    };
    let Some((header, _)) = rest.split_once("\n---") else {
        return meta;
    };
    let Ok(yaml) = serde_yaml::from_str::<Value>(header) else {
        return meta;
    };
    let non_empty_str = |key: &str| {
        yaml.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    meta.id = non_empty_str("id");
    meta.title = non_empty_str("title");
    push_json_scope_values(yaml.get("scope"), &mut meta.scopes);
    push_json_scope_values(yaml.get("owners"), &mut meta.scopes);
    meta
}

pub(super) fn first_markdown_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(super) fn is_global_wildcard(pattern: &str) -> bool {
    matches!(pattern.trim(), "**" | "*" | "**/*")
}

pub(super) fn is_archived_doc_rel(rel_path: &str) -> bool {
    rel_path
        .split('/')
        .any(|seg| seg == "archive" || (seg.starts_with('.') && seg != "." && seg != ".."))
}

pub(super) fn any_glob_matches(globs: &[String], rel_path: &str) -> bool {
    globs.iter().any(|pattern| {
        expand_glob_braces(pattern).into_iter().any(|variant| {
            glob_matches(&variant, rel_path)
                || variant
                    .strip_prefix("**/")
                    .is_some_and(|rest| glob_matches(rest, rel_path))
        })
    })
}

pub(super) fn classify_path_candidate(
    rel: &str,
    frontmatter_scopes: &[String],
    owner_agents: &BTreeMap<String, bool>,
) -> (Vec<String>, bool, u64) {
    if rel == "AGENTS.md" {
        return (
            if frontmatter_scopes.is_empty() {
                vec!["**".to_owned()]
            } else {
                frontmatter_scopes.to_vec()
            },
            frontmatter_scopes.is_empty()
                || frontmatter_scopes.iter().any(|s| is_global_wildcard(s)),
            0,
        );
    }
    if rel.ends_with("/AGENTS.md") {
        let is_owner = owner_agents.get(rel).copied().unwrap_or(false);
        let bonus = if is_owner { 500 } else { 0 };
        if frontmatter_scopes.is_empty() {
            if let Some((dir, _)) = rel.rsplit_once('/') {
                return (vec![format!("{dir}/**")], false, bonus);
            }
        }
        return (
            frontmatter_scopes.to_vec(),
            frontmatter_scopes.iter().any(|s| is_global_wildcard(s)),
            bonus,
        );
    }
    if rel == "TESTS.md" {
        if frontmatter_scopes.is_empty() {
            return (vec!["tests/**".to_owned(), "TESTS.md".to_owned()], false, 200);
        }
        return (
            frontmatter_scopes.to_vec(),
            frontmatter_scopes.iter().any(|s| is_global_wildcard(s)),
            200,
        );
    }
    let is_global =
        frontmatter_scopes.is_empty() || frontmatter_scopes.iter().any(|s| is_global_wildcard(s));
    (frontmatter_scopes.to_vec(), is_global, 0)
}

pub(super) fn push_json_scope_values(val: Option<&Value>, out: &mut Vec<String>) {
    match val {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
                out.push(trimmed.to_owned());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                push_json_scope_values(Some(item), out);
            }
        }
        _ => {}
    }
}

fn is_generic_basename_stem(stem: &str) -> bool {
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "AGENTS" | "TESTS" | "README" | "INDEX" | "MOD" | "LIB"
    )
}

pub(super) fn build_doc_id_tokens(rel_path: &str, explicit_id: Option<&str>) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(id) = explicit_id.map(str::trim).filter(|s| !s.is_empty()) {
        tokens.push(id.to_owned());
    }
    if let Some(stem) = Path::new(rel_path).file_stem().and_then(|s| s.to_str()) {
        if stem.len() >= 4
            && !is_generic_basename_stem(stem)
            && !tokens.iter().any(|t| t == stem)
        {
            tokens.push(stem.to_owned());
        }
        let prefix = stem.split('-').next().unwrap_or_default();
        if prefix.len() >= 4
            && prefix.bytes().all(|b| b.is_ascii_digit())
            && rel_path.starts_with("docs/adr/")
        {
            for adr_id in [format!("ADR-{prefix}"), format!("ADR {prefix}")] {
                if !tokens.iter().any(|t| t == &adr_id) {
                    tokens.push(adr_id);
                }
            }
        }
    }
    tokens
}

fn is_citation_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'#')
}

fn contains_ascii_ci_word(haystack_lower: &str, needle: &str) -> bool {
    let needle_lower = needle.trim().to_ascii_lowercase();
    if needle_lower.is_empty() {
        return false;
    }
    let bytes = haystack_lower.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack_lower[start..].find(&needle_lower) {
        let abs = start + pos;
        let end = abs + needle_lower.len();
        let left_ok = abs == 0
            || (!is_citation_ident_char(bytes[abs - 1]) && bytes[abs - 1] != b'.');
        let right_ok = end == bytes.len()
            || (!is_citation_ident_char(bytes[end])
                && !(bytes[end] == b'.'
                    && bytes.get(end + 1).is_some_and(u8::is_ascii_alphanumeric)));
        if left_ok && right_ok {
            return true;
        }
        start = abs + haystack_lower[abs..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn is_direct_citation(cand: &RankedDocCandidate, citation_lower: &str) -> bool {
    let is_generic_root_doc = matches!(cand.rel_path.as_str(), "AGENTS.md" | "TESTS.md" | "README.md");
    if !is_generic_root_doc && contains_ascii_ci_word(citation_lower, &cand.rel_path) {
        return true;
    }
    if cand
        .id_tokens
        .iter()
        .any(|token| contains_ascii_ci_word(citation_lower, token))
    {
        return true;
    }
    cand.title
        .as_deref()
        .is_some_and(|title| title.len() >= 4 && contains_ascii_ci_word(citation_lower, title))
}

fn candidate_matches_diff_symbols(
    explicit_symbols: &[String],
    diff_symbols: &BTreeSet<String>,
) -> bool {
    explicit_symbols.iter().any(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return false;
        }
        let dotted = trimmed.replace("::", ".");
        let suffix = format!(".{dotted}");
        diff_symbols
            .iter()
            .any(|sym| sym == trimmed || sym == &dotted || sym.ends_with(&suffix))
    })
}

type DocEdgeMatches = (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<(String, String)>,
);

fn collect_code_map_doc_edge_paths(
    conn: &Connection,
    candidate_paths: &BTreeSet<String>,
    admitted_scope: &[String],
    diff_paths: &BTreeSet<String>,
    changed_hashes: &BTreeSet<i64>,
    policy: &WorkspacePathPolicy,
) -> Result<DocEdgeMatches> {
    let mut scope_linked = BTreeSet::new();
    let mut diff_linked = BTreeSet::new();
    let mut symbol_sections = BTreeSet::new();
    if admitted_scope.is_empty() && diff_paths.is_empty() {
        return Ok((scope_linked, diff_linked, symbol_sections));
    }
    let candidates_json = serde_json::to_string(candidate_paths)?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT p_src.path, n_src.kind, n_src.name, e.src_hash, p_dst.path, n_dst.kind, n_dst.name, e.dst_hash
         FROM edges e
         JOIN nodes n_src ON n_src.node_hash = e.src_hash
         JOIN path_dictionary p_src ON p_src.path_id = n_src.path_id
         JOIN nodes n_dst ON n_dst.node_hash = e.dst_hash
         JOIN path_dictionary p_dst ON p_dst.path_id = n_dst.path_id
         WHERE e.kind IN ('documents','references_doc','cites_doc','governed_by','governs')
           AND (
             p_src.path IN (SELECT value FROM json_each(?1))
             OR p_dst.path IN (SELECT value FROM json_each(?1))
           )",
    )?;
    let rows = stmt.query_map([candidates_json], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;
    let mut record_endpoint =
        |doc_path: &str, doc_kind: &str, doc_name: &str, peer_path: &str, peer_hash: i64| {
            if !candidate_paths.contains(doc_path) {
                return;
            }
            if !changed_hashes.is_empty()
                && changed_hashes.contains(&peer_hash)
                && doc_kind == "doc_section"
            {
                symbol_sections.insert((doc_path.to_owned(), doc_name.to_owned()));
            }
            if !diff_paths.is_empty() && diff_paths.contains(peer_path) {
                diff_linked.insert(doc_path.to_owned());
            }
            if !admitted_scope.is_empty() && policy.is_scope_admitted(peer_path, admitted_scope) {
                scope_linked.insert(doc_path.to_owned());
            }
        };
    for (src_path, src_kind, src_name, src_hash, dst_path, dst_kind, dst_name, dst_hash) in
        rows.flatten()
    {
        record_endpoint(&src_path, &src_kind, &src_name, &dst_path, dst_hash);
        record_endpoint(&dst_path, &dst_kind, &dst_name, &src_path, src_hash);
    }
    Ok((scope_linked, diff_linked, symbol_sections))
}
