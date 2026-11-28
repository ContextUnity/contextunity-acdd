use super::types::{
    decode_edge_descriptor, edge_endpoints, predecessor_endpoints, resolve_traversal_kinds,
    reverse_predecessor_union, ImpactDirection, ReachedNodeDto, TraversalMode, TraversalOptions,
    ViaPredecessorDto, MAX_DEEP_TRAVERSAL_FRONTIER,
};
use crate::db::{
    commitments::{self, read_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES},
    paging::{self, QueryBudget},
    reader::{selectors::strip_selector_uri_prefix, sql::rows},
    source, symbols,
};
use crate::models::{
    details::NodeDetailKey,
    stable_hash64,
    tags::{EdgeKind, NodeKind},
};
use acdd_core::{
    config, git,
    path_policy::WorkspacePathPolicy,
    response::{Detail, QueryOptions},
};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Component, Path},
};

const MAX_CHANGE_FILES: usize = 100;
const MAX_CHANGE_SEEDS: usize = 64;
const MAX_CHANGE_RESULT_NODES: usize = 10_000;
const MAX_GIT_DIFF_BYTES: usize = MAX_QUERY_RESULT_BYTES;

#[derive(Clone, Debug)]
struct ChangedRange {
    path: String,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct ParsedHunk {
    added_range: Option<(usize, usize)>,
    deleted_only: bool,
}

struct HunkState {
    added_range: Option<(usize, usize)>,
    removed: bool,
    added: bool,
    old_remaining: usize,
    new_remaining: usize,
}

#[derive(Clone, Debug)]
struct ChangedFile {
    status: String,
    path: String,
    old_path: Option<String>,
    ranges: Vec<ChangedRange>,
}

struct ChangeSnapshot {
    mode: String,
    files: Vec<ChangedFile>,
    ranges: Vec<ChangedRange>,
    unmapped: Vec<Value>,
    fingerprint: String,
    exposure: source::PathExposureFilter,
}

fn excluded_ids_csv(ids: &[i64]) -> Option<String> {
    (!ids.is_empty()).then(|| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","))
}

fn append_fingerprint(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn git_bytes(root: &Path, args: &[String], context: &str) -> Result<Vec<u8>> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = git::git_command(root, &args)?;
    if !output.status.success() {
        bail!(
            "Git {context} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_diff_prefix(mode: &str, base_oid: Option<&str>) -> Vec<String> {
    let mut args = vec!["diff".to_owned()];
    if let Some(oid) = base_oid {
        args.push(oid.to_owned());
        return args;
    }
    match mode {
        "staged" => args.push("--cached".to_owned()),
        "head" => args.push("HEAD".to_owned()),
        _ => {}
    }
    args
}

fn valid_workspace_path(value: &str) -> bool {
    !value.is_empty()
        && config::safe_relative(Path::new(value))
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn path_is_within(path: &str, scope: Option<&str>) -> bool {
    scope.is_none_or(|scope| path == scope || path.starts_with(&format!("{scope}/")))
}

fn next_nul_field<'a>(bytes: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    if *offset >= bytes.len() {
        return None;
    }
    let rest = &bytes[*offset..];
    let length = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    *offset += length.saturating_add(1);
    Some(&rest[..length])
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<(String, String, Option<String>)>> {
    let mut offset = 0;
    let mut changes = Vec::new();
    while let Some(status) = next_nul_field(bytes, &mut offset) {
        if status.is_empty() {
            continue;
        }
        let status = std::str::from_utf8(status).context("Git returned a non-UTF-8 status")?;
        let first = next_nul_field(bytes, &mut offset)
            .context("Git returned an incomplete NUL-delimited change status")?;
        if status.starts_with('R') || status.starts_with('C') {
            let second = next_nul_field(bytes, &mut offset)
                .context("Git returned an incomplete rename status")?;
            let old_path = std::str::from_utf8(first)
                .context("Git change paths must be valid UTF-8")?
                .to_owned();
            let path = std::str::from_utf8(second)
                .context("Git change paths must be valid UTF-8")?
                .to_owned();
            changes.push((status.to_owned(), path, Some(old_path)));
        } else {
            let path = std::str::from_utf8(first)
                .context("Git change paths must be valid UTF-8")?
                .to_owned();
            changes.push((status.to_owned(), path, None));
        }
    }
    Ok(changes)
}

fn push_hunk(hunks: &mut Vec<ParsedHunk>, current: &mut Option<HunkState>) {
    if let Some(state) = current.take() {
        hunks.push(ParsedHunk {
            added_range: state.added.then_some(state.added_range).flatten(),
            deleted_only: state.removed && !state.added,
        });
    }
}

fn parse_hunk_ranges(patch: &[u8]) -> Vec<ParsedHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<HunkState> = None;
    let text = String::from_utf8_lossy(patch);
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("@@ -") {
            push_hunk(&mut hunks, &mut current);
            let Some((old_range, new_range)) = header.split_once(" +") else {
                continue;
            };
            let old_range = old_range.split_whitespace().next().unwrap_or_default();
            let (_, old_count) = old_range.split_once(',').unwrap_or((old_range, "1"));
            let Ok(old_remaining) = old_count.parse::<usize>() else {
                continue;
            };
            let new_range = new_range.split_whitespace().next().unwrap_or_default();
            let (start, count) = new_range
                .split_once(',')
                .map_or((new_range, "1"), |(start, count)| (start, count));
            let (Ok(start), Ok(new_remaining)) = (start.parse::<usize>(), count.parse::<usize>())
            else {
                continue;
            };
            current = Some(HunkState {
                added_range: if new_remaining > 0 {
                    start.checked_add(new_remaining - 1).map(|end| (start, end))
                } else {
                    None
                },
                removed: false,
                added: false,
                old_remaining,
                new_remaining,
            });
            if old_remaining == 0 && new_remaining == 0 {
                push_hunk(&mut hunks, &mut current);
            }
            continue;
        }
        if let Some(state) = current.as_mut() {
            match line.as_bytes().first() {
                Some(b'+') => {
                    state.added = true;
                    state.new_remaining = state.new_remaining.saturating_sub(1);
                }
                Some(b'-') => {
                    state.removed = true;
                    state.old_remaining = state.old_remaining.saturating_sub(1);
                }
                Some(b' ') => {
                    state.old_remaining = state.old_remaining.saturating_sub(1);
                    state.new_remaining = state.new_remaining.saturating_sub(1);
                }
                _ => {}
            }
            if state.old_remaining == 0 && state.new_remaining == 0 {
                push_hunk(&mut hunks, &mut current);
            }
        }
    }
    push_hunk(&mut hunks, &mut current);
    hunks
}

fn unmapped_change(path: &str, reason: &str, old_path: Option<&str>) -> Value {
    let mut item = json!({"path":path,"reason":reason});
    if let Some(old_path) = old_path {
        item["old_path"] = json!(old_path);
    }
    item
}

fn indexed_file_digest(conn: &Connection, path: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT digest FROM files WHERE path=?1", [path], |row| {
            row.get(0)
        })
        .optional()?)
}

fn range_has_indexed_symbol(
    conn: &Connection,
    path: &str,
    start: usize,
    end: usize,
) -> Result<bool> {
    let found: i64 = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.kind NOT IN('{module}','file') AND n.line<=?3 AND n.end_line>=?2)",
            module = NodeKind::Module.as_str(),
        ),
        rusqlite::params![path, i64::try_from(start)?, i64::try_from(end)?],
        |row| row.get(0),
    )?;
    Ok(found != 0)
}

fn worktree_postimage(root: &Path, adapter: &config::Adapter, path: &str) -> Result<Vec<u8>> {
    source::read_confined_source_bytes(
        root,
        adapter,
        path,
        config::MAX_FILE_BYTES,
        "change postimage unavailable",
        "change postimage exceeds the indexed file byte budget",
    )
}

fn collect_change_snapshot(
    conn: &Connection,
    root: &Path,
    mode: &str,
    path_scope: Option<&str>,
    base: Option<&str>,
    policy: Option<&WorkspacePathPolicy>,
    allow_tree_base: bool,
) -> Result<ChangeSnapshot> {
    anyhow::ensure!(
        matches!(mode, "staged" | "unstaged" | "head"),
        "changes must be staged, unstaged, or head"
    );
    if let Some(path) = path_scope {
        let valid = valid_workspace_path(path);
        anyhow::ensure!(valid, "path must be workspace-relative");
    }
    git::ensure_git_repo(root)?;
    let base_oid = base
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|rev| {
            if allow_tree_base {
                git::resolve_git_tree_or_commit(root, rev)
            } else {
                git::resolve_git_commit(root, rev)
            }
        })
        .transpose()?;
    let adapter = config::load_adapter(root, None)?;
    let exposure = source::PathExposureFilter::new(root, &adapter, policy)?;
    let graph_generation = read_metadata(conn, MetadataKey::OutputRoot)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(b"acdd-impact-git-change-view-v2");
    append_fingerprint(&mut fingerprint, graph_generation.as_bytes());
    append_fingerprint(&mut fingerprint, mode.as_bytes());
    append_fingerprint(
        &mut fingerprint,
        base_oid.as_deref().unwrap_or("").as_bytes(),
    );
    append_fingerprint(&mut fingerprint, path_scope.unwrap_or("").as_bytes());
    append_fingerprint(&mut fingerprint, exposure.fingerprint().as_bytes());

    if base_oid.is_none() && mode == "head" && git::git_head_oid(root)?.is_none() {
        return Ok(ChangeSnapshot {
            mode: mode.to_owned(),
            files: Vec::new(),
            ranges: Vec::new(),
            unmapped: Vec::new(),
            fingerprint: hex::encode(fingerprint.finalize()),
            exposure,
        });
    }

    let mut status_args = git_diff_prefix(mode, base_oid.as_deref());
    status_args.extend([
        "--name-status".to_owned(),
        "-z".to_owned(),
        "--find-renames".to_owned(),
    ]);
    if let Some(path) = path_scope {
        status_args.push("--".to_owned());
        status_args.push(path.to_owned());
    }
    let status_bytes = git_bytes(root, &status_args, "change status")?;
    let changes: Vec<_> = parse_name_status(&status_bytes)?
        .into_iter()
        .filter(|(_, path, _)| path_is_within(path, path_scope) && exposure.is_allowed(path))
        .map(|(status, path, old_path)| {
            let safe_old = old_path.filter(|old| exposure.is_allowed(old));
            (status, path, safe_old)
        })
        .collect();
    anyhow::ensure!(
        changes.len() <= MAX_CHANGE_FILES,
        "Git change view exceeds the {MAX_CHANGE_FILES}-file work budget"
    );

    let mut files = Vec::with_capacity(changes.len());
    let mut ranges = Vec::new();
    let mut unmapped = Vec::new();
    let mut total_diff_bytes = status_bytes.len();
    for (status, path, old_path) in changes {
        let mut patch_args = git_diff_prefix(mode, base_oid.as_deref());
        patch_args.extend([
            "--unified=0".to_owned(),
            "--no-ext-diff".to_owned(),
            "--no-textconv".to_owned(),
            "--no-color".to_owned(),
            "--find-renames".to_owned(),
            "--".to_owned(),
            path.clone(),
        ]);
        if matches!(status.as_bytes().first(), Some(b'R' | b'C')) {
            if let Some(old_path) = &old_path {
                patch_args.push(old_path.clone());
            }
        }
        let patch = git_bytes(root, &patch_args, "change diff")?;
        total_diff_bytes = total_diff_bytes.saturating_add(patch.len());
        anyhow::ensure!(
            total_diff_bytes <= MAX_GIT_DIFF_BYTES,
            "Git change diff exceeds the 8 MiB work budget"
        );
        let hunks = parse_hunk_ranges(&patch);
        let hunk_ranges = hunks
            .iter()
            .filter_map(|hunk| hunk.added_range)
            .collect::<Vec<_>>();
        let is_binary = patch
            .windows(b"GIT binary patch".len())
            .any(|window| window == b"GIT binary patch")
            || patch
                .windows(b"Binary files".len())
                .any(|window| window == b"Binary files");
        let postimage = if status.starts_with('D') {
            None
        } else if base_oid.is_none() && mode == "staged" {
            let object = format!(":0:{path}");
            let output = git::git_command(root, &["show", &object])?;
            output.status.success().then_some(output.stdout)
        } else {
            worktree_postimage(root, &adapter, &path).ok()
        };
        append_fingerprint(&mut fingerprint, status.as_bytes());
        append_fingerprint(&mut fingerprint, path.as_bytes());
        append_fingerprint(
            &mut fingerprint,
            old_path.as_deref().unwrap_or("").as_bytes(),
        );
        append_fingerprint(&mut fingerprint, &patch);
        append_fingerprint(&mut fingerprint, postimage.as_deref().unwrap_or_default());

        let file = ChangedFile {
            status: status.clone(),
            path: path.clone(),
            old_path: old_path.clone(),
            ranges: hunk_ranges
                .iter()
                .map(|(start, end)| ChangedRange {
                    path: path.clone(),
                    start: *start,
                    end: *end,
                })
                .collect(),
        };
        let indexed_digest = indexed_file_digest(conn, &path)?;
        let postimage_digest = postimage.as_deref().map(commitments::hash);
        let reason = if status.starts_with('D') {
            Some("deleted_range")
        } else if is_binary
            || postimage
                .as_ref()
                .is_some_and(|bytes| std::str::from_utf8(bytes).is_err())
        {
            Some("binary")
        } else if status.starts_with('R') && hunks.is_empty() {
            Some("rename_only")
        } else if hunks.is_empty() {
            Some("no_changed_lines")
        } else if postimage.is_none() {
            Some("unavailable_postimage")
        } else if indexed_digest.is_none() {
            Some("unindexed_path")
        } else if indexed_digest != postimage_digest {
            Some("snapshot_mismatch")
        } else {
            None
        };
        if let Some(reason) = reason {
            unmapped.push(unmapped_change(&path, reason, old_path.as_deref()));
        } else {
            for _ in hunks.iter().filter(|hunk| hunk.deleted_only) {
                unmapped.push(unmapped_change(&path, "deleted_range", old_path.as_deref()));
            }
            for range in &file.ranges {
                if range_has_indexed_symbol(conn, &range.path, range.start, range.end)? {
                    ranges.push(range.clone());
                } else {
                    let mut item = unmapped_change(&path, "no_indexed_symbol", old_path.as_deref());
                    item["start"] = json!(range.start);
                    item["end"] = json!(range.end);
                    unmapped.push(item);
                }
            }
        }
        files.push(file);
    }
    Ok(ChangeSnapshot {
        mode: mode.to_owned(),
        files,
        ranges,
        unmapped,
        fingerprint: hex::encode(fingerprint.finalize()),
        exposure,
    })
}

fn change_ranges_json(ranges: &[ChangedRange]) -> Result<String> {
    Ok(serde_json::to_string(
        &ranges
            .iter()
            .map(|range| json!({"path":range.path,"start":range.start,"end":range.end}))
            .collect::<Vec<_>>(),
    )?)
}

fn changed_range_from_sql() -> String {
    format!(
        "json_each(?1) r JOIN path_dictionary p ON p.path=json_extract(r.value,'$.path') JOIN nodes n ON n.path_id=p.path_id AND n.kind NOT IN('{module}','file') AND n.line<=CAST(json_extract(r.value,'$.end') AS INTEGER) AND n.end_line>=CAST(json_extract(r.value,'$.start') AS INTEGER)",
        module = NodeKind::Module.as_str(),
    )
}

struct ChangedSeed {
    id: String,
    hash: i64,
    is_test: bool,
    record: Value,
}

fn changed_symbols_page(
    conn: &Connection,
    ranges: &[ChangedRange],
    options: &QueryOptions,
    generation: &str,
) -> Result<(Value, Vec<ChangedSeed>)> {
    let ranges_json = change_ranges_json(ranges)?;
    let from = changed_range_from_sql();
    let total = paging::count(
        conn,
        &format!("SELECT count(DISTINCT n.node_hash) FROM {from}"),
        &[&ranges_json],
    )?;
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let columns = paging::nodes_with_path("n", options.detail, "p.path");
    let sig_expr = "MAX(CASE WHEN n.line>=CAST(json_extract(r.value,'$.start') AS INTEGER) AND n.line<=CAST(json_extract(r.value,'$.end') AS INTEGER) THEN 1 ELSE 0 END) AS signature_touched";
    let mut page = rows(
        conn,
        &format!(
            "SELECT {columns},{sig_expr} FROM {from} GROUP BY n.node_hash ORDER BY p.path,n.line,n.id LIMIT ?2 OFFSET ?3"
        ),
        &[&ranges_json, &limit, &offset],
        options.limit,
    )?;
    for row in &mut page {
        let touched = row["signature_touched"].as_i64() == Some(1);
        row["signature_touched"] = json!(touched);
    }
    let seed_limit = i64::try_from(MAX_CHANGE_SEEDS.saturating_add(1))?;
    let seed_rows = rows(
        conn,
        &format!(
            "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line,n.end_line,n.is_test,{sig_expr} \
             FROM {from} GROUP BY n.node_hash ORDER BY p.path,n.line,n.id LIMIT ?2"
        ),
        &[&ranges_json, &seed_limit],
        MAX_CHANGE_SEEDS.saturating_add(1),
    )?;
    let mut seeds = Vec::with_capacity(seed_rows.len());
    for mut seed in seed_rows {
        let id = seed["id"]
            .as_str()
            .context("changed symbol has no public id")?
            .to_owned();
        let is_test = seed["is_test"].as_i64() == Some(1);
        let touched = seed["signature_touched"].as_i64() == Some(1);
        seed["signature_touched"] = json!(touched);
        if let Some(obj) = seed.as_object_mut() {
            obj.remove("is_test");
        }
        seeds.push(ChangedSeed {
            hash: stable_hash64(&id),
            id,
            is_test,
            record: seed,
        });
    }
    Ok((paging::value(page, total, options, generation), seeds))
}

fn changed_first_frontier(
    conn: &Connection,
    seed_hashes: &[i64],
    direction: ImpactDirection,
    forward: &str,
    reverse: &str,
) -> Result<usize> {
    let seed_json = serde_json::to_string(seed_hashes)?;
    let (from, to) = edge_endpoints(direction);
    let reverse_edges = if reverse.is_empty() {
        String::new()
    } else {
        format!(
            " UNION ALL SELECT 1 FROM edges e JOIN json_each(?1) s ON e.{to}=CAST(s.value AS INTEGER) WHERE e.kind IN({reverse})"
        )
    };
    paging::count(
        conn,
        &format!(
            "SELECT count(*) FROM (SELECT 1 FROM edges e JOIN json_each(?1) s ON e.{from}=CAST(s.value AS INTEGER) WHERE e.kind IN({forward}){reverse_edges})"
        ),
        &[&seed_json],
    )
}

fn changed_reachability_cte(steps: &str) -> String {
    format!(
        "WITH RECURSIVE walk(id,depth,visited) AS (SELECT CAST(value AS INTEGER),0,printf(',%lld,',CAST(value AS INTEGER)) FROM json_each(?1) UNION {steps} LIMIT {}), reached(id,distance) AS MATERIALIZED (SELECT id,min(depth) FROM walk GROUP BY id)",
        MAX_CHANGE_RESULT_NODES.saturating_add(1)
    )
}

fn changed_dependency_steps(
    direction: ImpactDirection,
    forward: &str,
    reverse: &str,
    excluded_csv: Option<&str>,
) -> String {
    let (from, to) = edge_endpoints(direction);
    let (fwd_ex, rev_ex) = match excluded_csv {
        Some(ex) => (
            format!(
                " AND e.path_id NOT IN({ex}) AND (SELECT path_id FROM nodes WHERE node_hash=e.{to}) NOT IN({ex})"
            ),
            format!(
                " AND e.path_id NOT IN({ex}) AND (SELECT path_id FROM nodes WHERE node_hash=e.{from}) NOT IN({ex})"
            ),
        ),
        None => (String::new(), String::new()),
    };
    let forward_step = format!(
        "SELECT e.{to},w.depth+1,w.visited||printf('%lld,',e.{to}) FROM walk w JOIN edges e ON e.{from}=w.id WHERE w.depth<?2 AND e.kind IN({forward}) AND instr(w.visited,printf(',%lld,',e.{to}))=0{fwd_ex}"
    );
    if reverse.is_empty() {
        forward_step
    } else {
        format!(
            "{forward_step} UNION SELECT e.{from},w.depth+1,w.visited||printf('%lld,',e.{from}) FROM walk w JOIN edges e ON e.{to}=w.id WHERE w.depth<?2 AND e.kind IN({reverse}) AND instr(w.visited,printf(',%lld,',e.{from}))=0{rev_ex}"
        )
    }
}

fn changed_nodes_page(
    conn: &Connection,
    seed_ids: &[String],
    seed_hashes: &[i64],
    request: &TraversalOptions<'_>,
    generation: &str,
    excluded_csv: Option<&str>,
) -> Result<Value> {
    if seed_hashes.is_empty() {
        return Ok(paging::value(Vec::new(), 0, request.page, generation));
    }
    let depth = request.depth;
    let direction = request.direction;
    let mode = request.mode;
    let (forward, reverse) = resolve_traversal_kinds(mode, request.edge_types, true);
    if depth > 0 {
        let immediate = changed_first_frontier(conn, seed_hashes, direction, &forward, &reverse)?;
        if immediate > MAX_DEEP_TRAVERSAL_FRONTIER {
            bail!("Git change view has {immediate} immediate graph links; narrow the changed files or use depth=1 before paging");
        }
    }
    let seed_json = serde_json::to_string(seed_hashes)?;
    let steps = changed_dependency_steps(direction, &forward, &reverse, excluded_csv);
    let cte = changed_reachability_cte(&steps);
    let walked = paging::count(
        conn,
        &format!("{cte} SELECT count(*) FROM walk"),
        &[&seed_json, &depth],
    )?;
    if walked > MAX_CHANGE_RESULT_NODES {
        bail!("Git change impact exceeds the {MAX_CHANGE_RESULT_NODES}-state work budget; narrow the changed files or reduce depth");
    }
    let node_ex = excluded_csv
        .map(|ex| format!(" WHERE n.path_id NOT IN({ex})"))
        .unwrap_or_default();
    let edge_ex = excluded_csv
        .map(|ex| format!(" AND e.path_id NOT IN({ex})"))
        .unwrap_or_default();
    let total = paging::count(
        conn,
        &format!("{cte} SELECT count(*) FROM reached r JOIN nodes n ON n.node_hash=r.id{node_ex}"),
        &[&seed_json, &depth],
    )?;
    if total > MAX_CHANGE_RESULT_NODES {
        bail!("Git change impact exceeds the {MAX_CHANGE_RESULT_NODES}-node work budget; narrow the changed files or reduce depth");
    }
    let options = request.page;
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let columns = paging::nodes("n", options.detail);
    let (from, to) = edge_endpoints(direction);
    let reverse_edge = if reverse.is_empty() {
        String::new()
    } else {
        format!(" OR (e.{to} IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) AND e.{from}=n.node_hash AND e.kind IN({reverse}))")
    };
    let evidence = if options.detail == Detail::Full {
        ", 'evidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.evidence_id)"
    } else {
        ""
    };
    let edge = format!(
        "CASE WHEN r.distance=1 THEN (SELECT json_object('src_public_id',(SELECT id FROM nodes WHERE node_hash=e.src_hash),'dst_public_id',(SELECT id FROM nodes WHERE node_hash=e.dst_hash),'kind',e.kind,'path',(SELECT path FROM path_dictionary WHERE path_id=e.path_id),'line',e.line,'confidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.confidence_id),'occurrence_count',e.occurrence_count,'direction',CASE WHEN e.src_hash IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) THEN 'outbound' ELSE 'inbound' END{evidence}) FROM edges e WHERE (((e.{from} IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) AND e.{to}=n.node_hash AND e.kind IN({forward})){reverse_edge}){edge_ex}) ORDER BY e.kind,e.src_hash,e.dst_hash,e.path_id,e.line,e.evidence_id LIMIT 1) END AS __edge"
    );
    let (forward_current, forward_predecessor, reverse_current, reverse_predecessor) =
        predecessor_endpoints(direction);
    let reverse_via = reverse_predecessor_union(
        &reverse,
        reverse_current,
        reverse_predecessor,
        "r.distance",
        "n.node_hash",
    );
    let via = format!(
        "CASE WHEN r.distance>1 THEN (SELECT json_array(predecessor,edge_kind) FROM (SELECT prior_node.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_predecessor} AND prior.distance=r.distance-1 JOIN nodes prior_node ON prior_node.node_hash=prior.id WHERE e.{forward_current}=n.node_hash AND e.kind IN({forward}){edge_ex}{reverse_via}) ORDER BY predecessor,edge_kind LIMIT 1) END AS __via"
    );
    let sql = format!(
        "{cte} SELECT {columns},r.distance,{edge},{via} FROM reached r JOIN nodes n ON n.node_hash=r.id{node_ex} ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4"
    );
    let raw_items = rows(
        conn,
        &sql,
        &[&seed_json, &depth, &limit, &offset],
        options.limit,
    )?;
    let mut items = Vec::with_capacity(raw_items.len());
    for raw in raw_items {
        let mut row: ReachedNodeDto = serde_json::from_value(raw)?;
        let id = row
            .fields
            .get("id")
            .and_then(Value::as_str)
            .context("change impact node has no id")?;
        let is_seed = seed_ids.iter().any(|seed| seed == id);
        let edge = decode_edge_descriptor(row.__edge.take().unwrap_or(Value::Null))?;
        let via = row.__via.take();
        row.is_seed = Some(is_seed);
        if !is_seed && !edge.is_null() {
            row.edge = Some(edge);
        } else if !is_seed {
            if row.distance == Some(1) {
                bail!("depth-one change impact node has no persisted edge evidence");
            }
            let (pred_id, pred_kind) = via
                .map(|encoded| serde_json::from_str::<(String, String)>(&encoded))
                .transpose()?
                .context("reached change impact node has no predecessor")?;
            row.via = Some(ViaPredecessorDto {
                id: pred_id,
                kind: pred_kind,
            });
        }
        items.push(serde_json::to_value(row)?);
    }
    Ok(paging::value(items, total, options, generation))
}

fn resolve_call_path_nodes(
    conn: &Connection,
    chains: &[Vec<i64>],
) -> Result<HashMap<i64, (String, String, i64)>> {
    let unique: Vec<i64> = chains
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if unique.is_empty() {
        return Ok(HashMap::new());
    }
    let hashes_json = serde_json::to_string(&unique)?;
    let mut stmt = conn.prepare(
        "SELECT n.node_hash, n.id, p.path, n.line \
         FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id \
         WHERE n.node_hash IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
    )?;
    let mut rows_iter = stmt.query([&hashes_json])?;
    let mut map = HashMap::with_capacity(unique.len());
    while let Some(row) = rows_iter.next()? {
        map.insert(row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?));
    }
    Ok(map)
}

fn verified_reaching_tests_page(
    conn: &Connection,
    seed_hashes: &[i64],
    depth: u32,
    options: &QueryOptions,
    generation: &str,
    excluded_csv: Option<&str>,
) -> Result<Value> {
    if seed_hashes.is_empty() {
        return Ok(paging::value(Vec::new(), 0, options, generation));
    }
    let seed_json = serde_json::to_string(seed_hashes)?;
    let calls = EdgeKind::Calls.as_str();
    let max_walk = MAX_CHANGE_RESULT_NODES.saturating_add(1);
    let step_ex = excluded_csv
        .map(|ex| {
            format!(
                " AND e.path_id NOT IN({ex}) AND (SELECT path_id FROM nodes WHERE node_hash=e.src_hash) NOT IN({ex})"
            )
        })
        .unwrap_or_default();
    let node_ex = excluded_csv
        .map(|ex| format!(" AND n.path_id NOT IN({ex})"))
        .unwrap_or_default();
    let cte = format!(
        "WITH RECURSIVE walk(id,depth,visited,chain) AS (\
           SELECT CAST(value AS INTEGER),0,printf(',%lld,',CAST(value AS INTEGER)),printf('%lld',CAST(value AS INTEGER)) FROM json_each(?1) \
           UNION \
           SELECT e.src_hash,w.depth+1,w.visited||printf('%lld,',e.src_hash),printf('%lld,',e.src_hash)||w.chain \
           FROM walk w JOIN edges e ON e.dst_hash=w.id \
           WHERE w.depth<?2 AND e.kind='{calls}' AND instr(w.visited,printf(',%lld,',e.src_hash))=0{step_ex} \
           LIMIT {max_walk}\
         ), reached(id,hops) AS MATERIALIZED (SELECT id,min(depth) FROM walk GROUP BY id)"
    );
    let walked = paging::count(
        conn,
        &format!("{cte} SELECT count(*) FROM walk"),
        &[&seed_json, &depth],
    )?;
    if walked > MAX_CHANGE_RESULT_NODES {
        bail!(
            "Git change test mapping exceeds the {MAX_CHANGE_RESULT_NODES}-state work budget; narrow the changed files or reduce depth"
        );
    }
    let filter = format!(
        "FROM reached r JOIN nodes n ON n.node_hash=r.id JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.is_test=1 AND json_extract(n.details,'$.test_role')='test_case'{node_ex}"
    );
    let total = paging::count(
        conn,
        &format!("{cte} SELECT count(*) {filter}"),
        &[&seed_json, &depth],
    )?;
    if total > MAX_CHANGE_RESULT_NODES {
        bail!("Git change test mapping exceeds the {MAX_CHANGE_RESULT_NODES}-test work budget");
    }
    let cols = paging::nodes_with_path("n", options.detail, "p.path");
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let mut items = rows(
        conn,
        &format!(
            "{cte} SELECT {cols},json_extract(n.details,'$.test_role') AS test_role,r.hops,\
             (SELECT w.chain FROM walk w WHERE w.id=r.id AND w.depth=r.hops ORDER BY w.chain LIMIT 1) AS __chain \
             {filter} ORDER BY r.hops,p.path,n.line,n.id LIMIT ?3 OFFSET ?4"
        ),
        &[&seed_json, &depth, &limit, &offset],
        options.limit,
    )?;
    let chains: Vec<Vec<i64>> = items
        .iter()
        .map(|item| {
            item["__chain"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .filter_map(|part| part.parse::<i64>().ok())
                .collect()
        })
        .collect();
    let node_info = resolve_call_path_nodes(conn, &chains)?;
    for (item, chain) in items.iter_mut().zip(chains) {
        if let Some(obj) = item.as_object_mut() {
            obj.remove("__chain");
            let call_path: Vec<Value> = chain
                .iter()
                .filter_map(|h| node_info.get(h).map(|(id, _, _)| json!(id)))
                .collect();
            let path_nodes: Vec<Value> = chain
                .iter()
                .filter_map(|h| {
                    node_info
                        .get(h)
                        .map(|(id, path, line)| json!({"id": id, "path": path, "line": line}))
                })
                .collect();
            obj.insert(
                "connection".to_owned(),
                json!({
                    "kind": calls,
                    "call_path": call_path,
                    "verified_call_path_nodes": path_nodes,
                }),
            );
        }
    }
    Ok(paging::value(items, total, options, generation))
}

fn governed_affected_docs_page(
    conn: &Connection,
    seed_hashes: &[i64],
    options: &QueryOptions,
    generation: &str,
    excluded_csv: Option<&str>,
) -> Result<Value> {
    if seed_hashes.is_empty() {
        return Ok(paging::value(Vec::new(), 0, options, generation));
    }
    let seed_json = serde_json::to_string(seed_hashes)?;
    let governs = EdgeKind::Governs.as_str();
    let refs_doc = EdgeKind::ReferencesDoc.as_str();
    let rule_key = NodeDetailKey::RULE;
    let inv_ex = excluded_csv
        .map(|ex| format!(" AND inv.path_id NOT IN({ex})"))
        .unwrap_or_default();
    let dn_ex = excluded_csv
        .map(|ex| format!(" AND dn.path_id NOT IN({ex})"))
        .unwrap_or_default();
    let body = format!(
        "SELECT COALESCE(d.doc_id,inv.id) AS doc_id,p.path AS path,\
                COALESCE(d.section_title,inv.name) AS section_title,\
                COALESCE(d.doc_type,'invariant') AS doc_type,\
                inv.name AS invariant_id,json_extract(inv.details,'$.{rule_key}') AS rule,\
                inv.line AS line,target.id AS target_symbol \
         FROM json_each(?1) s \
         JOIN edges e ON e.dst_hash=CAST(s.value AS INTEGER) AND e.kind='{governs}' \
         JOIN nodes inv ON inv.node_hash=e.src_hash \
         JOIN path_dictionary p ON p.path_id=inv.path_id \
         JOIN nodes target ON target.node_hash=e.dst_hash \
         LEFT JOIN doc_sections d ON d.path=p.path AND d.is_invariant=1 AND instr(d.invariants,inv.name)>0 \
         WHERE p.path NOT LIKE 'archive/%' AND p.path NOT LIKE '%/archive/%'{inv_ex} \
         UNION \
         SELECT d.doc_id AS doc_id,d.path AS path,d.section_title AS section_title,\
                d.doc_type AS doc_type,d.invariants AS invariant_id,NULL AS rule,\
                e.line AS line,target.id AS target_symbol \
         FROM json_each(?1) s \
         JOIN edges e ON e.src_hash=CAST(s.value AS INTEGER) AND e.kind='{refs_doc}' \
         JOIN nodes target ON target.node_hash=e.src_hash \
         JOIN nodes dn ON dn.node_hash=e.dst_hash \
         JOIN doc_sections d ON d.doc_id=dn.id \
         WHERE d.is_invariant=1 \
           AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%'{dn_ex} \
           AND NOT EXISTS (\
             SELECT 1 FROM edges ge JOIN nodes inv ON inv.node_hash=ge.src_hash \
             JOIN path_dictionary ip ON ip.path_id=inv.path_id \
             WHERE ge.dst_hash=e.src_hash AND ge.kind='{governs}' AND ip.path=d.path\
           )"
    );
    let total = paging::count(
        conn,
        &format!("SELECT count(*) FROM ({body})"),
        &[&seed_json],
    )?;
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let items = rows(
        conn,
        &format!(
            "SELECT * FROM ({body}) ORDER BY path,line,doc_id,invariant_id LIMIT ?2 OFFSET ?3"
        ),
        &[&seed_json, &limit, &offset],
        options.limit,
    )?;
    Ok(paging::value(items, total, options, generation))
}

fn uncovered_symbols_page(
    conn: &Connection,
    seeds: &[ChangedSeed],
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
    let prod_seeds: Vec<&ChangedSeed> = seeds.iter().filter(|s| !s.is_test).collect();
    if prod_seeds.is_empty() {
        return Ok(paging::value(Vec::new(), 0, options, generation));
    }
    let prod_hashes: Vec<i64> = prod_seeds.iter().map(|s| s.hash).collect();
    let covered: HashSet<i64> = symbols::verified_calls_covered_hashes(conn, Some(&prod_hashes))?;
    let uncovered: Vec<Value> = prod_seeds
        .into_iter()
        .filter(|s| !covered.contains(&s.hash))
        .map(|s| s.record.clone())
        .collect();
    let total = uncovered.len();
    let items = uncovered
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    Ok(paging::value(items, total, options, generation))
}

fn snapshot_file_values(snapshot: &ChangeSnapshot) -> Vec<Value> {
    snapshot
        .files
        .iter()
        .map(|file| {
            let ranges = file
                .ranges
                .iter()
                .map(|range| json!({"start":range.start,"end":range.end}))
                .collect::<Vec<_>>();
            let mut value = json!({"path":file.path,"status":file.status,"ranges":ranges});
            if let Some(old_path) = &file.old_path {
                value["old_path"] = json!(old_path);
            }
            value
        })
        .collect()
}

fn validate_snapshot_continuation(
    conn: &Connection,
    page_opts: &QueryOptions,
    fingerprint: &str,
) -> Result<String> {
    let mut page = page_opts.clone();
    page.generation = Some(read_metadata(conn, MetadataKey::OutputRoot)?);
    let graph_generation = paging::generation(conn, &page)?;
    if let Some(previous) = page_opts.generation.as_deref() {
        anyhow::ensure!(
            previous.len() >= 8 && fingerprint.starts_with(previous),
            "Git change view changed; restart at offset 0 without generation"
        );
    }
    if page_opts.offset > 0 && page_opts.generation.is_none() {
        bail!("continuation requires generation from the previous page");
    }
    Ok(graph_generation)
}

struct CoreChangeImpact {
    snapshot: ChangeSnapshot,
    excluded_csv: Option<String>,
    graph_generation: String,
    changed_symbols: Value,
    seeds: Vec<ChangedSeed>,
    seed_hashes: Vec<i64>,
    nodes: Value,
    unmapped_changes: Value,
    complete: bool,
}

fn compute_core_change_impact(
    conn: &Connection,
    root: &Path,
    mode: &str,
    path_scope: Option<&str>,
    base: Option<&str>,
    policy: Option<&WorkspacePathPolicy>,
    request: &TraversalOptions<'_>,
) -> Result<CoreChangeImpact> {
    let snapshot = collect_change_snapshot(conn, root, mode, path_scope, base, policy, false)?;
    let graph_generation =
        validate_snapshot_continuation(conn, request.page, &snapshot.fingerprint)?;
    let excluded_csv = excluded_ids_csv(&snapshot.exposure.disallowed_path_ids(conn)?);
    let (changed_symbols, mut seeds) =
        changed_symbols_page(conn, &snapshot.ranges, request.page, &snapshot.fingerprint)?;
    let seed_overflow = seeds.len() > MAX_CHANGE_SEEDS;
    seeds.truncate(MAX_CHANGE_SEEDS);
    let seed_ids: Vec<String> = seeds.iter().map(|s| s.id.clone()).collect();
    let seed_hashes: Vec<i64> = seeds.iter().map(|s| s.hash).collect();
    let mut unmapped = snapshot.unmapped.clone();
    if seed_overflow {
        unmapped.push(json!({"path":"*","reason":"seed_budget_exceeded","limit":MAX_CHANGE_SEEDS}));
    }
    let nodes = changed_nodes_page(
        conn,
        &seed_ids,
        &seed_hashes,
        request,
        &snapshot.fingerprint,
        excluded_csv.as_deref(),
    )?;
    let unmapped_total = unmapped.len();
    let unmapped_items = unmapped
        .into_iter()
        .skip(request.page.offset)
        .take(request.page.limit)
        .collect();
    let complete = unmapped_total == 0;
    let unmapped_changes = paging::value(
        unmapped_items,
        unmapped_total,
        request.page,
        &snapshot.fingerprint,
    );
    Ok(CoreChangeImpact {
        snapshot,
        excluded_csv,
        graph_generation,
        changed_symbols,
        seeds,
        seed_hashes,
        nodes,
        unmapped_changes,
        complete,
    })
}

fn ensure_snapshot_stable(
    conn: &Connection,
    root: &Path,
    mode: &str,
    path_scope: Option<&str>,
    base: Option<&str>,
    policy: Option<&WorkspacePathPolicy>,
    expected_fingerprint: &str,
) -> Result<()> {
    let final_snapshot =
        collect_change_snapshot(conn, root, mode, path_scope, base, policy, false)?;
    anyhow::ensure!(
        final_snapshot.fingerprint == expected_fingerprint,
        "Git change view changed while computing impact; restart at offset 0"
    );
    Ok(())
}

fn collect_symbol_seed_rows(
    conn: &Connection,
    sql: &str,
    param: &str,
    names: &mut BTreeSet<String>,
    hashes: &mut BTreeSet<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([param], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for (hash, name, qualname) in rows.flatten() {
        hashes.insert(hash);
        if !name.is_empty() {
            names.insert(name);
        }
        if !qualname.is_empty() {
            names.insert(qualname);
        }
    }
    Ok(())
}

/// Collect `(symbol_names_and_qualnames, changed_node_hashes)` for declarations overlapping
/// active Git diff hunks against `base` (defaulting to `HEAD`), plus declarations in newly
/// added/untracked indexed files (`diff_paths`).
pub fn collect_changed_symbol_seeds(
    conn: &Connection,
    root: &Path,
    base: Option<&str>,
    diff_paths: &BTreeSet<String>,
) -> Result<(BTreeSet<String>, BTreeSet<i64>)> {
    let mut names = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    let Ok(snapshot) = collect_change_snapshot(conn, root, "head", None, base, None, true) else {
        // Large or unavailable diff snapshots return empty symbol seeds cleanly;
        // file-level diff overlap in docs_domain ranking still handles them.
        return Ok((names, hashes));
    };
    let covered_paths: BTreeSet<String> = snapshot.files.iter().map(|f| f.path.clone()).collect();
    let effective_ranges: Vec<ChangedRange> = snapshot
        .files
        .iter()
        .flat_map(|file| file.ranges.iter().cloned())
        .collect();
    if !effective_ranges.is_empty() {
        let ranges_json = change_ranges_json(&effective_ranges)?;
        let from = changed_range_from_sql();
        let sql = format!(
            "SELECT DISTINCT n.node_hash, n.name, n.qualname FROM {from} \
             WHERE n.kind NOT IN ('doc', 'doc_section', 'invariant', 'component')"
        );
        collect_symbol_seed_rows(conn, &sql, &ranges_json, &mut names, &mut hashes)?;
    }
    let untracked_paths: BTreeSet<&String> = diff_paths
        .iter()
        .filter(|p| !covered_paths.contains(*p))
        .collect();
    if !untracked_paths.is_empty() {
        let paths_json = serde_json::to_string(&untracked_paths)?;
        let sql = "SELECT DISTINCT n.node_hash, n.name, n.qualname
             FROM nodes n
             JOIN path_dictionary p ON p.path_id = n.path_id
             WHERE p.path IN (SELECT value FROM json_each(?1))
               AND n.kind NOT IN ('file', 'module', 'doc', 'doc_section', 'invariant', 'component')";
        collect_symbol_seed_rows(conn, sql, &paths_json, &mut names, &mut hashes)?;
    }
    Ok((names, hashes))
}

pub(crate) fn diff_impact_with_options(
    conn: &Connection,
    root: &Path,
    target: &str,
    base: Option<&str>,
    policy: Option<&WorkspacePathPolicy>,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    let trimmed = strip_selector_uri_prefix(target.trim()).trim_end_matches('/');
    let path_scope = (!trimmed.is_empty() && trimmed != ".").then_some(trimmed);
    let request = TraversalOptions {
        depth: 4,
        direction: ImpactDirection::Inbound,
        mode: TraversalMode::Calls,
        edge_types: None,
        page: options,
    };
    let core = compute_core_change_impact(conn, root, "head", path_scope, base, policy, &request)?;
    let fp = &core.snapshot.fingerprint;
    let ex = core.excluded_csv.as_deref();
    budget.check()?;
    let tests = verified_reaching_tests_page(conn, &core.seed_hashes, 12, options, fp, ex)?;
    budget.check()?;
    let uncovered_symbols = uncovered_symbols_page(conn, &core.seeds, options, fp)?;
    budget.check()?;
    let affected_docs = governed_affected_docs_page(conn, &core.seed_hashes, options, fp, ex)?;
    ensure_snapshot_stable(conn, root, "head", path_scope, base, policy, fp)?;
    Ok(json!({
        "mode": "diff_impact",
        "target": path_scope.unwrap_or(""),
        "base": base,
        "generation": fp,
        "graph_generation": core.graph_generation,
        "changes": {"mode": core.snapshot.mode, "base": base, "files": snapshot_file_values(&core.snapshot)},
        "changed_symbols": core.changed_symbols,
        "nodes": core.nodes,
        "tests": tests,
        "uncovered_symbols": uncovered_symbols,
        "affected_docs": affected_docs,
        "unmapped_changes": core.unmapped_changes,
        "complete": core.complete,
    }))
}

pub(super) fn changed_impact_with_path_options(
    conn: &Connection,
    root: &Path,
    mode: &str,
    path_scope: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    if request.depth > 16 {
        bail!("depth must be <=16");
    }
    let core = compute_core_change_impact(conn, root, mode, path_scope, None, None, request)?;
    let fp = &core.snapshot.fingerprint;
    let ex = core.excluded_csv.as_deref();
    let tests = verified_reaching_tests_page(
        conn,
        &core.seed_hashes,
        request.depth.max(12),
        request.page,
        fp,
        ex,
    )?;
    ensure_snapshot_stable(conn, root, mode, path_scope, None, None, fp)?;
    Ok(json!({
        "selector":{"kind":"git_changes","mode":core.snapshot.mode},
        "direction":request.direction.as_str(),
        "depth":request.depth,
        "mode":request.mode.as_str(),
        "generation":fp,
        "graph_generation":core.graph_generation,
        "changes":{"mode":core.snapshot.mode,"files":snapshot_file_values(&core.snapshot)},
        "changed_symbols":core.changed_symbols,
        "nodes":core.nodes,
        "tests":tests,
        "unmapped_changes":core.unmapped_changes,
        "complete":core.complete
    }))
}
