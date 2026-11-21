use super::{normalize_scope, page_bounds};
use crate::db::{
    commitments::{read_metadata_optional, MetadataKey},
    paging, reader, symbols,
};
use crate::models::tags::NodeKind;
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn compute_line_similarity(a: &[&str], b: &[&str]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let min_len = a.len().min(b.len());
    let matching = (0..min_len).filter(|&i| a[i] == b[i]).count();
    matching as f64 / a.len().max(b.len()) as f64
}

fn format_suggestion(name: &str, paths: &[&str]) -> String {
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

fn hash_line_slice(lines: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

fn normalize_symbol_body_lines<'a>(
    lines: &[&'a str],
) -> (Option<String>, Option<Vec<&'a str>>, usize) {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    let (Some(start), Some(end)) = (start, end) else {
        return (None, None, 0);
    };
    if start > end {
        return (None, None, start);
    }
    let slice = &lines[start..=end];
    let hash_hex = hash_line_slice(slice);

    let min_indent = slice
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);

    let common_indent = if min_indent > 0 {
        slice
            .iter()
            .find(|l| !l.trim().is_empty())
            .map(|l| {
                let byte_end = l
                    .char_indices()
                    .nth(min_indent)
                    .map_or(l.len(), |(idx, _)| idx);
                &l[..byte_end]
            })
            .unwrap_or("")
    } else {
        ""
    };

    let normalized = slice
        .iter()
        .map(|line| {
            if !common_indent.is_empty()
                && !line.trim().is_empty()
                && line.starts_with(common_indent)
            {
                &line[common_indent.len()..]
            } else {
                *line
            }
        })
        .collect();

    (Some(hash_hex), Some(normalized), start)
}

#[derive(Clone, Copy)]
struct DuplicateBlockWindow {
    candidate_idx: usize,
    window_start: usize,
    abs_line: usize,
    abs_end_line: usize,
}

fn duplicate_block_windows_overlap(
    a: DuplicateBlockWindow,
    b: DuplicateBlockWindow,
    min_lines: usize,
) -> bool {
    if a.candidate_idx != b.candidate_idx {
        return false;
    }
    let (lo, hi) = if a.window_start <= b.window_start {
        (a.window_start, b.window_start)
    } else {
        (b.window_start, a.window_start)
    };
    hi.saturating_sub(lo) < min_lines
}

fn non_overlapping_block_windows(
    occs: &[DuplicateBlockWindow],
    min_lines: usize,
) -> Vec<DuplicateBlockWindow> {
    let mut by_candidate: BTreeMap<usize, Vec<DuplicateBlockWindow>> = BTreeMap::new();
    for &occ in occs {
        by_candidate.entry(occ.candidate_idx).or_default().push(occ);
    }
    let mut selected = Vec::new();
    for (_, mut sym_occs) in by_candidate {
        sym_occs.sort_by_key(|o| o.window_start);
        let mut sym_selected = Vec::new();
        for occ in sym_occs {
            if sym_selected
                .iter()
                .any(|s| duplicate_block_windows_overlap(*s, occ, min_lines))
            {
                continue;
            }
            sym_selected.push(occ);
        }
        selected.extend(sym_selected);
    }
    selected
}

struct FileDuplicateGroup {
    digest: String,
    size_bytes: u64,
    locations: Vec<String>,
}

fn file_duplicates_paged(
    conn: &Connection,
    scope: &str,
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
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
    let (total, slice) = page_bounds(&file_groups, options);
    let paged_items: Vec<Value> = slice
        .iter()
        .map(|g| {
            let duplicate_count = g.locations.len();
            json!({
                "digest": g.digest,
                "size_bytes": g.size_bytes,
                "duplicate_count": duplicate_count,
                "locations": g.locations,
                "suggestion": format!(
                    "Consolidate duplicate files across {duplicate_count} locations into a single shared file or replace with re-exports."
                ),
            })
        })
        .collect();
    let page = paging::value(paged_items, total, options, generation);

    Ok(json!({
        "operation": "duplicates",
        "level": "file",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "total_duplicate_groups": total,
        "total_duplicated_bytes": total_duplicated_bytes,
        "items": page["items"],
        "total": page["total"],
        "offset": page["offset"],
        "limit": page["limit"],
        "has_more": page["has_more"],
        "next_offset": page["next_offset"],
        "generation": page["generation"],
    }))
}

struct RawCandidate {
    name: String,
    kind: String,
    line: usize,
    end_line: usize,
    path: String,
}

struct Candidate<'a> {
    name: String,
    kind: String,
    line: usize,
    end_line: usize,
    path: String,
    body_hash: Option<String>,
    normalized_lines: Option<Vec<&'a str>>,
    body_trim_start: usize,
}

#[derive(Clone, Copy)]
struct DuplicateLocation {
    candidate_idx: usize,
    line: usize,
    end_line: usize,
}

struct DuplicateGroup {
    rep_idx: usize,
    line_count: usize,
    match_type: &'static str,
    locations: Vec<DuplicateLocation>,
    duplicated_lines: usize,
}

fn push_symbol_group(
    candidates: &[Candidate<'_>],
    indices: &[usize],
    match_type: &'static str,
    counted_duplicate_locations: &mut HashSet<(String, usize)>,
    whole_symbol_duplicate_members: &mut HashSet<usize>,
    result_groups: &mut Vec<DuplicateGroup>,
) {
    let mut unique_locs = Vec::new();
    let mut seen_locs = HashSet::new();
    for &idx in indices {
        let c = &candidates[idx];
        if seen_locs.insert((c.path.as_str(), c.line)) {
            unique_locs.push(idx);
        }
    }
    if unique_locs.len() < 2 {
        return;
    }
    unique_locs.sort_by(|&a, &b| {
        candidates[a]
            .path
            .cmp(&candidates[b].path)
            .then_with(|| candidates[a].line.cmp(&candidates[b].line))
    });
    let mut dup_line_locations = 0;
    for &idx in &unique_locs[1..] {
        let c = &candidates[idx];
        if counted_duplicate_locations.insert((c.path.clone(), c.line)) {
            dup_line_locations += 1;
        }
    }
    let first = &candidates[unique_locs[0]];
    let line_count = first.end_line.saturating_sub(first.line) + 1;
    let locations = unique_locs
        .iter()
        .map(|&i| DuplicateLocation {
            candidate_idx: i,
            line: candidates[i].line,
            end_line: candidates[i].end_line,
        })
        .collect();
    for &idx in &unique_locs {
        whole_symbol_duplicate_members.insert(idx);
    }
    result_groups.push(DuplicateGroup {
        rep_idx: unique_locs[0],
        line_count,
        match_type,
        locations,
        duplicated_lines: line_count * dup_line_locations,
    });
}

/// Identifies duplicate files, functions, methods, and intra-function blocks with bounded paging.
pub fn duplicates_paged(
    conn: &Connection,
    target: Option<&str>,
    path: Option<&str>,
    level: Option<&str>,
    min_lines: Option<usize>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let scope_owned = normalize_scope(target, path, "target")?;
    let scope = scope_owned.as_str();

    let level_str = level.unwrap_or("symbol").trim().to_ascii_lowercase();
    anyhow::ensure!(
        level_str == "symbol" || level_str == "file",
        "invalid level '{level_str}': expected 'symbol' or 'file'"
    );

    let min_lines = min_lines.unwrap_or(5).max(1);
    if level_str == "file" {
        return file_duplicates_paged(conn, scope, options, &generation);
    }

    let (lower, upper) = reader::path_bounds(scope);
    let workspace_root =
        read_metadata_optional(conn, MetadataKey::WorkspaceRoot)?.map(std::path::PathBuf::from);

    let mut stmt = conn.prepare(&format!(
        "SELECT n.name, n.kind, n.line, n.end_line, p.path
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind IN ('{function}', '{method}')
           AND (n.end_line - n.line + 1) >= ?1
           AND (?2 = '' OR p.path = ?2 OR (p.path >= ?3 AND p.path < ?4))
         ORDER BY p.path, n.line",
        function = NodeKind::Function.as_str(),
        method = NodeKind::Method.as_str(),
    ))?;

    let mut file_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut source_loader = symbols::VerifiedSourceCache::new();
    let mut stale_files: BTreeSet<String> = BTreeSet::new();
    let mut raw_candidates = Vec::new();

    let mut rows = stmt.query(rusqlite::params![min_lines, scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let line: usize = row.get(2)?;
        let end_line: usize = row.get(3)?;
        let path: String = row.get(4)?;

        if let Some(root) = &workspace_root {
            let content = file_cache.entry(path.clone()).or_insert_with(|| {
                source_loader.read(conn, root, &path).ok()
            });
            if content.is_none() {
                stale_files.insert(path.clone());
            }
        }
        raw_candidates.push(RawCandidate {
            name,
            kind,
            line,
            end_line,
            path,
        });
    }
    drop(rows);
    drop(stmt);

    let file_lines: HashMap<&str, Vec<&str>> = file_cache
        .iter()
        .filter_map(|(path, content)| {
            content
                .as_deref()
                .map(|text| (path.as_str(), text.lines().collect()))
        })
        .collect();

    let candidates: Vec<Candidate<'_>> = raw_candidates
        .into_iter()
        .map(|raw| {
            let mut body_hash = None;
            let mut normalized_lines = None;
            let mut body_trim_start = 0;
            if let Some(lines) = file_lines.get(raw.path.as_str()) {
                if raw.line > 0 && raw.end_line >= raw.line && raw.line <= lines.len() {
                    let end_idx = raw.end_line.min(lines.len());
                    let slice = &lines[(raw.line - 1)..end_idx];
                    (body_hash, normalized_lines, body_trim_start) =
                        normalize_symbol_body_lines(slice);
                }
            }
            Candidate {
                name: raw.name,
                kind: raw.kind,
                line: raw.line,
                end_line: raw.end_line,
                path: raw.path,
                body_hash,
                normalized_lines,
                body_trim_start,
            }
        })
        .collect();

    let mut exact_groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        if let Some(hash) = c.body_hash.as_deref() {
            exact_groups.entry(hash).or_default().push(idx);
        }
    }

    let mut result_groups = Vec::new();
    let mut counted_duplicate_locations: HashSet<(String, usize)> = HashSet::new();
    let mut whole_symbol_duplicate_members: HashSet<usize> = HashSet::new();

    for (_, indices) in exact_groups {
        if indices.len() >= 2 {
            push_symbol_group(
                &candidates,
                &indices,
                "exact",
                &mut counted_duplicate_locations,
                &mut whole_symbol_duplicate_members,
                &mut result_groups,
            );
        }
    }

    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        if c.normalized_lines.is_some() {
            by_name.entry(c.name.as_str()).or_default().push(idx);
        }
    }

    for (_name, indices) in by_name {
        if indices.len() < 2 {
            continue;
        }
        let mut hash_to_candidates: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for &i in &indices {
            if let Some(h) = candidates[i].body_hash.as_deref() {
                hash_to_candidates.entry(h).or_default().push(i);
            }
        }
        if hash_to_candidates.len() < 2 {
            continue;
        }

        let mut hash_clusters: Vec<Vec<&str>> = Vec::new();
        for (h, inds) in &hash_to_candidates {
            let lines_rep = candidates[inds[0]].normalized_lines.as_deref().unwrap();
            let mut clustered = false;
            for cluster in &mut hash_clusters {
                let cluster_rep_idx = hash_to_candidates[cluster[0]][0];
                let cluster_rep_lines = candidates[cluster_rep_idx]
                    .normalized_lines
                    .as_deref()
                    .unwrap();
                if compute_line_similarity(lines_rep, cluster_rep_lines) >= 0.80 {
                    cluster.push(*h);
                    clustered = true;
                    break;
                }
            }
            if !clustered {
                hash_clusters.push(vec![*h]);
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
                &candidates,
                &cluster_indices,
                "similar",
                &mut counted_duplicate_locations,
                &mut whole_symbol_duplicate_members,
                &mut result_groups,
            );
        }
    }

    let mut block_hashes: BTreeMap<String, Vec<DuplicateBlockWindow>> = BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        let Some(lines) = c.normalized_lines.as_deref() else {
            continue;
        };
        if lines.len() < min_lines {
            continue;
        }
        for window_start in 0..=lines.len().saturating_sub(min_lines) {
            let hash = hash_line_slice(&lines[window_start..window_start + min_lines]);
            let abs_line = c.line + c.body_trim_start + window_start;
            block_hashes
                .entry(hash)
                .or_default()
                .push(DuplicateBlockWindow {
                    candidate_idx: idx,
                    window_start,
                    abs_line,
                    abs_end_line: abs_line + min_lines - 1,
                });
        }
    }

    for (_hash, occs) in block_hashes {
        let filtered = non_overlapping_block_windows(&occs, min_lines);
        if filtered.len() < 2 {
            continue;
        }
        let mut unique_locs = Vec::new();
        let mut seen_locs = HashSet::new();
        for occ in filtered {
            let c = &candidates[occ.candidate_idx];
            if seen_locs.insert((c.path.as_str(), occ.abs_line)) {
                unique_locs.push(occ);
            }
        }
        if unique_locs.len() < 2
            || unique_locs
                .iter()
                .all(|occ| whole_symbol_duplicate_members.contains(&occ.candidate_idx))
        {
            continue;
        }
        unique_locs.sort_by(|a, b| {
            candidates[a.candidate_idx]
                .path
                .cmp(&candidates[b.candidate_idx].path)
                .then_with(|| a.abs_line.cmp(&b.abs_line))
        });
        let mut dup_line_locations = 0;
        for occ in &unique_locs[1..] {
            let c = &candidates[occ.candidate_idx];
            if counted_duplicate_locations.insert((c.path.clone(), occ.abs_line)) {
                dup_line_locations += 1;
            }
        }
        let locations = unique_locs
            .iter()
            .map(|occ| DuplicateLocation {
                candidate_idx: occ.candidate_idx,
                line: occ.abs_line,
                end_line: occ.abs_end_line,
            })
            .collect();
        result_groups.push(DuplicateGroup {
            rep_idx: unique_locs[0].candidate_idx,
            line_count: min_lines,
            match_type: "block",
            locations,
            duplicated_lines: min_lines * dup_line_locations,
        });
    }

    result_groups.sort_by(|a, b| {
        b.duplicated_lines
            .cmp(&a.duplicated_lines)
            .then_with(|| b.locations.len().cmp(&a.locations.len()))
            .then_with(|| candidates[a.rep_idx].name.cmp(&candidates[b.rep_idx].name))
            .then_with(|| a.match_type.cmp(b.match_type))
            .then_with(|| {
                let path_a = a
                    .locations
                    .first()
                    .map_or("", |l| candidates[l.candidate_idx].path.as_str());
                let path_b = b
                    .locations
                    .first()
                    .map_or("", |l| candidates[l.candidate_idx].path.as_str());
                path_a.cmp(path_b)
            })
            .then_with(|| {
                let line_a = a.locations.first().map_or(0, |l| l.line);
                let line_b = b.locations.first().map_or(0, |l| l.line);
                line_a.cmp(&line_b)
            })
    });

    let total_duplicated_lines: usize = result_groups.iter().map(|g| g.duplicated_lines).sum();
    let (total, slice) = page_bounds(&result_groups, options);
    let paged_items: Vec<Value> = slice
        .iter()
        .map(|g| {
            let rep = &candidates[g.rep_idx];
            let loc_paths: Vec<&str> = g
                .locations
                .iter()
                .map(|l| candidates[l.candidate_idx].path.as_str())
                .collect();
            let locations: Vec<Value> = g
                .locations
                .iter()
                .map(|l| {
                    json!({
                        "path": candidates[l.candidate_idx].path,
                        "line": l.line,
                        "end_line": l.end_line,
                    })
                })
                .collect();
            let suggestion = format_suggestion(&rep.name, &loc_paths);
            json!({
                "name": rep.name,
                "kind": rep.kind,
                "line_count": g.line_count,
                "match_type": g.match_type,
                "locations": locations,
                "duplicate_count": g.locations.len(),
                "duplicated_lines": g.duplicated_lines,
                "suggestion": suggestion,
            })
        })
        .collect();
    let page = paging::value(paged_items, total, options, &generation);

    let mut response = json!({
        "operation": "duplicates",
        "level": "symbol",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "min_lines": min_lines,
        "total_duplicate_groups": total,
        "total_duplicated_lines": total_duplicated_lines,
        "items": page["items"],
        "total": page["total"],
        "offset": page["offset"],
        "limit": page["limit"],
        "has_more": page["has_more"],
        "next_offset": page["next_offset"],
        "generation": page["generation"],
    });

    if !stale_files.is_empty() {
        response["stale_files"] = json!(stale_files);
    }

    Ok(response)
}
