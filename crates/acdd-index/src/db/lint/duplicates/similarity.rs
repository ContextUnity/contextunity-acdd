use super::provenance::{
    is_trivial_trait_mapper, load_shadowed_paths, load_standard_mapper_method_ids,
};
use super::types::{
    effective_stmt_hash, path_matches_probe, DuplicateCandidate, DuplicateGroup, DuplicateLocation,
    DuplicateScanRequest, ReuseHint, SymbolDuplicateReport,
};
use crate::db::lint::types::{is_archived_path, is_test_context_path, load_test_line_spans};
use crate::models::{details::NodeDetailKey, tags::NodeKind, StatementFingerprint};
use anyhow::Result;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn lcs_statement_len(
    a: &[StatementFingerprint],
    b: &[StatementFingerprint],
    use_skeleton: bool,
) -> usize {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut prev = vec![0usize; short.len() + 1];
    let mut curr = vec![0usize; short.len() + 1];
    for x in long {
        let xh = effective_stmt_hash(x, use_skeleton);
        for (j, y) in short.iter().enumerate() {
            curr[j + 1] = if xh == effective_stmt_hash(y, use_skeleton)
                && x.token_count == y.token_count
            {
                prev[j] + 1
            } else {
                curr[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.fill(0);
    }
    prev[short.len()]
}

fn jaccard_statement_similarity(
    a: &[StatementFingerprint],
    b: &[StatementFingerprint],
    use_skeleton: bool,
) -> f64 {
    let mut counts: HashMap<(u64, usize), usize> = HashMap::with_capacity(a.len());
    for s in a {
        *counts
            .entry((effective_stmt_hash(s, use_skeleton), s.token_count))
            .or_default() += 1;
    }
    let mut intersection = 0usize;
    for s in b {
        if let Some(rem) = counts.get_mut(&(effective_stmt_hash(s, use_skeleton), s.token_count)) {
            if *rem > 0 {
                *rem -= 1;
                intersection += 1;
            }
        }
    }
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn compute_statement_similarity(
    a: &[StatementFingerprint],
    b: &[StatementFingerprint],
    use_skeleton: bool,
) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let max_len = a.len().max(b.len());
    let min_len = a.len().min(b.len());
    if min_len * 10 < max_len * 4 {
        return 0.0;
    }
    let lcs_ratio = lcs_statement_len(a, b, use_skeleton) as f64 / max_len as f64;
    let jaccard = jaccard_statement_similarity(a, b, use_skeleton);
    0.5 * lcs_ratio + 0.5 * jaccard
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
        by_candidate
            .entry(candidate_idx)
            .or_default()
            .push(stmt_start);
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

fn load_statement_fingerprints_for_paths(
    conn: &Connection,
    needed_paths: &HashSet<String>,
) -> Result<HashMap<(String, String), Vec<StatementFingerprint>>> {
    let mut fp_by_node: HashMap<(String, String), Vec<StatementFingerprint>> = HashMap::new();
    if needed_paths.is_empty() {
        return Ok(fp_by_node);
    }
    let mut stmt = conn.prepare("SELECT path, facts_blob FROM local_facts")?;
    let mut rows = stmt.query([])?;
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
    // Hydrate non-committed literal-masked skeleton hashes when `statement_skeletons` exists
    // (gracefully absent in synthetic in-memory DBs).
    if let Ok(mut skel_stmt) = conn.prepare("SELECT path, skeleton_blob FROM statement_skeletons") {
        if let Ok(mut skel_rows) = skel_stmt.query([]) {
            while let Ok(Some(row)) = skel_rows.next() {
                let Ok(path) = row.get::<_, String>(0) else {
                    continue;
                };
                if !needed_paths.contains(&path) {
                    continue;
                }
                let Ok(blob) = row.get::<_, Vec<u8>>(1) else {
                    continue;
                };
                let Some(decoded) = crate::typed_facts::decode_statement_skeletons(&blob) else {
                    continue;
                };
                for (node_id, skel_hashes) in decoded {
                    if let Some(stmts) = fp_by_node.get_mut(&(path.clone(), node_id)) {
                        for (stmt, skel) in stmts.iter_mut().zip(skel_hashes) {
                            stmt.skeleton_hash = skel;
                        }
                    }
                }
            }
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
    req: &DuplicateScanRequest<'_>,
) -> Result<SymbolDuplicateReport> {
    let min_stmts = req.min_lines.max(1);
    let include_tests = is_test_context_path(req.scope)
        || req
            .probe_paths
            .is_some_and(|paths| !paths.is_empty() && paths.iter().all(|p| is_test_context_path(p)));
    let test_spans = if include_tests {
        HashMap::new()
    } else {
        load_test_line_spans(conn)?
    };

    let shadowed_paths = load_shadowed_paths(conn)?;
    let standard_mapper_methods = load_standard_mapper_method_ids(conn, &shadowed_paths)?;

    let mut stmt = conn.prepare(&format!(
        "SELECT n.id, n.name, n.qualname, n.kind, n.line, n.end_line, p.path,
                COALESCE(json_extract(n.details, '$.signature'), ''),
                (SELECT COUNT(DISTINCT e.src_hash) FROM edges e WHERE e.dst_hash = n.node_hash AND e.kind = 'calls') AS inbound_callers,
                COALESCE(
                    json_extract(n.details, '$.{impl_trait}'),
                    (SELECT json_extract(owner.details, '$.{impl_trait}')
                     FROM edges e_own
                     JOIN nodes owner ON owner.node_hash = e_own.src_hash
                     WHERE e_own.dst_hash = n.node_hash
                       AND e_own.kind = 'contains'
                       AND owner.kind = '{impl_kind}'
                     LIMIT 1)
                ) AS resolved_impl_trait
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         LEFT JOIN files f ON f.path = p.path
         WHERE n.kind IN ('{function}', '{method}')
           AND (?1 = 1 OR (n.is_test = 0 AND n.generated = 0 AND COALESCE(f.is_test, 0) = 0))
         ORDER BY p.path, n.line",
        impl_trait = NodeDetailKey::IMPL_TRAIT,
        impl_kind = NodeKind::Impl.as_str(),
        function = NodeKind::Function.as_str(),
        method = NodeKind::Method.as_str(),
    ))?;

    let mut needed_paths: HashSet<String> = HashSet::new();
    let mut node_ids: Vec<String> = Vec::new();
    let mut raw_candidates = Vec::new();
    let mut is_probe = Vec::new();

    let mapped = stmt.query_map(rusqlite::params![i64::from(include_tests)], |row| {
        let id: String = row.get(0)?;
        let is_standard_mapper_impl = standard_mapper_methods.contains(&id);
        Ok((
            id,
            DuplicateCandidate {
                name: row.get(1)?,
                qualname: row.get(2)?,
                kind: row.get(3)?,
                line: row.get(4)?,
                end_line: row.get(5)?,
                path: row.get(6)?,
                signature: row.get(7)?,
                inbound_callers: row.get(8)?,
                impl_trait: row.get(9)?,
                is_standard_mapper_impl,
            },
        ))
    })?;
    for entry in mapped {
        let (id, cand) = entry?;
        if is_archived_path(&cand.path)
            || (!include_tests
                && (is_test_context_path(&cand.path)
                    || test_spans
                        .get(&cand.path)
                        .is_some_and(|spans| {
                            spans.iter().any(|&(s, e)| cand.line >= s && cand.line <= e)
                        })))
        {
            continue;
        }
        let probe_hit = path_matches_probe(&cand.path, req.scope, req.probe_paths);
        needed_paths.insert(cand.path.clone());
        node_ids.push(id);
        is_probe.push(probe_hit);
        raw_candidates.push(cand);
    }
    drop(stmt);

    let mut fp_by_node = load_statement_fingerprints_for_paths(conn, &needed_paths)?;

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
        if indices.len() >= 2 && indices.iter().any(|&i| is_probe[i]) {
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

    let mut stmt_index: HashMap<(u64, usize), Vec<usize>> = HashMap::new();
    let mut similar_pool: Vec<usize> = Vec::new();
    for (idx, p) in prepared.iter().enumerate() {
        if p.body_hash.is_some() {
            similar_pool.push(idx);
            let mut seen_stmt = HashSet::new();
            for s in &p.statements {
                let key = (effective_stmt_hash(s, true), s.token_count);
                if seen_stmt.insert(key) {
                    stmt_index.entry(key).or_default().push(idx);
                }
            }
        }
    }
    let mut clustered: HashSet<usize> = HashSet::new();
    let mut reuse_hints: Vec<ReuseHint> = Vec::new();
    for &rep_idx in &similar_pool {
        if !is_probe[rep_idx] || clustered.contains(&rep_idx) {
            continue;
        }
        let rep_stmts = &prepared[rep_idx].statements;
        let rep_trivial = is_trivial_trait_mapper(
            raw_candidates[rep_idx].is_standard_mapper_impl,
            rep_stmts.len(),
        );
        let mut overlap_counts: HashMap<usize, usize> = HashMap::new();
        for s in rep_stmts {
            let key = (effective_stmt_hash(s, true), s.token_count);
            if let Some(cands) = stmt_index.get(&key) {
                if cands.len() > 64 {
                    continue;
                }
                for &cand_idx in cands {
                    if cand_idx != rep_idx && !clustered.contains(&cand_idx) {
                        *overlap_counts.entry(cand_idx).or_default() += 1;
                    }
                }
            }
        }
        let mut group = vec![rep_idx];
        let mut best_hint: Option<(usize, f64)> = None;
        let mut candidates_to_score: Vec<usize> = overlap_counts
            .into_iter()
            .filter_map(|(cand_idx, shared)| {
                let max_len = rep_stmts.len().max(prepared[cand_idx].statements.len());
                (shared * 10 >= max_len * 4).then_some(cand_idx)
            })
            .collect();
        candidates_to_score.sort_unstable();
        for cand_idx in candidates_to_score {
            let cand_stmts = &prepared[cand_idx].statements;
            let use_skeleton = !rep_trivial
                && !is_trivial_trait_mapper(
                    raw_candidates[cand_idx].is_standard_mapper_impl,
                    cand_stmts.len(),
                );
            let exact_sim = compute_statement_similarity(rep_stmts, cand_stmts, false);
            let sim = if use_skeleton {
                exact_sim.max(compute_statement_similarity(rep_stmts, cand_stmts, true))
            } else {
                exact_sim
            };
            let min_len = rep_stmts.len().min(cand_stmts.len());
            let max_len = rep_stmts.len().max(cand_stmts.len());
            if sim >= 0.80 && min_len * 10 >= max_len * 6 {
                group.push(cand_idx);
            } else if sim >= 0.45
                && raw_candidates[cand_idx].path != raw_candidates[rep_idx].path
            {
                let better = best_hint.is_none_or(|(prev_idx, prev_sim)| {
                    (raw_candidates[cand_idx].inbound_callers, (sim * 1000.0) as usize)
                        > (
                            raw_candidates[prev_idx].inbound_callers,
                            (prev_sim * 1000.0) as usize,
                        )
                });
                if better {
                    best_hint = Some((cand_idx, sim));
                }
            }
        }
        let rep_hash = prepared[rep_idx].body_hash;
        if group.len() >= 2 && group.iter().any(|&m| prepared[m].body_hash != rep_hash) {
            for &m in &group {
                clustered.insert(m);
            }
            push_symbol_group(
                &raw_candidates,
                &group,
                "similar",
                &mut counted_duplicate_locations,
                &mut whole_symbol_duplicate_members,
                &mut result_groups,
            );
        } else if let Some((owner_idx, sim)) = best_hint {
            let probe = &raw_candidates[rep_idx];
            let owner = &raw_candidates[owner_idx];
            reuse_hints.push(ReuseHint {
                probe_name: probe.name.clone(),
                probe_path: probe.path.clone(),
                probe_line: probe.line,
                owner_name: owner.name.clone(),
                owner_path: owner.path.clone(),
                owner_line: owner.line,
                owner_signature: owner.signature.clone(),
                inbound_callers: owner.inbound_callers,
                similarity: sim,
            });
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
    result_groups.retain(|g| g.locations.iter().any(|l| is_probe[l.candidate_idx]));

    reuse_hints.sort_by(|a, b| {
        b.inbound_callers
            .cmp(&a.inbound_callers)
            .then_with(|| b.similarity.total_cmp(&a.similarity))
            .then_with(|| a.owner_path.cmp(&b.owner_path))
    });
    reuse_hints.truncate(5);

    result_groups.sort_by(|a, b| {
        b.duplicated_lines
            .cmp(&a.duplicated_lines)
            .then_with(|| b.locations.len().cmp(&a.locations.len()))
            .then_with(|| {
                raw_candidates[a.rep_idx]
                    .name
                    .cmp(&raw_candidates[b.rep_idx].name)
            })
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
    let candidate_statements = prepared.into_iter().map(|p| p.statements).collect();
    Ok(SymbolDuplicateReport {
        candidates: raw_candidates,
        candidate_statements,
        groups: result_groups,
        reuse_hints,
        total_duplicated_lines,
        stale_files: BTreeSet::new(),
    })
}
