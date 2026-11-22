use super::{LintScope, LintViolation};
use crate::db::reader;
use crate::models::{tags::NodeKind, StatementFingerprint};
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::Result;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub(crate) struct FileDuplicateGroup {
    pub(crate) digest: String,
    pub(crate) size_bytes: u64,
    pub(crate) locations: Vec<String>,
}

pub(crate) struct DuplicateCandidate {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
    pub(crate) path: String,
}

#[derive(Clone, Copy)]
pub(crate) struct DuplicateLocation {
    pub(crate) candidate_idx: usize,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
}

pub(crate) struct DuplicateGroup {
    pub(crate) rep_idx: usize,
    pub(crate) line_count: usize,
    pub(crate) match_type: &'static str,
    pub(crate) locations: Vec<DuplicateLocation>,
    pub(crate) duplicated_lines: usize,
}

pub(crate) struct SymbolDuplicateReport {
    pub(crate) candidates: Vec<DuplicateCandidate>,
    pub(crate) groups: Vec<DuplicateGroup>,
    pub(crate) total_duplicated_lines: usize,
    pub(crate) stale_files: BTreeSet<String>,
}

pub(crate) fn format_duplicate_suggestion(name: &str, paths: &[&str]) -> String {
    if paths.iter().any(|p| p.ends_with("build.rs")) {
        return format!(
            "Consider extracting '{name}' into a shared build module (e.g. crates/acdd-index/build_manifests.rs) and referencing it from build scripts."
        );
    }
    if paths.iter().any(|p| p.starts_with("crates/acdd-core")) {
        return format!(
            "Consider centralizing '{name}' in 'crates/acdd-core' and re-exporting it in dependent crates."
        );
    }
    let mut unique_crates = HashSet::new();
    for p in paths {
        let crate_name = if let Some(stripped) = p.strip_prefix("crates/") {
            stripped.split('/').next().unwrap_or("")
        } else if p.starts_with("src/") {
            "contextunity-acdd"
        } else if p.starts_with("tests/") {
            "tests"
        } else {
            p.split('/').next().unwrap_or("")
        };
        unique_crates.insert(crate_name);
    }
    if unique_crates.len() > 1 {
        format!(
            "Consolidate duplicate function '{name}' across multiple modules into a shared crate or module (e.g. 'crates/acdd-core') and re-export."
        )
    } else {
        format!(
            "Consider extracting duplicate function '{name}' into a shared helper module in the same crate."
        )
    }
}

fn compute_statement_similarity(a: &[StatementFingerprint], b: &[StatementFingerprint]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let min_len = a.len().min(b.len());
    let matching = (0..min_len)
        .filter(|&i| a[i].hash == b[i].hash && a[i].token_count == b[i].token_count)
        .count();
    matching as f64 / a.len().max(b.len()) as f64
}

fn hash_statement_slice(stmts: &[StatementFingerprint]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for stmt in stmts {
        hasher.update(stmt.hash.to_le_bytes());
        hasher.update(stmt.token_count.to_le_bytes());
    }
    hasher.finalize().into()
}

#[derive(Clone, Copy)]
struct StmtSpan {
    candidate_idx: usize,
    stmt_start: usize,
    stmt_end: usize,
}

fn non_overlapping_window_occs(occs: &[(usize, usize)], window_size: usize) -> Vec<(usize, usize)> {
    let mut by_candidate: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(candidate_idx, stmt_start) in occs {
        by_candidate.entry(candidate_idx).or_default().push(stmt_start);
    }
    let mut selected = Vec::new();
    for (candidate_idx, mut starts) in by_candidate {
        starts.sort_unstable();
        let mut sym_selected: Vec<usize> = Vec::new();
        for start in starts {
            if sym_selected
                .iter()
                .any(|&s| start.abs_diff(s) < window_size)
            {
                continue;
            }
            sym_selected.push(start);
        }
        for start in sym_selected {
            selected.push((candidate_idx, start));
        }
    }
    selected
}

struct PreparedDupCandidate {
    body_hash: Option<[u8; 32]>,
    statements: Vec<StatementFingerprint>,
}

fn dedup_sort_and_count_locations(
    candidates: &[DuplicateCandidate],
    mut raw: Vec<DuplicateLocation>,
    counted_duplicate_locations: &mut HashSet<(String, usize)>,
) -> Option<(Vec<DuplicateLocation>, usize)> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for loc in raw.drain(..) {
        if seen.insert((candidates[loc.candidate_idx].path.as_str(), loc.line)) {
            unique.push(loc);
        }
    }
    if unique.len() < 2 {
        return None;
    }
    unique.sort_by(|a, b| {
        candidates[a.candidate_idx]
            .path
            .cmp(&candidates[b.candidate_idx].path)
            .then_with(|| a.line.cmp(&b.line))
    });
    let dup_count = unique[1..]
        .iter()
        .filter(|loc| {
            counted_duplicate_locations
                .insert((candidates[loc.candidate_idx].path.clone(), loc.line))
        })
        .count();
    Some((unique, dup_count))
}

fn push_symbol_group(
    candidates: &[DuplicateCandidate],
    indices: &[usize],
    match_type: &'static str,
    counted_duplicate_locations: &mut HashSet<(String, usize)>,
    whole_symbol_duplicate_members: &mut HashSet<usize>,
    result_groups: &mut Vec<DuplicateGroup>,
) {
    let raw: Vec<DuplicateLocation> = indices
        .iter()
        .map(|&i| DuplicateLocation {
            candidate_idx: i,
            line: candidates[i].line,
            end_line: candidates[i].end_line,
        })
        .collect();
    let Some((locations, dup_line_locations)) =
        dedup_sort_and_count_locations(candidates, raw, counted_duplicate_locations)
    else {
        return;
    };
    let rep_idx = locations[0].candidate_idx;
    let first = &candidates[rep_idx];
    let line_count = first.end_line.saturating_sub(first.line) + 1;
    for loc in &locations {
        whole_symbol_duplicate_members.insert(loc.candidate_idx);
    }
    result_groups.push(DuplicateGroup {
        rep_idx,
        line_count,
        match_type,
        locations,
        duplicated_lines: line_count * dup_line_locations,
    });
}

pub(crate) fn collect_file_duplicates(
    conn: &Connection,
    scope: &str,
) -> Result<(Vec<FileDuplicateGroup>, u64)> {
    let (lower, upper) = reader::path_bounds(scope);
    let mut stmt = conn.prepare(
        "SELECT f.digest, f.size, f.path
         FROM files f
         WHERE (?1 = '' OR f.path = ?1 OR (f.path >= ?2 AND f.path < ?3))
           AND f.digest IN (
               SELECT digest FROM files
               WHERE (?1 = '' OR path = ?1 OR (path >= ?2 AND path < ?3))
               GROUP BY digest HAVING count(*) > 1
           )
         ORDER BY f.size DESC, f.digest, f.path",
    )?;
    let mut groups: BTreeMap<String, (u64, Vec<String>)> = BTreeMap::new();
    let mut rows = stmt.query(rusqlite::params![scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let digest: String = row.get(0)?;
        let size: u64 = row.get(1)?;
        let p: String = row.get(2)?;
        groups.entry(digest).or_insert((size, Vec::new())).1.push(p);
    }
    drop(rows);
    drop(stmt);

    let mut file_groups = Vec::new();
    let mut total_duplicated_bytes: u64 = 0;
    for (digest, (size_bytes, mut locations)) in groups {
        if locations.len() < 2 {
            continue;
        }
        locations.sort();
        total_duplicated_bytes += size_bytes * (locations.len() as u64 - 1);
        file_groups.push(FileDuplicateGroup {
            digest,
            size_bytes,
            locations,
        });
    }
    file_groups.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| b.locations.len().cmp(&a.locations.len()))
            .then_with(|| a.digest.cmp(&b.digest))
    });
    Ok((file_groups, total_duplicated_bytes))
}

fn load_statement_fingerprints_in_scope(
    conn: &Connection,
    scope: &str,
    lower: &str,
    upper: &str,
    needed_paths: &HashSet<String>,
) -> Result<HashMap<(String, String), Vec<StatementFingerprint>>> {
    let mut fp_by_node: HashMap<(String, String), Vec<StatementFingerprint>> = HashMap::new();
    if needed_paths.is_empty() {
        return Ok(fp_by_node);
    }
    let mut stmt = conn.prepare(
        "SELECT path, facts_blob
         FROM local_facts
         WHERE (?1 = '' OR path = ?1 OR (path >= ?2 AND path < ?3))",
    )?;
    let mut rows = stmt.query(rusqlite::params![scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if !needed_paths.contains(&path) {
            continue;
        }
        let blob: Vec<u8> = row.get(1)?;
        let Ok(typed) = crate::typed_facts::decode_facts(&blob) else {
            continue;
        };
        for entry in typed.facts.statement_fingerprints {
            fp_by_node.insert((path.clone(), entry.node_id), entry.statements);
        }
    }
    Ok(fp_by_node)
}

fn coalesce_block_window_matches(
    raw_candidates: &[DuplicateCandidate],
    prepared: &[PreparedDupCandidate],
    whole_symbol_duplicate_members: &HashSet<usize>,
    window_size: usize,
    counted_duplicate_locations: &mut HashSet<(String, usize)>,
    result_groups: &mut Vec<DuplicateGroup>,
) {
    let mut block_hashes: BTreeMap<[u8; 32], Vec<(usize, usize)>> = BTreeMap::new();
    for (idx, p) in prepared.iter().enumerate() {
        if p.statements.len() < window_size {
            continue;
        }
        for w_start in 0..=p.statements.len().saturating_sub(window_size) {
            let hash = hash_statement_slice(&p.statements[w_start..w_start + window_size]);
            block_hashes.entry(hash).or_default().push((idx, w_start));
        }
    }

    let mut buckets: BTreeMap<Vec<usize>, Vec<Vec<StmtSpan>>> = BTreeMap::new();
    for (_hash, occs) in block_hashes {
        let filtered = non_overlapping_window_occs(&occs, window_size);
        if filtered.len() < 2
            || filtered
                .iter()
                .all(|(idx, _)| whole_symbol_duplicate_members.contains(idx))
        {
            continue;
        }
        let mut seen_locs: HashSet<(&str, usize)> = HashSet::new();
        let mut spans: Vec<StmtSpan> = Vec::with_capacity(filtered.len());
        for (candidate_idx, stmt_start) in filtered {
            let path = raw_candidates[candidate_idx].path.as_str();
            let line = prepared[candidate_idx].statements[stmt_start].line;
            if seen_locs.insert((path, line)) {
                spans.push(StmtSpan {
                    candidate_idx,
                    stmt_start,
                    stmt_end: stmt_start + window_size,
                });
            }
        }
        if spans.len() < 2 {
            continue;
        }
        spans.sort_by(|a, b| {
            a.candidate_idx
                .cmp(&b.candidate_idx)
                .then_with(|| a.stmt_start.cmp(&b.stmt_start))
        });
        let cand_key: Vec<usize> = spans.iter().map(|s| s.candidate_idx).collect();
        buckets.entry(cand_key).or_default().push(spans);
    }

    for (_, mut group_list) in buckets {
        group_list.sort_by(|a, b| {
            a.iter()
                .map(|s| s.stmt_start)
                .cmp(b.iter().map(|s| s.stmt_start))
        });
        let mut merged: Vec<Vec<StmtSpan>> = Vec::new();
        for next in group_list {
            if let Some(prev) = merged.iter_mut().find(|prev| {
                prev.len() == next.len()
                    && prev
                        .iter()
                        .zip(&next)
                        .all(|(p, n)| n.stmt_start == p.stmt_end - window_size + 1)
            }) {
                for p in prev.iter_mut() {
                    p.stmt_end += 1;
                }
            } else {
                merged.push(next);
            }
        }
        for spans in merged {
            let raw_locs = spans
                .into_iter()
                .map(|span| {
                    let stmts = &prepared[span.candidate_idx].statements;
                    DuplicateLocation {
                        candidate_idx: span.candidate_idx,
                        line: stmts[span.stmt_start].line,
                        end_line: stmts[span.stmt_end - 1].end_line,
                    }
                })
                .collect();
            let Some((locations, dup_line_locations)) = dedup_sort_and_count_locations(
                raw_candidates,
                raw_locs,
                counted_duplicate_locations,
            ) else {
                continue;
            };
            let line_count = locations[0]
                .end_line
                .saturating_sub(locations[0].line)
                .saturating_add(1);
            result_groups.push(DuplicateGroup {
                rep_idx: locations[0].candidate_idx,
                line_count,
                match_type: "block",
                locations,
                duplicated_lines: line_count * dup_line_locations,
            });
        }
    }
}

pub(crate) fn collect_symbol_duplicates(
    conn: &Connection,
    scope: &str,
    min_lines: usize,
) -> Result<SymbolDuplicateReport> {
    let min_stmts = min_lines.max(1);
    let (lower, upper) = reader::path_bounds(scope);

    let mut stmt = conn.prepare(&format!(
        "SELECT n.id, n.name, n.kind, n.line, n.end_line, p.path
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind IN ('{function}', '{method}')
           AND (?1 = '' OR p.path = ?1 OR (p.path >= ?2 AND p.path < ?3))
         ORDER BY p.path, n.line",
        function = NodeKind::Function.as_str(),
        method = NodeKind::Method.as_str(),
    ))?;

    let mut needed_paths: HashSet<String> = HashSet::new();
    let mut node_ids: Vec<String> = Vec::new();
    let mut raw_candidates = Vec::new();

    let mut rows = stmt.query(rusqlite::params![scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let line: usize = row.get(3)?;
        let end_line: usize = row.get(4)?;
        let path: String = row.get(5)?;
        needed_paths.insert(path.clone());
        node_ids.push(id);
        raw_candidates.push(DuplicateCandidate {
            name,
            kind,
            line,
            end_line,
            path,
        });
    }
    drop(rows);
    drop(stmt);

    let mut fp_by_node =
        load_statement_fingerprints_in_scope(conn, scope, &lower, &upper, &needed_paths)?;

    let prepared: Vec<PreparedDupCandidate> = raw_candidates
        .iter()
        .zip(node_ids)
        .map(|(raw, node_id)| {
            let statements = fp_by_node
                .remove(&(raw.path.clone(), node_id))
                .unwrap_or_default();
            let body_hash =
                (statements.len() >= min_stmts).then(|| hash_statement_slice(&statements));
            PreparedDupCandidate {
                body_hash,
                statements,
            }
        })
        .collect();

    let mut exact_groups: BTreeMap<[u8; 32], Vec<usize>> = BTreeMap::new();
    for (idx, c) in prepared.iter().enumerate() {
        if let Some(hash) = c.body_hash {
            exact_groups.entry(hash).or_default().push(idx);
        }
    }

    let mut result_groups = Vec::new();
    let mut counted_duplicate_locations: HashSet<(String, usize)> = HashSet::new();
    let mut whole_symbol_duplicate_members: HashSet<usize> = HashSet::new();

    for (_, indices) in exact_groups {
        if indices.len() >= 2 {
            push_symbol_group(
                &raw_candidates,
                &indices,
                "exact",
                &mut counted_duplicate_locations,
                &mut whole_symbol_duplicate_members,
                &mut result_groups,
            );
        }
    }

    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, p) in prepared.iter().enumerate() {
        if p.body_hash.is_some() {
            by_name
                .entry(raw_candidates[idx].name.as_str())
                .or_default()
                .push(idx);
        }
    }

    for (_name, indices) in by_name {
        if indices.len() < 2 {
            continue;
        }
        let mut hash_to_candidates: BTreeMap<[u8; 32], Vec<usize>> = BTreeMap::new();
        for &i in &indices {
            if let Some(h) = prepared[i].body_hash {
                hash_to_candidates.entry(h).or_default().push(i);
            }
        }
        if hash_to_candidates.len() < 2 {
            continue;
        }
        let mut hash_clusters: Vec<Vec<[u8; 32]>> = Vec::new();
        for (&h, inds) in &hash_to_candidates {
            let stmts_rep = &prepared[inds[0]].statements;
            let mut clustered = false;
            for cluster in &mut hash_clusters {
                let cluster_rep_idx = hash_to_candidates[&cluster[0]][0];
                let cluster_rep_stmts = &prepared[cluster_rep_idx].statements;
                if compute_statement_similarity(stmts_rep, cluster_rep_stmts) >= 0.80 {
                    cluster.push(h);
                    clustered = true;
                    break;
                }
            }
            if !clustered {
                hash_clusters.push(vec![h]);
            }
        }

        for cluster in hash_clusters {
            if cluster.len() < 2 {
                continue;
            }
            let cluster_indices: Vec<usize> = cluster
                .iter()
                .flat_map(|h| hash_to_candidates[h].iter().copied())
                .collect();
            push_symbol_group(
                &raw_candidates,
                &cluster_indices,
                "similar",
                &mut counted_duplicate_locations,
                &mut whole_symbol_duplicate_members,
                &mut result_groups,
            );
        }
    }

    coalesce_block_window_matches(
        &raw_candidates,
        &prepared,
        &whole_symbol_duplicate_members,
        min_stmts,
        &mut counted_duplicate_locations,
        &mut result_groups,
    );

    result_groups.sort_by(|a, b| {
        b.duplicated_lines
            .cmp(&a.duplicated_lines)
            .then_with(|| b.locations.len().cmp(&a.locations.len()))
            .then_with(|| raw_candidates[a.rep_idx].name.cmp(&raw_candidates[b.rep_idx].name))
            .then_with(|| a.match_type.cmp(b.match_type))
            .then_with(|| {
                let path_a = a
                    .locations
                    .first()
                    .map_or("", |l| raw_candidates[l.candidate_idx].path.as_str());
                let path_b = b
                    .locations
                    .first()
                    .map_or("", |l| raw_candidates[l.candidate_idx].path.as_str());
                path_a.cmp(path_b)
            })
            .then_with(|| {
                let line_a = a.locations.first().map_or(0, |l| l.line);
                let line_b = b.locations.first().map_or(0, |l| l.line);
                line_a.cmp(&line_b)
            })
    });

    let total_duplicated_lines: usize = result_groups.iter().map(|g| g.duplicated_lines).sum();
    Ok(SymbolDuplicateReport {
        candidates: raw_candidates,
        groups: result_groups,
        total_duplicated_lines,
        stale_files: BTreeSet::new(),
    })
}

pub(super) fn collect_duplicate_violations(
    conn: &Connection,
    scope: &LintScope,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let sql_scope = scope.exact_prefix().unwrap_or("");
    let report = collect_symbol_duplicates(conn, sql_scope, 5)?;
    for g in report.groups {
        let rep = &report.candidates[g.rep_idx];
        let loc_paths: Vec<&str> = g
            .locations
            .iter()
            .map(|l| report.candidates[l.candidate_idx].path.as_str())
            .collect();
        let any_in_scope = loc_paths.iter().any(|p| scope.in_scope(p));
        if !any_in_scope {
            continue;
        }
        let first_loc = g.locations.first().copied().unwrap_or(DuplicateLocation {
            candidate_idx: g.rep_idx,
            line: rep.line,
            end_line: rep.end_line,
        });
        let first_path = &report.candidates[first_loc.candidate_idx].path;
        let suggestion = format_duplicate_suggestion(&rep.name, &loc_paths);
        violations.push(LintViolation {
            category: LintCategory::Duplicates,
            rule_id: format!("duplicates.{}", g.match_type),
            severity: LintSeverity::Warn,
            path: first_path.clone(),
            line: Some(first_loc.line),
            message: format!(
                "{} duplicate of '{}' ({} lines across {} locations)",
                g.match_type,
                rep.name,
                g.line_count,
                g.locations.len()
            ),
            invariant: None,
            rule_source: None,
            symbol: Some(rep.name.clone()),
            target: None,
            suggestion: Some(suggestion),
        });
    }
    Ok(())
}
