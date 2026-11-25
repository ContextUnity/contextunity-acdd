use super::types::{
    decode_edge_descriptor, edge_endpoints, resolve_traversal_kinds, ImpactDirection,
    ReachedNodeDto, TraversalMode, TraversalOptions, ViaPredecessorDto,
    MAX_DEEP_TRAVERSAL_FRONTIER,
};
use crate::db::{
    commitments::{self, read_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES},
    paging, reader, source, symbols,
};
use crate::models::{stable_hash64, tags::NodeKind};
use acdd_core::{
    config, git,
    response::{Detail, QueryOptions, MAX_PAGE_SIZE},
};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
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

fn git_diff_prefix(mode: &str) -> Vec<String> {
    let mut args = vec!["diff".to_owned()];
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
) -> Result<ChangeSnapshot> {
    anyhow::ensure!(
        matches!(mode, "staged" | "unstaged" | "head"),
        "changes must be staged, unstaged, or head"
    );
    if let Some(path) = path_scope {
        let valid = valid_workspace_path(path);
        anyhow::ensure!(valid, "path must be workspace-relative");
    }
    anyhow::ensure!(
        root.join(".git").exists(),
        "Git change views require a Git repository"
    );

    let mut status_args = git_diff_prefix(mode);
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
    let changes = parse_name_status(&status_bytes)?;
    anyhow::ensure!(
        changes.len() <= MAX_CHANGE_FILES,
        "Git change view exceeds the {MAX_CHANGE_FILES}-file work budget"
    );
    let graph_generation = read_metadata(conn, MetadataKey::OutputRoot)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(b"acdd-impact-git-change-view-v1");
    append_fingerprint(&mut fingerprint, graph_generation.as_bytes());
    append_fingerprint(&mut fingerprint, mode.as_bytes());
    append_fingerprint(&mut fingerprint, path_scope.unwrap_or("").as_bytes());
    append_fingerprint(&mut fingerprint, &status_bytes);

    let mut files = Vec::with_capacity(changes.len());
    let mut ranges = Vec::new();
    let mut unmapped = Vec::new();
    let mut total_diff_bytes = status_bytes.len();
    let mut worktree_adapter: Option<Option<config::Adapter>> = None;
    for (status, path, old_path) in changes {
        if !path_is_within(&path, path_scope) {
            continue;
        }
        let mut patch_args = git_diff_prefix(mode);
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
        } else if mode == "staged" {
            let object = format!(":0:{path}");
            let output = git::git_command(root, &["show", &object])?;
            output.status.success().then_some(output.stdout)
        } else {
            let adapter =
                worktree_adapter.get_or_insert_with(|| config::load_adapter(root, None).ok());
            adapter
                .as_ref()
                .and_then(|adapter| worktree_postimage(root, adapter, &path).ok())
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

fn changed_symbols_page(
    conn: &Connection,
    ranges: &[ChangedRange],
    options: &QueryOptions,
    generation: &str,
) -> Result<(Value, Vec<String>, Vec<i64>)> {
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
    let page = reader::rows(
        conn,
        &format!(
            "SELECT DISTINCT {columns} FROM {from} ORDER BY p.path,n.line,n.id LIMIT ?2 OFFSET ?3"
        ),
        &[&ranges_json, &limit, &offset],
        options.limit,
    )?;
    let seed_limit = i64::try_from(MAX_CHANGE_SEEDS.saturating_add(1))?;
    let seeds = reader::rows(
        conn,
        &format!("SELECT DISTINCT n.id FROM {from} ORDER BY p.path,n.line,n.id LIMIT ?2"),
        &[&ranges_json, &seed_limit],
        MAX_CHANGE_SEEDS.saturating_add(1),
    )?;
    let mut ids = Vec::with_capacity(seeds.len());
    let mut hashes = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let id = seed["id"]
            .as_str()
            .context("changed symbol has no public id")?
            .to_owned();
        hashes.push(stable_hash64(&id));
        ids.push(id);
    }
    Ok((paging::value(page, total, options, generation), ids, hashes))
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
) -> String {
    let (from, to) = edge_endpoints(direction);
    let forward_step = format!(
        "SELECT e.{to},w.depth+1,w.visited||printf('%lld,',e.{to}) FROM walk w JOIN edges e ON e.{from}=w.id WHERE w.depth<?2 AND e.kind IN({forward}) AND instr(w.visited,printf(',%lld,',e.{to}))=0"
    );
    if reverse.is_empty() {
        forward_step
    } else {
        format!(
            "{forward_step} UNION SELECT e.{from},w.depth+1,w.visited||printf('%lld,',e.{from}) FROM walk w JOIN edges e ON e.{to}=w.id WHERE w.depth<?2 AND e.kind IN({reverse}) AND instr(w.visited,printf(',%lld,',e.{from}))=0"
        )
    }
}

fn changed_nodes_page(
    conn: &Connection,
    seed_ids: &[String],
    seed_hashes: &[i64],
    request: &TraversalOptions<'_>,
    direction: ImpactDirection,
    mode: TraversalMode,
    generation: &str,
) -> Result<Value> {
    if seed_hashes.is_empty() {
        return Ok(paging::value(Vec::new(), 0, request.page, generation));
    }
    let depth = request.depth;
    let (forward, reverse) = resolve_traversal_kinds(mode, request.edge_types, true);
    if depth > 0 {
        let immediate = changed_first_frontier(conn, seed_hashes, direction, &forward, &reverse)?;
        if immediate > MAX_DEEP_TRAVERSAL_FRONTIER {
            bail!("Git change view has {immediate} immediate graph links; narrow the changed files or use depth=1 before paging");
        }
    }
    let seed_json = serde_json::to_string(seed_hashes)?;
    let steps = changed_dependency_steps(direction, &forward, &reverse);
    let cte = changed_reachability_cte(&steps);
    let walked = paging::count(
        conn,
        &format!("{cte} SELECT count(*) FROM walk"),
        &[&seed_json, &depth],
    )?;
    if walked > MAX_CHANGE_RESULT_NODES {
        bail!("Git change impact exceeds the {MAX_CHANGE_RESULT_NODES}-state work budget; narrow the changed files or reduce depth");
    }
    let total = paging::count(
        conn,
        &format!("{cte} SELECT count(*) FROM reached"),
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
        "CASE WHEN r.distance=1 THEN (SELECT json_object('src_public_id',(SELECT id FROM nodes WHERE node_hash=e.src_hash),'dst_public_id',(SELECT id FROM nodes WHERE node_hash=e.dst_hash),'kind',e.kind,'path',(SELECT path FROM path_dictionary WHERE path_id=e.path_id),'line',e.line,'confidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.confidence_id),'occurrence_count',e.occurrence_count,'direction',CASE WHEN e.src_hash IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) THEN 'outbound' ELSE 'inbound' END{evidence}) FROM edges e WHERE ((e.{from} IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) AND e.{to}=n.node_hash AND e.kind IN({forward})){reverse_edge}) ORDER BY e.kind,e.src_hash,e.dst_hash,e.path_id,e.line,e.evidence_id LIMIT 1) END AS __edge"
    );
    let (forward_current, forward_predecessor, reverse_current, reverse_predecessor) =
        super::predecessor_endpoints(direction);
    let reverse_via = super::reverse_predecessor_union(
        &reverse,
        reverse_current,
        reverse_predecessor,
        "r.distance",
        "n.node_hash",
    );
    let via = format!(
        "CASE WHEN r.distance>1 THEN (SELECT json_array(predecessor,edge_kind) FROM (SELECT prior_node.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_predecessor} AND prior.distance=r.distance-1 JOIN nodes prior_node ON prior_node.node_hash=prior.id WHERE e.{forward_current}=n.node_hash AND e.kind IN({forward}){reverse_via}) ORDER BY predecessor,edge_kind LIMIT 1) END AS __via"
    );
    let sql = format!(
        "{cte} SELECT {columns},r.distance,{edge},{via} FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4"
    );
    let raw_items = reader::rows(
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

fn changed_tests_page(
    conn: &Connection,
    seed_ids: &[String],
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
    let mut tests = BTreeMap::<String, Value>::new();
    for seed in seed_ids {
        let mut offset = 0;
        let mut index_generation: Option<String> = None;
        loop {
            let page_options = QueryOptions {
                limit: MAX_PAGE_SIZE,
                offset,
                detail: options.detail,
                generation: index_generation.clone(),
            };
            let page = symbols::tests_paged_with_path(
                conn,
                seed,
                None,
                ImpactDirection::Inbound.as_str(),
                &page_options,
            )?;
            let nodes = &page["nodes"];
            let total = nodes["total"]
                .as_u64()
                .context("changed test total is missing")? as usize;
            if total > MAX_CHANGE_RESULT_NODES {
                bail!("Git change test mapping exceeds the {MAX_CHANGE_RESULT_NODES}-test work budget");
            }
            index_generation = nodes["generation"].as_str().map(str::to_owned);
            if let Some(items) = nodes["items"].as_array() {
                for item in items {
                    let id = item["id"]
                        .as_str()
                        .context("changed test result has no id")?
                        .to_owned();
                    match tests.entry(id) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(item.clone());
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            let new_depth = item["connection"]["call_path"]
                                .as_array()
                                .map_or(usize::MAX, Vec::len);
                            let old_depth = entry.get()["connection"]["call_path"]
                                .as_array()
                                .map_or(usize::MAX, Vec::len);
                            if new_depth < old_depth {
                                entry.insert(item.clone());
                            }
                        }
                    }
                    if tests.len() > MAX_CHANGE_RESULT_NODES {
                        bail!("Git change test mapping exceeds the {MAX_CHANGE_RESULT_NODES}-test work budget");
                    }
                }
            }
            if nodes["has_more"] != true {
                break;
            }
            offset = nodes["next_offset"]
                .as_u64()
                .context("changed test continuation is missing")? as usize;
        }
    }
    let total = tests.len();
    let items = tests
        .into_values()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    Ok(paging::value(items, total, options, generation))
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
    let direction = request.direction;
    let traversal_mode = request.mode;
    let snapshot = collect_change_snapshot(conn, root, mode, path_scope)?;
    let mut page = request.page.clone();
    page.generation = Some(read_metadata(conn, MetadataKey::OutputRoot)?);
    let graph_generation = paging::generation(conn, &page)?;
    if let Some(previous) = request.page.generation.as_deref() {
        anyhow::ensure!(
            previous.len() >= 8 && snapshot.fingerprint.starts_with(previous),
            "Git change view changed; restart at offset 0 without generation"
        );
    }
    if request.page.offset > 0 && request.page.generation.is_none() {
        bail!("continuation requires generation from the previous page");
    }
    let (changed_symbols, mut seed_ids, mut seed_hashes) =
        changed_symbols_page(conn, &snapshot.ranges, request.page, &snapshot.fingerprint)?;
    let seed_overflow = seed_ids.len() > MAX_CHANGE_SEEDS;
    seed_ids.truncate(MAX_CHANGE_SEEDS);
    seed_hashes.truncate(MAX_CHANGE_SEEDS);
    let mut unmapped = snapshot.unmapped.clone();
    if seed_overflow {
        unmapped.push(json!({"path":"*","reason":"seed_budget_exceeded","limit":MAX_CHANGE_SEEDS}));
    }
    let nodes = changed_nodes_page(
        conn,
        &seed_ids,
        &seed_hashes,
        request,
        direction,
        traversal_mode,
        &snapshot.fingerprint,
    )?;
    let tests = changed_tests_page(conn, &seed_ids, request.page, &snapshot.fingerprint)?;
    let file_values = snapshot
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
        .collect::<Vec<_>>();
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
    let final_snapshot = collect_change_snapshot(conn, root, mode, path_scope)?;
    anyhow::ensure!(
        final_snapshot.fingerprint == snapshot.fingerprint,
        "Git change view changed while computing impact; restart at offset 0"
    );
    let dir_str = direction.as_str();
    Ok(json!({
        "selector":{"kind":"git_changes","mode":snapshot.mode},
        "direction":dir_str,
        "depth":request.depth,
        "mode":traversal_mode.as_str(),
        "generation":snapshot.fingerprint,
        "graph_generation":graph_generation,
        "changes":{"mode":snapshot.mode,"files":file_values},
        "changed_symbols":changed_symbols,
        "nodes":nodes,
        "tests":tests,
        "unmapped_changes":unmapped_changes,
        "complete":complete
    }))
}
