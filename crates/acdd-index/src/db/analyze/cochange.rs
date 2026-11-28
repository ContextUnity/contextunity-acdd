//! Bounded Git co-change analyzer over `code-map.sqlite` and `acdd_core::git`.

use super::common::{page_slice, path_in_scope, NON_DEP_EDGES};
use crate::db::{
    commitments::{read_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES},
    paging::{self, QueryBudget},
    reader::selectors::strip_selector_uri_prefix,
    source::PathExposureFilter,
};
use acdd_core::{
    config, git,
    path_policy::WorkspacePathPolicy,
    response::{QueryOptions, MAX_PAGE_SIZE},
};
use anyhow::{bail, ensure, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

const MAX_COCHANGE_COMMITS: usize = 200;
const MAX_COCHANGE_FILES_PER_COMMIT: usize = 30;
const MAX_COCHANGE_LOG_BYTES: usize = MAX_QUERY_RESULT_BYTES;

fn cochange_fingerprint(
    graph_generation: &str,
    head_oid: Option<&str>,
    base: Option<&str>,
    base_oid: Option<&str>,
    scope: &str,
    policy_fingerprint: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"acdd-analyze-cochange-v2");
    for part in [
        graph_generation,
        head_oid.unwrap_or(""),
        base.unwrap_or(""),
        base_oid.unwrap_or(""),
        scope,
        policy_fingerprint,
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn load_static_file_pairs(
    conn: &Connection,
    paths: &BTreeSet<String>,
    budget: &QueryBudget<'_>,
) -> Result<HashSet<(String, String)>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    budget.check()?;
    let paths_json = serde_json::to_string(&paths.iter().collect::<Vec<_>>())?;
    let sql = format!(
        "SELECT DISTINCT sp.path, dp.path \
         FROM edges e \
         JOIN nodes s ON s.node_hash=e.src_hash \
         JOIN path_dictionary sp ON sp.path_id=s.path_id \
         JOIN nodes d ON d.node_hash=e.dst_hash \
         JOIN path_dictionary dp ON dp.path_id=d.path_id \
         WHERE e.kind NOT IN ({NON_DEP_EDGES}) \
           AND sp.path!=dp.path \
           AND sp.path IN (SELECT value FROM json_each(?1)) \
           AND dp.path IN (SELECT value FROM json_each(?1))"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([&paths_json])?;
    let mut linked = HashSet::new();
    while let Some(row) = rows.next()? {
        let a: String = row.get(0)?;
        let b: String = row.get(1)?;
        linked.insert(if a < b { (a, b) } else { (b, a) });
    }
    Ok(linked)
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

pub(crate) struct CochangePairs {
    pub(crate) file_commits: HashMap<String, usize>,
    pub(crate) pair_counts: BTreeMap<(String, String), usize>,
    pub(crate) scanned_commits: usize,
    pub(crate) skipped_large_commits: usize,
}

/// Shared bounded Git co-change pair accumulator used by both `analyze_cochange` and `verify` Diff X-Ray.
pub(crate) fn collect_cochange_pairs(
    snapshot: &git::GitLogSnapshot,
    exposure: &PathExposureFilter,
    scope: &str,
    budget: &QueryBudget<'_>,
) -> Result<CochangePairs> {
    let mut rename_to_current: HashMap<String, String> = HashMap::new();
    let mut file_commits: HashMap<String, usize> = HashMap::new();
    let mut pair_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut scanned_commits = 0usize;
    let mut skipped_large_commits = 0usize;

    for commit in &snapshot.commits {
        budget.check()?;
        let is_large = commit.changes.len() > MAX_COCHANGE_FILES_PER_COMMIT;
        let mut exposed_files = BTreeSet::new();
        let mut commit_renames = Vec::new();
        for change in &commit.changes {
            let current = rename_to_current
                .get(&change.path)
                .cloned()
                .unwrap_or_else(|| change.path.clone());
            if change.status.starts_with('R') {
                if let Some(old_path) = &change.old_path {
                    commit_renames.push((old_path.clone(), current.clone()));
                }
            }
            if !is_large && exposure.is_allowed(&current) {
                exposed_files.insert(current);
            }
        }
        rename_to_current.extend(commit_renames);
        if is_large {
            skipped_large_commits += 1;
            continue;
        }
        scanned_commits += 1;
        let files: Vec<String> = exposed_files.into_iter().collect();
        for f in &files {
            *file_commits.entry(f.clone()).or_insert(0) += 1;
        }
        if files.len() < 2 {
            continue;
        }
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let (a, b) = (&files[i], &files[j]);
                if !scope.is_empty() && !path_in_scope(a, scope) && !path_in_scope(b, scope) {
                    continue;
                }
                *pair_counts.entry((a.clone(), b.clone())).or_insert(0) += 1;
            }
        }
    }
    Ok(CochangePairs {
        file_commits,
        pair_counts,
        scanned_commits,
        skipped_large_commits,
    })
}

/// Return top-`max_items` untouched co-change siblings (`(changed_file, untouched_sibling, count)`)
/// for Diff X-Ray diagnostics on passing `verify` runs.
pub fn collect_untouched_cochange_siblings(
    conn: &Connection,
    root: &Path,
    changed_paths: &[String],
    policy: Option<&WorkspacePathPolicy>,
    max_items: usize,
) -> Result<Vec<(String, String, usize)>> {
    if changed_paths.is_empty() || max_items == 0 {
        return Ok(Vec::new());
    }
    let adapter = config::load_adapter(root, None)?;
    let exposure = PathExposureFilter::new(root, &adapter, policy)?;
    let snapshot = git::collect_bounded_log_name_status(
        root,
        None,
        MAX_COCHANGE_COMMITS,
        MAX_COCHANGE_LOG_BYTES,
    )?;
    let budget = QueryBudget::new(conn);
    let pairs = collect_cochange_pairs(&snapshot, &exposure, "", &budget)?;
    let changed_set: HashSet<&str> = changed_paths.iter().map(String::as_str).collect();
    let mut siblings: Vec<(String, String, usize)> = Vec::new();
    for ((a, b), count) in pairs.pair_counts {
        if count < 2 {
            continue;
        }
        let a_changed = changed_set.contains(a.as_str());
        let b_changed = changed_set.contains(b.as_str());
        if a_changed && !b_changed {
            siblings.push((a, b, count));
        } else if b_changed && !a_changed {
            siblings.push((b, a, count));
        }
    }
    // Also surface untouched siblings that share graph callees (`kind = 'calls'`) with changed files
    // using a single batched query, filtering changed/unexposed siblings before ranking and truncation.
    let changed_json = serde_json::to_string(changed_paths)?;
    let mut callee_stmt = conn.prepare(
        "SELECT p_chg.path, p_sib.path, COUNT(DISTINCT e1.dst_hash) AS shared_callees
         FROM json_each(?1) chg
         JOIN path_dictionary p_chg ON p_chg.path = chg.value
         JOIN nodes n_chg ON n_chg.path_id = p_chg.path_id
         JOIN edges e1 ON e1.src_hash = n_chg.node_hash AND e1.kind = 'calls'
         JOIN edges e2 ON e2.dst_hash = e1.dst_hash AND e2.kind = 'calls'
         JOIN nodes n_sib ON n_sib.node_hash = e2.src_hash
         JOIN path_dictionary p_sib ON p_sib.path_id = n_sib.path_id
         WHERE p_sib.path NOT IN (SELECT value FROM json_each(?1))
         GROUP BY p_chg.path, p_sib.path
         ORDER BY shared_callees DESC, p_sib.path ASC",
    )?;
    let mut rows = callee_stmt.query([changed_json.as_str()])?;
    while let Some(row) = rows.next()? {
        let changed: String = row.get(0)?;
        let sib_path: String = row.get(1)?;
        let shared: usize = row.get(2)?;
        if shared > 0
            && !changed_set.contains(sib_path.as_str())
            && exposure.is_allowed(&sib_path)
        {
            siblings.push((changed, sib_path, shared));
        }
    }
    siblings.sort_by(|x, y| y.2.cmp(&x.2).then_with(|| x.1.cmp(&y.1)));
    let mut seen_sibling = HashSet::new();
    siblings.retain(|(_, sib, _)| seen_sibling.insert(sib.clone()));
    siblings.truncate(max_items);
    Ok(siblings)
}

pub(super) fn analyze_cochange(
    conn: &Connection,
    root: &Path,
    target: &str,
    base: Option<&str>,
    policy: Option<&WorkspacePathPolicy>,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    ensure!(
        (1..=MAX_PAGE_SIZE).contains(&options.limit),
        "limit must be between 1 and {MAX_PAGE_SIZE}"
    );
    let scope = strip_selector_uri_prefix(target.trim())
        .trim_end_matches('/')
        .to_owned();
    let scope = if scope == "." { String::new() } else { scope };

    let adapter = config::load_adapter(root, None)?;
    let exposure = PathExposureFilter::new(root, &adapter, policy)?;
    let snapshot = git::collect_bounded_log_name_status(
        root,
        base,
        MAX_COCHANGE_COMMITS,
        MAX_COCHANGE_LOG_BYTES,
    )?;
    let graph_generation = read_metadata(conn, MetadataKey::OutputRoot)?;
    let fingerprint = cochange_fingerprint(
        &graph_generation,
        snapshot.head_oid.as_deref(),
        base,
        snapshot.base_oid.as_deref(),
        &scope,
        exposure.fingerprint(),
    );
    if let Some(previous) = options.generation.as_deref() {
        ensure!(
            previous.len() >= 8 && fingerprint.starts_with(previous),
            "Git cochange history or policy changed; restart at offset 0 without generation"
        );
    }
    if options.offset > 0 && options.generation.is_none() {
        bail!("continuation requires generation from the previous page");
    }

    let CochangePairs {
        file_commits,
        pair_counts,
        scanned_commits,
        skipped_large_commits,
    } = collect_cochange_pairs(&snapshot, &exposure, &scope, budget)?;

    let candidate_paths: BTreeSet<String> = pair_counts
        .keys()
        .flat_map(|(a, b)| [a.clone(), b.clone()])
        .collect();
    let static_linked = load_static_file_pairs(conn, &candidate_paths, budget)?;

    let mut unlinked: Vec<(String, String, usize, usize, usize, f64)> = pair_counts
        .into_iter()
        .filter(|(pair, _)| !static_linked.contains(pair))
        .map(|((a, b), count)| {
            let ca = file_commits.get(&a).copied().unwrap_or(count);
            let cb = file_commits.get(&b).copied().unwrap_or(count);
            let conf = round6(count as f64 / ca.min(cb).max(1) as f64);
            (a, b, count, ca, cb, conf)
        })
        .collect();
    unlinked.sort_by(|x, y| {
        y.2.cmp(&x.2)
            .then_with(|| y.5.total_cmp(&x.5))
            .then_with(|| (&x.0, &x.1).cmp(&(&y.0, &y.1)))
    });

    ensure!(
        git::git_head_oid(root)? == snapshot.head_oid,
        "Git HEAD changed while computing cochange; restart at offset 0"
    );
    if let Some(rev) = base.map(str::trim).filter(|s| !s.is_empty()) {
        ensure!(
            Some(git::resolve_git_commit(root, rev)?) == snapshot.base_oid,
            "Git base ref changed while computing cochange; restart at offset 0"
        );
    }
    let total = unlinked.len();
    let items: Vec<Value> = page_slice(&unlinked, options)
        .iter()
        .map(|(a, b, count, ca, cb, conf)| {
            json!({
                "file_a": a,
                "file_b": b,
                "cochange_count": count,
                "commits_a": ca,
                "commits_b": cb,
                "confidence": conf,
                "static_edge": false,
            })
        })
        .collect();

    Ok(json!({
        "mode": "cochange",
        "target": scope,
        "base": base,
        "summary": {
            "commits_scanned": scanned_commits,
            "commits_skipped_large": skipped_large_commits,
            "max_commits": MAX_COCHANGE_COMMITS,
            "max_files_per_commit": MAX_COCHANGE_FILES_PER_COMMIT,
            "unlinked_pairs": total,
        },
        "pairs": paging::value(items, total, options, &fingerprint),
        "generation": fingerprint,
        "graph_generation": graph_generation,
    }))
}
